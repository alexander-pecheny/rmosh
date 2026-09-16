//! What a person sees on screen while typing, against a real server over a slow link.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mosh_client::prediction::DisplayPreference;
use mosh_client::{Event, Options, Session};
use mosh_terminal::emulator::Emulator;

const TYPED: &str = "the quick brown fox jumps over the lazy dog";
/// Gap between keystrokes, about the speed of a fast touch typist.
const KEYSTROKE_GAP: Duration = Duration::from_millis(60);
/// One-way delay the relay imposes, so the round trip is 200ms.
const LINK_DELAY: Duration = Duration::from_millis(100);
/// Keystrokes the screen may trail the keyboard by. Prediction exists to keep this at
/// nothing; one covers the keystroke that has not reached the session loop yet.
const ALLOWED_LAG: usize = 1;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The build directory holding this test, `mosh-server` and the examples.
fn build_dir() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().expect("test binary path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir
}

/// An example server program, built here because `cargo test --test` skips examples.
fn example(name: &str) -> String {
    let path = build_dir().join("examples").join(name);
    if !path.exists() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "mosh-client", "--example", name])
            .status()
            .expect("cargo");
        assert!(status.success(), "building the {name} example");
    }
    path.to_str().expect("path").to_string()
}

/// A server running `command`.
fn start_server(command: &str) -> (Server, u16, String) {
    let mut child = Command::new(build_dir().join("mosh-server"))
        .args(["new", "-i", "127.0.0.1", "--", command])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mosh-server");
    let stdout = child.stdout.take().expect("server stdout");
    let server = Server(child);
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("server output");
        if let Some(rest) = line.strip_prefix("MOSH CONNECT ") {
            let (port, key) = rest.split_once(' ').expect("port and key");
            return (server, port.parse().expect("port"), key.to_string());
        }
    }
    panic!("server never announced a port");
}

/// Relay datagrams to `server_port` and back, holding each for [`LINK_DELAY`].
///
/// A loopback link confirms a keystroke before the next one is typed, which is the one
/// case where prediction has nothing to do. It is only observable over a round trip long
/// enough that guesses are still outstanding while typing continues.
fn start_relay(server_port: u16) -> u16 {
    let client_side = UdpSocket::bind("127.0.0.1:0").expect("relay socket");
    let port = client_side.local_addr().expect("relay address").port();
    let server_side = UdpSocket::bind("127.0.0.1:0").expect("relay socket");
    let server: SocketAddr = format!("127.0.0.1:{server_port}").parse().expect("server");
    // The client roams between source ports, so the way back is wherever it last wrote
    // from rather than a fixed address.
    let client = Arc::new(Mutex::new(None::<SocketAddr>));

    for towards_server in [true, false] {
        let (from, to) = if towards_server {
            (client_side.try_clone(), server_side.try_clone())
        } else {
            (server_side.try_clone(), client_side.try_clone())
        };
        let (from, to) = (from.expect("clone"), to.expect("clone"));
        let client = Arc::clone(&client);
        thread::spawn(move || {
            from.set_read_timeout(Some(Duration::from_millis(2)))
                .expect("read timeout");
            let mut queue: VecDeque<(Instant, Vec<u8>, SocketAddr)> = VecDeque::new();
            let mut buf = [0u8; 2048];
            loop {
                if let Ok((n, src)) = from.recv_from(&mut buf) {
                    let target = if towards_server {
                        *client.lock().expect("client address") = Some(src);
                        Some(server)
                    } else {
                        *client.lock().expect("client address")
                    };
                    if let Some(target) = target {
                        queue.push_back((Instant::now() + LINK_DELAY, buf[..n].to_vec(), target));
                    }
                }
                while queue
                    .front()
                    .is_some_and(|(at, _, _)| *at <= Instant::now())
                {
                    let (_, payload, target) = queue.pop_front().expect("head");
                    let _ = to.send_to(&payload, target);
                }
            }
        });
    }
    port
}

/// A session against a server running `command`, one round trip away.
fn connect(command: &str) -> (Server, Session) {
    mosh_sys::set_native_locale();
    let (server, server_port, key) = start_server(command);
    let port = start_relay(server_port);
    let session = Session::new(
        &key,
        "127.0.0.1",
        port,
        Options {
            cols: 80,
            rows: 24,
            predict: DisplayPreference::Always,
            predict_overwrite: false,
            terminfo: false,
        },
    )
    .expect("connect");
    (server, session)
}

/// The text on the first `rows` rows of the screen, with runs of blanks collapsed.
fn screen_text(emulator: &Emulator, rows: i32) -> String {
    let mut text = String::new();
    for y in 0..rows {
        for x in 0..80 {
            emulator
                .fb()
                .cell(y, x)
                .expect("cell")
                .print_grapheme(&mut text);
        }
        text.push(' ');
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Type [`TYPED`] one keystroke at a time, then quit.
fn typist(handle: mosh_client::Handle, typed_so_far: Arc<AtomicUsize>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut bytes = TYPED.bytes();
        // Wait for the first echo: predictions start tentative and are withheld until
        // one has been confirmed, so the interesting frames come after that.
        handle.send(Event::Bytes(vec![bytes.next().expect("first byte")]));
        typed_so_far.store(1, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(800));
        for b in bytes {
            handle.send(Event::Bytes(vec![b]));
            typed_so_far.fetch_add(1, Ordering::SeqCst);
            thread::sleep(KEYSTROKE_GAP);
        }
        thread::sleep(Duration::from_millis(800));
        handle.send(Event::Quit);
    })
}

/// Type against `command` and fail if the screen ever trails the keyboard by more
/// than [`ALLOWED_LAG`].
fn assert_keeps_up(command: &str, rows: i32) {
    let (_server, session) = connect(command);
    let typed_so_far = Arc::new(AtomicUsize::new(0));
    let typist = typist(session.handle(), Arc::clone(&typed_so_far));

    let mut emulator = Emulator::new(80, 24);
    let mut worst: (usize, String) = (0, String::new());
    session
        .run(|bytes| {
            emulator.input(bytes);
            let text = screen_text(&emulator, rows);
            assert!(
                TYPED.starts_with(&text),
                "the screen showed text that was never typed: {text:?}"
            );
            // Blanks are left out of the count: a wrap may add some, and a space just
            // typed shows as nothing.
            let letters = |s: &str| s.chars().filter(|c| *c != ' ').count();
            let typed = &TYPED[..typed_so_far.load(Ordering::SeqCst)];
            let lag = letters(typed).saturating_sub(letters(&text));
            if lag > worst.0 {
                worst = (lag, text);
            }
            Ok(())
        })
        .expect("session");
    typist.join().expect("typist");
    assert!(
        worst.0 <= ALLOWED_LAG,
        "the screen fell {} keystrokes behind the keyboard, showing {:?}",
        worst.0,
        worst.1
    );
}

#[test]
fn the_screen_keeps_up_with_the_keyboard() {
    // `cat`, whose tty echoes each byte back exactly as typed.
    assert_keeps_up("cat", 1);
}

#[test]
fn the_screen_keeps_up_across_a_word_wrap() {
    // An input field that moves a half-typed word to the next line takes every guess
    // made about that word with it. The guesses for the keystrokes still in flight
    // have to be remade from where the word landed, or typing stalls until a pause.
    assert_keeps_up(&example("wrapping_editor"), 3);
}

/// Type [`TYPED`], then erase it with `key`, failing if the screen ever trails the
/// keyboard by more than [`ALLOWED_LAG`].
///
/// `erase` says what one press of `key` leaves behind.
fn assert_erasing_keeps_up(key: &[u8], erase: fn(&str) -> &str) {
    let (_server, session) = connect(&example("line_editor"));

    let mut steps = vec![TYPED.to_string()];
    while !steps.last().expect("never empty").is_empty() {
        let next = erase(steps.last().expect("never empty")).to_string();
        steps.push(next);
    }

    let erased_so_far = Arc::new(AtomicUsize::new(0));
    let handle = session.handle();
    let counter = Arc::clone(&erased_so_far);
    let presses = steps.len() - 1;
    let key = key.to_vec();
    let typist = thread::spawn(move || {
        for b in TYPED.bytes() {
            handle.send(Event::Bytes(vec![b]));
            thread::sleep(KEYSTROKE_GAP);
        }
        // Let the server catch up, so erasing starts from a screen we know is right.
        thread::sleep(Duration::from_millis(800));
        // A key the client cannot model, as an arrow key or a control key is. It
        // starts a fresh epoch, whose guesses are withheld until one of them is
        // confirmed -- and a run of deletions has to be able to confirm one.
        handle.send(Event::Bytes(vec![0x01]));
        thread::sleep(KEYSTROKE_GAP);
        for _ in 0..presses {
            handle.send(Event::Bytes(key.clone()));
            counter.fetch_add(1, Ordering::SeqCst);
            thread::sleep(KEYSTROKE_GAP);
        }
        thread::sleep(Duration::from_millis(800));
        handle.send(Event::Quit);
    });

    let letters = |s: &str| s.chars().filter(|c| *c != ' ').count();
    let mut emulator = Emulator::new(80, 24);
    let mut worst: (usize, String) = (0, String::new());
    session
        .run(|bytes| {
            // Read the count before the screen, so a keystroke in flight counts as not
            // yet pressed rather than as lag.
            let erased = erased_so_far.load(Ordering::SeqCst);
            emulator.input(bytes);
            let text = screen_text(&emulator, 1);
            let lag = letters(&text).saturating_sub(letters(&steps[erased]));
            if lag > worst.0 {
                worst = (lag, text);
            }
            Ok(())
        })
        .expect("session");
    typist.join().expect("typist");
    assert!(
        worst.0 <= ALLOWED_LAG,
        "the screen kept {} erased characters on screen, showing {:?}",
        worst.0,
        worst.1
    );
}

#[test]
fn the_screen_keeps_up_with_backspace() {
    assert_erasing_keeps_up(&[0x7f], |s| &s[..s.len() - 1]);
}

#[test]
fn the_screen_keeps_up_with_erasing_a_word() {
    // Option-Backspace: the shell kills the word to the left of the cursor.
    assert_erasing_keeps_up(&[0x1b, 0x7f], |s| {
        let s = s.trim_end_matches(' ');
        &s[..s.rfind(' ').map_or(0, |i| i + 1)]
    });
}

#[test]
fn a_paste_is_not_predicted() {
    let (_server, session) = connect("cat");
    let handle = session.handle();

    let pasted = "z".repeat(150);
    let sent = Arc::new(Mutex::new(None::<Instant>));
    let stamp = Arc::clone(&sent);
    let typist = thread::spawn(move || {
        // Get the first epoch confirmed, or nothing would be shown whatever the rule.
        handle.send(Event::Bytes(b"a".to_vec()));
        thread::sleep(Duration::from_millis(800));
        *stamp.lock().expect("stamp") = Some(Instant::now());
        handle.send(Event::Bytes(pasted.into_bytes()));
        thread::sleep(Duration::from_millis(1200));
        handle.send(Event::Quit);
    });

    let mut emulator = Emulator::new(80, 24);
    let mut first_seen: Option<Duration> = None;
    session
        .run(|bytes| {
            emulator.input(bytes);
            let mut screen = String::new();
            for y in 0..2 {
                for x in 0..80 {
                    emulator
                        .fb()
                        .cell(y, x)
                        .expect("cell")
                        .print_grapheme(&mut screen);
                }
            }
            if first_seen.is_none() && screen.contains('z') {
                if let Some(at) = *sent.lock().expect("stamp") {
                    first_seen = Some(at.elapsed());
                }
            }
            Ok(())
        })
        .expect("session");
    typist.join().expect("typist");

    // The server cannot have echoed sooner than a round trip, so anything on screen
    // before then was guessed.
    let elapsed = first_seen.expect("the paste never reached the screen");
    assert!(
        elapsed >= LINK_DELAY,
        "the paste was on screen after {elapsed:?}, before the server could have echoed it"
    );
}
