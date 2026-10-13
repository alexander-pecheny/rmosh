//! Building the command line the far end's `mosh-server` is started with.
//!
//! Transliterated from `scripts/mosh.pl`.

use crate::opts::{shell_quote, Opts};

/// Asks the far end to announce the address ssh reached it on, under `--experimental-
/// remote-ip=remote`. The user's shell may not be Posix, so sh is named explicitly.
pub const ANNOUNCE_SSH_CONNECTION: &str =
    r#"[ -n "$SSH_CONNECTION" ] && printf "\nMOSH SSH_CONNECTION %s\n" "$SSH_CONNECTION""#;

/// What to put in front of the server command so the far end announces its address.
pub fn announce_prefix() -> String {
    format!("sh -c {} ; ", shell_quote(ANNOUNCE_SSH_CONNECTION))
}

/// The locale variables to carry across, from this process's environment.
pub fn locale_from_env() -> Vec<(String, String)> {
    ["LANG", "LANGUAGE", "LC_CTYPE", "LC_ALL"]
        .iter()
        .filter_map(|var| std::env::var(var).ok().map(|v| (var.to_string(), v)))
        .collect()
}

/// The arguments the far end's mosh-server should receive. Both ends must agree on
/// character widths, so `locale` is carried across rather than left to the remote login.
pub fn server_args(o: &Opts, locale: &[(String, String)]) -> Vec<String> {
    let mut server: Vec<String> = vec![if o.pipe { "pipe" } else { "new" }.into()];

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

    for (var, value) in locale {
        server.push("-l".into());
        server.push(format!("{var}={value}"));
    }

    if !o.command.is_empty() {
        server.push("--".into());
        server.extend(o.command.iter().cloned());
    }

    server
}

/// The whole server command, ready to hand to a remote shell.
pub fn server_command(o: &Opts, locale: &[(String, String)]) -> String {
    format!(
        "{} {}",
        o.server,
        server_args(o, locale)
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn colors() -> u32 {
    // Mirrors the Perl: ask the terminal, and fall back to a safe 8.
    match std::env::var("TERM").as_deref() {
        Ok(t) if t.contains("256color") => 256,
        _ => 8,
    }
}

/// Drop any `user@` prefix; the client connects to the host itself.
pub fn strip_user(userhost: &str) -> &str {
    userhost
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(userhost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_with(f: impl FnOnce(&mut Opts)) -> Opts {
        let mut o = Opts {
            userhost: Some("host".into()),
            ..Default::default()
        };
        f(&mut o);
        o
    }

    fn args(o: &Opts) -> Vec<String> {
        server_args(o, &[])
    }

    #[test]
    fn the_server_is_told_to_bind_where_ssh_arrived() {
        let a = args(&opts_with(|_| {}));
        assert!(a.contains(&"-s".to_string()), "{a:?}");
    }

    #[test]
    fn a_local_run_binds_the_host_it_was_given() {
        let a = args(&opts_with(|o| o.localhost = true));
        let i = a.iter().position(|x| x == "-i").expect("no -i");
        assert_eq!(a[i + 1], "host");
    }

    #[test]
    fn an_explicit_bind_address_wins() {
        let a = args(&opts_with(|o| o.bind_ip = Some("10.0.0.1".into())));
        let i = a.iter().position(|x| x == "-i").expect("no -i");
        assert_eq!(a[i + 1], "10.0.0.1");
    }

    #[test]
    fn binding_to_any_passes_no_address_at_all() {
        let a = args(&opts_with(|o| o.bind_ip = Some("any".into())));
        assert!(!a.contains(&"-i".to_string()), "{a:?}");
        assert!(!a.contains(&"-s".to_string()), "{a:?}");
    }

    #[test]
    fn a_command_is_passed_after_a_double_dash() {
        let a = args(&opts_with(|o| o.command = vec!["ls".into(), "-l".into()]));
        let i = a.iter().position(|x| x == "--").expect("no --");
        assert_eq!(&a[i + 1..], &["ls".to_string(), "-l".to_string()]);
    }

    #[test]
    fn the_port_request_is_forwarded() {
        let a = args(&opts_with(|o| o.port_request = Some("60000:60010".into())));
        let i = a.iter().position(|x| x == "-p").expect("no -p");
        assert_eq!(a[i + 1], "60000:60010");
    }

    #[test]
    fn the_locale_is_carried_across() {
        let locale = [("LANG".to_string(), "en_US.UTF-8".to_string())];
        let a = server_args(&opts_with(|_| {}), &locale);
        let i = a.iter().position(|x| x == "-l").expect("no -l");
        assert_eq!(a[i + 1], "LANG=en_US.UTF-8");
    }

    #[test]
    fn the_command_quotes_what_a_shell_would_otherwise_read() {
        let o = opts_with(|o| {
            o.server = "mosh-server".into();
            o.command = vec!["tmux attach".into()];
        });
        let c = server_command(&o, &[]);
        assert!(c.starts_with("mosh-server new -c "), "{c}");
        assert!(c.ends_with("-- 'tmux attach'"), "{c}");
    }

    #[test]
    fn a_user_prefix_is_dropped_before_connecting() {
        // ssh needs user@host; the client needs only the host.
        assert_eq!(strip_user("alice@example.com"), "example.com");
        assert_eq!(strip_user("example.com"), "example.com");
        assert_eq!(strip_user("alice@bob@example.com"), "example.com");
    }
}
