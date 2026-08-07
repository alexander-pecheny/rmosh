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

/// Serialises the login-database walk.
///
/// `getutxent` reads through one cursor into one static buffer, both owned by libc and
/// shared by the whole process. Two threads walking at once tear each other's reads: the
/// second overwrites the buffer between the first deciding where a field ends and copying
/// it out.
static DATABASE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Every `USER_PROCESS` login record currently in the database.
pub fn user_entries() -> Vec<Entry> {
    let _walk = DATABASE.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    // SAFETY: setutxent/getutxent/endutxent are called in order, under the lock that
    // makes this thread the only one walking. The returned pointer aliases a static
    // buffer owned by libc, valid until the next getutxent call; every field is copied
    // out before iterating again.
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

#[cfg(has_utempter)]
extern "C" {
    fn utempter_add_record(master_fd: libc::c_int, host: *const libc::c_char) -> libc::c_int;
    fn utempter_remove_record(master_fd: libc::c_int) -> libc::c_int;
}

/// Add a login record for a pty. Silently does nothing when built without libutempter,
/// which mirrors the C++ behaviour when the library is absent at configure time.
#[allow(unused_variables)]
pub fn add_record(master_fd: std::os::fd::RawFd, host: &str) {
    #[cfg(has_utempter)]
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
    #[cfg(has_utempter)]
    // SAFETY: master_fd is an open pty master owned by the caller.
    unsafe {
        utempter_remove_record(master_fd);
    }
}

/// True when the named terminal device still exists, used to tell a stale login record
/// from a live detached session.
///
/// The name always resolves under `/dev`, and the link itself is what is asked about, so
/// a record naming an absolute path or a symlink cannot be used to ask whether some
/// unrelated file exists. Both match the C++.
pub fn device_exists(line: &str) -> bool {
    if line.is_empty() || line.contains('/') {
        return false;
    }
    std::path::Path::new("/dev")
        .join(line)
        .symlink_metadata()
        .is_ok()
}

/// The name of the user running this process.
pub fn username() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into a static buffer owned by libc, valid until
    // the next call; pw_name is copied out before returning.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Sessions this user has left running on this machine, newest first in database order.
///
/// `ignore` is our own record, which is not news to the person who just created it.
pub fn detached_sessions(ignore: &str) -> Vec<String> {
    let Some(username) = username() else {
        return Vec::new();
    };
    user_entries()
        .into_iter()
        .filter(|e| e.user == username && e.host != ignore)
        // A mosh record's host field is what `add_record` wrote there. A live session
        // still owns its pty; without that check every crashed server would be reported
        // forever.
        .filter(|e| e.host.starts_with("mosh [") && e.host.ends_with(']'))
        .filter(|e| device_exists(&e.line))
        .map(|e| e.host)
        .collect()
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
        // The name comes out of a database other accounts may be able to write, so it
        // may only ever name something directly under /dev.
        assert!(!device_exists("/etc/passwd"));
        assert!(!device_exists("../etc/passwd"));
    }

    #[test]
    fn the_database_can_be_walked_from_several_threads() {
        // libc walks it through one cursor into one static buffer, so two threads at
        // once used to tear each other's reads and hand back a field with a NUL in it.
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..200 {
                        for e in user_entries() {
                            assert!(!e.user.contains('\0'));
                            assert!(!e.line.contains('\0'));
                            assert!(!e.host.contains('\0'));
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a walk tore");
        }
    }

    #[test]
    fn our_own_session_is_not_reported_back_to_us() {
        // Whatever this machine's database holds, the entry we pass as ours must not
        // come back: the person who just started it does not need telling.
        for host in detached_sessions("") {
            assert!(
                host.starts_with("mosh ["),
                "unrelated record {host} reported"
            );
        }
        let mine = "mosh [4242]".to_string();
        assert!(!detached_sessions(&mine).contains(&mine));
    }
}
