//! Differential test for the whole emulator: parser, dispatcher and framebuffer.
//!
//! Both implementations consume the same bytes and print the resulting screen -- every
//! cell with its grapheme, renditions and flags, plus the cursor, modes and scrolling
//! region. Any divergence anywhere in the stack shows up as a diff.
//!
//! Skips unless the upstream submodule has been built; see `tests/cpp/build.sh`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cpp_harness() -> Option<PathBuf> {
    static HARNESS: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HARNESS.get_or_init(build_cpp_harness).clone()
}

fn build_cpp_harness() -> Option<PathBuf> {
    let root = repo_root();
    let upstream = root.join("third_party/mosh");
    let terminal_lib = upstream.join("src/terminal/libmoshterminal.a");
    let util_lib = upstream.join("src/util/libmoshutil.a");
    let source = root.join("tests/cpp/emu-dump.cc");
    if !terminal_lib.exists() || !util_lib.exists() || !source.exists() {
        return None;
    }

    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("emu-dump-cpp");
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
    let mut dir = std::env::current_exe().expect("current_exe");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    dir.join("examples/emu-dump")
}

fn run(tool: &Path, width: i32, height: i32, input: &[u8]) -> String {
    let mut child = Command::new(tool)
        .arg(width.to_string())
        .arg(height.to_string())
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

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Fragments weighted towards real escape sequences, since pure noise mostly lands in
/// the parser's error paths and never reaches the dispatcher.
const PIECES: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1b",
    b"\xc2\x9c",
    b"\xc2\x9b",
    b"\x07",
    b";",
    b":",
    b"0",
    b"1",
    b"2",
    b"3",
    b"7",
    b"38",
    b"48",
    b"?",
    b"!",
    b">",
    b"#",
    b"m",
    b"H",
    b"A",
    b"B",
    b"C",
    b"D",
    b"J",
    b"K",
    b"L",
    b"M",
    b"P",
    b"X",
    b"@",
    b"r",
    b"n",
    b"c",
    b"d",
    b"G",
    b"S",
    b"T",
    b"g",
    b"I",
    b"Z",
    b"h",
    b"l",
    b"p",
    b"8",
    b"hello",
    b"\xe4\xb8\x80",
    b"\xc3\xa9",
    b"\xcc\x81",
    b"\xf0\x9f\x98\x80",
    b"\xff",
    b"\xe4",
    b"\n",
    b"\r",
    b"\t",
    b"\x08",
    b"\x18",
    b"\x1a",
    b"\x0b",
    b"\x0c",
];

fn corpus(seed: u64, fragments: usize) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    let mut out = Vec::new();
    for _ in 0..fragments {
        if rng.below(100) < 85 {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
        } else {
            out.push(rng.below(256) as u8);
        }
    }
    out
}

fn compare(width: i32, height: i32, input: &[u8], label: &str) {
    let Some(cpp) = cpp_harness() else { return };
    let rust = rust_harness();
    if !rust.exists() {
        return;
    }

    let cpp_out = common::strip_attributes_upstream_lacks(&run(&cpp, width, height, input));
    let rust_out = common::strip_attributes_upstream_lacks(&run(&rust, width, height, input));

    if cpp_out != rust_out {
        let diff: Vec<String> = cpp_out
            .lines()
            .zip(rust_out.lines())
            .filter(|(a, b)| a != b)
            .take(8)
            .map(|(a, b)| format!("  C++  {a}\n  Rust {b}"))
            .collect();
        panic!("{label} diverged:\n{}", diff.join("\n"));
    }
}

#[test]
fn screens_match_the_cpp_emulator_under_fuzz() {
    if cpp_harness().is_none() {
        eprintln!("skipping: C++ libraries not built");
        return;
    }
    for seed in 1..=40u64 {
        let input = corpus(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 3000);
        compare(20, 6, &input, &format!("seed {seed}"));
    }
}

#[test]
fn screens_match_at_several_terminal_sizes() {
    if cpp_harness().is_none() {
        eprintln!("skipping: C++ libraries not built");
        return;
    }
    // A one-column and a one-row terminal exercise the wrap and scroll edges hardest.
    for (w, h) in [(1, 1), (2, 3), (5, 2), (80, 24), (132, 43)] {
        for seed in 1..=6u64 {
            let input = corpus(seed.wrapping_mul(0x1234_5678_9abc_def1), 800);
            compare(w, h, &input, &format!("{w}x{h} seed {seed}"));
        }
    }
}

#[test]
fn screens_match_on_hand_written_sequences() {
    if cpp_harness().is_none() {
        eprintln!("skipping: C++ libraries not built");
        return;
    }

    let cases: &[(&str, &[u8])] = &[
        ("plain text", b"hello world"),
        ("wrap", b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        (
            "wide char at margin",
            "aaaaaaaaaaaaaaaaaaa\u{4e00}".as_bytes(),
        ),
        ("combining", "e\u{301}a\u{301}\u{302}".as_bytes()),
        ("leading combining", "\u{301}x".as_bytes()),
        ("erase display", b"abc\r\ndef\x1b[2J"),
        ("erase line modes", b"abcdef\x1b[3G\x1b[0K\x1b[1K"),
        ("insert mode", b"world\x1b[H\x1b[4hAB"),
        ("scroll region", b"\x1b[2;4r\x1b[2Habc\r\n\r\n\r\nxyz"),
        ("reverse index", b"abc\x1bM\x1bMdef"),
        ("tabs", b"\tx\ty\x1b[3g\tz"),
        ("back tab", b"\t\t\x1b[2Zq"),
        ("sgr true colour", b"\x1b[38;2;1;2;3;48;2;4;5;6mX"),
        ("sgr 256", b"\x1b[38;5;200;48;5;17mY"),
        ("sgr zero colour", b"\x1b[1;38;5;0mZ"),
        ("osc title", b"\x1b]0;title here\x07"),
        ("osc clipboard", b"\x1b]52;c;aGVsbG8=\x07"),
        (
            "osc hyperlink",
            b"\x1b]8;id=1;http://example.com\x07link\x1b]8;;\x07",
        ),
        ("decaln", b"\x1b#8"),
        ("soft reset", b"\x1b[1m\x1b[!p"),
        ("full reset", b"\x1b[1mabc\x1bc"),
        ("save restore cursor", b"\x1b[5;5H\x1b7\x1b[1;1H\x1b8x"),
        ("origin mode", b"\x1b[?6h\x1b[3;5r\x1b[1;1Hx"),
        ("delete and insert chars", b"abcdef\x1b[3G\x1b[2P\x1b[2@"),
        ("insert delete lines", b"a\r\nb\r\nc\x1b[2H\x1b[L\x1b[M"),
        ("bell", b"\x07\x07"),
        ("bad utf8 then text", b"\xff\xfe ok"),
        ("truncated utf8", b"\xe4ok"),
        ("cursor visibility", b"\x1b[?25l\x1b[?25h\x1b[?25l"),
        ("bracketed paste", b"\x1b[?2004h"),
        ("app cursor keys", b"\x1b[?1h"),
        ("esc encoded c1", b"abc\x1bEx\x1bMy"),
    ];

    for (label, input) in cases {
        compare(20, 6, input, label);
        compare(5, 3, input, &format!("{label} (narrow)"));
    }
}
