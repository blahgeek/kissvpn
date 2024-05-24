pub const VPN_MTU: usize = 1300;
pub const TRANSPORT_MTU: usize = VPN_MTU + 28;  // 12 nonce, 16 mac
pub const UDP_MTU: usize = 1480;
