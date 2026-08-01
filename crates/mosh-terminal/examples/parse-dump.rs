//! Read bytes on stdin, print the action stream the parser produces.
//!
//! Output format matches the C++ harness exactly so the two can be diffed byte for byte.

use std::io::{Read, Write};

use mosh_terminal::parser::Utf8Parser;

fn main() {
    mosh_sys::set_native_locale();

    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("failed to read stdin");

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    let mut parser = Utf8Parser::new();
    let mut actions = Vec::new();
    for byte in input {
        actions.clear();
        parser.input(byte, &mut actions);
        for action in &actions {
            match action.ch {
                Some(ch) => writeln!(out, "{} {}", action.kind.name(), ch as u32),
                None => writeln!(out, "{}", action.kind.name()),
            }
            .expect("failed to write");
        }
    }
}
