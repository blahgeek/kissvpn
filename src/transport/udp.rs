use std::{net::{SocketAddr, ToSocketAddrs, UdpSocket}, ops::Deref, sync::Mutex};

use crate::constants::UDP_MTU;

use super::Transport;

use anyhow::Result;
use bytes::{Bytes, BytesMut};

pub struct UdpClientTransport {
    sock: UdpSocket,
}

impl UdpClientTransport {
    pub fn create<TL, TR>(local_addr: TL, remote_addr: TR)
                          -> Result<UdpClientTransport>
    where TL: ToSocketAddrs, TR: ToSocketAddrs {
        let sock = UdpSocket::bind(local_addr)?;
        sock.connect(remote_addr)?;
        Ok(UdpClientTransport {
            sock,
        })
    }
}

impl Transport for UdpClientTransport {
    fn send(&self, buf: Bytes) -> Result<()> {
        match self.sock.send(&buf) {
            // connection_refused is OK (server not started), nothing to do
            Err(e) if e.kind() != std::io::ErrorKind::ConnectionRefused
                => return Err(e)?,
            _ => return Ok(())
        }
    }

    fn receive(&self) -> Result<BytesMut> {
        // TODO: remove copy
        let mut buf = vec![0u8; UDP_MTU];
        loop {
            match self.sock.recv(&mut buf) {
                Ok(buf_len) => return Ok(BytesMut::from(&buf[0..buf_len])),
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
    fn send(&self, buf: Bytes) -> Result<()> {
        let peer_addr = self.peer_addr.lock().unwrap().ok_or(
            std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "No valid client yet"))?;
        self.sock.send_to(&buf, peer_addr)?;
        Ok(())
    }

    fn receive(&self) -> Result<BytesMut> {
        let mut buf = vec![0u8; UDP_MTU];
        let (buf_len, peer_addr) = self.sock.recv_from(&mut buf)?;
        let _ = self.last_peer_addr.lock().unwrap().insert(peer_addr);
        Ok(BytesMut::from(&buf[0..buf_len]))
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

    fn test_basic_send_receive() -> Result<()> {
        let server = UdpServerTransport::create("127.0.0.1:9999")?;
        let client = UdpClientTransport::create("127.0.0.1:0", "127.0.0.1:9999")?;
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
}
