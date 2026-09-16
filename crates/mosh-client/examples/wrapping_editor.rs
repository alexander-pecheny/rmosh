//! A text box that moves a word to the next line once it no longer fits, as the input
//! field of a TUI does. The typing test runs it on the server side.

use std::io::{Read, Write};

/// Narrower than the terminal, so the box wraps and the terminal never does.
const WIDTH: usize = 22;

fn main() {
    let saved = mosh_sys::pty::set_raw(0).expect("a tty on stdin");
    let mut text = String::new();
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];
    render(&text);
    while stdin.read(&mut byte).is_ok_and(|n| n == 1) {
        match byte[0] {
            0x04 => break,
            0x7f => {
                text.pop();
            }
            b => text.push(b as char),
        }
        render(&text);
    }
    mosh_sys::pty::restore_termios(0, &saved);
}

fn render(text: &str) {
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split_inclusive(' ') {
        let line = lines.last_mut().expect("never empty");
        if !line.is_empty() && line.len() + word.len() > WIDTH {
            lines.push(word.to_string());
        } else {
            line.push_str(word);
        }
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[H\x1b[J{}", lines.join("\r\n"));
    let _ = out.flush();
}
