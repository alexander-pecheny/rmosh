//! The child of a `forkpty` must not inherit the server's signal arrangements.
//!
//! An ignored signal, unlike a handler, survives `exec`. The Rust runtime ignores SIGPIPE
//! for every process it starts, and mosh-server ignores SIGHUP as well so that the ssh
//! session ending cannot kill it. Neither belongs to the shell it goes on to run.

use std::io::Read;

use mosh_sys::pty;

/// Run `sh -c script` on a pty and return what it wrote, with the signals reset first
/// only if `reset` says so.
fn run_on_pty(script: &str, reset: bool) -> String {
    let (master, child) = match pty::forkpty(24, 80).expect("forkpty") {
        pty::ForkPty::Child => {
            if reset {
                pty::reset_signal(libc::SIGPIPE);
                pty::reset_signal(libc::SIGHUP);
            }
            pty::exec(
                "/bin/sh",
                &["sh".to_string(), "-c".to_string(), script.to_string()],
                &[],
            );
            std::process::exit(1);
        }
        pty::ForkPty::Parent { master, child } => (master, child),
    };

    pty::set_nonblocking(std::os::fd::AsRawFd::as_raw_fd(&master)).expect("nonblocking");
    let mut file = std::fs::File::from(master);
    let mut out = Vec::new();
    let deadline = pty::now_ms() + 5000;
    let mut buf = [0u8; 1024];
    while pty::now_ms() < deadline {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            // The pty is empty for now, or the slave has gone, which reads as EIO.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    let _ = pty::try_wait(child);
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn a_reset_child_dies_of_the_signal_the_server_was_ignoring() {
    // Without the reset the shell survives its own SIGPIPE, which is the state that
    // would leak into every command the user runs -- `yes | head` never finishing, and
    // nothing under the session ever noticing a hangup.
    let inherited = run_on_pty("kill -PIPE $$; echo SURVIVED", false);
    assert!(
        inherited.contains("SURVIVED"),
        "SIGPIPE was not ignored to begin with, so this test proves nothing: {inherited:?}"
    );

    let reset = run_on_pty("kill -PIPE $$; echo SURVIVED", true);
    assert!(
        !reset.contains("SURVIVED"),
        "the child kept the server's ignored SIGPIPE across exec: {reset:?}"
    );
}
