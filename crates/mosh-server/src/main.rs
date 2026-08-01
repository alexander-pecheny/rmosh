//! The mosh server: owns the pty and the authoritative screen.
//!
//! Transliterated from `src/frontend/mosh-server.cc`.

#![forbid(unsafe_code)]

mod args;

use std::io::{Read, Write};
use std::os::fd::AsRawFd;

use mosh_network::connection::Connection;
use mosh_network::transport::Transport;
use mosh_state::{Complete, UserEvent, UserStream};
use mosh_sys::pty;

/// An idle-network shutdown, in milliseconds, from MOSH_SERVER_NETWORK_TMOUT.
///
/// Seconds in the environment, since that is what the variable has always meant.
fn network_timeout_from_env() -> Option<u64> {
    let raw = std::env::var("MOSH_SERVER_NETWORK_TMOUT").ok()?;
    match raw.trim().parse::<i64>() {
        Ok(v) if v >= 0 => Some(v as u64 * 1000),
        Ok(_) => {
            eprintln!("MOSH_SERVER_NETWORK_TMOUT is negative, ignoring");
            None
        }
        Err(_) => {
            eprintln!("MOSH_SERVER_NETWORK_TMOUT not a valid integer, ignoring");
            None
        }
    }
}

/// Give up if no client has ever connected within this long.
const TIMEOUT_IF_NO_CLIENT: u64 = 60000;
/// Give up if a client connected and then vanished for this long.
const TIMEOUT_IF_NO_CONTACT: u64 = 7 * 24 * 3600 * 1000;

fn main() {
    // Keep session keys out of any core file.
    let _ = mosh_sys::disable_dumping_core();

    let argv: Vec<String> = std::env::args().collect();
    let parsed = match args::parse(&argv) {
        Ok(a) => a,
        Err(args::ArgError::BadColors(v)) => {
            eprintln!("mosh-server: Bad number of colors ({v})");
            eprintln!("{}", args::USAGE);
            std::process::exit(1);
        }
        Err(args::ArgError::BadUsage) => {
            eprintln!("{}", args::USAGE);
            std::process::exit(1);
        }
    };

    if parsed.help {
        println!("{}", args::USAGE);
        return;
    }
    if parsed.version {
        println!("mosh-server (mosh 1.4.0)");
        return;
    }

    if let Some(p) = &parsed.desired_port {
        if args::parse_portrange(p).is_none() {
            eprintln!("mosh-server: Bad UDP port range ({p})");
            eprintln!("{}", args::USAGE);
            std::process::exit(1);
        }
    }

    if let Err(e) = run(parsed) {
        eprintln!("mosh-server: {e}");
        std::process::exit(1);
    }
}

fn run(args: args::Args) -> std::io::Result<()> {
    // The child inherits the locale, and our own width calculations depend on it.
    for (k, v) in &args.locale_vars {
        std::env::set_var(k, v);
    }
    mosh_sys::set_native_locale();

    let (cols, rows) = (80u16, 24u16);
    let now = pty::now_ms();

    let (port_low, port_high) = args
        .desired_port
        .as_deref()
        .and_then(args::parse_portrange)
        .unwrap_or((
            mosh_network::connection::PORT_RANGE_LOW,
            mosh_network::connection::PORT_RANGE_HIGH,
        ));

    let connection = Connection::new_server(args.desired_ip.as_deref(), port_low, port_high, now)?;
    let port = connection.port()?;
    let key = connection.key();

    // If we are on a pty, typeahead can echo and break the wrapper's detection of this
    // line, so put it on a fresh line first.
    if pty::isatty(std::io::stdin().as_raw_fd()) {
        println!("\r\n");
    }
    println!("MOSH CONNECT {port} {key}");
    let _ = std::io::stdout().flush();

    // Signals must not kill a detached server.
    pty::ignore_signal(libc_sighup());
    pty::ignore_signal(libc_sigpipe());

    // Detach so the ssh session that started us can end.
    if !pty::detach()? {
        return Ok(()); // parent
    }

    // Let go of the pipes we inherited, or whoever started us waits forever for
    // end-of-file. Kept open under -v so diagnostics stay visible, matching the C++.
    if args.verbose == 0 {
        let _ = pty::close_standard_streams();
    }

    let terminal = Complete::new(cols as i32, rows as i32);
    let mut transport = Transport::new(connection, terminal, UserStream::new(), now);
    transport.verbose = args.verbose;
    transport.sender.verbose = args.verbose;

    // Allocate the pty and start the child.
    let (master, child) = match pty::forkpty(rows, cols)? {
        pty::ForkPty::Child => {
            let (program, argv) = child_command(&args);
            let env = child_env(&args);
            let e = pty::exec(&program, &argv, &env);
            // Only reachable if exec failed; the parent will see the pty close.
            eprintln!("mosh-server: exec: {e}");
            std::process::exit(1);
        }
        pty::ForkPty::Parent { master, child } => (master, child),
    };

    serve(&mut transport, master, child, args.verbose)
}

fn serve(
    transport: &mut Transport<Complete, UserStream>,
    master_fd: std::os::fd::OwnedFd,
    child: i32,
    verbose: u32,
) -> std::io::Result<()> {
    let master = master_fd.as_raw_fd();
    // A ^S arriving between the poll and the read can otherwise leave read() blocking
    // even though poll reported data, wedging everything attached to the pty.
    let _ = pty::set_nonblocking(master);
    let mut host = std::fs::File::from(master_fd);
    // Output the pty could not accept yet, retried on later passes rather than blocking.
    let mut pending_to_host: Vec<u8> = Vec::new();
    let mut last_remote_num = transport.remote_state_num();
    let mut buf = [0u8; 16384];
    let start = pty::now_ms();
    let mut child_exit: Option<i32> = None;
    // Set once the child is reaped. The pty can stay open past that, because a
    // background process the child started still holds the slave, so this alone must
    // not end the session -- we drain first.
    let mut child_reaped = false;
    let network_timeout = network_timeout_from_env();

    loop {
        let now = pty::now_ms();

        // Wake for whichever comes first: the transport's own schedule or new input.
        let wait = transport.wait_time(now).min(1000) as i32;

        let mut fds: Vec<std::os::fd::RawFd> = transport
            .connection
            .sockets()
            .iter()
            .map(|s| s.as_raw_fd())
            .collect();
        let network_count = fds.len();
        if child_exit.is_none() {
            fds.push(master);
        }

        let ready = pty::poll_readable(&fds, wait.max(0))?;
        let now = pty::now_ms();

        let mut terminal_to_host: Vec<u8> = Vec::new();

        if ready.iter().any(|&i| i < network_count) {
            while let Some(result) = transport.recv(now) {
                if let Err(e) = result {
                    if verbose > 0 {
                        eprintln!("mosh-server: {e}");
                    }
                }
            }

            if transport.remote_state_num() != last_remote_num
                && !transport.sender.shutdown_in_progress()
            {
                last_remote_num = transport.remote_state_num();

                let mut us = UserStream::new();
                if us.apply_string(&transport.get_remote_diff()).is_ok() {
                    for i in 0..us.len() {
                        match us.get(i) {
                            Some(UserEvent::Byte(b)) => {
                                // Keystrokes go to the child, and to our own terminal
                                // only through the echo the child produces.
                                terminal_to_host.push(b);
                            }
                            Some(UserEvent::Resize { width, height }) => {
                                // Only the last of a run of resizes is worth acting on.
                                if matches!(us.get(i + 1), Some(UserEvent::Resize { .. })) {
                                    continue;
                                }
                                let _ = pty::set_window_size(master, height as u16, width as u16);
                                let reply = transport
                                    .sender
                                    .current_state_mut()
                                    .act_resize(width, height);
                                terminal_to_host.extend_from_slice(reply.as_bytes());
                            }
                            None => {}
                        }
                    }
                    if !us.is_empty() {
                        transport
                            .sender
                            .current_state_mut()
                            .register_input_frame(last_remote_num, now);
                    }
                }
            }
        }

        // Whether the pty gave us anything this time round.
        let mut drained_this_pass = false;
        if child_exit.is_none()
            && !transport.sender.shutdown_in_progress()
            && ready.iter().any(|&i| i >= network_count)
        {
            match host.read(&mut buf) {
                Ok(0) => child_exit = Some(0),
                Ok(n) => {
                    drained_this_pass = true;
                    let reply = transport.sender.current_state_mut().act(&buf[..n]);
                    terminal_to_host.extend_from_slice(reply.as_bytes());
                }
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                // The pty slave closing surfaces as EIO, which means the same as EOF.
                Err(_) => child_exit = Some(0),
            }
        }

        // Write what the pty will take. Blocking here would deadlock: the child can be
        // stopped by a ^S while still owing us output, so we must stay able to read.
        pending_to_host.extend_from_slice(&terminal_to_host);
        if !pending_to_host.is_empty() && child_exit.is_none() {
            match host.write(&pending_to_host) {
                Ok(n) => {
                    pending_to_host.drain(..n);
                }
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => child_exit = Some(0),
            }
        }

        // Late echo acknowledgement, so the client can retire its predictions. Frozen
        // once shutdown starts, like every other change to the current state.
        let now = pty::now_ms();
        if !transport.sender.shutdown_in_progress() {
            transport.sender.current_state_mut().set_echo_ack(now);
        }

        transport.tick(now);

        // Reap the child so it does not linger as a zombie. Its exit alone does not end
        // the session: output it wrote just before exiting may still be in the pty.
        if pty::try_wait(child).is_some() {
            child_reaped = true;
        }

        // Once the child is gone and the pty has nothing left for us, the session is
        // over. Waiting for EOF alone is not enough -- a background process the child
        // started can hold the slave open indefinitely -- and acting on the exit alone
        // is not enough either, because it would discard the child's last output.
        if child_reaped && child_exit.is_none() && !drained_this_pass {
            child_exit = Some(0);
        }

        if child_exit.is_some() {
            if !transport.sender.shutdown_in_progress() {
                transport.sender.start_shutdown(now);
            }
            if transport.sender.shutdown_acknowledged()
                || transport.sender.shutdown_ack_timed_out(now)
            {
                break;
            }
        }

        // Give up if nobody ever arrives, or if a client vanishes for a very long time.
        let since_remote = now.saturating_sub(transport.get_remote_state_timestamp());

        // An operator-set idle timeout, which exists so an abandoned session does not
        // hold a pty open indefinitely.
        if let Some(limit) = network_timeout {
            if since_remote >= limit {
                eprintln!("Network idle for {} seconds.", since_remote / 1000);
                break;
            }
        }
        if transport.remote_state_num() == 0 && now.saturating_sub(start) >= TIMEOUT_IF_NO_CLIENT {
            eprintln!("mosh-server: No client arrived; exiting.");
            break;
        }
        if since_remote >= TIMEOUT_IF_NO_CONTACT {
            eprintln!("mosh-server: Client vanished; exiting.");
            break;
        }
    }

    eprintln!("\r\n[mosh-server is exiting.]\r\n");
    Ok(())
}

/// What to run in the pty: the requested command, or the user's shell.
fn child_command(args: &args::Args) -> (String, Vec<String>) {
    if !args.command.is_empty() {
        return (args.command[0].clone(), args.command.clone());
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    // A leading '-' asks the shell to behave as a login shell.
    let base = shell.rsplit('/').next().unwrap_or("sh").to_string();
    (shell, vec![format!("-{base}")])
}

fn child_env(args: &args::Args) -> Vec<(String, String)> {
    let mut env = vec![
        // mosh draws the screen itself, so the child should assume a capable terminal.
        ("TERM".to_string(), pick_term(args)),
        // Tell applications they are under mosh, as the C++ does.
        ("NCURSES_NO_UTF8_ACS".to_string(), "1".to_string()),
    ];
    for (k, v) in &args.locale_vars {
        env.push((k.clone(), v.clone()));
    }
    env
}

fn pick_term(args: &args::Args) -> String {
    if args.colors >= 256 {
        "xterm-256color".to_string()
    } else {
        std::env::var("TERM").unwrap_or_else(|_| "xterm".to_string())
    }
}

fn libc_sighup() -> i32 {
    1
}

fn libc_sigpipe() -> i32 {
    13
}
