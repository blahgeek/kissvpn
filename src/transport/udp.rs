use core::slice;
use std::collections::{btree_map, BTreeMap};
use std::time;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::{ops::Deref, sync::{Arc, Mutex}};

use crate::constants::UDP_MTU;

use super::Transport;

use anyhow::Result;
use log::{info, trace, warn};
use bytes::{Buf, BytesMut};
use rand::RngCore;
use nix::sys::epoll::{Epoll, EpollEvent, EpollFlags, EpollTimeout};

pub struct UdpClientTransportOptions {
    /// Max number of sockets at each timepoint
    pub max_send_sockets: usize,
    /// Max duration that each socket is used for sending
    pub socket_send_duration: time::Duration,
    /// Max extra duration that each socket is used for receiving after finished sending
    pub socket_lingering_duration: time::Duration,
}

impl Default for UdpClientTransportOptions {
    fn default() -> Self {
        Self {
            max_send_sockets: 10,
            socket_send_duration: time::Duration::from_secs(60),
            socket_lingering_duration: time::Duration::from_secs(60),
        }
    }
}

struct SockContext {
    sock: Arc<UdpSocket>,
    created: time::Instant,
}

pub struct UdpClientTransport {
    remote_addr: SocketAddr,
    options: UdpClientTransportOptions,
    sock_ctxs: Mutex<BTreeMap<u64, SockContext>>,
    epoll: Epoll,
}

impl UdpClientTransport {
    pub fn create<TR>(remote_addr: TR, options: UdpClientTransportOptions) -> Result<UdpClientTransport>
    where TR: ToSocketAddrs {
        let remote_addr = remote_addr.to_socket_addrs()?
            .next().ok_or(anyhow::format_err!("lookup_host failed"))?;
        info!("Creating udp client transport to {remote_addr}");
        Ok(UdpClientTransport {
            remote_addr,
            options,
            sock_ctxs: Mutex::new(BTreeMap::new()),
            epoll: Epoll::new(nix::sys::epoll::EpollCreateFlags::empty())?,
        })
    }

    fn get_or_create_socket_for_sending(&self) -> Result<(u64, Arc<UdpSocket>)> {
        let mut sock_ctxs = self.sock_ctxs.lock().unwrap();

        // clear outdated sockets
        let now = time::Instant::now();
        let outdated_time = now - self.options.socket_send_duration - self.options.socket_lingering_duration;
        while !sock_ctxs.is_empty() {
            let first = sock_ctxs.first_entry().unwrap();
            if first.get().created >= outdated_time {
                break;
            }
            self.epoll.delete(&first.get().sock)?;
            first.remove_entry();
        }

        // find all sockets avaibale for sending
        let available_socks: Vec<(u64, Arc<UdpSocket>)> =
            sock_ctxs.iter()
            .filter_map(|(id, x)| {
                if x.created >= now - self.options.socket_send_duration {
                    Some((id.clone(), x.sock.clone()))
                } else {
                    None
                }
            })
            .collect();

        // create new socket if required
        if available_socks.len() < self.options.max_send_sockets {
            trace!("Creating new udp socket");
            let sock = UdpSocket::bind("0.0.0.0:0")?;
            // read timeout should not happen because we use epoll, just in case
            sock.set_read_timeout(Some(std::time::Duration::from_millis(1)))?;
            sock.connect(self.remote_addr)?;

            let sock = Arc::new(sock);
            let sock_id = sock_ctxs.last_entry().map_or(0, |e| e.key() + 1);
            sock_ctxs.insert(sock_id, SockContext {
                sock: sock.clone(),
                created: now,
            });

            // add to epoll
            self.epoll.add(sock.as_ref(), EpollEvent::new(EpollFlags::EPOLLIN, sock_id))?;

            return Ok((sock_id, sock));
        }

        let rand_id = rand::thread_rng().next_u32() as usize % available_socks.len();
        return Ok(available_socks[rand_id].clone());
    }

    fn get_socket_by_id(&self, id: u64) -> Option<Arc<UdpSocket>> {
        let sock_ctxs = self.sock_ctxs.lock().unwrap();
        sock_ctxs.get(&id).map(|x| x.sock.clone())
    }

    fn remove_socket_by_id(&self, id: u64) {
        let mut sock_ctxs = self.sock_ctxs.lock().unwrap();
        if let btree_map::Entry::Occupied(e) = sock_ctxs.entry(id) {
            self.epoll.delete(&e.get().sock).expect("epoll delete error");
            e.remove_entry();
        }
    }
}

impl Transport for UdpClientTransport {
    fn needs_keepalive(&self) -> bool { true }

    fn send(&self, mut buf: impl Buf) -> Result<()> {
        let (sock_id, sock) = self.get_or_create_socket_for_sending()?;
        match sock.send(&buf.copy_to_bytes(buf.remaining())) {
            Err(e) => {
                trace!("Udp send error: {}", e);
                // connection_refused is OK (server not started)
                if e.kind() != std::io::ErrorKind::ConnectionRefused {
                    self.remove_socket_by_id(sock_id);
                }
                return Err(e)?
            },
            _ => return Ok(())
        }
    }

    fn receive(&self) -> Result<BytesMut> {
        let mut epoll_event = EpollEvent::empty();
        let epoll_event_size =
            self.epoll.wait(slice::from_mut(&mut epoll_event), EpollTimeout::NONE)?;
        assert_eq!(epoll_event_size, 1);

        if epoll_event.events() != EpollFlags::EPOLLIN {
            trace!("epoll result contains error: {:?}", epoll_event.events());
        }

        let sock_id = epoll_event.data();
        let sock = self.get_socket_by_id(sock_id).unwrap();
        let mut buf = BytesMut::zeroed(UDP_MTU);
        match sock.recv(&mut buf) {
            Ok(buf_len) => {
                buf.truncate(buf_len);
                return Ok(buf)
            },
            Err(e) => {
                warn!("Udp receive error: {e}");
                // connection_refused is OK (server not started), keep retring
                if e.kind() != std::io::ErrorKind::ConnectionRefused {
                    self.remove_socket_by_id(sock_id);
                }
                return Err(e)?
            },
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
        let local_addr = local_addr.to_socket_addrs()?
            .next().ok_or(anyhow::format_err!("lookup_host failed"))?;
        info!("Creating udp server transport on {local_addr}");
        let sock = UdpSocket::bind(local_addr)?;
        Ok(UdpServerTransport {
            sock,
            peer_addr: Mutex::new(None),
            last_peer_addr: Mutex::new(None),
        })
    }
}

impl Transport for UdpServerTransport {
    fn needs_keepalive(&self) -> bool { false }

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
