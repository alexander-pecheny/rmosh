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

    if o.userhost.is_none() {
        eprintln!("{}", opts::USAGE);
        std::process::exit(1);
    }

    if let Err(e) = run(o) {
        eprintln!("mosh: {e}");
        std::process::exit(1);
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

fn run(o: Opts) -> std::io::Result<()> {
    let userhost = o.userhost.clone().unwrap_or_default();
    let server_command = format!(
        "{} {}",
        o.server,
        server_args(&o)
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let mut child = if o.localhost {
        // --local runs the server here instead of over ssh, which is what the test
        // suite uses so it needs no working ssh to localhost.
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
        cmd.arg(&userhost)
            .arg("--")
            .arg(&server_command)
            .stdout(Stdio::piped())
            .spawn()?
    };

    // Read the handshake. Anything else on the way is the remote shell talking, and is
    // passed through so the user sees it.
    let stdout = child.stdout.take().expect("piped");
    let mut port = None;
    let mut key = None;
    let mut ip = None;

    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("MOSH IP ") {
            ip = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("MOSH CONNECT ") {
            let mut parts = rest.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(p), Some(k)) if k.len() == 22 => {
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

    let target = ip.unwrap_or_else(|| strip_user(&userhost).to_string());

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
    userhost.rsplit_once('@').map(|(_, h)| h).unwrap_or(userhost)
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
