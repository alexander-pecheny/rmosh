//! Read bytes on stdin, print the resulting screen state.
//!
//! Output format matches the C++ harness exactly so the two can be diffed byte for byte.

use std::io::{Read, Write};

use mosh_terminal::emulator::Emulator;

fn main() {
    mosh_sys::set_native_locale();

    let args: Vec<String> = std::env::args().collect();
    let (width, height) = if args.len() > 2 {
        (
            args[1].parse().unwrap_or(20),
            args[2].parse().unwrap_or(6),
        )
    } else {
        (20, 6)
    };

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).expect("read stdin");

    let mut emu = Emulator::new(width, height);
    emu.input(&input);

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let fb = emu.fb();

    writeln!(out, "cursor {} {}", fb.ds.cursor_row(), fb.ds.cursor_col()).unwrap();
    writeln!(out, "visible {}", fb.ds.cursor_visible as i32).unwrap();
    writeln!(out, "reverse {}", fb.ds.reverse_video as i32).unwrap();
    writeln!(out, "origin {}", fb.ds.origin_mode as i32).unwrap();
    writeln!(out, "autowrap {}", fb.ds.auto_wrap_mode as i32).unwrap();
    writeln!(out, "insert {}", fb.ds.insert_mode as i32).unwrap();
    writeln!(out, "bracketed {}", fb.ds.bracketed_paste as i32).unwrap();
    writeln!(
        out,
        "appcursor {}",
        fb.ds.application_mode_cursor_keys as i32
    )
    .unwrap();
    writeln!(
        out,
        "region {} {}",
        fb.ds.scrolling_region_top_row(),
        fb.ds.scrolling_region_bottom_row()
    )
    .unwrap();
    writeln!(out, "bell {}", fb.bell_count()).unwrap();
    writeln!(out, "clipseq {}", fb.clipboard_seq()).unwrap();
    writeln!(out, "colorqueries {}", fb.color_queries()).unwrap();
    writeln!(out, "sgr {}", fb.ds.renditions().sgr()).unwrap();

    for y in 0..fb.ds.height() {
        for x in 0..fb.ds.width() {
            let cell = fb.cell(y, x).unwrap();
            let mut grapheme = String::new();
            cell.print_grapheme(&mut grapheme);
            writeln!(
                out,
                "cell {} {} [{}] {} w{} f{} r{}",
                y,
                x,
                grapheme,
                cell.renditions().sgr(),
                cell.wide() as i32,
                cell.fallback() as i32,
                cell.wrap() as i32
            )
            .unwrap();
        }
    }
}
