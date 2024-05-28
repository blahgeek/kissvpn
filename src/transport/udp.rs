use core::slice;
use std::time;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::{collections::VecDeque, ops::Deref, sync::{Arc, Mutex}};

use crate::constants::UDP_MTU;

use super::Transport;

use anyhow::Result;
use bytes::{Buf, BytesMut};
use rand::RngCore;
use nix::sys::epoll::{Epoll, EpollEvent, EpollFlags, EpollTimeout};

pub struct UdpClientTransportOptions {
    /// Max number of sockets at each timepoint
    pub max_num_sockets: usize,
    /// Max duration that each socket is used for sending
    pub socket_send_duration: time::Duration,
    /// Max extra duration that each socket is used for receiving after finished sending
    pub socket_lingering_duration: time::Duration,
}

impl Default for UdpClientTransportOptions {
    fn default() -> Self {
        Self {
            max_num_sockets: 10,
            socket_send_duration: time::Duration::from_secs(60),
            socket_lingering_duration: time::Duration::from_secs(60),
        }
    }
}

struct SockContext {
    sock: Arc<UdpSocket>,
    created: time::Instant,
}

struct SockContexts {
    ctxs: VecDeque<SockContext>,
    first_id: u64,   // n-th element in socks has id of socks_first_id + n; useful for epoll
}

pub struct UdpClientTransport {
    remote_addr: SocketAddr,
    options: UdpClientTransportOptions,
    sock_ctxs: Mutex<SockContexts>,
    epoll: Epoll,
}

impl UdpClientTransport {
    pub fn create<TR>(remote_addr: TR, options: UdpClientTransportOptions) -> Result<UdpClientTransport>
    where TR: ToSocketAddrs {
        let remote_addr = remote_addr.to_socket_addrs()?
            .next().ok_or(anyhow::format_err!("lookup_host failed"))?;
        Ok(UdpClientTransport {
            remote_addr,
            options,
            sock_ctxs: Mutex::new(SockContexts {
                ctxs: VecDeque::default(),
                first_id: 0,
            }),
            epoll: Epoll::new(nix::sys::epoll::EpollCreateFlags::empty())?,
        })
    }

    fn get_or_create_socket(&self) -> Result<Arc<UdpSocket>> {
        let mut sock_ctxs = self.sock_ctxs.lock().unwrap();

        // clear outdated sockets
        let now = time::Instant::now();
        let outdated_time = now - self.options.socket_send_duration - self.options.socket_lingering_duration;
        while !sock_ctxs.ctxs.is_empty() {
            let first = sock_ctxs.ctxs.front().unwrap();
            if first.created >= outdated_time {
                break;
            }
            sock_ctxs.ctxs.pop_front();
            sock_ctxs.first_id += 1;
        }

        // create new socket if required
        if sock_ctxs.ctxs.len() < self.options.max_num_sockets {
            let sock = UdpSocket::bind("0.0.0.0:0")?;
            // read timeout should not happen because we use epoll, just in case
            sock.set_read_timeout(Some(std::time::Duration::from_millis(1)))?;
            sock.connect(self.remote_addr)?;

            let sock = Arc::new(sock);
            sock_ctxs.ctxs.push_back(SockContext {
                sock: sock.clone(),
                created: now,
            });

            // add to epoll
            let sock_id = sock_ctxs.first_id + sock_ctxs.ctxs.len() as u64 - 1;
            self.epoll.add(sock.as_ref(), EpollEvent::new(EpollFlags::EPOLLIN, sock_id))?;

            return Ok(sock);
        }

        let rand_id = rand::thread_rng().next_u32() as usize % sock_ctxs.ctxs.len();
        return Ok(sock_ctxs.ctxs[rand_id].sock.clone());
    }

    fn get_socket_by_id(&self, id: u64) -> Option<Arc<UdpSocket>> {
        let sock_ctxs = self.sock_ctxs.lock().unwrap();

        if id < sock_ctxs.first_id || id >= sock_ctxs.first_id + sock_ctxs.ctxs.len() as u64 {
            return None
        }
        return Some(sock_ctxs.ctxs[(id - sock_ctxs.first_id) as usize].sock.clone());
    }
}

impl Transport for UdpClientTransport {
    fn send(&self, mut buf: impl Buf) -> Result<()> {
        let sock = self.get_or_create_socket()?;
        match sock.send(&buf.copy_to_bytes(buf.remaining())) {
            // connection_refused is OK (server not started), nothing to do
            Err(e) if e.kind() != std::io::ErrorKind::ConnectionRefused
                => return Err(e)?,
            _ => return Ok(())
        }
    }

    fn receive(&self) -> Result<BytesMut> {
        loop {
            let mut epoll_event = EpollEvent::empty();
            let epoll_event_size =
                self.epoll.wait(slice::from_mut(&mut epoll_event), EpollTimeout::NONE)?;
            assert_eq!(epoll_event_size, 1);
            // TODO: handle error
            assert_eq!(epoll_event.events(), EpollFlags::EPOLLIN);

            let sock = self.get_socket_by_id(epoll_event.data()).unwrap();
            let mut buf = BytesMut::zeroed(UDP_MTU);
            match sock.recv(&mut buf) {
                Ok(buf_len) => {
                    buf.truncate(buf_len);
                    return Ok(buf)
                },
                // connection_refused is OK (server not started), keep retring
                Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused
                    => continue,
                Err(e) => return Err(e)?,
            }
        }
    }
}


pub struct UdpServerTransport {
    sock: UdpSocket,
    peer_addr: Mutex<Option<SocketAddr>>,
    last_peer_addr: Mutex<Option<SocketAddr>>,
}

impl UdpServerTransport {
    pub fn create<T>(local_addr: T) -> Result<UdpServerTransport>
    where T: ToSocketAddrs {
        let sock = UdpSocket::bind(local_addr)?;
        Ok(UdpServerTransport {
            sock,
            peer_addr: Mutex::new(None),
            last_peer_addr: Mutex::new(None),
        })
    }
}

impl Transport for UdpServerTransport {
    fn send(&self, mut buf: impl Buf) -> Result<()> {
        let peer_addr = self.peer_addr.lock().unwrap().ok_or(
            std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "No valid client yet"))?;
        self.sock.send_to(&buf.copy_to_bytes(buf.remaining()), peer_addr)?;
        Ok(())
    }

    fn receive(&self) -> Result<BytesMut> {
        let mut buf = BytesMut::zeroed(UDP_MTU);
        let (buf_len, peer_addr) = self.sock.recv_from(&mut buf)?;
        let _ = self.last_peer_addr.lock().unwrap().insert(peer_addr);
        buf.truncate(buf_len);
        Ok(buf)
    }

    fn mark_last_received_valid(&self) {
        if let Some(peer_addr) = self.last_peer_addr.lock().unwrap().deref() {
            let _ = self.peer_addr.lock().unwrap().insert(peer_addr.clone());
        }
    }

    fn ready_to_send(&self) -> bool {
        self.peer_addr.lock().unwrap().is_some()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_basic_send_receive() -> Result<()> {
        let server = UdpServerTransport::create("127.0.0.1:9999")?;
        let client = UdpClientTransport::create("127.0.0.1:9999", UdpClientTransportOptions::default())?;
        assert!(!server.ready_to_send());
        assert!(client.ready_to_send());

        let payload = Bytes::from("hello world!");
        client.send(payload.clone())?;

        {
            let received_payload = server.receive()?;
            assert_eq!(payload, received_payload);
        }

        assert!(!server.ready_to_send());
        server.mark_last_received_valid();
        assert!(server.ready_to_send());

        server.send(payload.clone())?;

        {
            let received_payload = client.receive()?;
            assert_eq!(payload, received_payload);
        }

        Ok(())
    }

    #[test]
    fn test_multiple_request_response() -> Result<()> {
        fn _run_server() -> Result<()> {
            let server = UdpServerTransport::create("127.0.0.1:9998").unwrap();

            loop {
                let received = server.receive()?;
                if received.len() == 0 {
                    return Ok(());
                }
                server.mark_last_received_valid();
                server.send(received)?;
            }
        }
        let server_thread = std::thread::spawn(|| {
            _run_server().expect("run server error");
        });

        let client = UdpClientTransport::create("127.0.0.1:9998", UdpClientTransportOptions::default())?;
        for i in 0..10000 {
            let payload_str = format!("{}", i);
            let payload = payload_str.as_bytes();
            client.send(payload)?;

            let received = client.receive()?;
            assert_eq!(payload, received);
        }
        client.send(&[] as &[u8])?;

        server_thread.join().unwrap();
        Ok(())
    }
}
