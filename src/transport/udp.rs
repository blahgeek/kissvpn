use std::{net::SocketAddr, sync::Mutex, ops::Deref};

use crate::constants::UDP_MTU;

use super::Transport;

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use async_trait::async_trait;
use tokio::net::{UdpSocket, ToSocketAddrs};

const MTU: usize = 1450;

pub struct UdpClientTransport {
    sock: UdpSocket,
}

impl UdpClientTransport {
    pub async fn create<TL, TR>(local_addr: TL, remote_addr: TR)
                                -> Result<UdpClientTransport>
    where TL: ToSocketAddrs, TR: ToSocketAddrs {
        let sock = UdpSocket::bind(local_addr).await?;
        sock.connect(remote_addr).await?;
        Ok(UdpClientTransport {
            sock,
        })
    }
}

#[async_trait]
impl Transport for UdpClientTransport {
    async fn send(&self, buf: Bytes) -> Result<()> {
        match self.sock.send(&buf).await {
            // connection_refused is OK (server not started), nothing to do
            Err(e) if e.kind() != std::io::ErrorKind::ConnectionRefused
                => return Err(e)?,
            _ => return Ok(())
        }
    }

    async fn receive(&self) -> Result<BytesMut> {
        let mut buf = BytesMut::with_capacity(UDP_MTU);
        loop {
            match self.sock.recv_buf(&mut buf).await {
                Ok(_) => return Ok(buf),
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
    pub async fn create<T>(local_addr: T) -> Result<UdpServerTransport>
    where T: ToSocketAddrs {
        let sock = UdpSocket::bind(local_addr).await?;
        Ok(UdpServerTransport {
            sock,
            peer_addr: Mutex::new(None),
            last_peer_addr: Mutex::new(None),
        })
    }
}

#[async_trait]
impl Transport for UdpServerTransport {
    async fn send(&self, buf: Bytes) -> Result<()> {
        let peer_addr = self.peer_addr.lock().unwrap().ok_or(
            std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "No valid client yet"))?;
        self.sock.send_to(&buf, peer_addr).await?;
        Ok(())
    }

    async fn receive(&self) -> Result<BytesMut> {
        let mut buf = BytesMut::with_capacity(UDP_MTU);
        let (_, peer_addr) = self.sock.recv_buf_from(&mut buf).await?;
        let _ = self.last_peer_addr.lock().unwrap().insert(peer_addr);
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

    #[tokio::test]
    async fn test_basic_send_receive() -> Result<()> {
        let server = UdpServerTransport::create("127.0.0.1:9999").await?;
        let client = UdpClientTransport::create("127.0.0.1:0", "127.0.0.1:9999").await?;
        assert!(!server.ready_to_send());
        assert!(client.ready_to_send());

        let payload = Bytes::from("hello world!");
        client.send(payload.clone()).await?;

        {
            let received_payload = server.receive().await?;
            assert_eq!(payload, received_payload);
        }

        assert!(!server.ready_to_send());
        server.mark_last_received_valid();
        assert!(server.ready_to_send());

        server.send(payload.clone()).await?;

        {
            let received_payload = client.receive().await?;
            assert_eq!(payload, received_payload);
        }

        Ok(())
    }
}
