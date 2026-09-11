//! Reading the far end's startup announcement off the merged ssh stream.
//!
//! Transliterated from the handshake loop in `scripts/mosh.pl`. Everything the remote
//! shell says on the way is passed back to the caller to show; only the three `MOSH`
//! lines are ours.

/// What the far end has told us so far.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Handshake {
    /// The address a `--fake-proxy` reached, or the one resolved here under `local`.
    pub ip: Option<String>,
    /// The server's own address, from `$SSH_CONNECTION` under `remote`.
    pub ssh_connection_ip: Option<String>,
    /// The session's UDP port and key, once `MOSH CONNECT` has arrived.
    pub connect: Option<(u16, String)>,
}

impl Handshake {
    /// Feed one line of the merged ssh output, without its line ending. Returns the line
    /// again when it was not part of the handshake and the user should see it.
    pub fn line<'a>(&mut self, line: &'a str) -> Result<Option<&'a str>, String> {
        if let Some(rest) = line.strip_prefix("MOSH IP ") {
            if self.ip.is_some() {
                return Err("detected attempt to redefine MOSH IP".into());
            }
            self.ip = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("MOSH SSH_CONNECTION ") {
            // $SSH_CONNECTION is "client_ip client_port server_ip server_port".
            let words: Vec<&str> = rest.split_whitespace().collect();
            let Some(server_ip) = words.get(2).filter(|_| words.len() == 4) else {
                return Err(format!("Bad MOSH SSH_CONNECTION string: {line}"));
            };
            self.ssh_connection_ip = Some(server_ip.to_string());
        } else if let Some(rest) = line.strip_prefix("MOSH CONNECT ") {
            // Both halves are checked for shape before they go anywhere. They arrive
            // from the far end of an ssh session, and the port becomes an argument to
            // the client, where a value beginning with '-' would read as an option.
            let mut parts = rest.split_whitespace();
            match (parts.next(), parts.next(), parts.next()) {
                (Some(p), Some(k), None)
                    if k.len() == 22
                        && k.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/') =>
                {
                    let port = p
                        .parse::<u16>()
                        .map_err(|_| format!("Bad MOSH CONNECT string: {line}"))?;
                    self.connect = Some((port, k.to_string()));
                }
                _ => return Err(format!("Bad MOSH CONNECT string: {line}")),
            }
        } else {
            return Ok(Some(line));
        }
        Ok(None)
    }

    /// True once there is nothing left to wait for.
    pub fn complete(&self) -> bool {
        self.connect.is_some()
    }

    /// The address the client should send its datagrams to.
    pub fn target(&self) -> Option<&str> {
        self.ip.as_deref().or(self.ssh_connection_ip.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &[&str]) -> (Handshake, Vec<String>) {
        let mut h = Handshake::default();
        let mut shown = Vec::new();
        for line in lines {
            if let Some(text) = h.line(line).unwrap() {
                shown.push(text.to_string());
            }
        }
        (h, shown)
    }

    #[test]
    fn the_connect_line_carries_the_port_and_key() {
        let (h, shown) = feed(&["MOSH CONNECT 60001 iA4fBYJ0bLZ0CnyQLXTFLg"]);
        assert_eq!(h.connect, Some((60001, "iA4fBYJ0bLZ0CnyQLXTFLg".into())));
        assert!(h.complete() && shown.is_empty());
    }

    #[test]
    fn the_shell_is_heard_before_the_handshake() {
        let (h, shown) = feed(&[
            "Last login: today",
            "MOSH CONNECT 60001 iA4fBYJ0bLZ0CnyQLXTFLg",
        ]);
        assert_eq!(shown, ["Last login: today"]);
        assert!(h.complete());
    }

    #[test]
    fn the_server_address_comes_from_ssh_connection() {
        let (h, _) = feed(&["MOSH SSH_CONNECTION 10.0.0.2 51000 10.0.0.1 22"]);
        assert_eq!(h.target(), Some("10.0.0.1"));
    }

    #[test]
    fn a_proxied_address_wins_over_the_announced_one() {
        let (h, _) = feed(&[
            "MOSH IP 10.0.0.9",
            "MOSH SSH_CONNECTION 10.0.0.2 51000 10.0.0.1 22",
        ]);
        assert_eq!(h.target(), Some("10.0.0.9"));
    }

    #[test]
    fn malformed_lines_are_refused() {
        for line in [
            "MOSH CONNECT 60001 short",
            "MOSH CONNECT -1 iA4fBYJ0bLZ0CnyQLXTFLg",
            "MOSH CONNECT 60001 iA4fBYJ0bLZ0CnyQLXTFLg extra",
            "MOSH SSH_CONNECTION 10.0.0.2 51000 10.0.0.1",
        ] {
            assert!(Handshake::default().line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn the_address_cannot_be_redefined() {
        let mut h = Handshake {
            ip: Some("10.0.0.9".into()),
            ..Default::default()
        };
        assert!(h.line("MOSH IP 10.0.0.8").is_err());
    }
}
