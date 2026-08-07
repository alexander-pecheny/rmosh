//! Pseudo-terminal allocation and the child process that runs in it.
//!
//! `forkpty` has no safe wrapper anywhere: it forks, and between fork and exec only
//! async-signal-safe calls are permitted, which no signature can enforce. It therefore
//! lives here with the rest of the unsafe. See ADR 0003.

use std::ffi::CString;
use std::os::fd::RawFd;

/// Which side of a `forkpty` we came back on.
pub enum ForkPty {
    /// The parent, holding the master side and the child's process id.
    ///
    /// The master is handed over as an owned descriptor so callers that forbid unsafe
    /// can read and write it through the standard traits without adopting a raw fd.
    Parent {
        master: std::os::fd::OwnedFd,
        child: libc::pid_t,
    },
    /// The child, whose standard streams are already the pty.
    Child,
}

/// Allocate a pty and fork, giving the child the slave side as its controlling terminal.
///
/// # Safety of what follows
///
/// In the child, only async-signal-safe calls may be made before `exec`. Callers must
/// exec (or `_exit`) immediately; allocating, locking or running Rust destructors there
/// is undefined behaviour in a multi-threaded parent.
pub fn forkpty(rows: u16, cols: u16) -> std::io::Result<ForkPty> {
    let mut master: libc::c_int = -1;
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    // SAFETY: master points at a live local we own. Passing null for the slave name and
    // for termios asks for the defaults. winsize is a valid, fully-initialised struct.
    // forkpty writes the master descriptor through the first pointer in the parent only.
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut::<libc::termios>(),
            &winsize as *const libc::winsize as *mut libc::winsize,
        )
    };

    match pid {
        -1 => Err(std::io::Error::last_os_error()),
        0 => Ok(ForkPty::Child),
        child => {
            // SAFETY: forkpty just returned this descriptor to us in the parent and no
            // one else holds it, so taking ownership is sound.
            let master =
                unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(master) };
            Ok(ForkPty::Parent { master, child })
        }
    }
}

/// Replace this process with `program`. Only returns on failure.
///
/// `args` is the whole argv, argv[0] included: a login shell is spelled with the
/// program `/bin/zsh` and an argv[0] of `-zsh`, so the two cannot be conflated.
///
/// Intended for use immediately after [`forkpty`] returns [`ForkPty::Child`].
pub fn exec(program: &str, args: &[String], env: &[(String, String)]) -> std::io::Error {
    let Ok(prog) = CString::new(program) else {
        return std::io::Error::new(std::io::ErrorKind::InvalidInput, "program name has a NUL");
    };
    let mut argv: Vec<CString> = Vec::with_capacity(args.len());
    for a in args {
        match CString::new(a.as_str()) {
            Ok(c) => argv.push(c),
            Err(_) => {
                return std::io::Error::new(std::io::ErrorKind::InvalidInput, "argument has a NUL")
            }
        }
    }

    for (k, v) in env {
        // SAFETY: setenv copies both strings. This runs in the freshly forked child
        // before exec, which is single-threaded by construction.
        unsafe {
            let (Ok(k), Ok(v)) = (CString::new(k.as_str()), CString::new(v.as_str())) else {
                continue;
            };
            libc::setenv(k.as_ptr(), v.as_ptr(), 1);
        }
    }

    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    // SAFETY: prog and every argv entry are NUL-terminated and outlive the call; the
    // array is null-terminated as execvp requires. On success this never returns.
    unsafe {
        libc::execvp(prog.as_ptr(), argv_ptrs.as_ptr());
    }
    std::io::Error::last_os_error()
}

/// Set the window size of a pty.
pub fn set_window_size(fd: RawFd, rows: u16, cols: u16) -> std::io::Result<()> {
    let winsize = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd is an open pty master owned by the caller and winsize is a valid,
    // fully-initialised struct of the type TIOCSWINSZ expects.
    let r = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &winsize) };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Put a terminal into raw mode, returning its previous settings.
///
/// The client must see every keystroke unprocessed, since it is the remote application
/// that decides what they mean.
pub fn set_raw(fd: RawFd) -> std::io::Result<libc::termios> {
    // SAFETY: an all-zero termios is overwritten by tcgetattr before it is read.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: fd is owned by the caller; saved is a live, correctly-typed struct.
    if unsafe { libc::tcgetattr(fd, &mut saved) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut raw = saved;
    // SAFETY: cfmakeraw only rewrites the flags of the struct we pass it.
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    // The locale is UTF-8, so tell the line discipline as much; without it, erase
    // handling miscounts multibyte characters.
    raw.c_iflag |= libc::IUTF8;
    // SAFETY: raw is a live, fully-initialised termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(saved)
}

/// Restore terminal settings saved by [`set_raw`].
pub fn restore_termios(fd: RawFd, saved: &libc::termios) {
    // SAFETY: saved is a live, fully-initialised termios owned by the caller.
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, saved);
    }
}

/// Is this descriptor a terminal?
pub fn isatty(fd: RawFd) -> bool {
    // SAFETY: isatty only inspects the descriptor and never writes through a pointer.
    unsafe { libc::isatty(fd) == 1 }
}

/// Read a pty's window size.
pub fn window_size(fd: RawFd) -> std::io::Result<(u16, u16)> {
    let mut winsize = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd is owned by the caller and winsize is a live, correctly-typed struct
    // that TIOCGWINSZ fills in.
    let r = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut winsize) };
    if r < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((winsize.ws_row, winsize.ws_col))
}

/// Detach from the controlling terminal by forking; the parent exits.
///
/// Returns false in the parent, which should exit, and true in the child.
pub fn detach() -> std::io::Result<bool> {
    // SAFETY: fork itself is always permitted. The parent returns immediately and the
    // child continues as a normal process; nothing between fork and return allocates.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => Err(std::io::Error::last_os_error()),
        0 => {
            // SAFETY: setsid in a freshly forked child that is not a process group
            // leader always succeeds and takes no arguments.
            unsafe {
                libc::setsid();
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Put a descriptor into non-blocking mode.
///
/// The pty master needs this: a ^S written between the poll and the read can leave
/// read() blocking even though poll said data was ready, which wedges everything
/// attached to the pty. See third_party/mosh/src/tests/pty-deadlock.test.
pub fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: both calls only read and write the descriptor's own flags.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Point the standard streams at /dev/null.
///
/// A detached server must let go of the pipes it inherited, or whoever started it keeps
/// waiting for end-of-file that never comes. The C++ does this too, and skips it under
/// -v so that diagnostics remain visible.
pub fn close_standard_streams() -> std::io::Result<()> {
    // SAFETY: opening /dev/null and duplicating it over the three standard descriptors
    // is a plain descriptor operation; nothing is read or written through a pointer.
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull < 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if libc::dup2(devnull, fd) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        if devnull > libc::STDERR_FILENO {
            libc::close(devnull);
        }
    }
    Ok(())
}

/// Ignore a signal, so it cannot kill a detached server.
pub fn ignore_signal(sig: libc::c_int) {
    // SAFETY: SIG_IGN is a valid disposition for every catchable signal, and signal()
    // with it has no handler to be unsafe about.
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
    }
}

/// Restore a signal's default disposition.
///
/// A child must run this for everything the server ignores. An ignored signal, unlike a
/// handler, survives `exec`, so without it the user's shell and every process it starts
/// would inherit the server's own arrangements.
pub fn reset_signal(sig: libc::c_int) {
    // SAFETY: SIG_DFL is a valid disposition for every signal, and signal() with it has
    // no handler to be unsafe about. Async-signal-safe, so sound between fork and exec.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
    }
}

static TERMINATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn note_termination(_sig: libc::c_int) {
    TERMINATED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Turn a signal into a flag the caller can read, so a kill ends the session in order
/// rather than stopping the process where it stands, leaving its login record behind.
///
/// Deliberately without `SA_RESTART`: the interrupted poll is how the loop finds out.
pub fn catch_termination(sig: libc::c_int) {
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe, and sa
    // points at a live local for the duration of the call.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        let handler: extern "C" fn(libc::c_int) = note_termination;
        sa.sa_sigaction = handler as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

/// True once a signal handed to [`catch_termination`] has arrived.
pub fn termination_requested() -> bool {
    TERMINATED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reap a child if it has exited. Returns its exit status when it has.
pub fn try_wait(child: libc::pid_t) -> Option<i32> {
    let mut status: libc::c_int = 0;
    // SAFETY: status points at a live local we own; WNOHANG makes this non-blocking.
    let r = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
    if r == child {
        // WIFEXITED / WEXITSTATUS, spelled out because libc does not export the macros.
        if status & 0x7f == 0 {
            Some((status >> 8) & 0xff)
        } else {
            Some(128 + (status & 0x7f))
        }
    } else {
        None
    }
}

/// Milliseconds since an arbitrary fixed point, for the protocol's clocks.
pub fn now_ms() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // The newest reading, to fall back on. A failed call leaves `ts` zeroed, and
    // answering zero would read as the clock jumping back to the start of the session,
    // which every timeout in the protocol would then misjudge.
    static LAST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use std::sync::atomic::Ordering::Relaxed;

    // SAFETY: ts points at a live local we own.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return LAST_MS.load(Relaxed);
    }
    let ms = (ts.tv_sec as u64) * 1000 + (ts.tv_nsec as u64) / 1_000_000;
    LAST_MS.fetch_max(ms, Relaxed);
    ms
}

/// Wait until one of `fds` is readable, or `timeout_ms` elapses.
///
/// Returns which of them are readable, by index.
pub fn poll_readable(fds: &[RawFd], timeout_ms: i32) -> std::io::Result<Vec<usize>> {
    let mut pollfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();

    // SAFETY: pollfds is a live, correctly-typed array of exactly the length passed.
    let r = unsafe {
        libc::poll(
            pollfds.as_mut_ptr(),
            pollfds.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        // An interrupted poll is normal, not a failure.
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(Vec::new());
        }
        return Err(e);
    }

    Ok(pollfds
        .iter()
        .enumerate()
        .filter(|(_, p)| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
        .map(|(i, _)| i)
        .collect())
}

/// Spawn `cmd` with its stdout and stderr joined into the single pipe returned.
///
/// The launcher needs this because the two halves of the ssh handshake arrive on
/// different streams: the server's `MOSH CONNECT` on stdout, and the proxy's `MOSH IP`
/// on stderr. The Perl merged them with `open(STDERR, ">&STDOUT")` for the same reason.
pub fn spawn_with_merged_output(
    cmd: &mut std::process::Command,
) -> std::io::Result<(std::process::Child, std::fs::File)> {
    use std::os::fd::FromRawFd;

    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: fds is a live array of two ints, which is what pipe() writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe() just gave us both descriptors and no one else holds them.
    let (read, write) = unsafe {
        (
            std::os::fd::OwnedFd::from_raw_fd(fds[0]),
            std::os::fd::OwnedFd::from_raw_fd(fds[1]),
        )
    };
    // Neither end should survive into the child under its own number; the dup2 that
    // installs stdout and stderr clears the flag on the copies it makes.
    for fd in [&read, &write] {
        set_cloexec(fd)?;
    }

    cmd.stdout(std::process::Stdio::from(write.try_clone()?));
    cmd.stderr(std::process::Stdio::from(write));
    let child = cmd.spawn()?;
    Ok((child, std::fs::File::from(read)))
}

fn set_cloexec(fd: &std::os::fd::OwnedFd) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fd is owned and open for the duration of the call.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isatty_does_not_disturb_the_terminal() {
        // This exists because an earlier version tested for a terminal by *setting* the
        // window size to 0x0, which resized the real terminal and made a peer assert on
        // a zero-height screen. Querying must never modify anything.
        assert!(!isatty(-1));
    }

    #[test]
    fn the_clock_advances() {
        let a = now_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = now_ms();
        assert!(b >= a + 10, "clock went from {a} to {b}");
    }

    #[test]
    fn polling_nothing_times_out_rather_than_hanging() {
        let start = now_ms();
        let ready = poll_readable(&[], 30).unwrap();
        assert!(ready.is_empty());
        assert!(now_ms() >= start + 20);
    }

    #[test]
    fn a_readable_pipe_is_reported() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: fds is a live array of two ints, which is what pipe() writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);

        // Nothing written yet, so it must not be readable.
        assert!(poll_readable(&[fds[0]], 10).unwrap().is_empty());

        // SAFETY: fds[1] is the open write end; we write one byte from a live buffer.
        unsafe {
            libc::write(fds[1], b"x".as_ptr() as *const libc::c_void, 1);
        }
        assert_eq!(poll_readable(&[fds[0]], 100).unwrap(), vec![0]);

        // SAFETY: both descriptors are open and owned by this test.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
