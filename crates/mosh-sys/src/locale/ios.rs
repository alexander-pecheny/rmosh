//! iOS ships no locale data, so the C library cannot leave the "C" locale there and
//! `wcwidth` and `mbrtowc` reject every byte above ASCII. This is the one platform where
//! ADR 0002 cannot hold; text is UTF-8 by fiat and widths come from a Unicode table.

#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use super::Decoded;
use unicode_width::UnicodeWidthChar;

pub fn set_native_locale() {}

pub fn set_locale(_name: &str) -> bool {
    true
}

pub fn set_posix_locale() {}

pub fn codeset() -> String {
    "UTF-8".into()
}

pub fn wcwidth(c: char) -> Option<u8> {
    if c.is_control() {
        return None;
    }
    c.width().map(|w| w.min(2) as u8)
}

#[derive(Default)]
pub struct MbDecoder;

impl MbDecoder {
    pub fn new() -> Self {
        MbDecoder
    }

    pub fn reset(&mut self) {}

    pub fn decode(&mut self, buf: &[u8]) -> Decoded {
        let Some(&lead) = buf.first() else {
            return Decoded::Incomplete;
        };
        if lead == 0 {
            return Decoded::Nul;
        }
        let n = match lead {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Decoded::Invalid,
        };
        if buf.len() < n {
            return match std::str::from_utf8(buf) {
                Err(e) if e.error_len().is_none() => Decoded::Incomplete,
                _ => Decoded::Invalid,
            };
        }
        match std::str::from_utf8(&buf[..n]) {
            Ok(s) => Decoded::Char(s.chars().next().unwrap(), n),
            Err(_) => Decoded::Invalid,
        }
    }
}

pub fn append_wchar(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_one_character_at_a_time() {
        let mut d = MbDecoder::new();
        assert_eq!(d.decode(b"a"), Decoded::Char('a', 1));
        assert_eq!(d.decode("❯x".as_bytes()), Decoded::Char('\u{276f}', 3));
        assert_eq!(d.decode("🌐".as_bytes()), Decoded::Char('\u{1f310}', 4));
        assert_eq!(d.decode(&"🌐".as_bytes()[..2]), Decoded::Incomplete);
        assert_eq!(d.decode(b"\xff"), Decoded::Invalid);
        assert_eq!(d.decode(b"\xe2\x28\xa1"), Decoded::Invalid);
        assert_eq!(d.decode(b"\0"), Decoded::Nul);
        assert_eq!(d.decode(b""), Decoded::Incomplete);
    }

    #[test]
    fn widths_follow_the_unicode_table() {
        assert_eq!(wcwidth('a'), Some(1));
        assert_eq!(wcwidth('\u{4e00}'), Some(2));
        assert_eq!(wcwidth('\u{0301}'), Some(0));
        assert_eq!(wcwidth('\u{276f}'), Some(1));
        assert_eq!(wcwidth('\u{e0a0}'), Some(1)); // a nerd-font glyph in the private use area
        assert_eq!(wcwidth('\x07'), None);
        assert_eq!(wcwidth('\u{9b}'), None);
    }

    #[test]
    fn round_trips_through_append() {
        let mut out = Vec::new();
        append_wchar(&mut out, 'ы');
        append_wchar(&mut out, 'a');
        assert_eq!(out, "ыa".as_bytes());
    }
}
