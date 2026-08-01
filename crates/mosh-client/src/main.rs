//! The mosh client: renders the server's screen and predicts what it will echo.
//!
//! Transliterated from `mosh-client.cc` and `stmclient.cc`.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use mosh_client::prediction::{DisplayPreference, PredictionEngine};
use mosh_network::connection::Connection;
use mosh_network::transport::Transport;
use mosh_state::{Complete, UserStream};
use mosh_sys::pty;
use mosh_terminal::display::Display;
use mosh_terminal::framebuffer::Framebuffer;

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
    stdin_fd: std::os::fd::RawFd,
) -> std::io::Result<()> {
    let (rows, cols) = pty::window_size(stdin_fd).unwrap_or((24, 80));
    let (rows, cols) = (rows.max(1) as i32, cols.max(1) as i32);

    let display = Display::new(true).map_err(std::io::Error::other)?;
    let mut out = std::io::stdout();

    let mut local_framebuffer = Framebuffer::new(cols, rows);
    // Paint the initial screen, so what we diff against is what the terminal shows.
    out.write_all(display.open().as_bytes())?;
    let init = display.new_frame(false, &local_framebuffer, &local_framebuffer);
    out.write_all(init.as_bytes())?;
    out.flush()?;

    let now = pty::now_ms();
    let connection = Connection::new_client(key, ip, port, now)?;
    let mut transport = Transport::new(
        connection,
        UserStream::new(),
        Complete::new(cols, rows),
        now,
    );
    // Keystrokes should leave as soon as possible; batching them adds visible latency.
    transport.sender.set_send_delay(1);

    // Tell the server how big our terminal is before anything else.
    transport
        .sender
        .current_state_mut()
        .push_resize(cols, rows);

    let mut predictions = PredictionEngine::new();
    predictions.set_display_preference(predict);
    predictions.set_predict_overwrite(predict_overwrite);

    let mut buf = [0u8; 16384];
    let mut stdin = std::io::stdin();
    let mut quit_sequence_started = false;

    loop {
        let now = pty::now_ms();
        let wait = transport
            .wait_time(now)
            .min(predictions.wait_time())
            .min(100) as i32;

        let mut fds: Vec<std::os::fd::RawFd> = transport
            .connection
            .sockets()
            .iter()
            .map(|s| s.as_raw_fd())
            .collect();
        let network_count = fds.len();
        fds.push(stdin_fd);

        let ready = pty::poll_readable(&fds, wait.max(0))?;
        let now = pty::now_ms();

        if ready.iter().any(|&i| i < network_count) {
            while let Some(result) = transport.recv(now) {
                let _ = result;
            }
            // Tell the prediction engine what the server has confirmed, which is what
            // lets it retire guesses and judge whether it is guessing well.
            predictions.set_local_frame_acked(transport.sender.sent_state_acked());
            predictions
                .set_local_frame_late_acked(transport.get_latest_remote_state().echo_ack());
            predictions.set_send_interval(
                transport.sender.send_interval(transport.connection.srtt()),
            );
        }

        if ready.iter().any(|&i| i >= network_count) {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for &b in &buf[..n] {
                        // Ctrl-^ then '.' quits, matching the C++ default escape.
                        if quit_sequence_started {
                            quit_sequence_started = false;
                            if b == b'.' {
                                transport.sender.start_shutdown(now);
                                continue;
                            }
                        } else if b == 0x1e {
                            quit_sequence_started = true;
                            continue;
                        }

                        if !transport.sender.shutdown_in_progress() {
                            predictions.new_user_byte(b, &local_framebuffer, now);
                            transport.sender.current_state_mut().push_byte(b);
                        }
                    }
                    predictions
                        .set_local_frame_sent(transport.sender.sent_state_last());
                }
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => break,
            }
        }

        // Draw: the server's screen, with our guesses laid over it.
        let mut new_state = transport.get_latest_remote_state().fb().clone();
        predictions.cull(&new_state, now);
        predictions.apply(&mut new_state);

        let diff = display.new_frame(true, &local_framebuffer, &new_state);
        if !diff.is_empty() {
            out.write_all(diff.as_bytes())?;
            out.flush()?;
        }
        local_framebuffer = new_state;

        // The server signals it is leaving by moving to the shutdown state number. The
        // client has to notice and shut down too, or it waits for a peer that is gone.
        if transport.remote_state_num() == u64::MAX && !transport.sender.shutdown_in_progress() {
            transport.sender.start_shutdown(now);
        }

        transport.tick(now);

        if transport.sender.shutdown_in_progress()
            && (transport.sender.shutdown_acknowledged()
                || transport.sender.counterparty_shutdown_acknowledged()
                || transport.sender.shutdown_ack_timed_out(now))
        {
            break;
        }
    }

    out.write_all(display.close().as_bytes())?;
    out.flush()?;
    Ok(())
}
