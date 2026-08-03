//! Command-line handling, matching `mosh-server`'s documented syntax.
//!
//! The wrapper script and the test harness both depend on this exactly, so the accepted
//! flags and the treatment of `--` follow the C++ rather than being tidied up.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub desired_ip: Option<String>,
    pub desired_port: Option<String>,
    /// Everything after `--`, run instead of the user's shell.
    pub command: Vec<String>,
    pub colors: i32,
    pub verbose: u32,
    /// `-l NAME=VALUE`, passed through to the child.
    pub locale_vars: Vec<(String, String)>,
    pub help: bool,
    pub version: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    BadColors(String),
    BadUsage,
}

/// Parse a port or port range, as `-p` accepts.
pub fn parse_portrange(s: &str) -> Option<(u16, u16)> {
    match s.split_once(':') {
        None => {
            let p: u16 = s.parse().ok()?;
            (p != 0).then_some((p, p))
        }
        Some((lo, hi)) => {
            let lo: u16 = lo.parse().ok()?;
            let hi: u16 = hi.parse().ok()?;
            (lo != 0 && hi >= lo).then_some((lo, hi))
        }
    }
}

pub fn parse(argv: &[String]) -> Result<Args, ArgError> {
    let mut args = Args::default();

    // `--` is mandatory before a command, and everything before it is options.
    let mut opts_end = argv.len();
    for (i, a) in argv.iter().enumerate().skip(1) {
        if a == "--help" || a == "-h" {
            args.help = true;
            return Ok(args);
        }
        if a == "--version" {
            args.version = true;
            return Ok(args);
        }
        if a == "--" {
            if i != argv.len() - 1 {
                args.command = argv[i + 1..].to_vec();
            }
            opts_end = i;
            break;
        }
    }
    let opts = &argv[..opts_end];

    if opts.len() >= 2 && opts[1] == "new" {
        // A hand-rolled getopt over "@:i:p:c:svl:". Bundling matters: the test harness
        // passes -vv, which getopt reads as two v flags, and a value may be attached
        // to its letter (-c256) or separate (-c 256).
        const TAKES_VALUE: &str = "@ipcl";
        let mut i = 2;
        while i < opts.len() {
            let arg = opts[i].clone();
            if !arg.starts_with('-') || arg == "-" {
                i += 1;
                continue;
            }
            let mut chars = arg[1..].chars();
            while let Some(c) = chars.next() {
                if TAKES_VALUE.contains(c) {
                    // The rest of this argument is the value, or the next argument is.
                    let rest: String = chars.by_ref().collect();
                    let value = if rest.is_empty() {
                        i += 1;
                        opts.get(i).cloned()
                    } else {
                        Some(rest)
                    };
                    match c {
                        // Undocumented, and deliberately kept: it eats its argument so
                        // a script can prepend to an argv without the result being
                        // misparsed.
                        '@' => {}
                        'i' => args.desired_ip = value,
                        'p' => args.desired_port = value,
                        'c' => {
                            let v = value.unwrap_or_default();
                            args.colors = v.parse().map_err(|_| ArgError::BadColors(v))?;
                        }
                        'l' => {
                            if let Some((k, val)) =
                                value.as_deref().and_then(|v| v.split_once('='))
                            {
                                args.locale_vars.push((k.to_string(), val.to_string()));
                            }
                        }
                        _ => unreachable!("TAKES_VALUE and this match must agree"),
                    }
                    break; // the value consumed the rest of this argument
                }
                match c {
                    'v' => args.verbose += 1,
                    // Bind to the address ssh came in on, so the client reaches us the
                    // same way it reached sshd.
                    's' => args.desired_ip = ssh_ip(),
                    // Unknown options do not abort, matching the C++, so a newer
                    // client's flag cannot break an older server.
                    _ => {}
                }
            }
            i += 1;
        }
    } else if opts.len() == 2 {
        args.desired_ip = Some(opts[1].clone());
    } else if opts.len() == 3 {
        args.desired_ip = Some(opts[1].clone());
        args.desired_port = Some(opts[2].clone());
    } else if opts.len() > 3 {
        return Err(ArgError::BadUsage);
    }

    Ok(args)
}

/// The local address of the ssh connection that started us, from `$SSH_CONNECTION`.
fn ssh_ip() -> Option<String> {
    let conn = std::env::var("SSH_CONNECTION").ok()?;
    // "client_ip client_port server_ip server_port"
    conn.split_whitespace().nth(2).map(|s| s.to_string())
}

pub const USAGE: &str = "\
Usage: mosh-server new [-s] [-v] [-i LOCALADDR] [-p PORT[:PORT2]] [-c COLORS] [-l NAME=VALUE] [-- COMMAND...]";

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_test_harness_invocation_parses() {
        // This is what third_party/mosh/src/tests/mosh-server runs.
        let a = parse(&argv(&["mosh-server", "new", "-vv", "-@", "--", "printf", "hi"])).unwrap();
        assert_eq!(a.verbose, 2);
        assert_eq!(a.command, argv(&["printf", "hi"]));
    }

    #[test]
    fn the_wrapper_invocation_parses() {
        let a = parse(&argv(&[
            "mosh-server", "new", "-s", "-c", "256", "-l", "LANG=en_US.UTF-8",
        ]))
        .unwrap();
        assert_eq!(a.colors, 256);
        assert_eq!(a.locale_vars, vec![("LANG".into(), "en_US.UTF-8".into())]);
    }

    #[test]
    fn the_eating_option_discards_its_argument() {
        // -@ exists so a prepended argv does not confuse the parse.
        let a = parse(&argv(&["mosh-server", "new", "-v", "-@", "new", "-c", "256"])).unwrap();
        assert_eq!(a.colors, 256);
        assert_eq!(a.verbose, 1);
    }

    #[test]
    fn a_command_requires_the_double_dash() {
        let a = parse(&argv(&["mosh-server", "new", "--", "bash", "-l"])).unwrap();
        assert_eq!(a.command, argv(&["bash", "-l"]));

        // A trailing `--` with nothing after it means no command.
        let a = parse(&argv(&["mosh-server", "new", "--"])).unwrap();
        assert!(a.command.is_empty());
    }

    #[test]
    fn addresses_and_ports_are_read() {
        let a = parse(&argv(&["mosh-server", "new", "-i", "10.0.0.1", "-p", "60000"])).unwrap();
        assert_eq!(a.desired_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(a.desired_port.as_deref(), Some("60000"));
    }

    #[test]
    fn legacy_positional_syntax_still_works() {
        let a = parse(&argv(&["mosh-server", "10.0.0.1"])).unwrap();
        assert_eq!(a.desired_ip.as_deref(), Some("10.0.0.1"));

        let a = parse(&argv(&["mosh-server", "10.0.0.1", "60000"])).unwrap();
        assert_eq!(a.desired_port.as_deref(), Some("60000"));
    }

    #[test]
    fn bad_colors_are_rejected() {
        assert_eq!(
            parse(&argv(&["mosh-server", "new", "-c", "many"])),
            Err(ArgError::BadColors("many".into()))
        );
    }

    #[test]
    fn bundled_short_flags_are_counted_separately() {
        // getopt reads -vv as two v flags; the test harness relies on it.
        assert_eq!(parse(&argv(&["mosh-server", "new", "-vv"])).unwrap().verbose, 2);
        assert_eq!(parse(&argv(&["mosh-server", "new", "-vvv"])).unwrap().verbose, 3);
    }

    #[test]
    fn a_value_may_be_attached_to_its_flag() {
        let a = parse(&argv(&["mosh-server", "new", "-c256"])).unwrap();
        assert_eq!(a.colors, 256);
        let a = parse(&argv(&["mosh-server", "new", "-p60000"])).unwrap();
        assert_eq!(a.desired_port.as_deref(), Some("60000"));
    }

    #[test]
    fn unknown_options_do_not_abort() {
        // The C++ prints usage but carries on, so a newer client's flag cannot break us.
        assert!(parse(&argv(&["mosh-server", "new", "-Q", "-v"])).is_ok());
    }

    #[test]
    fn port_ranges_parse_and_validate() {
        assert_eq!(parse_portrange("60000"), Some((60000, 60000)));
        assert_eq!(parse_portrange("60000:60010"), Some((60000, 60010)));
        // Backwards, zero, and non-numeric are all refused.
        assert_eq!(parse_portrange("60010:60000"), None);
        assert_eq!(parse_portrange("0"), None);
        assert_eq!(parse_portrange("abc"), None);
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(parse(&argv(&["mosh-server", "--help"])).unwrap().help);
        assert!(parse(&argv(&["mosh-server", "--version"])).unwrap().version);
    }
}
