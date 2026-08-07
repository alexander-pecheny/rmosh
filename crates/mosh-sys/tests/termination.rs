//! A terminated server has to get far enough to tidy up after itself.
//!
//! `kill` is how a session nobody is attached to is ended, and the default disposition
//! stops the process where it stands: no destructors, so the login record it added stays
//! in the database and `who` goes on listing a session that is over.

use mosh_sys::pty;

#[test]
fn a_kill_becomes_a_flag_instead_of_the_end_of_the_process() {
    assert!(
        !pty::termination_requested(),
        "the flag was already set before anything sent a signal"
    );

    pty::catch_termination(libc::SIGTERM);
    // Reaching the next line at all is half of what this test asserts: without the
    // handler this test binary is killed here and the run fails.
    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);

    // The handler runs on the raising thread before raise() returns.
    assert!(
        pty::termination_requested(),
        "we survived the signal but never noticed it, so the loop would never exit"
    );
}
