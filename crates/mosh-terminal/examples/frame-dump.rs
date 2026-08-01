//! Build two screens and print the frame that turns one into the other.
//!
//! Input format and output match the C++ harness exactly so the two can be diffed.

use std::io::{Read, Write};

use mosh_terminal::display::Display;
use mosh_terminal::emulator::Emulator;

fn main() {
    mosh_sys::set_native_locale();

    let args: Vec<String> = std::env::args().collect();
    let (width, height, initialized) = if args.len() > 3 {
        (
            args[1].parse().unwrap_or(20),
            args[2].parse().unwrap_or(6),
            args[3].parse::<i32>().unwrap_or(1) != 0,
        )
    } else {
        (20, 6, true)
    };

    let mut all = Vec::new();
    std::io::stdin().read_to_end(&mut all).expect("read stdin");
    if all.len() < 4 {
        std::process::exit(1);
    }

    let mut split = u32::from_be_bytes([all[0], all[1], all[2], all[3]]) as usize;
    if split > all.len() - 4 {
        split = all.len() - 4;
    }
    let first = all[4..4 + split].to_vec();
    let second = all[4 + split..].to_vec();

    let mut emu = Emulator::new(width, height);
    emu.input(&first);
    // Deriving the second screen from the first keeps rows shared, which is what real
    // use does and what the scroll shortcut depends on.
    let last = emu.fb().clone();

    emu.input(&second);

    let display = Display::new(false).expect("no environment needed");
    let frame = display.new_frame(initialized, &last, emu.fb());

    std::io::stdout()
        .write_all(frame.as_bytes())
        .expect("write frame");
}
