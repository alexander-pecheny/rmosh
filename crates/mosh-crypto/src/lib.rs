//! Message protection for a mosh session.
//!
//! A datagram on the wire is an 8-byte nonce suffix followed by an AES-128-OCB3
//! ciphertext that carries its own 16-byte tag. That framing is part of the wire
//! contract and is shared with C++ endpoints, so none of it may drift.

#![forbid(unsafe_code)]

pub mod base64;

use aes::Aes128;
use ocb3::aead::{Aead, KeyInit, Payload};
use ocb3::Ocb3;

type Aes128Ocb3 = Ocb3<Aes128>;

/// Bytes added to a plaintext by encryption, not counting the nonce.
pub const ADDED_BYTES: usize = 16;
/// Largest datagram we will accept.
pub const RECEIVE_MTU: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    BadKeyLength,
    BadKeyEncoding,
    NotA128BitKey,
    BadNonceLength,
    TooShort,
    IntegrityCheck,
    CounterWrapped,
    BlockLimitReached,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CryptoError::BadKeyLength => "Key must be 22 letters long.",
            CryptoError::BadKeyEncoding => "Key must be well-formed base64.",
            CryptoError::NotA128BitKey => "Base64 key was not encoded 128-bit key.",
            CryptoError::BadNonceLength => "Nonce representation must be 8 octets long.",
            CryptoError::TooShort => "Ciphertext must contain nonce and tag.",
            CryptoError::IntegrityCheck => "Packet failed integrity check.",
            CryptoError::CounterWrapped => "Counter wrapped",
            CryptoError::BlockLimitReached => "Encrypted 2^47 blocks.",
        };
        f.write_str(s)
    }
}

impl std::error::Error for CryptoError {}

/// A 128-bit session key, written down as 22 base64 characters.
#[derive(Clone, PartialEq, Eq)]
pub struct Base64Key {
    key: [u8; 16],
}

impl Base64Key {
    /// A fresh random key.
    pub fn random() -> Self {
        let mut key = [0u8; 16];
        getrandom::fill(&mut key).expect("system randomness unavailable");
        Base64Key { key }
    }

    pub fn from_bytes(key: [u8; 16]) -> Self {
        Base64Key { key }
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.key
    }

    /// Parse the printable form. Rejects anything that would not print back identically,
    /// which catches a 22-character string whose trailing bits were altered.
    pub fn parse(printable: &str) -> Result<Self, CryptoError> {
        if printable.len() != 22 {
            return Err(CryptoError::BadKeyLength);
        }
        let mut padded = printable.as_bytes().to_vec();
        padded.extend_from_slice(b"==");
        let key = base64::decode_key(&padded).ok_or(CryptoError::BadKeyEncoding)?;
        let this = Base64Key { key };
        if this.printable() != printable {
            return Err(CryptoError::NotA128BitKey);
        }
        Ok(this)
    }

    /// The 22-character printable form, with padding stripped.
    pub fn printable(&self) -> String {
        let encoded = base64::encode_key(&self.key);
        String::from_utf8_lossy(&encoded[..22]).into_owned()
    }
}

impl std::fmt::Debug for Base64Key {
    /// Deliberately opaque: a key must not reach a log by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Base64Key(<redacted>)")
    }
}

/// A 12-byte nonce: four zero bytes then a 64-bit counter, big-endian.
///
/// Only the counter travels on the wire; the zero prefix is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nonce {
    bytes: [u8; 12],
}

impl Nonce {
    pub const LEN: usize = 12;

    pub fn new(val: u64) -> Self {
        let mut bytes = [0u8; 12];
        bytes[4..].copy_from_slice(&val.to_be_bytes());
        Nonce { bytes }
    }

    /// Rebuild from the 8 bytes that travelled on the wire.
    pub fn from_wire(wire: &[u8]) -> Result<Self, CryptoError> {
        if wire.len() != 8 {
            return Err(CryptoError::BadNonceLength);
        }
        let mut bytes = [0u8; 12];
        bytes[4..].copy_from_slice(wire);
        Ok(Nonce { bytes })
    }

    pub fn val(&self) -> u64 {
        let mut v = [0u8; 8];
        v.copy_from_slice(&self.bytes[4..]);
        u64::from_be_bytes(v)
    }

    /// The 8 bytes that travel on the wire.
    pub fn wire(&self) -> &[u8] {
        &self.bytes[4..]
    }

    pub fn as_bytes(&self) -> &[u8; 12] {
        &self.bytes
    }
}

/// A decrypted datagram together with the nonce it arrived under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub nonce: Nonce,
    pub text: Vec<u8>,
}

impl Message {
    pub fn new(nonce: Nonce, text: Vec<u8>) -> Self {
        Message { nonce, text }
    }
}

/// Encrypts and decrypts datagrams under one session key.
pub struct Session {
    cipher: Aes128Ocb3,
    blocks_encrypted: u64,
}

impl Session {
    pub fn new(key: &Base64Key) -> Self {
        Session {
            cipher: Aes128Ocb3::new(key.as_bytes().into()),
            blocks_encrypted: 0,
        }
    }

    /// Encrypt, returning the datagram as it goes on the wire: nonce suffix then
    /// ciphertext-with-tag.
    pub fn encrypt(&mut self, plaintext: &Message) -> Result<Vec<u8>, CryptoError> {
        let ct = self
            .cipher
            .encrypt(
                plaintext.nonce.as_bytes().into(),
                Payload {
                    msg: &plaintext.text,
                    aad: &[],
                },
            )
            .map_err(|_| CryptoError::IntegrityCheck)?;

        self.blocks_encrypted += (plaintext.text.len() >> 4) as u64;
        if plaintext.text.len() & 0xf != 0 {
            self.blocks_encrypted += 1;
        }
        // OCB's guarantees degrade as the square of the number of blocks seen. Both ends
        // share a key, so the per-side ceiling is 2^47, not 2^48.
        if self.blocks_encrypted >> 47 != 0 {
            return Err(CryptoError::BlockLimitReached);
        }

        let mut out = Vec::with_capacity(8 + ct.len());
        out.extend_from_slice(plaintext.nonce.wire());
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a datagram received from the peer.
    pub fn decrypt(&self, datagram: &[u8]) -> Result<Message, CryptoError> {
        if datagram.len() < 24 {
            return Err(CryptoError::TooShort);
        }
        let nonce = Nonce::from_wire(&datagram[..8])?;
        let text = self
            .cipher
            .decrypt(
                nonce.as_bytes().into(),
                Payload {
                    msg: &datagram[8..],
                    aad: &[],
                },
            )
            .map_err(|_| CryptoError::IntegrityCheck)?;
        Ok(Message { nonce, text })
    }
}

/// A source of nonce values that never repeats within a process.
#[derive(Debug, Default)]
pub struct Unique {
    counter: u64,
}

impl Unique {
    pub fn new() -> Self {
        Unique { counter: 0 }
    }

    /// The next never-before-used value.
    pub fn next_value(&mut self) -> Result<u64, CryptoError> {
        let rv = self.counter;
        self.counter = self.counter.wrapping_add(1);
        if self.counter == 0 {
            return Err(CryptoError::CounterWrapped);
        }
        Ok(rv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_printable_round_trip() {
        let key = Base64Key::random();
        let printed = key.printable();
        assert_eq!(printed.len(), 22);
        assert_eq!(Base64Key::parse(&printed).unwrap(), key);
    }

    #[test]
    fn key_rejects_altered_trailing_bits() {
        // The final character encodes only 4 significant bits; changing it to a value
        // with different low bits must not decode back to the same key.
        let key = Base64Key::from_bytes([0u8; 16]);
        let printed = key.printable();
        let mut bad = printed.into_bytes();
        bad[21] = b'B';
        let bad = String::from_utf8(bad).unwrap();
        assert_eq!(Base64Key::parse(&bad), Err(CryptoError::NotA128BitKey));
    }

    #[test]
    fn key_rejects_wrong_length() {
        assert_eq!(Base64Key::parse("short"), Err(CryptoError::BadKeyLength));
    }

    #[test]
    fn nonce_round_trips_through_the_wire_form() {
        for val in [0u64, 1, 42, u64::MAX, 1 << 47] {
            let n = Nonce::new(val);
            assert_eq!(n.val(), val);
            assert_eq!(n.wire().len(), 8);
            assert_eq!(Nonce::from_wire(n.wire()).unwrap(), n);
        }
        // The four leading bytes are always zero.
        assert_eq!(&Nonce::new(u64::MAX).as_bytes()[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn nonce_rejects_wrong_wire_length() {
        assert_eq!(Nonce::from_wire(&[0; 7]), Err(CryptoError::BadNonceLength));
        assert_eq!(Nonce::from_wire(&[0; 9]), Err(CryptoError::BadNonceLength));
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = Base64Key::random();
        let mut session = Session::new(&key);
        for len in [0usize, 1, 15, 16, 17, 100, 1000] {
            let text: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let msg = Message::new(Nonce::new(len as u64), text.clone());
            let wire = session.encrypt(&msg).unwrap();
            // 8 nonce bytes + plaintext + 16 tag bytes.
            assert_eq!(wire.len(), 8 + len + ADDED_BYTES);
            let back = session.decrypt(&wire).unwrap();
            assert_eq!(back.text, text);
            assert_eq!(back.nonce, msg.nonce);
        }
    }

    #[test]
    fn tampering_is_detected() {
        let key = Base64Key::random();
        let mut session = Session::new(&key);
        let msg = Message::new(Nonce::new(7), b"hello world, this is mosh".to_vec());
        let wire = session.encrypt(&msg).unwrap();
        for i in 0..wire.len() {
            let mut bad = wire.clone();
            bad[i] ^= 0x01;
            assert!(
                session.decrypt(&bad).is_err(),
                "flipping a bit at offset {i} went undetected"
            );
        }
    }

    #[test]
    fn a_different_key_cannot_decrypt() {
        let mut a = Session::new(&Base64Key::random());
        let b = Session::new(&Base64Key::random());
        let wire = a
            .encrypt(&Message::new(Nonce::new(1), b"secret".to_vec()))
            .unwrap();
        assert_eq!(b.decrypt(&wire), Err(CryptoError::IntegrityCheck));
    }

    #[test]
    fn short_datagrams_are_rejected() {
        let session = Session::new(&Base64Key::random());
        assert_eq!(session.decrypt(&[0; 23]), Err(CryptoError::TooShort));
    }

    #[test]
    fn unique_counter_starts_at_zero_and_advances() {
        let mut u = Unique::new();
        assert_eq!(u.next_value().unwrap(), 0);
        assert_eq!(u.next_value().unwrap(), 1);
    }
}
