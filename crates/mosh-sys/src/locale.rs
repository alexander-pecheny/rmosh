//! Locale services taken from the platform's C library.
//!
//! These are bound rather than reimplemented on purpose. Both endpoints of a session
//! compute character width and decode multibyte input independently, so the two must
//! agree exactly; a Rust reimplementation disagrees with glibc on 95 character widths
//! and on whether 814,732 codepoints are printable at all. See ADR 0002.

use std::ffi::{CStr, CString};

// The libc crate does not re-export the locale-dependent character functions, so we
// declare them here. Signatures follow POSIX.
mod sys {
    // The libc crate exports mbstate_t only on some platforms, and its size differs (8
    // bytes on glibc, 128 on Darwin), so we reserve the larger. Zero is the initial
    // conversion state everywhere.
    #[repr(C, align(8))]
    #[derive(Clone, Copy)]
    pub struct MbState(pub [u8; 128]);

    extern "C" {
        pub fn wcwidth(c: libc::wchar_t) -> libc::c_int;
        pub fn mbrtowc(
            pwc: *mut libc::wchar_t,
            s: *const libc::c_char,
            n: libc::size_t,
            ps: *mut MbState,
        ) -> libc::size_t;
        pub fn wcrtomb(s: *mut libc::c_char, wc: libc::wchar_t, ps: *mut MbState) -> libc::size_t;
    }
}

/// Upper bound on the bytes one character can encode to, from <limits.h>.
const MB_LEN_MAX: usize = 16;

/// Adopt the user's locale for every category, as `set_native_locale()` does in C++.
pub fn set_native_locale() {
    // SAFETY: LC_ALL is a valid category and "" is a valid NUL-terminated string.
    // setlocale returns a pointer to static storage that we do not retain.
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
}

/// Force the C locale, as `set_posix_locale()` does in C++.
pub fn set_posix_locale() {
    // SAFETY: as above; "C" is a valid locale name.
    unsafe {
        libc::setlocale(libc::LC_ALL, c"C".as_ptr());
    }
}

/// The current locale's codeset name, e.g. `UTF-8`.
pub fn codeset() -> String {
    // SAFETY: CODESET is a valid nl_item. nl_langinfo returns a pointer to static
    // storage owned by the C library, valid until the next setlocale/nl_langinfo call;
    // we copy out of it immediately and never retain it.
    let ptr = unsafe { libc::nl_langinfo(libc::CODESET) };
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: nl_langinfo returned non-null, so it points at a NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// True when the current locale encodes text as UTF-8.
pub fn is_utf8_locale() -> bool {
    let cs = codeset();
    cs.eq_ignore_ascii_case("UTF-8") || cs.eq_ignore_ascii_case("UTF8")
}

/// Columns a character occupies: `None` if it is not printable, otherwise 0, 1 or 2.
///
/// 0 means the character combines into the preceding cell rather than claiming one.
pub fn wcwidth(c: char) -> Option<u8> {
    // SAFETY: wcwidth accepts any wchar_t. On Linux wchar_t is 32-bit and every Rust
    // char is a valid Unicode scalar value, so the conversion cannot produce a trap
    // representation.
    let w = unsafe { sys::wcwidth(c as libc::wchar_t) };
    if w < 0 {
        None
    } else {
        Some(w as u8)
    }
}

/// One step of an incremental multibyte decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoded {
    /// A character was produced, consuming this many bytes.
    Char(char, usize),
    /// A NUL character was produced, consuming one byte.
    Nul,
    /// The input is a valid prefix; more bytes are needed.
    Incomplete,
    /// The input is not a valid sequence in this locale.
    Invalid,
}

/// Incremental multibyte decoder holding the C library's conversion state.
///
/// Wraps `mbstate_t` so that sequences split across reads decode correctly, which is
/// what the parser relies on when a character straddles two reads from the pty.
pub struct MbDecoder {
    state: sys::MbState,
}

impl Default for MbDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MbDecoder {
    pub fn new() -> Self {
        // SAFETY: an all-zero mbstate_t is the documented initial conversion state.
        MbDecoder {
            state: unsafe { std::mem::zeroed() },
        }
    }

    /// Reset to the initial conversion state.
    pub fn reset(&mut self) {
        // SAFETY: as above.
        self.state = unsafe { std::mem::zeroed() };
    }

    /// Decode the next character from `buf`.
    pub fn decode(&mut self, buf: &[u8]) -> Decoded {
        if buf.is_empty() {
            return Decoded::Incomplete;
        }
        let mut wc: libc::wchar_t = 0;
        // SAFETY: buf is non-empty and we pass its true length, so mbrtowc reads only
        // within it. &mut wc and &mut self.state are valid, uniquely borrowed, and
        // correctly typed. mbrtowc writes at most one wchar_t through the first pointer.
        let n = unsafe {
            sys::mbrtowc(
                &mut wc,
                buf.as_ptr() as *const libc::c_char,
                buf.len(),
                &mut self.state,
            )
        };
        match n {
            0 => Decoded::Nul,
            // (size_t)-1: invalid sequence. (size_t)-2: valid but incomplete prefix.
            n if n == usize::MAX => Decoded::Invalid,
            n if n == usize::MAX - 1 => Decoded::Incomplete,
            n => match char::from_u32(wc as u32) {
                Some(c) => Decoded::Char(c, n),
                None => Decoded::Invalid,
            },
        }
    }
}

/// Encode a character into the locale's multibyte form, appending to `out`.
///
/// Mirrors `Cell::append_to_str`, including its ASCII fast path.
pub fn append_wchar(out: &mut Vec<u8>, c: char) {
    if (c as u32) <= 0x7f {
        out.push(c as u8);
        return;
    }
    let mut buf = [0u8; MB_LEN_MAX];
    let mut state: sys::MbState =
        // SAFETY: an all-zero mbstate_t is the documented initial conversion state.
        unsafe { std::mem::zeroed() };
    // SAFETY: buf is MB_LEN_MAX bytes, the maximum wcrtomb can write for one character.
    // &mut state is valid and uniquely borrowed.
    let n = unsafe {
        sys::wcrtomb(
            buf.as_mut_ptr() as *mut libc::c_char,
            c as libc::wchar_t,
            &mut state,
        )
    };
    if n != usize::MAX {
        out.extend_from_slice(&buf[..n]);
    }
}

/// Look up a value in the environment without inheriting it, used for `MOSH_KEY`.
pub fn take_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let cname = CString::new(name).ok()?;
    // SAFETY: cname is a valid NUL-terminated string that outlives the call. This is
    // called before any thread is spawned, so the single-threaded requirement of
    // unsetenv holds.
    unsafe {
        libc::unsetenv(cname.as_ptr());
    }
    Some(value)
}
