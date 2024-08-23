use std::{fmt::{Debug, Display}, net::SocketAddrV4};

use bytes::{Buf, BufMut, BytesMut};
use rand::Rng;

use crate::constants::BUF_CAPACITY;

// Very simple fake tcp:
// 1. No active ACK
// 2. Sequence id will not overflow
//   a. start with lower half
//   b. limit usage per socket (controlled by upper layer)
// 3. no disconnection, only RST, by client
// 4. also, no disconnect callback. server should simply drop old connections after certain time when new is created

pub trait RawSocketSender {
    fn send(&self, pkt: &[u8]) -> std::io::Result<()>;
}

const TCP_FLAG_SYN: u8 = 0x02;
const TCP_FLAG_ACK: u8 = 0x10;
const TCP_FLAG_RST: u8 = 0x04;

#[derive(PartialEq, Eq, Debug)]
enum SocketState {
    Initial,  // server only
    SynSent,  // client only
    SynReceived,  // server only
    Established,
}

#[derive(Debug)]
pub struct Socket<RAW> {
    local_addr: SocketAddrV4,
    remote_addr: SocketAddrV4,

    state: SocketState,

    next_send_seq: u32,
    next_recv_seq: u32,  // expected next received seq

    raw_sock: RAW,
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

impl<RAW> Display for Socket<RAW> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Socket[{}->{}, {:?}]", self.local_addr, self.remote_addr, self.state)
    }
}

impl<RAW> Socket<RAW> where RAW: RawSocketSender {
    pub fn new_connect(local_addr: SocketAddrV4, remote_addr: SocketAddrV4, raw_sock: RAW) -> std::io::Result<Self> {
        let mut sock = Self {
            local_addr,
            remote_addr,
            state: SocketState::SynSent,
            next_send_seq: 0,
            next_recv_seq: 0,
            raw_sock
        };
        sock._send_syn(/* with_ack */ false)?;
        Ok(sock)
    }

    /// Create a server-side socket. The state after creation is initial, the first SYN packet should be fed after creation.
    pub fn new_listened(local_addr: SocketAddrV4, remote_addr: SocketAddrV4, raw_sock: RAW) -> std::io::Result<Self> {
        Ok(Self {
            local_addr,
            remote_addr,
            state: SocketState::Initial,
            next_send_seq: 0,
            next_recv_seq: 0,
            raw_sock
        })
    }

    pub fn remote_addr(&self) -> &SocketAddrV4 { &self.remote_addr }
    pub fn local_addr(&self) -> &SocketAddrV4 { &self.local_addr }

    pub fn send(&mut self, data: &[u8]) -> std::io::Result<()> {
        assert!(self.ready());
        self._send_tcp_packet(TCP_FLAG_ACK, &[], data)?;
        self.next_send_seq += data.len() as u32;
        Ok(())
    }

    pub fn send_rst(&mut self) -> std::io::Result<()> {
        self._send_tcp_packet(TCP_FLAG_RST, &[], &[])?;
        Ok(())
    }

    /// feed received packet (that belong to this socket (port)).
    /// maybe return data if any;
    /// or the socket's state may change (to ready).
    pub fn feed_packet<'a>(&mut self, mut pkt: &'a [u8]) -> std::io::Result<&'a [u8]> {
        if pkt.len() < 20 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        if pkt.get_u16() != self.remote_addr.port() || pkt.get_u16() != self.local_addr.port() {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        let seq_n = pkt.get_u32();
        let ack_n = pkt.get_u32();
        let data_offset = ((pkt.get_u8() >> 4) * 4) as usize;
        if data_offset < 20 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        let flags = pkt.get_u8();
        // skip window size, checksum, urgent pointer, and options
        let skip_size = 6 + data_offset - 20;
        if pkt.len() < skip_size {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        pkt.advance(skip_size);

        match self.state {
            SocketState::SynSent => {
                if flags == (TCP_FLAG_ACK | TCP_FLAG_SYN) && ack_n == self.next_send_seq {
                    self.state = SocketState::Established;
                    self.next_recv_seq = seq_n + 1;
                    self._send_tcp_packet(TCP_FLAG_ACK, &[], &[])?;
                }
            },
            SocketState::Initial => {
                if flags == TCP_FLAG_SYN {
                    self.state = SocketState::SynReceived;
                    self.next_recv_seq = seq_n + 1;
                    self._send_syn(/* with ack */ true)?;
                }
            },
            SocketState::SynReceived => {
                if flags == TCP_FLAG_ACK && ack_n == self.next_send_seq {
                    self.state = SocketState::Established;
                }
            },
            SocketState::Established => {
                if flags == TCP_FLAG_ACK {
                    self.next_recv_seq = self.next_recv_seq.max(seq_n + pkt.remaining() as u32);
                    return Ok(pkt)
                }
            },
        }
        return Ok(&[])
    }

    pub fn ready(&self) -> bool {
        self.state == SocketState::Established
    }

    fn _send_tcp_packet(&mut self, flags: u8, options: &[u8], data: &[u8]) -> std::io::Result<()> {
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

        self.raw_sock.send(&packet)
    }

    fn _send_syn(&mut self, with_ack: bool) -> std::io::Result<()> {
        // 0 to half, do not want to handle overflow
        self.next_send_seq = rand::thread_rng().gen_range(0..(u32::MAX / 2));

        // kind = 3, length = 3, scale factor = 14
        let window_scaling_option: &[u8] = &[3, 3, 14, 0];

        self._send_tcp_packet(
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
    use std::sync::mpsc;

    use super::*;

    impl RawSocketSender for mpsc::Sender<Bytes> {
        fn send(&self, pkt: &[u8]) -> std::io::Result<()> {
            (self as &mpsc::Sender<Bytes>).send(Bytes::copy_from_slice(pkt)).unwrap();
            Ok(())
        }
    }

    #[test]
    fn test_basic_connect() -> Result<()> {
        let local_addr = SocketAddrV4::from_str("127.0.0.1:1234")?;
        let remote_addr = SocketAddrV4::from_str("127.0.0.1:1235")?;

        let (raw_sock_sender, raw_sock_receiver) = mpsc::channel::<Bytes>();

        let sock = Socket::new_connect(local_addr, remote_addr, raw_sock_sender)?;
        assert!(!sock.ready());

        let pkt = raw_sock_receiver.recv_timeout(std::time::Duration::ZERO).unwrap();
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

    #[test]
    fn test_interconnect() -> Result<()> {
        let local_addr = SocketAddrV4::from_str("127.0.0.1:1234")?;
        let remote_addr = SocketAddrV4::from_str("127.0.0.1:1235")?;

        let (local_sock_sender, local_sock_receiver) = mpsc::channel::<Bytes>();
        let (remote_sock_sender, remote_sock_receiver) = mpsc::channel::<Bytes>();

        let mut local_sock = Socket::new_connect(local_addr, remote_addr, local_sock_sender)?;
        let mut remote_sock = Socket::new_listened(remote_addr, local_addr, remote_sock_sender)?;

        assert_eq!(remote_sock.feed_packet(&local_sock_receiver.try_recv()?)?, &[]);
        assert!(!remote_sock.ready());

        assert_eq!(local_sock.feed_packet(&remote_sock_receiver.try_recv()?)?, &[]);
        assert!(local_sock.ready());

        assert_eq!(remote_sock.feed_packet(&local_sock_receiver.try_recv()?)?, &[]);
        assert!(remote_sock.ready());

        local_sock.send(b"hello")?;
        assert_eq!(remote_sock.feed_packet(&local_sock_receiver.try_recv()?)?, b"hello");

        Ok(())
    }

}
