//! Shared bits for the differential tests against upstream mosh.

use std::process::Command;

fn pkg_config(flag: &str, package: &str) -> Option<Vec<String>> {
    let out = Command::new("pkg-config")
        .arg(flag)
        .arg(package)
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect()
    })
}

/// Drop the two attributes the Rust has and upstream does not.
///
/// SGR 2 (faint) and SGR 9 (strikethrough) are parsed and reported here but ignored by
/// upstream, which stores neither. Comparing them would fail on every screen that uses
/// one, so both sides are stripped of them before the screens are compared; the Rust
/// behaviour itself is covered by the unit tests in `framebuffer.rs`.
pub fn strip_attributes_upstream_lacks(dump: &str) -> String {
    let mut out = String::with_capacity(dump.len());
    let mut rest = dump;
    while let Some(start) = rest.find("\x1b[0") {
        let Some(end) = rest[start..].find('m').map(|i| start + i) else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str("\x1b[");
        let mut attributes = true;
        for (i, token) in rest[start + 2..end].split(';').enumerate() {
            attributes &= i == 0 || matches!(token, "1" | "2" | "3" | "4" | "5" | "7" | "8" | "9");
            if attributes && (token == "2" || token == "9") {
                continue;
            }
            if i > 0 {
                out.push(';');
            }
            out.push_str(token);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// How to link the terminfo library, which upstream's terminal code needs. Mirrors the
/// order `mosh-sys` probes in: tinfo where it is split out, ncurses where it is not.
pub fn terminfo_libs() -> Vec<String> {
    for name in ["tinfo", "ncursesw", "ncurses"] {
        if let Some(flags) = pkg_config("--libs", name) {
            return flags;
        }
    }
    vec!["-lncurses".to_string()]
}
