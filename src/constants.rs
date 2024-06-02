pub const VPN_MTU: usize = 1362;

// VPN_MTU -> TRANSPORT_MTU
// the encryption requires extra 28 bytes (12 nonce, 16 mac).
// remaining bytes are for obfs.

pub const TRANSPORT_MTU: usize = 1392;

static_assertions::const_assert!(VPN_MTU + 12 + 16 <= TRANSPORT_MTU);

// PPPoE MTU = 1492, IPv4 header = 20, UDP header = 8
// TODO: no support for ipv6 for now
// So use UDP MTU of 1472
pub const UDP_MTU: usize = 1464;


pub const BUF_CAPACITY: usize = 1500;
