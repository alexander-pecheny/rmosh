//! Differential test for frame generation.
//!
//! Every byte a frame contains reaches the user's real terminal and is compared directly
//! by the end-to-end capture tests, so this asserts byte equality with the C++ rather
//! than equivalence. It covers not just what is drawn but which shortcut is chosen: a
//! backspace instead of an absolute move, an erase instead of spaces, a scroll instead
//! of a repaint.
//!
//! Skips when the C++ static libraries have not been built.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cpp_harness() -> Option<PathBuf> {
    static HARNESS: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HARNESS.get_or_init(build_cpp_harness).clone()
}

fn build_cpp_harness() -> Option<PathBuf> {
    let root = repo_root();
    let terminal_lib = root.join("src/terminal/libmoshterminal.a");
    let util_lib = root.join("src/util/libmoshutil.a");
    let source = root.join("src/tests/frame-dump.cc");
    if !terminal_lib.exists() || !util_lib.exists() || !source.exists() {
        return None;
    }

    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("frame-dump-cpp");
    let status = Command::new("g++")
        .arg("-O2")
        .arg("-I")
        .arg(&root)
        .arg("-o")
        .arg(&out)
        .arg(&source)
        .arg(&terminal_lib)
        .arg(&util_lib)
        .arg("-ltinfo")
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
    dir.join("examples/frame-dump")
}

fn run(tool: &Path, w: i32, h: i32, initialized: bool, input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(tool)
        .arg(w.to_string())
        .arg(h.to_string())
        .arg(if initialized { "1" } else { "0" })
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", tool.display()));
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap().stdout
}

/// Package two byte streams the way both harnesses expect.
fn payload(before: &[u8], after: &[u8]) -> Vec<u8> {
    let mut v = (before.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(before);
    v.extend_from_slice(after);
    v
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

const PIECES: &[&[u8]] = &[
    b"\x1b[", b"\x1b]", b"\x1b", b"\xc2\x9c", b"\x07", b";", b"0", b"1", b"2", b"3", b"7", b"38",
    b"48", b"?", b"!", b"#", b"m", b"H", b"A", b"B", b"C", b"D", b"J", b"K", b"L", b"M", b"P",
    b"X", b"@", b"r", b"d", b"G", b"S", b"T", b"g", b"Z", b"h", b"l", b"8", b"hello", b"ab",
    b"\xe4\xb8\x80", b"\xc3\xa9", b"\xcc\x81", b"\xff", b"\n", b"\r", b"\t", b"\x08",
];

fn stream(rng: &mut Rng, fragments: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..fragments {
        if rng.below(100) < 88 {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
        } else {
            out.push(rng.below(256) as u8);
        }
    }
    out
}

fn compare(w: i32, h: i32, initialized: bool, before: &[u8], after: &[u8], label: &str) {
    let Some(cpp) = cpp_harness() else { return };
    let rust = rust_harness();
    if !rust.exists() {
        return;
    }
    let input = payload(before, after);
    let cpp_out = run(&cpp, w, h, initialized, &input);
    let rust_out = run(&rust, w, h, initialized, &input);
    assert_eq!(
        String::from_utf8_lossy(&cpp_out),
        String::from_utf8_lossy(&rust_out),
        "{label} ({w}x{h}, initialized={initialized}) produced a different frame"
    );
}

#[test]
fn frames_match_the_cpp_display_under_fuzz() {
    if cpp_harness().is_none() {
        eprintln!("skipping: C++ libraries not built");
        return;
    }
    for seed in 1..=60u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let before = stream(&mut rng, 400);
        let after = stream(&mut rng, 400);
        for (w, h) in [(20, 6), (80, 24), (5, 3)] {
            for initialized in [false, true] {
                compare(w, h, initialized, &before, &after, &format!("seed {seed}"));
            }
        }
    }
}

#[test]
fn frames_match_on_the_cases_the_shortcuts_target() {
    if cpp_harness().is_none() {
        eprintln!("skipping: C++ libraries not built");
        return;
    }

    let cases: &[(&str, &[u8], &[u8])] = &[
        ("no change", b"hello", b""),
        ("one character appended", b"hello", b"!"),
        ("one character overwritten", b"hello", b"\x1b[1;1Hj"),
        ("short move left", b"hello", b"\x1b[1;3Hx"),
        ("scroll by one", b"a\r\nb\r\nc\r\nd\r\ne\r\nf", b"\r\ng"),
        ("scroll by several", b"a\r\nb\r\nc\r\nd\r\ne\r\nf", b"\r\ng\r\nh\r\ni"),
        ("clear to end of line", b"hello world", b"\x1b[1;6H\x1b[K"),
        ("erase run of cells", b"hello world", b"\x1b[1;3H\x1b[6X"),
        ("full clear", b"hello\r\nworld", b"\x1b[2J"),
        ("background colour run", b"", b"\x1b[41m\x1b[2J"),
        ("rendition change only", b"abc", b"\x1b[1;1H\x1b[1mabc"),
        ("hyperlink appears", b"abc", b"\x1b[1;1H\x1b]8;;http://x\x07abc"),
        ("wide characters", b"", "\u{4e00}\u{4e00}\u{4e00}".as_bytes()),
        ("combining characters", b"", "e\u{301}o\u{308}".as_bytes()),
        ("wrapping row", b"", b"aaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("title change", b"x", b"\x1b]0;new title\x07"),
        ("bell", b"x", b"\x07"),
        ("cursor hidden", b"x", b"\x1b[?25l"),
        ("reverse video", b"x", b"\x1b[?5h"),
        ("bracketed paste", b"x", b"\x1b[?2004h"),
        ("mouse reporting on", b"x", b"\x1b[?1000h"),
        ("mouse reporting switch", b"\x1b[?1000h", b"\x1b[?1003h"),
        ("mouse encoding", b"x", b"\x1b[?1006h"),
        ("clipboard", b"x", b"\x1b]52;c;aGk=\x07"),
        ("colour query", b"x", b"\x1b]4;1;?\x07"),
    ];

    for (label, before, after) in cases {
        for (w, h) in [(20, 6), (80, 24), (5, 3), (1, 1)] {
            for initialized in [false, true] {
                compare(w, h, initialized, before, after, label);
            }
        }
    }
}
