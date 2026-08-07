//! The mosh launcher: starts a server on the far end and hands off to the client.
//!
//! Transliterated from `scripts/mosh.pl`. It is not part of a session -- it exists to
//! get one started and then get out of the way.

#![forbid(unsafe_code)]

mod opts;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use opts::{shell_quote, Opts};

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let o = match opts::parse(&argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("{}", opts::USAGE);
            std::process::exit(1);
        }
    };

    if o.help {
        println!("{}", opts::USAGE);
        return;
    }
    if o.version {
        println!("mosh 1.4.0");
        println!("Copyright 2012 Keith Winstein <mosh-devel@mit.edu>");
        println!("License GPLv3+: GNU GPL version 3 or later <http://gnu.org/licenses/gpl.html>.");
        return;
    }

    if o.fake_proxy {
        if let Err(e) = fake_proxy(&o) {
            eprintln!("mosh: {e}");
            std::process::exit(1);
        }
        return;
    }

    if o.userhost.is_none() {
        eprintln!("{}", opts::USAGE);
        std::process::exit(1);
    }

    if let Err(e) = run(o) {
        eprintln!("mosh: {e}");
        std::process::exit(1);
    }
}

/// Stand in for ssh's ProxyCommand: connect the TCP socket ourselves and report the
/// address we reached.
///
/// This exists because mosh-client accepts only a numeric address, while the name the
/// user typed may be an ssh_config alias that resolves nowhere. ssh expands it to a real
/// hostname before handing it to us as `%h`, so resolving it here is the only place the
/// launcher can learn the address the session actually runs over. Once the address is
/// reported we are just a pipe between ssh and the socket.
fn fake_proxy(o: &Opts) -> std::io::Result<()> {
    let (Some(host), Some(port)) = (o.command.first(), o.command.get(1)) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--fake-proxy takes a host and a port",
        ));
    };
    let port: u16 = port
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad port"))?;

    let mut last = None;
    for addr in resolve(host, port, &o.family)? {
        match std::net::TcpStream::connect(addr) {
            Ok(sock) => {
                // The launcher reads this off the merged ssh stream.
                eprintln!("MOSH IP {}", addr.ip());
                return shuttle(sock);
            }
            Err(e) => last = Some((addr, e)),
        }
    }
    Err(match last {
        Some((addr, e)) => std::io::Error::other(format!("could not connect to {addr}: {e}")),
        None => std::io::Error::other(format!("could not resolve {host}")),
    })
}

/// Addresses to try for `host`, ordered as the requested family asks.
fn resolve(host: &str, port: u16, family: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
    use std::net::ToSocketAddrs;

    let all: Vec<std::net::SocketAddr> = (host, port).to_socket_addrs()?.collect();
    Ok(match family {
        "inet" => all.into_iter().filter(|a| a.is_ipv4()).collect(),
        "inet6" => all.into_iter().filter(|a| a.is_ipv6()).collect(),
        "prefer-inet6" => {
            let (v6, v4): (Vec<_>, Vec<_>) = all.into_iter().partition(|a| a.is_ipv6());
            v6.into_iter().chain(v4).collect()
        }
        // prefer-inet is the default, and auto/all take the resolver's own order.
        "prefer-inet" => {
            let (v4, v6): (Vec<_>, Vec<_>) = all.into_iter().partition(|a| a.is_ipv4());
            v4.into_iter().chain(v6).collect()
        }
        _ => all,
    })
}

/// Copy between our standard streams and the socket until either side is done.
fn shuttle(sock: std::net::TcpStream) -> std::io::Result<()> {
    let mut up = sock.try_clone()?;
    std::thread::spawn(move || {
        let _ = pump(&mut std::io::stdin().lock(), &mut up);
        let _ = up.shutdown(std::net::Shutdown::Write);
    });

    let mut down = sock;
    pump(&mut down, &mut std::io::stdout().lock())
    // The upward thread is still parked in read(); exiting is what ends it, which is
    // also how the Perl leaves its child.
}

/// Forward everything from `from` to `to`, flushing each chunk.
///
/// The flush is the point: Rust's stdout is line-buffered, and ssh's key exchange
/// carries no newlines, so a buffered copy leaves the far end waiting forever.
fn pump(from: &mut impl std::io::Read, to: &mut impl std::io::Write) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                to.write_all(&buf[..n])?;
                to.flush()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// The arguments the far end's mosh-server should receive.
fn server_args(o: &Opts) -> Vec<String> {
    let mut server: Vec<String> = vec!["new".into()];

    server.push("-c".into());
    server.push(colors().to_string());

    // How the server should choose the address it replies from.
    match o.bind_ip.as_deref() {
        None | Some("ssh") => {
            if o.localhost {
                server.push("-i".into());
                server.push(o.userhost.clone().unwrap_or_default());
            } else {
                server.push("-s".into());
            }
        }
        Some("any") => {}
        Some(ip) => {
            server.push("-i".into());
            server.push(ip.to_string());
        }
    }

    if let Some(p) = &o.port_request {
        server.push("-p".into());
        server.push(p.clone());
    }

    // Carry the locale across, since both ends must agree on character widths.
    for var in ["LANG", "LANGUAGE", "LC_CTYPE", "LC_ALL"] {
        if let Ok(v) = std::env::var(var) {
            server.push("-l".into());
            server.push(format!("{var}={v}"));
        }
    }

    if !o.command.is_empty() {
        server.push("--".into());
        server.extend(o.command.iter().cloned());
    }

    server
}

fn colors() -> u32 {
    // Mirrors the Perl: ask the terminal, and fall back to a safe 8.
    match std::env::var("TERM").as_deref() {
        Ok(t) if t.contains("256color") => 256,
        _ => 8,
    }
}

/// Asks the far end to announce the address ssh reached it on, under `--experimental-
/// remote-ip=remote`. The user's shell may not be Posix, so sh is named explicitly.
const ANNOUNCE_SSH_CONNECTION: &str =
    r#"[ -n "$SSH_CONNECTION" ] && printf "\nMOSH SSH_CONNECTION %s\n" "$SSH_CONNECTION""#;

fn run(o: Opts) -> std::io::Result<()> {
    let mut userhost = o.userhost.clone().unwrap_or_default();

    // Under 'local' the address is resolved here, before the fork, so that ssh and the
    // client are given the same one.
    let mut ip = None;
    if o.remote_ip == "local" {
        let host = strip_user(&userhost);
        let user = &userhost[..userhost.len() - host.len()];
        let addr = resolve(host, 22, &o.family)?
            .into_iter()
            .next()
            .ok_or_else(|| std::io::Error::other(format!("could not find address for {host}")))?;
        ip = Some(addr.ip().to_string());
        userhost = format!("{user}{}", addr.ip());
    }

    let server_command = format!(
        "{} {}",
        o.server,
        server_args(&o)
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut reader = None;
    let mut child = if o.localhost {
        // --local runs the server here instead of over ssh, which is what the test
        // suite uses so it needs no working ssh to localhost. Nothing resolves an
        // address in that case, so the host stands in for one.
        ip = Some(userhost.clone());
        Command::new("sh")
            .arg("-c")
            .arg(&server_command)
            .stdout(Stdio::piped())
            .spawn()?
    } else {
        let mut cmd = Command::new(&o.ssh[0]);
        cmd.args(&o.ssh[1..]);
        if o.ssh_pty {
            cmd.arg("-t");
        }
        match o.family.as_str() {
            "inet" => {
                cmd.arg("-4");
            }
            "inet6" => {
                cmd.arg("-6");
            }
            _ => {}
        }
        // Under 'proxy', route the connection through ourselves so we learn the address
        // ssh reached; see fake_proxy. A non-standard login shell on this side can break
        // the proxy, so it runs under /bin/sh as the Perl arranges. Under 'remote' the
        // server announces the address instead, and ssh connects for itself.
        let mut announce = String::new();
        match o.remote_ip.as_str() {
            "proxy" => {
                let exe = std::env::current_exe()?;
                let proxy = format!(
                    "{} {} --fake-proxy -- %h %p",
                    shell_quote(&exe.to_string_lossy()),
                    shell_quote(&format!("--family={}", o.family))
                );
                cmd.env("SHELL", "/bin/sh")
                    .arg("-S")
                    .arg("none")
                    .arg("-o")
                    .arg(format!("ProxyCommand={proxy}"));
            }
            "remote" => {
                announce = format!("sh -c {} ; ", shell_quote(ANNOUNCE_SSH_CONNECTION));
            }
            _ => {}
        }

        cmd.arg(&userhost)
            .arg("--")
            .arg(format!("{announce}{server_command}"));
        let (child, merged) = mosh_sys::pty::spawn_with_merged_output(&mut cmd)?;
        reader = Some(merged);
        child
    };

    // Read the handshake. Anything else on the way is the remote shell talking, and is
    // passed through so the user sees it.
    let stdout: Box<dyn std::io::Read> = match reader {
        Some(f) => Box::new(f),
        None => Box::new(child.stdout.take().expect("piped")),
    };
    let mut port = None;
    let mut key = None;
    let mut sship = None;

    // Read bytes, not text. Everything before the handshake is the remote shell talking,
    // and a login banner in any encoding but UTF-8 would otherwise end the connection
    // before the handshake was ever reached. The Perl reads bytes for the same reason.
    let mut stdout = BufReader::new(stdout);
    loop {
        let mut raw = Vec::new();
        if stdout.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("MOSH IP ") {
            if ip.is_some() {
                return Err(std::io::Error::other(
                    "detected attempt to redefine MOSH IP",
                ));
            }
            ip = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("MOSH SSH_CONNECTION ") {
            // $SSH_CONNECTION is "client_ip client_port server_ip server_port".
            let words: Vec<&str> = rest.split_whitespace().collect();
            let Some(server_ip) = words.get(2).filter(|_| words.len() == 4) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Bad MOSH SSH_CONNECTION string: {line}"),
                ));
            };
            sship = Some(server_ip.to_string());
        } else if let Some(rest) = line.strip_prefix("MOSH CONNECT ") {
            // Both halves are checked for shape before they go anywhere. They arrive
            // from the far end of an ssh session, and the port becomes an argument to
            // the client, where a value beginning with '-' would read as an option.
            let mut parts = rest.split_whitespace();
            match (parts.next(), parts.next(), parts.next()) {
                (Some(p), Some(k), None)
                    if p.parse::<u16>().is_ok()
                        && k.len() == 22
                        && k.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/') =>
                {
                    port = Some(p.to_string());
                    key = Some(k.to_string());
                    break;
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Bad MOSH CONNECT string: {line}"),
                    ))
                }
            }
        } else {
            println!("{line}");
        }
    }

    let _ = child.wait();

    let (Some(port), Some(key)) = (port, key) else {
        return Err(std::io::Error::other(
            "Did not find mosh server startup message.",
        ));
    };

    let target = match (ip, sship) {
        (Some(ip), _) => ip,
        (None, Some(sship)) => {
            eprintln!(
                "mosh: Using remote IP address {sship} from $SSH_CONNECTION for hostname {userhost}"
            );
            sship
        }
        (None, None) => {
            return Err(std::io::Error::other(
                "Did not find remote IP address (is SSH ProxyCommand disabled?).",
            ))
        }
    };

    // The key goes across in the environment rather than on the command line, so it
    // never appears in the process table.
    let mut client = Command::new(&o.client);
    client.env("MOSH_KEY", &key);
    client.env("MOSH_PREDICTION_DISPLAY", o.predict.as_str());
    if o.predict_overwrite {
        client.env("MOSH_PREDICTION_OVERWRITE", "yes");
    }
    if !o.term_init {
        client.env("MOSH_NO_TERM_INIT", "1");
    }
    client.arg(&target).arg(&port);

    let status = client.status()?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Drop any `user@` prefix; the client connects to the host itself.
fn strip_user(userhost: &str) -> &str {
    userhost
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(userhost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opts::Predict;

    fn opts_with(f: impl FnOnce(&mut Opts)) -> Opts {
        let mut o = Opts {
            userhost: Some("host".into()),
            ..Default::default()
        };
        f(&mut o);
        o
    }

    #[test]
    fn the_server_is_told_to_bind_where_ssh_arrived() {
        let a = server_args(&opts_with(|_| {}));
        assert!(a.contains(&"-s".to_string()), "{a:?}");
    }

    #[test]
    fn a_local_run_binds_the_host_it_was_given() {
        let a = server_args(&opts_with(|o| o.localhost = true));
        let i = a.iter().position(|x| x == "-i").expect("no -i");
        assert_eq!(a[i + 1], "host");
    }

    #[test]
    fn an_explicit_bind_address_wins() {
        let a = server_args(&opts_with(|o| o.bind_ip = Some("10.0.0.1".into())));
        let i = a.iter().position(|x| x == "-i").expect("no -i");
        assert_eq!(a[i + 1], "10.0.0.1");
    }

    #[test]
    fn binding_to_any_passes_no_address_at_all() {
        let a = server_args(&opts_with(|o| o.bind_ip = Some("any".into())));
        assert!(!a.contains(&"-i".to_string()), "{a:?}");
        assert!(!a.contains(&"-s".to_string()), "{a:?}");
    }

    #[test]
    fn a_command_is_passed_after_a_double_dash() {
        let a = server_args(&opts_with(|o| o.command = vec!["ls".into(), "-l".into()]));
        let i = a.iter().position(|x| x == "--").expect("no --");
        assert_eq!(&a[i + 1..], &["ls".to_string(), "-l".to_string()]);
    }

    #[test]
    fn the_port_request_is_forwarded() {
        let a = server_args(&opts_with(|o| o.port_request = Some("60000:60010".into())));
        let i = a.iter().position(|x| x == "-p").expect("no -p");
        assert_eq!(a[i + 1], "60000:60010");
    }

    #[test]
    fn a_user_prefix_is_dropped_before_connecting() {
        // ssh needs user@host; the client needs only the host.
        assert_eq!(strip_user("alice@example.com"), "example.com");
        assert_eq!(strip_user("example.com"), "example.com");
        assert_eq!(strip_user("alice@bob@example.com"), "example.com");
    }

    #[test]
    fn prediction_mode_names_survive_the_round_trip() {
        for p in [
            Predict::Always,
            Predict::Never,
            Predict::Adaptive,
            Predict::Experimental,
        ] {
            assert_eq!(Predict::parse(p.as_str()), Some(p));
        }
    }
}
