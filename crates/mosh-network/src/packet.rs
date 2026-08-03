//! The datagram payload beneath the encryption: direction, sequence and timestamps.
//!
//! Transliterated from the `Packet` half of `third_party/mosh/src/network/network.cc`.
//!
//! This is the outermost layer of the wire contract. The nonce carries the direction in
//! its top bit and the sequence number in the rest, and the plaintext begins with two
//! big-endian 16-bit timestamps before the fragment itself.

use mosh_crypto::{CryptoError, Message, Nonce};

/// The protocol this implementation speaks. Bumped when echo-ack was added.
pub const MOSH_PROTOCOL_VERSION: u32 = 2;

/// Bytes the transport adds to every datagram, on top of the crypto tag.
pub const ADDED_BYTES: usize = 8 /* sequence number, doubling as the nonce */
    + 4 /* two timestamps */;

const DIRECTION_MASK: u64 = 1u64 << 63;
const SEQUENCE_MASK: u64 = u64::MAX ^ DIRECTION_MASK;

/// A timestamp value meaning "none".
pub const TIMESTAMP_NONE: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToServer = 0,
    ToClient = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub seq: u64,
    pub direction: Direction,
    pub timestamp: u16,
    pub timestamp_reply: u16,
    pub payload: Vec<u8>,
}

impl Packet {
    pub fn new(
        seq: u64,
        direction: Direction,
        timestamp: u16,
        timestamp_reply: u16,
        payload: Vec<u8>,
    ) -> Self {
        Packet {
            seq,
            direction,
            timestamp,
            timestamp_reply,
            payload,
        }
    }

    /// Read a packet out of a decrypted message.
    pub fn from_message(message: &Message) -> Result<Self, CryptoError> {
        // The C++ uses dos_assert here, which aborts on a datagram an attacker chose.
        if message.text.len() < 4 {
            return Err(CryptoError::TooShort);
        }
        let val = message.nonce.val();
        Ok(Packet {
            seq: val & SEQUENCE_MASK,
            direction: if val & DIRECTION_MASK != 0 {
                Direction::ToClient
            } else {
                Direction::ToServer
            },
            timestamp: u16::from_be_bytes([message.text[0], message.text[1]]),
            timestamp_reply: u16::from_be_bytes([message.text[2], message.text[3]]),
            payload: message.text[4..].to_vec(),
        })
    }

    /// Render as a message ready to encrypt.
    pub fn to_message(&self) -> Message {
        let direction_seq =
            ((self.direction == Direction::ToClient) as u64) << 63 | (self.seq & SEQUENCE_MASK);

        let mut text = Vec::with_capacity(4 + self.payload.len());
        text.extend_from_slice(&self.timestamp.to_be_bytes());
        text.extend_from_slice(&self.timestamp_reply.to_be_bytes());
        text.extend_from_slice(&self.payload);

        Message::new(Nonce::new(direction_seq), text)
    }
}

/// The low 16 bits of a millisecond clock, avoiding the "none" value.
pub fn timestamp16(now: u64) -> u16 {
    let ts = (now % 65536) as u16;
    if ts == TIMESTAMP_NONE {
        ts.wrapping_add(1)
    } else {
        ts
    }
}

/// Difference between two 16-bit timestamps, allowing for wraparound.
pub fn timestamp_diff(tsnew: u16, tsold: u16) -> u16 {
    tsnew.wrapping_sub(tsold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosh_crypto::{Base64Key, Session};

    #[test]
    fn direction_lives_in_the_top_bit_of_the_nonce() {
        let to_client = Packet::new(5, Direction::ToClient, 1, 2, vec![]).to_message();
        assert_eq!(to_client.nonce.val() & DIRECTION_MASK, DIRECTION_MASK);
        assert_eq!(to_client.nonce.val() & SEQUENCE_MASK, 5);

        let to_server = Packet::new(5, Direction::ToServer, 1, 2, vec![]).to_message();
        assert_eq!(to_server.nonce.val() & DIRECTION_MASK, 0);
    }

    #[test]
    fn timestamps_are_big_endian_and_come_first() {
        let msg = Packet::new(0, Direction::ToServer, 0x0102, 0x0304, b"body".to_vec()).to_message();
        assert_eq!(&msg.text[..4], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&msg.text[4..], b"body");
    }

    #[test]
    fn a_packet_round_trips_through_its_message_form() {
        for direction in [Direction::ToServer, Direction::ToClient] {
            for payload in [&b""[..], b"hello", &[0xffu8; 1000][..]] {
                let p = Packet::new(12345, direction, 0xbeef, 0xcafe, payload.to_vec());
                let back = Packet::from_message(&p.to_message()).unwrap();
                assert_eq!(back, p);
            }
        }
    }

    #[test]
    fn a_packet_survives_encryption() {
        let key = Base64Key::random();
        let mut session = Session::new(&key);
        let p = Packet::new(7, Direction::ToClient, 111, 222, b"payload".to_vec());

        let wire = session.encrypt(&p.to_message()).unwrap();
        let decrypted = session.decrypt(&wire).unwrap();
        assert_eq!(Packet::from_message(&decrypted).unwrap(), p);
    }

    #[test]
    fn the_sequence_number_never_collides_with_the_direction_bit() {
        // A sequence number with the top bit set must not be read back as a direction.
        let p = Packet::new(u64::MAX, Direction::ToServer, 0, 0, vec![]);
        let back = Packet::from_message(&p.to_message()).unwrap();
        assert_eq!(back.direction, Direction::ToServer);
        assert_eq!(back.seq, SEQUENCE_MASK);
    }

    #[test]
    fn a_truncated_message_is_rejected_not_fatal() {
        for len in 0..4usize {
            let msg = Message::new(Nonce::new(0), vec![0; len]);
            assert_eq!(Packet::from_message(&msg), Err(CryptoError::TooShort));
        }
        // Exactly the timestamps with no payload is legitimate.
        let msg = Message::new(Nonce::new(0), vec![0; 4]);
        assert!(Packet::from_message(&msg).is_ok());
    }

    #[test]
    fn timestamp16_avoids_the_none_value() {
        assert_eq!(timestamp16(0), 0);
        assert_eq!(timestamp16(65535), 0);
        assert_eq!(timestamp16(65536), 0);
        assert_ne!(timestamp16(65535), TIMESTAMP_NONE);
    }

    #[test]
    fn timestamp_diff_wraps_around() {
        assert_eq!(timestamp_diff(100, 50), 50);
        // The clock wrapped between the two readings.
        assert_eq!(timestamp_diff(10, 65530), 16);
    }
}
