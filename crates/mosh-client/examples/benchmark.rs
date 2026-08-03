//! The perf comparison against `third_party/mosh/src/examples/benchmark.cc`.
//!
//! One iteration is what the client does for every keystroke: predict the character,
//! take the server's screen, lay the predictions over it, and compute the frame that
//! turns the previous screen into the new one. Argument handling matches the C++ so the
//! two can be run with identical parameters.

use mosh_client::prediction::PredictionEngine;
use mosh_state::Complete;
use mosh_terminal::display::Display;
use mosh_terminal::framebuffer::Framebuffer;

const ITERATIONS: usize = 100_000;

fn main() {
    mosh_sys::set_native_locale();
    assert!(
        mosh_sys::is_utf8_locale(),
        "benchmark requires a UTF-8 locale"
    );

    let args: Vec<String> = std::env::args().collect();
    let iterations = args
        .get(1)
        .and_then(|a| a.parse::<usize>().ok())
        .unwrap_or(ITERATIONS);
    if iterations < 1 || iterations > 1_000_000_000 {
        eprintln!("bogus iteration count");
        std::process::exit(1);
    }
    let (width, height) = if args.len() > 3 {
        (
            args[2].parse::<i32>().unwrap_or(80),
            args[3].parse::<i32>().unwrap_or(24),
        )
    } else {
        (80, 24)
    };
    if !(1..=1000).contains(&width) || !(1..=1000).contains(&height) {
        eprintln!("bogus window size");
        std::process::exit(1);
    }

    // Two framebuffers swapped each round, as the C++ does, so the cost of the copy is
    // measured rather than avoided.
    let mut local_framebuffers = [Framebuffer::new(width, height), Framebuffer::new(width, height)];
    let mut fbmod = 0usize;

    let mut predictions = PredictionEngine::new();
    let display = Display::new(false).expect("no environment needed");
    let local_terminal = Complete::new(width, height);

    for i in 0..iterations {
        let (cur, next) = if fbmod == 0 {
            let (a, b) = local_framebuffers.split_at_mut(1);
            (&mut a[0], &mut b[0])
        } else {
            let (a, b) = local_framebuffers.split_at_mut(1);
            (&mut b[0], &mut a[0])
        };

        // Type a character.
        let byte = (b'x' as usize + i) as u8;
        predictions.new_user_byte(byte, cur, 0);

        // Fetch the target state.
        *next = local_terminal.fb().clone();

        // Apply local overlays.
        predictions.apply(next);

        // Compute the minimal difference from where we are.
        let diff = display.new_frame(false, cur, next);

        // Make sure the diff is actually used, so it cannot be optimised away.
        if diff.len() > i32::MAX as usize {
            std::process::exit(1);
        }

        fbmod ^= 1;
    }
}
