use std::{ops::Deref, os::fd::{AsFd, OwnedFd}};

use super::socket::RawSocketSender;

impl RawSocketSender for OwnedFd {
    fn send(&self, pkt: &[u8]) -> std::io::Result<()> {
        let written = nix::unistd::write(self.as_fd(), pkt)?;
        if written == pkt.len() {
            Ok(())
        } else {
            Err(std::io::Error::from(std::io::ErrorKind::Other))
        }
    }
}

impl<T> RawSocketSender for std::sync::Arc<T> where T: RawSocketSender {
    fn send(&self, pkt: &[u8]) -> std::io::Result<()> {
        self.deref().send(pkt)
    }
}
