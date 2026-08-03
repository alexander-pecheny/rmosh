//! Terminal capability lookups, taken from the same terminfo database ncurses reads.
//!
//! Only five capabilities are ever queried, but their answers decide which escape
//! sequences we emit, and the end-to-end tests compare captured terminal output
//! directly. Reading the database with the same library the C++ uses keeps those bytes
//! identical rather than merely equivalent.

use std::ffi::{CStr, CString};

extern "C" {
    fn setupterm(
        term: *const libc::c_char,
        filedes: libc::c_int,
        errret: *mut libc::c_int,
    ) -> libc::c_int;
    fn tigetstr(capname: *const libc::c_char) -> *mut libc::c_char;
    fn tigetflag(capname: *const libc::c_char) -> libc::c_int;
    fn tigetnum(capname: *const libc::c_char) -> libc::c_int;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermError {
    Hardcopy,
    UnknownType,
    DatabaseMissing,
    Unknown,
    InvalidCapability(String),
}

impl std::fmt::Display for TermError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TermError::Hardcopy => {
                write!(
                    f,
                    "Terminal is hardcopy and cannot be used by curses applications."
                )
            }
            TermError::UnknownType => write!(f, "Unknown terminal type."),
            TermError::DatabaseMissing => write!(f, "Terminfo database could not be found."),
            TermError::Unknown => write!(f, "Unknown terminfo error."),
            TermError::InvalidCapability(c) => write!(f, "Invalid terminfo capability {c}"),
        }
    }
}

impl std::error::Error for TermError {}

/// Initialise terminfo from `$TERM`. Must succeed before any capability lookup.
pub fn setup() -> Result<(), TermError> {
    let mut errret: libc::c_int = -2;
    // SAFETY: a null term argument tells setupterm to read $TERM itself. Passing fd 1
    // matches the C++ call. errret points at a live local we exclusively own.
    let ret = unsafe { setupterm(std::ptr::null(), 1, &mut errret) };
    if ret == 0 {
        return Ok(());
    }
    Err(match errret {
        1 => TermError::Hardcopy,
        0 => TermError::UnknownType,
        -1 => TermError::DatabaseMissing,
        _ => TermError::Unknown,
    })
}

/// A string capability, or `None` when the terminal lacks it.
pub fn string(capname: &str) -> Result<Option<Vec<u8>>, TermError> {
    let name = CString::new(capname).map_err(|_| TermError::InvalidCapability(capname.into()))?;
    // SAFETY: name is a valid NUL-terminated string alive across the call. tigetstr
    // returns storage owned by ncurses which stays valid until the next setupterm;
    // we copy out of it immediately.
    let val = unsafe { tigetstr(name.as_ptr()) };
    if val.is_null() {
        return Ok(None);
    }
    // ncurses signals "not a string capability" with (char *)-1.
    if val as isize == -1 {
        return Err(TermError::InvalidCapability(capname.into()));
    }
    // SAFETY: val is non-null and not the sentinel, so it is a NUL-terminated string.
    Ok(Some(unsafe { CStr::from_ptr(val) }.to_bytes().to_vec()))
}

/// A boolean capability.
pub fn flag(capname: &str) -> Result<bool, TermError> {
    let name = CString::new(capname).map_err(|_| TermError::InvalidCapability(capname.into()))?;
    // SAFETY: name is a valid NUL-terminated string alive across the call.
    let val = unsafe { tigetflag(name.as_ptr()) };
    match val {
        -1 => Err(TermError::InvalidCapability(capname.into())),
        0 => Ok(false),
        _ => Ok(true),
    }
}

/// A numeric capability; negative when absent.
pub fn number(capname: &str) -> libc::c_int {
    let Ok(name) = CString::new(capname) else {
        return -1;
    };
    // SAFETY: name is a valid NUL-terminated string alive across the call.
    unsafe { tigetnum(name.as_ptr()) }
}
