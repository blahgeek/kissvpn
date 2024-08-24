use std::{collections::BTreeMap, os::fd::OwnedFd, sync::Mutex};

use super::Transport;

// Very simple fake TCP

// - After handshake and initial content, only one way sending (use multiple connection for bidirectional communication)
//   - for simplicity, different port ranges are used for two directions
// - No ACK
// - No termination. Only send RST at the end, to prevent overflowing the NAT
// - Each connection last only certain duration


const CLIENT2SERVER_PORT_RANGE: std::ops::Range::<u16> = 10000..20000;
const SERVER2CLIENT_PORT_RANGE: std::ops::Range::<u16> = 20000..30000;

struct ClientConnState {
    established: bool,  // false means in SYN SENT state
    next_seq_id: u32,
}

pub struct FaketcpClientTransport {
    /// raw socket fd
    fd: OwnedFd,
    /// source port -> connection state
    conns: Mutex<BTreeMap<u16, ClientConnState>>,
}
