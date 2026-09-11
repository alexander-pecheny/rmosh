//! The mosh client: renders the server's screen and predicts what it will echo.
//!
//! Transliterated from `mosh-client.cc` and `stmclient.cc`. The session itself lives in
//! the library; this is the part that owns a real terminal.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};

use mosh_client::prediction::DisplayPreference;
use mosh_client::session::{Event, Handle, Options, Session};
use mosh_sys::pty;

const USAGE: &str = "Usage: mosh-client [-# ARGS] IP PORT";

fn main() {
    let argv: Vec<String> = std::env::args().collect();

    if argv.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if argv.iter().any(|a| a == "--version") {
        println!("mosh-client (mosh 1.4.0)");
        return;
    }

    // The launcher runs us with -c to learn how many colours the terminal has, before
    // any session exists. It must answer and exit without needing a key.
    if argv.iter().any(|a| a == "-c") {
        print_colorcount();
        return;
    }

    // The key arrives in the environment so it never appears in the process table, and
    // is removed immediately so the child of any later fork cannot see it.
    let Some(key) = mosh_sys::take_env("MOSH_KEY") else {
        eprintln!("MOSH_KEY environment variable not found.");
        std::process::exit(1);
    };

    let positional: Vec<&String> = argv[1..].iter().filter(|a| !a.starts_with('-')).collect();
    if positional.len() < 2 {
        eprintln!("{USAGE}");
        std::process::exit(1);
    }
    let (ip, port) = (positional[0].clone(), positional[1].clone());
    let Ok(port) = port.parse::<u16>() else {
        eprintln!("{USAGE}");
        std::process::exit(1);
    };

    let predict = match std::env::var("MOSH_PREDICTION_DISPLAY").as_deref() {
        Ok("always") => DisplayPreference::Always,
        Ok("never") => DisplayPreference::Never,
        Ok("experimental") => DisplayPreference::Experimental,
        _ => DisplayPreference::Adaptive,
    };
    let predict_overwrite = std::env::var("MOSH_PREDICTION_OVERWRITE").as_deref() == Ok("yes");

    mosh_sys::set_native_locale();
    if !mosh_sys::is_utf8_locale() {
        eprintln!("mosh-client requires a UTF-8 locale.");
        std::process::exit(1);
    }

    if let Err(e) = run(&key, &ip, port, predict, predict_overwrite) {
        eprintln!("mosh-client: {e}");
        std::process::exit(1);
    }
}

/// Report the terminal's colour count for the launcher, which passes it to the server.
fn print_colorcount() {
    if mosh_sys::terminfo::setup().is_err() {
        println!("-1");
        return;
    }
    let color_val = mosh_sys::terminfo::number("colors");
    if color_val == -2 {
        eprintln!("Invalid terminfo numeric capability: colors");
    }
    println!("{color_val}");
}

fn run(
    key: &str,
    ip: &str,
    port: u16,
    predict: DisplayPreference,
    predict_overwrite: bool,
) -> std::io::Result<()> {
    let stdin_fd = std::io::stdin().as_raw_fd();
    let saved = pty::set_raw(stdin_fd)?;
    let result = session(key, ip, port, predict, predict_overwrite, stdin_fd);
    // However we leave, the user's terminal must be usable again.
    pty::restore_termios(stdin_fd, &saved);
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[?1l\x1b[0m\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    result
}

fn session(
    key: &str,
    ip: &str,
    port: u16,
    predict: DisplayPreference,
    predict_overwrite: bool,
    stdin_fd: RawFd,
) -> std::io::Result<()> {
    let (rows, cols) = pty::window_size(stdin_fd).unwrap_or((24, 80));
    let session = Session::new(
        key,
        ip,
        port,
        Options {
            cols: cols.max(1) as i32,
            rows: rows.max(1) as i32,
            predict,
            predict_overwrite,
            terminfo: true,
        },
    )?;

    pty::catch_window_change();
    pty::catch_continue();
    let handle = session.handle();
    std::thread::spawn(move || keyboard(stdin_fd, handle));

    let mut out = std::io::stdout();
    session.run(|bytes| {
        out.write_all(bytes)?;
        out.flush()
    })
}

/// Turn what is typed, and what the signals say, into Events for the session.
fn keyboard(stdin_fd: RawFd, handle: Handle) {
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 16384];
    let mut quit_sequence_started = false;

    loop {
        // A signal wakes whichever thread it is delivered to, which may be the session's,
        // so this one wakes on its own account too and reads the flags every time round.
        let Ok(ready) = pty::poll_readable(&[stdin_fd], 100) else {
            return handle.send(Event::Eof);
        };

        if !ready.is_empty() {
            match stdin.read(&mut buf) {
                Ok(0) => return handle.send(Event::Eof),
                Ok(n) => {
                    let (mut typed, mut repaint) = (Vec::new(), false);
                    for &b in &buf[..n] {
                        // Ctrl-^ then '.' quits, matching the C++ default escape.
                        if quit_sequence_started {
                            quit_sequence_started = false;
                            if b == b'.' {
                                handle.send(Event::Bytes(std::mem::take(&mut typed)));
                                handle.send(Event::Quit);
                                continue;
                            }
                        } else if b == 0x1e {
                            quit_sequence_started = true;
                            continue;
                        }
                        // Ctrl-L reaches the application as usual, and also repaints,
                        // which is the user's way out of a screen that went wrong.
                        repaint |= b == 0x0c;
                        typed.push(b);
                    }
                    if !typed.is_empty() {
                        handle.send(Event::Bytes(typed));
                    }
                    if repaint {
                        handle.send(Event::Repaint);
                    }
                }
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => return handle.send(Event::Eof),
            }
        }

        if pty::window_changed() {
            if let Ok((rows, cols)) = pty::window_size(stdin_fd) {
                handle.send(Event::Resize(cols.max(1) as i32, rows.max(1) as i32));
            }
        }

        // Coming back from a stop, the terminal is as whoever held it meanwhile left it:
        // cooked, and showing their screen rather than ours.
        if pty::continued() {
            let _ = pty::set_raw(stdin_fd);
            handle.send(Event::Reopen);
        }
    }
}
