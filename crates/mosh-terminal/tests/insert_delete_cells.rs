//! ICH and DCH shift a row by a count that comes straight out of a CSI parameter.
//!
//! They used to do it one cell at a time, so `CSI 65535 @` moved a whole row 65535 times
//! and a few hundred bytes of output bought seconds of CPU. Shifting once is only correct
//! if it agrees with the repeated move at every boundary, which is what this pins down.

use mosh_terminal::emulator::Emulator;

const WIDTH: usize = 8;

fn screen(e: &Emulator) -> String {
    let mut s = String::new();
    for x in 0..WIDTH as i32 {
        e.fb().cell(0, x).unwrap().print_grapheme(&mut s);
    }
    s
}

/// A row of `abcdefgh` with the cursor left at `col`.
fn row_at(col: usize) -> Emulator {
    let mut e = Emulator::new(WIDTH as i32, 2);
    e.input(&b"abcdefgh"[..WIDTH]);
    e.input(format!("\x1b[1;{}H", col + 1).as_bytes());
    e
}

/// What repeating the single-cell move `count` times would leave behind.
fn reference(op: u8, col: usize, count: usize) -> String {
    let mut cells: Vec<char> = "abcdefgh"[..WIDTH].chars().collect();
    for _ in 0..count {
        if op == b'@' {
            cells.insert(col, ' ');
            cells.pop();
        } else {
            cells.push(' ');
            cells.remove(col);
        }
    }
    cells.into_iter().collect()
}

#[test]
fn a_bulk_shift_agrees_with_the_repeated_one() {
    for op in [b'@', b'P'] {
        for col in 0..WIDTH {
            // Counts either side of the row's remaining width, and past any real screen.
            for count in [
                0usize,
                1,
                2,
                WIDTH - col - 1,
                WIDTH - col,
                WIDTH - col + 1,
                WIDTH,
                65535,
            ] {
                let mut e = row_at(col);
                e.input(format!("\x1b[{}{}", count, op as char).as_bytes());

                // A count of zero means one, which is what the C++ default does.
                let expected = reference(op, col, count.max(1));
                assert_eq!(
                    screen(&e),
                    expected,
                    "CSI {count} {} at column {col}",
                    op as char
                );
            }
        }
    }
}

#[test]
fn a_huge_count_costs_no_more_than_a_small_one() {
    // The guarantee is that the count no longer drives the work, so an absurd one has to
    // land in the same time as an ordinary one rather than some multiple of it.
    let run = |count: usize| {
        let mut e = Emulator::new(1000, 24);
        let input = format!("\x1b[{count}@").repeat(200);
        let start = std::time::Instant::now();
        e.input(input.as_bytes());
        start.elapsed()
    };

    let small = run(1);
    let huge = run(65535);
    assert!(
        huge < small * 20 + std::time::Duration::from_millis(50),
        "65535 cells took {huge:?} against {small:?} for one"
    );
}
