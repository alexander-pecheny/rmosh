//! zlib compression of the payload before it is fragmented.
//!
//! Transliterated from `src/network/compressor.cc`.
//!
//! Only the format is contractual, not the bytes: the peer inflates whatever we produce,
//! so a different deflate implementation is fine as long as it writes a zlib stream. The
//! C++ uses zlib's one-shot `compress`/`uncompress` at the default level, and flate2's
//! zlib encoder at its default matches that framing.

use std::io::prelude::*;

use flate2::read::{ZlibDecoder, ZlibEncoder};
use flate2::Compression;

/// The C++ decompresses into a fixed buffer of this size and dies if the result does not
/// fit, which is its effective limit on terminal size. We enforce the same ceiling, but
/// by refusing rather than by aborting: the input comes from the network.
pub const BUFFER_SIZE: usize = 2048 * 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressError {
    /// The data does not decompress, or is not a zlib stream at all.
    Malformed,
    /// The result would exceed the size limit.
    TooLarge,
}

impl std::fmt::Display for CompressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressError::Malformed => f.write_str("could not decompress payload"),
            CompressError::TooLarge => f.write_str("decompressed payload is too large"),
        }
    }
}

impl std::error::Error for CompressError {}

pub fn compress(input: &[u8]) -> Result<Vec<u8>, CompressError> {
    let mut encoder = ZlibEncoder::new(input, Compression::default());
    let mut out = Vec::new();
    encoder
        .read_to_end(&mut out)
        .map_err(|_| CompressError::Malformed)?;
    Ok(out)
}

pub fn uncompress(input: &[u8]) -> Result<Vec<u8>, CompressError> {
    // An empty payload is never a zlib stream -- even compressing nothing produces a
    // header and a checksum -- but the decoder treats it as a clean end of input, so
    // reject it before asking.
    if input.is_empty() {
        return Err(CompressError::Malformed);
    }
    // Read one byte past the limit so that hitting it exactly is distinguishable from
    // overrunning it.
    let mut decoder = ZlibDecoder::new(input).take(BUFFER_SIZE as u64 + 1);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|_| CompressError::Malformed)?;
    if out.len() > BUFFER_SIZE {
        return Err(CompressError::TooLarge);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for case in [
            &b""[..],
            b"hello",
            &[0u8; 10_000][..],
            "some unicode \u{4e00}\u{e9}".as_bytes(),
        ] {
            assert_eq!(uncompress(&compress(case).unwrap()).unwrap(), case);
        }
    }

    #[test]
    fn compresses_repetitive_data() {
        let input = vec![b'a'; 100_000];
        let compressed = compress(&input).unwrap();
        assert!(
            compressed.len() < input.len() / 10,
            "repetitive data barely compressed: {} -> {}",
            input.len(),
            compressed.len()
        );
    }

    #[test]
    fn writes_a_zlib_stream() {
        // The peer's zlib must recognise the framing: 0x78 is the zlib CMF byte for a
        // 32K window with deflate, which is what the C++ produces.
        let out = compress(b"hello").unwrap();
        assert_eq!(out[0], 0x78, "not a zlib stream: {:02x?}", &out[..2]);
    }

    #[test]
    fn rejects_garbage_rather_than_dying() {
        assert_eq!(uncompress(b"not compressed"), Err(CompressError::Malformed));
        assert_eq!(uncompress(&[]), Err(CompressError::Malformed));
    }

    #[test]
    fn rejects_a_decompression_bomb() {
        // A long run of zeroes compresses to almost nothing and expands past the limit.
        let bomb = compress(&vec![0u8; BUFFER_SIZE + 1]).unwrap();
        assert!(bomb.len() < 10_000, "test bomb was not actually small");
        assert_eq!(uncompress(&bomb), Err(CompressError::TooLarge));
    }
}
