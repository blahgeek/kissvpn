use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use anyhow::Result;

#[async_trait]
pub trait Transport: Send + Sync {
    // fn init(&mut self) -> Result<()>;
    fn get_mtu(&self) -> usize;

    // Following methods are all using `&self` instead of `&mut self`, just like tokio::net::UdpSocket

    // If keep_alive_interval is not none, the caller should call keep_alive at fixed interval
    fn get_keep_alive_interval(&self) -> Option<std::time::Duration> { None }
    async fn keep_alive(&self) -> Result<()> { Ok(()) }

    async fn send(&self, buf: Bytes) -> Result<()>;
    async fn receive(&self) -> Result<BytesMut>;

    // The caller must call this if last received packet is crypto verified,
    // so that the transport knows the peer is trusted.
    // Usually, for a server-side transport, ready_to_send() only returns true after this.
    fn mark_last_received_valid(&self) {}

    // Return true if this transport is ready for sending.
    // Mostly useful for server side, because it's only ready after receiving from client first.
    fn ready_to_send(&self) -> bool { true }
}


pub mod udp;
