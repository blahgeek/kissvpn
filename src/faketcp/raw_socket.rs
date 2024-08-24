use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::{Arc, Weak};

use bytes::{Bytes, BytesMut};

use crate::constants::BUF_CAPACITY;

#[derive(Clone)]
pub struct RawSocketSendHalf(Weak<OwnedFd>);

impl super::socket::RawSender for RawSocketSendHalf {
    fn send(&mut self, pkt: &[u8]) -> std::io::Result<()> {
        let fd = self.0.upgrade().unwrap();
        let written = nix::unistd::write(fd.as_fd(), pkt)?;
        if written == pkt.len() {
            Ok(())
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::Other))
        }
    }
}

/// receive half, cannot clone
pub struct RawSocketRecvHalf(Weak<OwnedFd>);

impl RawSocketRecvHalf {
    pub fn recv(&mut self) -> std::io::Result<Bytes> {
        let fd = self.0.upgrade().unwrap();
        let mut buf = BytesMut::zeroed(BUF_CAPACITY);
        let buf_len = nix::unistd::read(fd.as_raw_fd(), &mut buf)?;
        if buf_len == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        buf.truncate(buf_len);
        return Ok(buf.freeze());
    }
}

/// split fd into send half and receive half. both are weak ref to first result, which should live longer than them.
/// the send half can be cloned, while the receive half cannot.
/// the purpose is to prevent misuse of raw fd and ensure only one is receiving.
pub fn new_splitted_raw_socket(fd: OwnedFd) -> (Arc<OwnedFd>, RawSocketSendHalf, RawSocketRecvHalf) {
    let fd = Arc::new(fd);
    (fd.clone(), RawSocketSendHalf(Arc::downgrade(&fd)), RawSocketRecvHalf(Arc::downgrade(&fd)))
}

