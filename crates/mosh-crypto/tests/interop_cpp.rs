//! Differential tests against the C++ implementation.
//!
//! The wire contract says a datagram written by one implementation must be readable by
//! the other. These tests assert exactly that, using the `encrypt` and `decrypt` example
//! tools from the C++ tree. They skip when those binaries have not been built, so the
//! Rust suite stays runnable on its own.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use mosh_crypto::{Base64Key, Message, Nonce, Session};

fn cpp_tool(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/mosh/src/examples")
        .join(name);
    path.exists().then_some(path)
}

/// Run a C++ tool over stdin, returning (stdout, stderr).
fn run(tool: &PathBuf, arg: &str, input: &[u8]) -> (Vec<u8>, String) {
    let mut child = Command::new(tool)
        .arg(arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn C++ tool");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input)
        .expect("failed writing to C++ tool");
    let out = child.wait_with_output().expect("C++ tool did not finish");
    assert!(
        out.status.success(),
        "C++ tool failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn rust_decrypts_what_cpp_encrypted() {
    let Some(encrypt) = cpp_tool("encrypt") else {
        eprintln!("skipping: C++ encrypt not built");
        return;
    };

    for (nonce_val, plaintext) in [
        (0u64, &b""[..]),
        (1, b"hello"),
        (42, b"a plaintext of exactly sixteen b"),
        (999_999, &[0xffu8; 300][..]),
    ] {
        let (ciphertext, stderr) = run(&encrypt, &nonce_val.to_string(), plaintext);

        // The tool reports the random key it generated on stderr as "Key: <22 chars>".
        let printable = stderr
            .lines()
            .find_map(|l| l.strip_prefix("Key: "))
            .expect("C++ encrypt did not report a key")
            .trim();
        let key = Base64Key::parse(printable).expect("C++ produced an unparseable key");

        let session = Session::new(&key);
        let message = session
            .decrypt(&ciphertext)
            .expect("Rust could not decrypt a C++ datagram");

        assert_eq!(message.text, plaintext, "plaintext differs for nonce {nonce_val}");
        assert_eq!(message.nonce.val(), nonce_val, "nonce differs");
    }
}

#[test]
fn cpp_decrypts_what_rust_encrypted() {
    let Some(decrypt) = cpp_tool("decrypt") else {
        eprintln!("skipping: C++ decrypt not built");
        return;
    };

    let key = Base64Key::random();
    let printable = key.printable();
    let mut session = Session::new(&key);

    for (nonce_val, plaintext) in [
        (0u64, &b""[..]),
        (7, b"hello"),
        (12345, b"a plaintext of exactly sixteen b"),
        (1 << 40, &[0x5au8; 512][..]),
    ] {
        let wire = session
            .encrypt(&Message::new(Nonce::new(nonce_val), plaintext.to_vec()))
            .expect("Rust encryption failed");

        let (recovered, stderr) = run(&decrypt, &printable, &wire);

        assert_eq!(recovered, plaintext, "C++ recovered a different plaintext");
        assert!(
            stderr.contains(&format!("Nonce = {nonce_val}")),
            "C++ read a different nonce; stderr was {stderr:?}"
        );
    }
}

#[test]
fn cpp_rejects_a_tampered_rust_datagram() {
    let Some(decrypt) = cpp_tool("decrypt") else {
        eprintln!("skipping: C++ decrypt not built");
        return;
    };

    let key = Base64Key::random();
    let mut session = Session::new(&key);
    let mut wire = session
        .encrypt(&Message::new(Nonce::new(3), b"integrity matters".to_vec()))
        .unwrap();
    let last = wire.len() - 1;
    wire[last] ^= 0x80;

    let mut child = Command::new(&decrypt)
        .arg(key.printable())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child.stdin.as_mut().unwrap().write_all(&wire).unwrap();
    let out = child.wait_with_output().unwrap();

    assert!(
        !out.status.success(),
        "C++ accepted a datagram whose tag we corrupted"
    );
}

#[test]
fn key_encoding_agrees_with_cpp() {
    let Some(encrypt) = cpp_tool("encrypt") else {
        eprintln!("skipping: C++ encrypt not built");
        return;
    };

    // Every key the C++ generates must round-trip through our parser unchanged.
    for _ in 0..16 {
        let (_, stderr) = run(&encrypt, "1", b"x");
        let printable = stderr
            .lines()
            .find_map(|l| l.strip_prefix("Key: "))
            .unwrap()
            .trim();
        let key = Base64Key::parse(printable).expect("could not parse a C++ key");
        assert_eq!(key.printable(), printable);
    }
}
