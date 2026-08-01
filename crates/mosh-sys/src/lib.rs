//! The one crate in this workspace permitted to contain `unsafe`.
//!
//! Everything here is FFI that no safe wrapper crate provides: the locale functions,
//! terminal capability lookups, and login records. Anything `nix` already wraps safely
//! belongs there instead, not here. Every `unsafe` block below states the invariant that
//! makes it sound. See ADR 0003.

pub mod locale;
pub mod terminfo;
pub mod utmp;

pub use locale::{
    append_wchar, codeset, is_utf8_locale, set_native_locale, set_posix_locale, take_env, wcwidth,
    Decoded, MbDecoder,
};

/// Stop the process dumping core, so session keys cannot reach the disk.
pub fn disable_dumping_core() -> std::io::Result<u64> {
    let mut limit: libc::rlimit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is a live, exclusively owned rlimit of the right type.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let saved = limit.rlim_cur;
    limit.rlim_cur = 0;
    // SAFETY: as above; lowering rlim_cur to 0 is always permitted.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(saved)
}

/// Restore the core limit saved by [`disable_dumping_core`]. Failure is deliberately
/// silent, matching the C++.
pub fn reenable_dumping_core(saved: u64) {
    let mut limit: libc::rlimit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is a live, exclusively owned rlimit of the right type.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_CORE, &mut limit) == 0 {
            limit.rlim_cur = saved as libc::rlim_t;
            libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_widths_match_expectations() {
        locale::set_native_locale();
        assert_eq!(wcwidth('a'), Some(1));
        assert_eq!(wcwidth(' '), Some(1));
        // A control character is not printable.
        assert_eq!(wcwidth('\x07'), None);
    }

    #[test]
    fn combining_marks_claim_no_cell() {
        locale::set_native_locale();
        if !locale::is_utf8_locale() {
            return; // width answers are only meaningful in a UTF-8 locale
        }
        // COMBINING ACUTE ACCENT joins the preceding grapheme.
        assert_eq!(wcwidth('\u{0301}'), Some(0));
        // A CJK ideograph claims two cells.
        assert_eq!(wcwidth('\u{4e00}'), Some(2));
    }

    #[test]
    fn decoder_handles_split_sequences() {
        locale::set_native_locale();
        if !locale::is_utf8_locale() {
            return;
        }
        let mut d = MbDecoder::new();
        // U+00E9 is two bytes; feed them one at a time.
        assert_eq!(d.decode(&[0xc3]), Decoded::Incomplete);
        assert_eq!(d.decode(&[0xa9]), Decoded::Char('\u{e9}', 1));
    }

    #[test]
    fn decoder_rejects_invalid_sequences() {
        locale::set_native_locale();
        if !locale::is_utf8_locale() {
            return;
        }
        let mut d = MbDecoder::new();
        assert_eq!(d.decode(&[0xff]), Decoded::Invalid);
    }

    #[test]
    fn round_trips_non_ascii() {
        locale::set_native_locale();
        if !locale::is_utf8_locale() {
            return;
        }
        let mut out = Vec::new();
        append_wchar(&mut out, '\u{4e00}');
        let mut d = MbDecoder::new();
        assert_eq!(d.decode(&out), Decoded::Char('\u{4e00}', out.len()));
    }
}
