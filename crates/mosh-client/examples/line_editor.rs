//! A line editor that redraws the whole line on every keystroke, as a shell's does.
//! It handles backspace and Option-Backspace (kill the word to the left). The typing
//! test runs it on the server side.

use std::io::{Read, Write};

fn main() {
    let saved = mosh_sys::pty::set_raw(0).expect("a tty on stdin");
    let mut text = String::new();
    let mut stdin = std::io::stdin();
    let mut byte = [0u8; 1];
    let mut escaped = false;
    render(&text);
    while stdin.read(&mut byte).is_ok_and(|n| n == 1) {
        match byte[0] {
            0x04 => break,
            0x1b => {
                escaped = true;
                continue;
            }
            0x7f if escaped => {
                while text.ends_with(' ') {
                    text.pop();
                }
                while !text.is_empty() && !text.ends_with(' ') {
                    text.pop();
                }
            }
            0x7f => {
                text.pop();
            }
            b if b < 0x20 => {}
            b => text.push(b as char),
        }
        escaped = false;
        render(&text);
    }
    mosh_sys::pty::restore_termios(0, &saved);
}

fn render(text: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[H\x1b[J{text}");
    let _ = out.flush();
}
