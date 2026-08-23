//! Minimal MQTT 3.1.1 packet codec — encode/decode just enough
//! control packets for the `mqtt-roundtrip` example:
//!
//! * CONNECT (client → broker)
//! * CONNACK (broker → client)
//! * SUBSCRIBE (client → broker)
//! * SUBACK (broker → client)
//! * PUBLISH (client ↔ broker)
//! * DISCONNECT (client → broker)
//!
//! QoS is fixed at 0 for this example.

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// MQTT 3.1.1 control packet type (high nibble of the first
/// fixed-header byte).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Connect = 1,
    Connack = 2,
    Publish = 3,
    Puback = 4,
    Subscribe = 8,
    Suback = 9,
    Disconnect = 14,
}

impl PacketType {
    fn from_byte(b: u8) -> Option<Self> {
        match b & 0xF0 {
            0x10 => Some(Self::Connect),
            0x20 => Some(Self::Connack),
            0x30 => Some(Self::Publish),
            0x40 => Some(Self::Puback),
            0x80 => Some(Self::Subscribe),
            0x90 => Some(Self::Suback),
            0xE0 => Some(Self::Disconnect),
            _ => None,
        }
    }
}

/// Decoded MQTT 3.1.1 control packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Client → broker: open a session.
    Connect { client_id: String },
    /// Broker → client: accept a session.
    Connack,
    /// Client → broker: register an interest in a topic.
    Subscribe { topic: String },
    /// Broker → client: subscription acknowledged.
    Suback,
    /// Either direction: data on a topic.
    Publish { topic: String, payload: Bytes },
    /// Client → broker: close the session cleanly.
    Disconnect,
}

/// Errors from the codec.
#[derive(Debug)]
pub enum CodecError {
    /// I/O / socket read failure.
    Io(std::io::Error),
    /// Variable-length remaining-length decoding found a
    /// malformed encoding (more than 4 bytes).
    MalformedRemainingLength,
    /// Stream ended mid-packet.
    UnexpectedEof,
    /// Packet type we don't recognise.
    UnknownPacketType(u8),
    /// Body shorter than the announced remaining length.
    Truncated,
}

impl From<std::io::Error> for CodecError {
    fn from(e: std::io::Error) -> Self {
        CodecError::Io(e)
    }
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Io(e) => write!(f, "io: {}", e),
            CodecError::MalformedRemainingLength => f.write_str("malformed remaining length"),
            CodecError::UnexpectedEof => f.write_str("unexpected EOF"),
            CodecError::UnknownPacketType(b) => write!(f, "unknown packet type byte {:#x}", b),
            CodecError::Truncated => f.write_str("truncated packet body"),
        }
    }
}
impl std::error::Error for CodecError {}

/// Read one complete MQTT control packet from `src`.
/// Returns the parsed packet and the number of bytes consumed
/// from `src`.
pub fn decode_packet(mut src: &[u8]) -> Result<(Packet, usize), CodecError> {
    if src.is_empty() {
        return Err(CodecError::UnexpectedEof);
    }
    let consumed_start = src.len();
    let first = src[0];
    let ptype = PacketType::from_byte(first)
        .ok_or(CodecError::UnknownPacketType(first))?;
    src.advance(1);

    // Variable-length remaining length (1..=4 bytes).
    let mut multiplier = 1u32;
    let mut remaining = 0u32;
    let mut len_bytes = 0;
    loop {
        if src.is_empty() {
            return Err(CodecError::UnexpectedEof);
        }
        let byte = src[0];
        src.advance(1);
        len_bytes += 1;
        remaining += (byte as u32 & 0x7F) * multiplier;
        if byte & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
        if len_bytes >= 4 {
            return Err(CodecError::MalformedRemainingLength);
        }
    }
    let remaining = remaining as usize;

    if src.len() < remaining {
        return Err(CodecError::Truncated);
    }
    let body = &src[..remaining];
    src.advance(remaining);

    let packet = match ptype {
        PacketType::Connect => decode_connect(body)?,
        PacketType::Connack => {
            if body.len() != 2 {
                return Err(CodecError::Truncated);
            }
            Packet::Connack
        }
        PacketType::Subscribe => decode_subscribe(body)?,
        PacketType::Suback => Packet::Suback,
        PacketType::Publish => decode_publish(first, body)?,
        PacketType::Disconnect => Packet::Disconnect,
        PacketType::Puback => return Err(CodecError::UnknownPacketType(first)),
    };
    let consumed = consumed_start - src.len();
    Ok((packet, consumed))
}

fn decode_connect(body: &[u8]) -> Result<Packet, CodecError> {
    // CONNECT variable header: protocol name (length-prefixed UTF-8) +
    // protocol level (1 byte) + connect flags (1 byte) + keep-alive (2 bytes).
    // Both string lengths are 2-byte big-endian (u16) per MQTT 3.1.1 §2.2.1.
    let mut cur = body;
    if cur.len() < 2 + 4 + 1 + 1 + 2 {
        return Err(CodecError::Truncated);
    }
    let name_len = u16::from_be_bytes([cur[0], cur[1]]) as usize;
    cur = &cur[2..];
    if cur.len() < name_len + 1 + 1 + 2 {
        return Err(CodecError::Truncated);
    }
    let name = std::str::from_utf8(&cur[..name_len])
        .map_err(|_| CodecError::Truncated)?;
    if name != "MQTT" {
        return Err(CodecError::Truncated);
    }
    cur = &cur[name_len..];
    let _level = cur[0];
    cur = &cur[1..];
    let _flags = cur[0];
    cur = &cur[1..];
    let _keepalive = u16::from_be_bytes([cur[0], cur[1]]);
    cur = &cur[2..];

    // Payload: client-id (2-byte length-prefixed UTF-8). For
    // QoS 0 + no will + no username/password, that's all.
    if cur.is_empty() {
        return Ok(Packet::Connect {
            client_id: String::new(),
        });
    }
    if cur.len() < 2 {
        return Err(CodecError::Truncated);
    }
    let id_len = u16::from_be_bytes([cur[0], cur[1]]) as usize;
    cur = &cur[2..];
    if cur.len() < id_len {
        return Err(CodecError::Truncated);
    }
    let client_id = std::str::from_utf8(&cur[..id_len])
        .map_err(|_| CodecError::Truncated)?
        .to_string();
    Ok(Packet::Connect { client_id })
}

fn decode_subscribe(body: &[u8]) -> Result<Packet, CodecError> {
    // Variable header: packet-id (2 bytes). Payload: a list of
    // (topic-filter length, topic-filter, QoS).
    if body.len() < 2 {
        return Err(CodecError::Truncated);
    }
    let mut cur = &body[2..];
    if cur.is_empty() {
        return Err(CodecError::Truncated);
    }
    let topic_len = u16::from_be_bytes([cur[0], cur[1]]) as usize;
    cur = &cur[2..];
    if cur.len() < topic_len + 1 {
        return Err(CodecError::Truncated);
    }
    let topic = std::str::from_utf8(&cur[..topic_len])
        .map_err(|_| CodecError::Truncated)?
        .to_string();
    Ok(Packet::Subscribe { topic })
}

fn decode_publish(first_byte: u8, body: &[u8]) -> Result<Packet, CodecError> {
    // Variable header: topic-name (length-prefixed UTF-8). QoS 0
    // has no packet identifier.
    if body.is_empty() {
        return Err(CodecError::Truncated);
    }
    let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    if body.len() < 2 + topic_len {
        return Err(CodecError::Truncated);
    }
    let topic = std::str::from_utf8(&body[2..2 + topic_len])
        .map_err(|_| CodecError::Truncated)?
        .to_string();
    let payload = Bytes::copy_from_slice(&body[2 + topic_len..]);
    let _ = first_byte;
    Ok(Packet::Publish { topic, payload })
}

/// Encode a packet into a fresh `BytesMut`.
pub fn encode_packet(p: &Packet) -> BytesMut {
    let mut body = BytesMut::new();
    let first = match p {
        Packet::Connect { .. } => {
            encode_connect_body(&mut body, p);
            0x10
        }
        Packet::Connack => {
            body.put_u8(0); // session-present
            body.put_u8(0); // return code: accepted
            0x20
        }
        Packet::Subscribe { topic } => {
            body.put_u16(0x1234); // packet id
            body.put_u16(topic.len() as u16);
            body.put_slice(topic.as_bytes());
            body.put_u8(0); // QoS 0
            0x82
        }
        Packet::Suback => {
            body.put_u16(0x1234); // packet id (echo)
            body.put_u8(0);       // QoS 0 granted
            0x90
        }
        Packet::Publish { topic, payload } => {
            body.put_u16(topic.len() as u16);
            body.put_slice(topic.as_bytes());
            body.put_slice(payload);
            0x30
        }
        Packet::Disconnect => 0xE0,
    };
    let mut out = BytesMut::with_capacity(1 + 4 + body.len());
    out.put_u8(first);
    encode_remaining_length(&mut out, body.len());
    out.put(body);
    out
}

fn encode_connect_body(body: &mut BytesMut, p: &Packet) {
    let Packet::Connect { client_id } = p else {
        unreachable!()
    };
    body.put_slice(b"\x00\x04MQTT"); // protocol name "MQTT"
    body.put_u8(4); // protocol level (3.1.1)
    body.put_u8(0b0000_0010); // clean session, no will, QoS 0, no username/password
    body.put_u16(30); // keep-alive: 30 s
    body.put_u16(client_id.len() as u16);
    body.put_slice(client_id.as_bytes());
}

fn encode_remaining_length(out: &mut BytesMut, len: usize) {
    let mut x = len;
    loop {
        let mut byte = (x & 0x7F) as u8;
        x >>= 7;
        if x > 0 {
            byte |= 0x80;
        }
        out.put_u8(byte);
        if x == 0 {
            break;
        }
    }
}