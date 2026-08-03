//! Differential test: our parser must produce the same action stream as the C++ one.
//!
//! Both implementations read bytes and print one line per action. Any divergence in
//! state transitions, UTF-8 recovery, or the 0xA0 codepoint folding shows up as a diff.
//!
//! Skips unless the upstream submodule has been built; see `tests/cpp/build.sh`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Build the C++ harness against the already-compiled static libraries.
///
/// Returns None when the C++ tree has not been built, so the Rust suite stays runnable
/// on its own. Tests share one process, so the OnceLock keeps concurrent tests from
/// compiling over each other's output.
fn cpp_harness() -> Option<PathBuf> {
    static HARNESS: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HARNESS.get_or_init(build_cpp_harness).clone()
}

fn build_cpp_harness() -> Option<PathBuf> {
    let root = repo_root();
    let upstream = root.join("third_party/mosh");
    let terminal_lib = upstream.join("src/terminal/libmoshterminal.a");
    let util_lib = upstream.join("src/util/libmoshutil.a");
    let source = root.join("tests/cpp/parse-dump.cc");
    if !terminal_lib.exists() || !util_lib.exists() || !source.exists() {
        return None;
    }

    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("parse-dump-cpp");
    if out.exists() {
        return Some(out);
    }

    let status = Command::new("g++")
        .arg("-O2")
        .arg("-I")
        .arg(&upstream)
        .arg("-o")
        .arg(&out)
        .arg(&source)
        .arg(&terminal_lib)
        .arg(&util_lib)
        .args(common::terminfo_libs())
        .status()
        .ok()?;

    status.success().then_some(out)
}

fn rust_harness() -> PathBuf {
    // The example binary sits next to the test binary's directory.
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop(); // the test binary's own name
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("examples/parse-dump")
}

fn run(tool: &Path, input: &[u8]) -> String {
    let mut child = Command::new(tool)
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", tool.display()));
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{} failed", tool.display());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A small deterministic generator, so failures are reproducible without a rand crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Fragments chosen to steer the generator into escape sequences rather than pure noise,
/// which is where the two state machines could realistically disagree.
const PIECES: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1bX",
    b"\x1b",
    b"\xc2\x9c", // ST
    b"\xc2\x9b", // CSI
    b"\xc2\x90", // DCS
    b"\x07",
    b";",
    b":",
    b"0",
    b"1",
    b"38",
    b"m",
    b"H",
    b"A",
    b"hello",
    b"\xe4\xb8\x80", // wide
    b"\xc3\xa9",     // accented
    b"\xf0\x9f\x98\x80", // emoji
    b"\xff",         // never valid UTF-8
    b"\xe4",         // truncated lead byte
    b"\n",
    b"\r",
    b"\x18",
    b"\x1a",
];

fn corpus(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(len * 2);
    while out.len() < len {
        if rng.below(4) != 0 {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
        } else {
            out.push(rng.below(256) as u8);
        }
    }
    out
}

#[test]
fn action_streams_match_the_cpp_parser() {
    let Some(cpp) = cpp_harness() else {
        eprintln!("skipping: C++ libraries not built");
        return;
    };
    let rust = rust_harness();
    assert!(
        rust.exists(),
        "build the example first: cargo build -p mosh-terminal --example parse-dump"
    );

    for seed in 1..=24u64 {
        let input = corpus(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 4096);
        let cpp_out = run(&cpp, &input);
        let rust_out = run(&rust, &input);

        if cpp_out != rust_out {
            let first_diff = cpp_out
                .lines()
                .zip(rust_out.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(cpp_out.lines().count().min(rust_out.lines().count()));
            let ctx = |s: &str| {
                s.lines()
                    .skip(first_diff.saturating_sub(3))
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            panic!(
                "seed {seed} diverged at action {first_diff}\n--- C++ ---\n{}\n--- Rust ---\n{}",
                ctx(&cpp_out),
                ctx(&rust_out)
            );
        }
    }
}

#[test]
fn every_single_byte_is_handled_identically() {
    let Some(cpp) = cpp_harness() else {
        eprintln!("skipping: C++ libraries not built");
        return;
    };
    let rust = rust_harness();
    if !rust.exists() {
        eprintln!("skipping: Rust example not built");
        return;
    }

    // Feed each byte value on its own, from a clean parser state.
    let all: Vec<u8> = (0..=255u8).collect();
    for b in all {
        let input = [b];
        assert_eq!(
            run(&cpp, &input),
            run(&rust, &input),
            "byte 0x{b:02x} handled differently"
        );
    }
}

#[test]
fn exhaustive_two_byte_sequences_after_escape() {
    let Some(cpp) = cpp_harness() else {
        eprintln!("skipping: C++ libraries not built");
        return;
    };
    let rust = rust_harness();
    if !rust.exists() {
        eprintln!("skipping: Rust example not built");
        return;
    }

    // ESC followed by every byte: covers the whole Escape dispatch table at once.
    let mut input = Vec::new();
    for b in 0..=255u8 {
        input.push(0x1b);
        input.push(b);
    }
    assert_eq!(run(&cpp, &input), run(&rust, &input));

    // CSI followed by every byte.
    let mut input = Vec::new();
    for b in 0..=255u8 {
        input.extend_from_slice(b"\x1b[");
        input.push(b);
    }
    assert_eq!(run(&cpp, &input), run(&rust, &input));
}
