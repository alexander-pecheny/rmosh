//! Login records.
//!
//! The server registers its pty so that `who` lists the session, and scans existing
//! records to warn about sessions the user has left detached. Neither has a safe wrapper
//! crate, which is why they live here.

/// One login record, reduced to the fields the server actually reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub user: String,
    pub line: String,
    pub host: String,
}

/// Every `USER_PROCESS` login record currently in the database.
pub fn user_entries() -> Vec<Entry> {
    let mut out = Vec::new();
    // SAFETY: setutxent/getutxent/endutxent are called in order on this thread. The
    // returned pointer aliases a static buffer owned by libc, valid until the next
    // getutxent call; every field is copied out before iterating again.
    unsafe {
        libc::setutxent();
        loop {
            let entry = libc::getutxent();
            if entry.is_null() {
                break;
            }
            if (*entry).ut_type == libc::USER_PROCESS {
                out.push(Entry {
                    user: cstr_field(&(*entry).ut_user),
                    line: cstr_field(&(*entry).ut_line),
                    host: cstr_field(&(*entry).ut_host),
                });
            }
        }
        libc::endutxent();
    }
    out
}

/// Read a fixed-size, possibly unterminated character array out of a login record.
///
/// SAFETY: caller guarantees `field` points at a live array of the given length.
unsafe fn cstr_field(field: &[libc::c_char]) -> String {
    let bytes: &[u8] =
        // SAFETY: c_char and u8 have the same size and alignment, and we only read.
        unsafe { std::slice::from_raw_parts(field.as_ptr() as *const u8, field.len()) };
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(feature = "utempter")]
extern "C" {
    fn utempter_add_record(master_fd: libc::c_int, host: *const libc::c_char) -> libc::c_int;
    fn utempter_remove_record(master_fd: libc::c_int) -> libc::c_int;
}

/// Add a login record for a pty. Silently does nothing when built without libutempter,
/// which mirrors the C++ behaviour when the library is absent at configure time.
#[allow(unused_variables)]
pub fn add_record(master_fd: std::os::fd::RawFd, host: &str) {
    #[cfg(feature = "utempter")]
    {
        let Ok(host) = std::ffi::CString::new(host) else {
            return;
        };
        // SAFETY: master_fd is an open pty master owned by the caller; host is a valid
        // NUL-terminated string alive across the call.
        unsafe {
            utempter_add_record(master_fd, host.as_ptr());
        }
    }
}

/// Remove the login record for a pty.
#[allow(unused_variables)]
pub fn remove_record(master_fd: std::os::fd::RawFd) {
    #[cfg(feature = "utempter")]
    // SAFETY: master_fd is an open pty master owned by the caller.
    unsafe {
        utempter_remove_record(master_fd);
    }
}

/// True when the named terminal device still exists, used to tell a stale login record
/// from a live detached session.
pub fn device_exists(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    let path = if line.starts_with('/') {
        std::path::PathBuf::from(line)
    } else {
        std::path::Path::new("/dev").join(line)
    };
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_login_database_without_crashing() {
        // Content depends on the machine; we assert only that iteration terminates and
        // that fields are NUL-trimmed rather than fixed-width.
        for e in user_entries() {
            assert!(!e.user.contains('\0'));
            assert!(!e.line.contains('\0'));
            assert!(!e.host.contains('\0'));
        }
    }

    #[test]
    fn device_existence_is_checked_under_dev() {
        assert!(device_exists("null"));
        assert!(!device_exists("definitely-not-a-tty-12345"));
        assert!(!device_exists(""));
    }
}
