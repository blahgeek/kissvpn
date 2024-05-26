use bytes::{Buf, BytesMut};
use anyhow::Result;

pub trait Transport: Sync {
    // Following methods are all using `&self` instead of `&mut self`,
    // so that they can be used separately in sending and receiving loop

    fn send(&self, buf: impl Buf) -> Result<()>;
    fn receive(&self) -> Result<BytesMut>;

    // The caller must call this if last received packet is crypto verified,
    // so that the transport knows the peer is trusted.
    // Usually, for a server-side transport, ready_to_send() only returns true after this.
    fn mark_last_received_valid(&self) {}

    // Return true if this transport is ready for sending.
    // Mostly useful for server side, because it's only ready after receiving from client first.
    fn ready_to_send(&self) -> bool { true }
}


pub mod udp;
pub mod fakedns;
