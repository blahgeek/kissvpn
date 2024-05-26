use std::{net::ToSocketAddrs, sync::atomic};

use bytes::{Buf, BufMut, BytesMut};
use anyhow::Result;
use rand::RngCore;

use crate::constants::{TRANSPORT_MTU, UDP_MTU};
use super::{udp::{UdpClientTransport, UdpServerTransport}, Transport};

// https://datatracker.ietf.org/doc/html/rfc1035
const DNS_QTYPE_NULL: u16 = 10;
const DNS_QCLASS_IN: u16 = 1;

// Query:
// - header: 12 bytes; each question: QNAME + 4 bytes.
// - Use QNAME in question section for all data
// - Each QNAME can store at most 63+63+63+61=250 bytes (+5 label length bytes, total 255)
// - "(Although) labels can contain any 8 bit values in octets that make up a label ... "
// - So, max payload = 1414 (using 6 questions)

const QUERY_MAX_PAYLOAD: usize = 1414;
static_assertions::const_assert!(TRANSPORT_MTU <= QUERY_MAX_PAYLOAD);

fn encode_to_query(mut payload: impl Buf, id: u16) -> BytesMut {
    debug_assert!(payload.remaining() <= QUERY_MAX_PAYLOAD);

    let mut result = BytesMut::with_capacity(UDP_MTU);
    let question_count = (payload.remaining() + 250 - 1) / 250;

    // header
    result.put_u16(id);
    result.put_u8( /* RD */ 1);
    result.put_u8(0);
    result.put_u16(question_count as u16);  // QDCOUNT
    result.put_u16(0);  // ANCOUNT
    result.put_u16(0);  // NSCOUNT
    result.put_u16(0);  // ARCOUNT

    // questions
    let mut added_question_count = 0;
    while payload.has_remaining() {
        let result_len_limit = result.len() + 254;  // QNAME should stop here (following a '\0')
        while payload.has_remaining() && result.len() + 1 < result_len_limit {
            let label_len = usize::min(
                usize::min(payload.remaining(), result_len_limit - result.len() - 1),
                63);
            result.put_u8(label_len as u8);
            result.put(payload.copy_to_bytes(label_len));
        }
        result.put_u8(0);

        result.put_u16(DNS_QTYPE_NULL);
        result.put_u16(DNS_QCLASS_IN);
        added_question_count += 1;
    }
    debug_assert!(result.len() <= UDP_MTU);
    debug_assert_eq!(added_question_count, question_count);

    return result;
}

fn decode_from_query(mut buf: impl Buf) -> Result<(BytesMut, u16)> {
    if buf.remaining() < 12 {
        anyhow::bail!("no enough data for header");
    }
    let query_id = buf.get_u16();
    buf.advance(2);
    let question_count = buf.get_u16();
    buf.advance(6);

    let mut result = BytesMut::with_capacity(buf.remaining());
    for _ in 0..question_count {
        loop {
            if !buf.has_remaining() {
                anyhow::bail!("invalid label");
            }
            let label_len = buf.get_u8() as usize;
            if label_len > 63 || buf.remaining() < label_len {
                anyhow::bail!("invalid label_len");
            }
            if label_len == 0 {
                break;
            }
            result.put(buf.copy_to_bytes(label_len));
        }
        if buf.remaining() < 4 {
            anyhow::bail!("invalid question section");
        }
        buf.advance(4);
    }

    Ok((result, query_id))
}


// Response:
// - header: 12 bytes
// - single resource record
//   - name: ".cn."  4 bytes
//   - type, class (NULL), ttl: 8 bytes
//   - rdlength: 2 bytes
//   - rdata:  payload
// So, max payload = 1454
const RESPONSE_MAX_PAYLOAD: usize = 1454;
static_assertions::const_assert!(TRANSPORT_MTU <= RESPONSE_MAX_PAYLOAD);

fn encode_to_response(payload: impl Buf, id: u16) -> BytesMut {
    debug_assert!(payload.remaining() <= RESPONSE_MAX_PAYLOAD);

    let mut result = BytesMut::with_capacity(UDP_MTU);

    // header
    result.put_u16(id);
    result.put_u8( /* response */ 1 << 7);
    result.put_u8( /* recursive answer */ 1 << 7);
    result.put_u16(0);  // QDCOUNT
    result.put_u16(1);  // ANCOUNT
    result.put_u16(0);  // NSCOUNT
    result.put_u16(0);  // ARCOUNT

    // resource record
    result.put_u8(2);
    result.put_slice(&['c' as u8, 'n' as u8]);
    result.put_u8(0);  // end of name
    result.put_u16(DNS_QTYPE_NULL);
    result.put_u16(DNS_QCLASS_IN);
    result.put_u32(300);  // TTL
    result.put_u16(payload.remaining() as u16);  // RDLENGTH
    result.put(payload);

    debug_assert!(result.len() <= UDP_MTU);
    result
}

fn decode_from_response(mut buf: impl Buf) -> Result<BytesMut> {
    if buf.remaining() < 26 {
        anyhow::bail!("no enough data for header");
    }
    buf.advance(24);
    let payload_len = buf.get_u16() as usize;
    if buf.remaining() != payload_len {
        anyhow::bail!("invalid payload length");
    }

    let mut result = BytesMut::with_capacity(payload_len);
    result.put(buf.copy_to_bytes(payload_len));
    Ok(result)
}


pub struct FakednsClientTransport {
    udp_transport: UdpClientTransport,
}

impl FakednsClientTransport {
    pub fn create<TL, TR>(local_addr: TL, remote_addr: TR)
                          -> Result<FakednsClientTransport>
    where TL: ToSocketAddrs, TR: ToSocketAddrs {
        Ok(FakednsClientTransport {
            udp_transport: UdpClientTransport::create(local_addr, remote_addr)?
        })
    }
}

impl Transport for FakednsClientTransport {
    fn send(&self, buf: impl Buf) -> Result<()> {
        let query_id = rand::thread_rng().next_u32() as u16;
        let encoded = encode_to_query(buf, query_id);
        self.udp_transport.send(encoded)?;
        Ok(())
    }

    fn receive(&self) -> Result<BytesMut> {
        let buf = self.udp_transport.receive()?;
        decode_from_response(buf)
    }
}


pub struct FakednsServerTransport {
    udp_transport: UdpServerTransport,
    query_id: atomic::AtomicU16,
    last_query_id: atomic::AtomicU16,
}

impl FakednsServerTransport {
    pub fn create<T>(local_addr: T) -> Result<FakednsServerTransport>
    where T: ToSocketAddrs {
        Ok(FakednsServerTransport {
            udp_transport: UdpServerTransport::create(local_addr)?,
            query_id: atomic::AtomicU16::new(0),
            last_query_id: atomic::AtomicU16::new(0),
        })
    }
}

impl Transport for FakednsServerTransport {
    fn send(&self, buf: impl Buf) -> Result<()> {
        let encoded = encode_to_response(buf, self.query_id.load(atomic::Ordering::Acquire));
        self.udp_transport.send(encoded)?;
        Ok(())
    }

    fn receive(&self) -> Result<BytesMut> {
        let buf = self.udp_transport.receive()?;
        let (decoded, decoded_query_id) = decode_from_query(buf)?;
        self.last_query_id.store(decoded_query_id, atomic::Ordering::Release);
        Ok(decoded)
    }

    fn mark_last_received_valid(&self) {
        self.query_id.store(
            self.last_query_id.load(atomic::Ordering::Acquire),
            atomic::Ordering::Release);
        self.udp_transport.mark_last_received_valid();
    }

    fn ready_to_send(&self) -> bool {
        self.udp_transport.ready_to_send()
    }
}


#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_encode_decode_query() -> Result<()> {
        let mut rng = rand::thread_rng();
        for payload_len in 1..=QUERY_MAX_PAYLOAD {
            let mut payload = vec![0u8; payload_len];
            rng.fill_bytes(&mut payload);

            let query_id = rng.next_u32() as u16;
            let encoded = encode_to_query(payload.as_slice(), query_id);
            assert!(encoded.len() <= UDP_MTU);

            let (decoded_payload, decoded_query_id) = decode_from_query(encoded)?;

            assert_eq!(query_id, decoded_query_id);
            assert_eq!(payload, decoded_payload);
        }
        Ok(())
    }

    #[test]
    fn test_encode_decode_response() -> Result<()> {
        let mut rng = rand::thread_rng();
        for payload_len in 1..=RESPONSE_MAX_PAYLOAD {
            let mut payload = vec![0u8; payload_len];
            rng.fill_bytes(&mut payload);

            let query_id = rng.next_u32() as u16;
            let encoded = encode_to_response(payload.as_slice(), query_id);
            assert!(encoded.len() <= UDP_MTU);

            let decoded_payload = decode_from_response(encoded)?;

            assert_eq!(payload, decoded_payload);
        }
        Ok(())
    }
}
