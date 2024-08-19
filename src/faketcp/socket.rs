use std::net::SocketAddrV4;

use bytes::{BufMut, BytesMut};
use rand::Rng;

use crate::constants::BUF_CAPACITY;

// Very simple fake tcp:
// 1. No active ACK
// 2. Which means only two-way handshake
// 3. Sequence id will not overflow
//   a. start with lower half
//   b. limit usage per socket (controlled by upper layer)

pub trait SocketDelegate {
    fn handle_socket_ready(&mut self) {}
    fn handle_socket_data_received(&mut self) {}
    fn handle_socket_error(&mut self) {}

    fn send_raw_socket_data(&mut self, data: &[u8]) -> std::io::Result<()>;
}

const TCP_FLAG_SYN: u8 = 0x02;
const TCP_FLAG_ACK: u8 = 0x10;
const TCP_FLAG_RST: u8 = 0x04;

pub struct Socket<D> {
    local_addr: SocketAddrV4,
    remote_addr: SocketAddrV4,

    is_waiting_syn: bool,  // only for client socket

    next_send_seq: u32,
    next_recv_seq: u32,  // expected next received seq

    delegate: D,
}

fn ones_complement_add_by_16bit(data: &[u8], init: u16) -> u16 {
    let mut sum = init as u32;
    for (idx, v) in data.iter().enumerate() {
        if idx % 2 == 0 {
            sum += *v as u32;
        } else {
            sum += (*v as u32) << 8;
        }
    }
    while (sum >> 16) > 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    return sum as u16;
}

impl<D> Socket<D> where D: SocketDelegate {
    pub fn connect(local_addr: SocketAddrV4, remote_addr: SocketAddrV4, delegate: D) -> std::io::Result<Self> {
        let mut sock = Self {
            local_addr,
            remote_addr,
            is_waiting_syn: true,
            next_send_seq: 0,
            next_recv_seq: 0,
            delegate
        };
        sock.send_syn(/* with_ack */ false)?;
        Ok(sock)
    }

    pub fn ready(&self) -> bool {
        !self.is_waiting_syn
    }

    fn send_tcp_packet(&mut self, flags: u8, options: &[u8], data: &[u8]) -> std::io::Result<()> {
        // pseudo header for checksum
        let mut pseudo_header = BytesMut::with_capacity(12);
        pseudo_header.put_slice(&self.local_addr.ip().octets());
        pseudo_header.put_slice(&self.remote_addr.ip().octets());
        pseudo_header.put_u8(0);  // reserved
        pseudo_header.put_u8(6);  // IPPROTO_TCP
        pseudo_header.put_u16((20 + options.len() + data.len()) as u16);  // length
        let checksum = ones_complement_add_by_16bit(&pseudo_header, 0);

        assert_eq!(options.len() % 4, 0);
        let mut packet = BytesMut::with_capacity(BUF_CAPACITY);
        packet.put_u16(self.local_addr.port());
        packet.put_u16(self.remote_addr.port());
        packet.put_u32(self.next_send_seq);  // seq
        packet.put_u32(self.next_recv_seq);  // ack
        packet.put_u8(((5 + options.len() / 4) as u8) << 4);  // data offset shift 4
        packet.put_u8(flags);
        packet.put_u16(0xffff);  // window size

        let checksum = ones_complement_add_by_16bit(&packet, checksum);
        let checksum = ones_complement_add_by_16bit(options, checksum);
        let checksum = ones_complement_add_by_16bit(data, checksum);
        packet.put_u16_ne(!checksum);
        packet.put_u16(0);  // urgent pointer

        packet.put(options);
        packet.put(data);

        self.delegate.send_raw_socket_data(&packet)
    }

    fn send_syn(&mut self, with_ack: bool) -> std::io::Result<()> {
        // 0 to half, do not want to handle overflow
        self.next_send_seq = rand::thread_rng().gen_range(0..(u32::MAX / 2));

        // kind = 3, length = 3, scale factor = 14
        let window_scaling_option: &[u8] = &[3, 3, 14, 0];

        self.send_tcp_packet(
            if with_ack { TCP_FLAG_SYN | TCP_FLAG_ACK } else { TCP_FLAG_SYN },
            window_scaling_option,
            &[],
        )?;
        self.next_send_seq += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use anyhow::Result;
    use bytes::Bytes;

    use super::*;

    struct TestDelegate {
        sent_packets: Vec<Bytes>,
    }

    impl SocketDelegate for &mut TestDelegate {
        fn send_raw_socket_data(&mut self, data: &[u8]) -> std::io::Result<()> {
            self.sent_packets.push(Bytes::copy_from_slice(data));
            Ok(())
        }
    }

    #[test]
    fn test_basic_connect() -> Result<()> {
        let local_addr = SocketAddrV4::from_str("127.0.0.1:1234")?;
        let remote_addr = SocketAddrV4::from_str("127.0.0.1:1235")?;

        let mut delegate = TestDelegate{
            sent_packets: Vec::new(),
        };

        let sock = Socket::connect(local_addr, remote_addr, &mut delegate)?;
        assert!(!sock.ready());

        assert_eq!(delegate.sent_packets.len(), 1);
        let pkt = &delegate.sent_packets[0];
        assert_eq!(pkt.len(), 24);
        assert_eq!(pkt.slice(0..4), b"\x04\xd2\x04\xd3" as &[u8]); // port
        assert_eq!(pkt.slice(12..16), b"\x60\x02\xff\xff" as &[u8]);
        assert_eq!(pkt.slice(18..24), b"\x00\x00\x03\x03\x0e\x00" as &[u8]);

        let mut verify_checksum: u32 =
            (127 + (1 << 8)) * 2  // 127.0.0.1 *2
            + (6 << 8) + (24 << 8);  // protocol, length
        for i in (0..pkt.len()).step_by(2) {
            verify_checksum += (pkt[i] as u32) + ((pkt[i+1] as u32) << 8);
        }
        while verify_checksum > 0xffff {
            verify_checksum = (verify_checksum >> 16) + (verify_checksum & 0xffff);
        }
        assert_eq!(verify_checksum, 0xffff);

        Ok(())
    }
}
