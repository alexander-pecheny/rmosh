//! Option handling for the launcher.
//!
//! Transliterated from the GetOptions table in `scripts/mosh.pl`. The spellings are
//! part of the user interface and are kept exactly, including the single-letter
//! aliases and the negatable forms.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub client: String,
    pub server: String,
    pub predict: Predict,
    pub predict_overwrite: bool,
    pub port_request: Option<String>,
    pub family: String,
    pub ssh: Vec<String>,
    pub ssh_pty: bool,
    pub term_init: bool,
    pub localhost: bool,
    pub bind_ip: Option<String>,
    pub help: bool,
    pub version: bool,
    /// Run as the ssh ProxyCommand, whose job is to report the resolved address.
    pub fake_proxy: bool,
    /// How to learn the address the session runs over: proxy, remote or local.
    pub remote_ip: String,
    /// host, or user@host.
    pub userhost: Option<String>,
    /// A command to run instead of the login shell.
    pub command: Vec<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            client: "mosh-client".into(),
            server: "mosh-server".into(),
            predict: Predict::Adaptive,
            predict_overwrite: false,
            port_request: None,
            family: "prefer-inet".into(),
            ssh: vec!["ssh".into()],
            ssh_pty: true,
            term_init: true,
            localhost: false,
            bind_ip: None,
            help: false,
            version: false,
            fake_proxy: false,
            remote_ip: "proxy".into(),
            userhost: None,
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predict {
    Always,
    Never,
    Adaptive,
    Experimental,
}

impl Predict {
    pub fn as_str(self) -> &'static str {
        match self {
            Predict::Always => "always",
            Predict::Never => "never",
            Predict::Adaptive => "adaptive",
            Predict::Experimental => "experimental",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "always" => Some(Predict::Always),
            "never" => Some(Predict::Never),
            "adaptive" => Some(Predict::Adaptive),
            "experimental" => Some(Predict::Experimental),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptError {
    BadPredict(String),
    MissingValue(String),
    BadRemoteIp(String),
}

impl std::fmt::Display for OptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptError::BadPredict(v) => write!(
                f,
                "Unknown prediction mode {v}. Choose one of: always, never, adaptive, experimental"
            ),
            OptError::MissingValue(o) => write!(f, "Option {o} requires a value"),
            OptError::BadRemoteIp(v) => write!(
                f,
                "Unknown parameter {v}. Choose one of: local, remote, proxy"
            ),
        }
    }
}

/// Split `--opt=value` into its parts.
fn split_value(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((k, v)) => (k, Some(v)),
        None => (arg, None),
    }
}

pub fn parse(argv: &[String]) -> Result<Opts, OptError> {
    let mut o = Opts::default();
    // The prediction mode may also come from the environment, and an invalid value
    // there is reported against the variable rather than a flag.
    if let Ok(env) = std::env::var("MOSH_PREDICTION_DISPLAY") {
        o.predict = Predict::parse(&env).ok_or(OptError::BadPredict(env))?;
    }
    if std::env::var("MOSH_PREDICTION_OVERWRITE").as_deref() == Ok("yes") {
        o.predict_overwrite = true;
    }

    let mut i = 1;
    let mut positional: Vec<String> = Vec::new();

    while i < argv.len() {
        let arg = argv[i].clone();

        // Everything after a bare `--` is the remote command.
        if arg == "--" {
            o.command = argv[i + 1..].to_vec();
            break;
        }

        if !arg.starts_with('-') || arg == "-" {
            positional.push(arg);
            i += 1;
            continue;
        }

        let (name, attached) = split_value(&arg);
        let take = |i: &mut usize| -> Result<String, OptError> {
            if let Some(v) = attached {
                return Ok(v.to_string());
            }
            *i += 1;
            argv.get(*i)
                .cloned()
                .ok_or_else(|| OptError::MissingValue(name.to_string()))
        };

        match name {
            "--client" => o.client = take(&mut i)?,
            "--server" => o.server = take(&mut i)?,
            "--predict" => {
                let v = take(&mut i)?;
                o.predict = Predict::parse(&v).ok_or(OptError::BadPredict(v))?;
            }
            "--predict-overwrite" | "-o" => o.predict_overwrite = true,
            "--no-predict-overwrite" => o.predict_overwrite = false,
            "--port" | "-p" => o.port_request = Some(take(&mut i)?),
            "-a" => o.predict = Predict::Always,
            "-n" => o.predict = Predict::Never,
            "--family" => o.family = take(&mut i)?.to_lowercase(),
            "-4" => o.family = "inet".into(),
            "-6" => o.family = "inet6".into(),
            "--ssh" => o.ssh = shellwords(&take(&mut i)?),
            "--ssh-pty" => o.ssh_pty = true,
            "--no-ssh-pty" => o.ssh_pty = false,
            "--init" => o.term_init = true,
            "--no-init" => o.term_init = false,
            "--local" => o.localhost = true,
            "--bind-server" => o.bind_ip = Some(take(&mut i)?),
            "--experimental-remote-ip" => {
                let v = take(&mut i)?.to_lowercase();
                if !["local", "remote", "proxy"].contains(&v.as_str()) {
                    return Err(OptError::BadRemoteIp(v));
                }
                o.remote_ip = v;
            }
            "--fake-proxy" => o.fake_proxy = true,
            "--no-fake-proxy" => o.fake_proxy = false,
            "--help" => o.help = true,
            "--version" => o.version = true,
            // Unrecognised options are passed through to ssh, as the Perl does for
            // anything its table does not claim.
            _ => o.ssh.push(arg),
        }
        i += 1;
    }

    if !positional.is_empty() {
        o.userhost = Some(positional[0].clone());
        // A command may also follow the host without a `--`.
        if o.command.is_empty() && positional.len() > 1 {
            o.command = positional[1..].to_vec();
        }
    }

    Ok(o)
}

/// Split a string the way a shell would, honouring quotes.
///
/// Used for `--ssh`, whose value is a command line rather than a single word.
pub fn shellwords(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut quote: Option<char> = None;
    let mut has_word = false;

    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), ch) if ch == q => quote = None,
            (Some(_), ch) => cur.push(ch),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                has_word = true;
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                    has_word = true;
                }
            }
            (None, ch) if ch.is_whitespace() => {
                if has_word || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            (None, ch) => {
                cur.push(ch);
                has_word = true;
            }
        }
    }
    if has_word || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Quote a word so a remote shell receives it unchanged.
pub fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c))
    {
        return word.to_string();
    }
    // Single quotes protect everything except a single quote itself.
    format!("'{}'", word.replace('\'', r"'\''"))
}

pub const USAGE: &str = "\
Usage: mosh [options] [--] [user@]host [command...]
        --client=PATH        mosh client on local machine
        --server=COMMAND     mosh server on remote machine
        --predict=adaptive   local echo for slower links
        --predict=always|never|experimental
    -a                       -\x2d-predict=always
    -n                       -\x2d-predict=never
        --predict-overwrite  aggressive prediction over existing text
    -p, --port=PORT[:PORT2]  server-side UDP port or port range
        --bind-server={ssh|any|IP}
    -4                       use IPv4 only
    -6                       use IPv6 only
        --ssh=COMMAND        ssh command to run
        --no-ssh-pty         do not allocate a pty on the ssh connection
        --no-init            do not send terminal initialization string
        --local              run mosh-server locally without using ssh
        --experimental-remote-ip=(local|remote|proxy)  select the method for
                             discovering the remote IP address to use for mosh
                             (default: \"proxy\")
        --help               this message
        --version            version and copyright information";

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_test_harness_invocation_parses() {
        let o = parse(&argv(&[
            "mosh",
            "--client=../frontend/mosh-client",
            "--server=/path/mosh-server",
            "--local",
            "--bind-server=127.0.0.1",
            "127.0.0.1",
        ]))
        .unwrap();
        assert_eq!(o.client, "../frontend/mosh-client");
        assert_eq!(o.server, "/path/mosh-server");
        assert!(o.localhost);
        assert_eq!(o.bind_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(o.userhost.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn a_command_may_follow_the_host_with_or_without_a_dash_dash() {
        let o = parse(&argv(&["mosh", "host", "--", "printf", "hi"])).unwrap();
        assert_eq!(o.userhost.as_deref(), Some("host"));
        assert_eq!(o.command, argv(&["printf", "hi"]));

        let o = parse(&argv(&["mosh", "host", "ls", "-l"])).unwrap();
        assert_eq!(o.command, argv(&["ls"]), "options after the host go to ssh");
    }

    #[test]
    fn values_attach_with_equals_or_stand_alone() {
        let a = parse(&argv(&["mosh", "--port=60000", "h"])).unwrap();
        let b = parse(&argv(&["mosh", "--port", "60000", "h"])).unwrap();
        assert_eq!(a.port_request, b.port_request);
        assert_eq!(a.port_request.as_deref(), Some("60000"));
    }

    #[test]
    fn prediction_modes_and_their_aliases() {
        assert_eq!(parse(&argv(&["mosh", "-a", "h"])).unwrap().predict, Predict::Always);
        assert_eq!(parse(&argv(&["mosh", "-n", "h"])).unwrap().predict, Predict::Never);
        assert_eq!(
            parse(&argv(&["mosh", "--predict=experimental", "h"]))
                .unwrap()
                .predict,
            Predict::Experimental
        );
        assert_eq!(
            parse(&argv(&["mosh", "--predict=sideways", "h"])),
            Err(OptError::BadPredict("sideways".into()))
        );
    }

    #[test]
    fn negatable_options_work_both_ways() {
        assert!(!parse(&argv(&["mosh", "--no-init", "h"])).unwrap().term_init);
        assert!(parse(&argv(&["mosh", "--init", "h"])).unwrap().term_init);
        assert!(!parse(&argv(&["mosh", "--no-ssh-pty", "h"])).unwrap().ssh_pty);
    }

    #[test]
    fn the_remote_ip_method_defaults_to_proxy_and_takes_the_three_spellings() {
        assert_eq!(parse(&argv(&["mosh", "h"])).unwrap().remote_ip, "proxy");
        for method in ["local", "remote", "proxy"] {
            let o = parse(&argv(&["mosh", &format!("--experimental-remote-ip={method}"), "h"]));
            assert_eq!(o.unwrap().remote_ip, method);
        }
        assert_eq!(
            parse(&argv(&["mosh", "--experimental-remote-ip=sideways", "h"])),
            Err(OptError::BadRemoteIp("sideways".into())),
            "an unknown method should be rejected rather than passed to ssh"
        );
    }

    #[test]
    fn the_fake_proxy_takes_its_host_and_port_after_the_separator() {
        let o = parse(&argv(&[
            "mosh",
            "--family=inet",
            "--fake-proxy",
            "--",
            "10.0.0.1",
            "22",
        ]))
        .unwrap();
        assert!(o.fake_proxy);
        assert_eq!(o.command, argv(&["10.0.0.1", "22"]));
    }

    #[test]
    fn the_address_family_flags_agree_with_the_long_form() {
        assert_eq!(parse(&argv(&["mosh", "-4", "h"])).unwrap().family, "inet");
        assert_eq!(parse(&argv(&["mosh", "-6", "h"])).unwrap().family, "inet6");
        assert_eq!(
            parse(&argv(&["mosh", "--family=INET6", "h"])).unwrap().family,
            "inet6",
            "family should be case-insensitive"
        );
    }

    #[test]
    fn the_ssh_option_is_a_command_line_not_a_word() {
        let o = parse(&argv(&["mosh", "--ssh=ssh -p 2222", "h"])).unwrap();
        assert_eq!(o.ssh, argv(&["ssh", "-p", "2222"]));
    }

    #[test]
    fn unrecognised_options_are_handed_to_ssh() {
        // So that ssh flags can be given without mosh having to know them all.
        let o = parse(&argv(&["mosh", "-J", "h"])).unwrap();
        assert!(o.ssh.contains(&"-J".to_string()));
    }

    #[test]
    fn shellwords_honours_quotes_and_escapes() {
        assert_eq!(shellwords("a b  c"), argv(&["a", "b", "c"]));
        assert_eq!(shellwords("'a b' c"), argv(&["a b", "c"]));
        assert_eq!(shellwords(r#""a b" c"#), argv(&["a b", "c"]));
        assert_eq!(shellwords(r"a\ b"), argv(&["a b"]));
        // An empty quoted string is a word, not nothing.
        assert_eq!(shellwords("'' x"), argv(&["", "x"]));
    }

    #[test]
    fn shell_quote_protects_what_needs_it() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("/usr/bin/mosh-server"), "/usr/bin/mosh-server");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        // An empty argument must survive as an empty argument.
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn help_and_version_are_recognised() {
        assert!(parse(&argv(&["mosh", "--help"])).unwrap().help);
        assert!(parse(&argv(&["mosh", "--version"])).unwrap().version);
    }
}
