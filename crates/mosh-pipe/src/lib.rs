//! A command's standard streams carried over mosh's transport instead of a terminal.
//!
//! The server runs the command and keeps its output as a [`Stream`] the client follows;
//! the client's own input goes the other way as one too. Each end can roam, sleep and
//! come back as a mosh session does, and the command never notices.

#![forbid(unsafe_code)]

mod stream;

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use mosh_network::connection::Connection;
use mosh_network::sender::SyncState;
use mosh_network::transport::Transport;
use mosh_sys::pty;

pub use stream::Stream;

/// Output read ahead of the client's acknowledgements before the command is left to
/// wait, which bounds what a lost datagram costs to resend.
const BACKLOG: usize = 128 * 1024;
/// The server gives up on a client that never arrives, or that has been away this long.
const NO_CLIENT: u64 = 60_000;
const CLIENT_GONE: u64 = 24 * 3600 * 1000;
/// The client gives up after this long awake without hearing from the server. Time
/// asleep does not count, so a laptop that sleeps through the night picks up where it was.
const SERVER_LOST: Duration = Duration::from_secs(20);
const SUSPENDED: Duration = Duration::from_secs(5);
/// How long a command has to finish once its input is closed, before it is killed.
const GRACE: Duration = Duration::from_millis(500);
/// The state number a peer moves to when it is leaving.
const SHUTDOWN_NUM: u64 = u64::MAX;

type Pipe = Transport<Stream, Stream>;

/// New bytes from the peer since the last call, if its state moved on.
fn received(transport: &mut Pipe, last: &mut u64) -> Option<Stream> {
    if transport.remote_state_num() == *last {
        return None;
    }
    *last = transport.remote_state_num();
    let mut got = Stream::default();
    got.apply_string(&transport.get_remote_diff()).ok()?;
    Some(got)
}

fn finished(transport: &Pipe, now: u64) -> bool {
    let sender = &transport.sender;
    sender.shutdown_in_progress()
        && (sender.shutdown_acknowledged() || sender.counterparty_shutdown_acknowledged() || sender.shutdown_ack_timed_out(now))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// Read once from a descriptor poll said was ready: its bytes, or None at its end.
fn read_ready(from: &mut impl Read, buf: &mut [u8]) -> Option<usize> {
    match from.read(buf) {
        Ok(0) => None,
        Ok(n) => Some(n),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) => Some(0),
        Err(_) => None,
    }
}

/// Runs `command` and serves its streams to one client until either side leaves.
pub fn serve(connection: Connection, command: &[String]) -> std::io::Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "a pipe needs a command"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take();
    if let Some(stdin) = &stdin {
        pty::set_nonblocking(stdin.as_raw_fd())?;
    }
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());

    let start = pty::now_ms();
    let mut transport = Transport::new(connection, Stream::default(), Stream::default(), start);
    let mut last_remote = 0;
    let mut to_child: Vec<u8> = Vec::new();
    let mut input_ended = false;
    let mut buf = [0u8; 16384];

    loop {
        if pty::termination_requested() {
            break;
        }
        let now = pty::now_ms();
        let reaping = out.is_none() && err.is_none() && transport.sender.current_state().end.is_none();
        let wait = transport.wait_time(now).min(if reaping { 50 } else { 1000 }) as i32;

        let mut fds: Vec<RawFd> = transport.connection.sockets().iter().map(|s| s.as_raw_fd()).collect();
        let network = fds.len();
        let room = transport.sender.current_state().backlog() < BACKLOG && !transport.sender.shutdown_in_progress();
        let out_at = out.as_ref().filter(|_| room).map(|o| { fds.push(o.as_raw_fd()); fds.len() - 1 });
        let err_at = err.as_ref().filter(|_| room).map(|e| { fds.push(e.as_raw_fd()); fds.len() - 1 });

        let ready = pty::poll_readable(&fds, wait.max(0))?;
        let now = pty::now_ms();

        if ready.iter().any(|&i| i < network) {
            while transport.recv(now).is_some() {}
        }
        if let Some(got) = received(&mut transport, &mut last_remote) {
            to_child.extend_from_slice(&got.out);
            input_ended |= got.end.is_some();
        }

        if out_at.is_some_and(|at| ready.contains(&at)) {
            match read_ready(out.as_mut().expect("polled"), &mut buf) {
                Some(n) => transport.sender.current_state_mut().out.extend_from_slice(&buf[..n]),
                None => out = None,
            }
        }
        if err_at.is_some_and(|at| ready.contains(&at)) {
            match read_ready(err.as_mut().expect("polled"), &mut buf) {
                Some(n) => transport.sender.current_state_mut().err.extend_from_slice(&buf[..n]),
                None => err = None,
            }
        }

        if let (Some(input), false) = (stdin.as_mut(), to_child.is_empty()) {
            match input.write(&to_child) {
                Ok(n) => drop(to_child.drain(..n)),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => stdin = None,
            }
        }
        if input_ended && to_child.is_empty() {
            stdin = None;
        }

        if reaping {
            if let Some(status) = child.try_wait()? {
                transport.sender.current_state_mut().end = Some(exit_code(status));
            }
        }

        if transport.remote_state_num() == SHUTDOWN_NUM && !transport.sender.shutdown_in_progress() {
            transport.sender.start_shutdown(now);
        }
        transport.tick(now);
        if finished(&transport, now) {
            break;
        }

        let since_client = now.saturating_sub(transport.get_remote_state_timestamp());
        if transport.remote_state_num() == 0 && now.saturating_sub(start) >= NO_CLIENT || since_client >= CLIENT_GONE {
            break;
        }
    }

    // Closing its input first lets a command that watches it end in order.
    drop(stdin);
    let deadline = Instant::now() + GRACE;
    while child.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// How following a server ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
    /// The command exited with this status.
    Exited(i32),
    /// `stop` asked us to leave.
    Stopped,
    /// The server left before its command ended.
    ServerEnded,
    /// The server never answered, or stopped answering; the text says which.
    Lost(String),
}

/// Follows the server at `ip`:`port` until its command ends or a signal stops us, writing
/// what it prints to our own stdout and stderr and passing our stdin to it. Returns the command's exit
/// status, or 255 when the server stopped answering or never did.
pub fn connect(key: &str, ip: &str, port: u16) -> std::io::Result<i32> {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        pty::catch_termination(sig);
    }
    let output = |got: &Stream| {
        let mut stdout = std::io::stdout().lock();
        let wrote = stdout.write_all(&got.out).and_then(|()| stdout.flush());
        let _ = std::io::stderr().lock().write_all(&got.err);
        wrote.is_ok()
    };
    Ok(match follow(key, ip, port, &mut std::io::stdin(), pty::termination_requested, output)? {
        Ending::Exited(status) => status,
        Ending::Stopped => 0,
        Ending::ServerEnded => {
            eprintln!("mosh: the server ended the pipe");
            1
        }
        Ending::Lost(why) => {
            eprintln!("mosh: {why}");
            255
        }
    })
}

/// Follows the server at `ip`:`port` until its command ends or `stop` says to leave. What
/// it prints goes to `output`, which returns false once nobody reads it; `input` goes to the
/// command, and its end closes the command's input.
pub fn follow(
    key: &str,
    ip: &str,
    port: u16,
    input: &mut (impl Read + AsRawFd),
    stop: impl Fn() -> bool,
    mut output: impl FnMut(&Stream) -> bool,
) -> std::io::Result<Ending> {
    let now = pty::now_ms();
    let connection = Connection::new_client(key, ip, port, now)?;
    let mut transport = Transport::new(connection, Stream::default(), Stream::default(), now);
    let mut last_remote = 0;
    let mut heard = Instant::now();
    let mut turned = Instant::now();
    let mut ever_heard = false;
    let mut status = None;
    let mut input_open = true;
    let mut buf = [0u8; 16384];

    loop {
        let now = pty::now_ms();
        if stop() && !transport.sender.shutdown_in_progress() {
            transport.sender.start_shutdown(now);
        }
        let wait = transport.wait_time(now).min(1000) as i32;
        let mut fds: Vec<RawFd> = transport.connection.sockets().iter().map(|s| s.as_raw_fd()).collect();
        let network = fds.len();
        let reading = input_open && !transport.sender.shutdown_in_progress();
        if reading {
            fds.push(input.as_raw_fd());
        }

        let ready = pty::poll_readable(&fds, wait.max(0))?;
        let now = pty::now_ms();
        // A turn far longer than any wait means we were frozen, as an app is in the
        // background, and the server cannot be blamed for the silence.
        if turned.elapsed() > SUSPENDED {
            heard = Instant::now();
        }
        turned = Instant::now();

        if ready.iter().any(|&i| i < network) {
            while let Some(result) = transport.recv(now) {
                if result.is_ok() {
                    heard = Instant::now();
                    ever_heard = true;
                }
            }
        }
        if let Some(got) = received(&mut transport, &mut last_remote) {
            // Nobody reading what we print, as when whoever started us has died, ends the pipe.
            if !output(&got) || got.end.is_some() {
                status = got.end;
                transport.sender.start_shutdown(now);
            }
        }
        if transport.remote_state_num() == SHUTDOWN_NUM && !transport.sender.shutdown_in_progress() {
            transport.sender.start_shutdown(now);
        }

        if reading && ready.contains(&network) {
            // As over ssh, the end of our input closes the command's, and leaves it running.
            match read_ready(input, &mut buf) {
                Some(n) => transport.sender.current_state_mut().out.extend_from_slice(&buf[..n]),
                None => {
                    input_open = false;
                    transport.sender.current_state_mut().end = Some(0);
                }
            }
        }

        transport.tick(now);
        if finished(&transport, now) {
            break;
        }
        if heard.elapsed() > SERVER_LOST {
            let (low, high) = (mosh_network::connection::PORT_RANGE_LOW, mosh_network::connection::PORT_RANGE_HIGH);
            return Ok(Ending::Lost(if ever_heard {
                format!("{ip} stopped answering over UDP")
            } else {
                format!("{ip} never answered over UDP. Open ports {low}-{high}/udp in its firewall.")
            }));
        }
    }
    Ok(match status {
        Some(status) => Ending::Exited(status),
        None if stop() => Ending::Stopped,
        None => Ending::ServerEnded,
    })
}
