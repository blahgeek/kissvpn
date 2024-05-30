pub const VPN_MTU: usize = 1300;

// the encryption requires extra 28 bytes (12 nonce, 16 mac). remaining bytes are for obfs.
pub const TRANSPORT_MTU: usize = 1400;

pub const UDP_MTU: usize = 1480;
