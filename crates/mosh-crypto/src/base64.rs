//! Base64 restricted to what mosh actually needs: 16-byte keys as 22 printable
//! characters. The C++ makes the same restriction and asserts on any other length.

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Reverse map from ASCII to a six-bit value; `None` for anything not in the alphabet.
fn sixbit(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode 24 base64 characters ("22 payload + ==") into 16 bytes.
pub fn decode_key(b64: &[u8]) -> Option<[u8; 16]> {
    if b64.len() != 24 {
        return None;
    }
    let mut raw = [0u8; 16];
    let mut bytes: u32 = 0;
    let mut out = 0;
    for (i, &c) in b64[..22].iter().enumerate() {
        bytes = (bytes << 6) | u32::from(sixbit(c)?);
        // Every fourth character completes three output bytes.
        if i % 4 == 3 {
            raw[out] = (bytes >> 16) as u8;
            raw[out + 1] = (bytes >> 8) as u8;
            raw[out + 2] = bytes as u8;
            out += 3;
            bytes = 0;
        }
    }
    raw[15] = (bytes >> 4) as u8;
    if b64[22] != b'=' || b64[23] != b'=' {
        return None;
    }
    Some(raw)
}

/// Encode 16 bytes as 24 base64 characters, the last two of which are always `=`.
pub fn encode_key(raw: &[u8; 16]) -> [u8; 24] {
    let mut b64 = [0u8; 24];
    for i in 0..5 {
        let r = &raw[i * 3..];
        let bytes = (u32::from(r[0]) << 16) | (u32::from(r[1]) << 8) | u32::from(r[2]);
        let o = &mut b64[i * 4..];
        o[0] = TABLE[((bytes >> 18) & 0x3f) as usize];
        o[1] = TABLE[((bytes >> 12) & 0x3f) as usize];
        o[2] = TABLE[((bytes >> 6) & 0x3f) as usize];
        o[3] = TABLE[(bytes & 0x3f) as usize];
    }
    let last = raw[15];
    b64[20] = TABLE[((last >> 2) & 0x3f) as usize];
    b64[21] = TABLE[((last << 4) & 0x3f) as usize];
    b64[22] = b'=';
    b64[23] = b'=';
    b64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_single_bit() {
        for byte in 0..16 {
            for bit in 0..8 {
                let mut raw = [0u8; 16];
                raw[byte] = 1 << bit;
                let encoded = encode_key(&raw);
                assert_eq!(decode_key(&encoded), Some(raw), "byte {byte} bit {bit}");
            }
        }
    }

    #[test]
    fn rejects_characters_outside_the_alphabet() {
        let mut encoded = encode_key(&[0u8; 16]);
        encoded[0] = b'!';
        assert_eq!(decode_key(&encoded), None);
    }

    #[test]
    fn rejects_missing_padding() {
        let mut encoded = encode_key(&[0u8; 16]);
        encoded[23] = b'A';
        assert_eq!(decode_key(&encoded), None);
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!(decode_key(b"tooshort"), None);
    }

    #[test]
    fn all_zeroes_and_all_ones() {
        for raw in [[0u8; 16], [0xffu8; 16]] {
            assert_eq!(decode_key(&encode_key(&raw)), Some(raw));
        }
    }
}
