//! Proves our inlined schemas encode identically to upstream's extension-based ones.
//!
//! Each test encodes a message with prost and hands the bytes to `protoc`, which parses
//! them against the *original* `third_party/mosh/src/protobufs/*.proto` — extensions and all. If inlining
//! had renumbered or reshaped anything, protoc would fail or report different fields.
//!
//! Skips when protoc or the upstream schemas are absent.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mosh_protobufs::{host, transport, user, Message};

fn upstream_protos() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/mosh/src/protobufs");
    dir.join("hostinput.proto").exists().then_some(dir)
}

fn have_protoc() -> bool {
    Command::new("protoc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Decode `bytes` with protoc against an upstream schema, returning its text output.
fn protoc_decode(dir: &Path, file: &str, message: &str, bytes: &[u8]) -> String {
    let mut child = Command::new("protoc")
        .arg(format!("--decode={message}"))
        .arg("--proto_path")
        .arg(dir)
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn protoc");
    child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "protoc rejected bytes we encoded for {message}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn setup() -> Option<PathBuf> {
    if !have_protoc() {
        eprintln!("skipping: protoc not available");
        return None;
    }
    let Some(dir) = upstream_protos() else {
        eprintln!("skipping: upstream .proto files not found");
        return None;
    };
    Some(dir)
}

#[test]
fn host_extensions_decode_upstream() {
    let Some(dir) = setup() else { return };

    let msg = host::HostMessage {
        instruction: vec![
            host::Instruction {
                hostbytes: Some(host::HostBytes {
                    hoststring: Some(b"hello".to_vec()),
                }),
                ..Default::default()
            },
            host::Instruction {
                resize: Some(host::ResizeMessage {
                    width: Some(80),
                    height: Some(24),
                }),
                ..Default::default()
            },
            host::Instruction {
                echoack: Some(host::EchoAck {
                    echo_ack_num: Some(4321),
                }),
                ..Default::default()
            },
        ],
    };

    let text = protoc_decode(
        &dir,
        "hostinput.proto",
        "HostBuffers.HostMessage",
        &msg.encode_to_vec(),
    );

    // protoc renders extension fields in brackets, which is how we know it matched the
    // upstream extension declarations rather than some unknown field.
    assert!(text.contains("[HostBuffers.hostbytes]"), "{text}");
    assert!(text.contains("hoststring: \"hello\""), "{text}");
    assert!(text.contains("[HostBuffers.resize]"), "{text}");
    assert!(text.contains("width: 80"), "{text}");
    assert!(text.contains("height: 24"), "{text}");
    assert!(text.contains("[HostBuffers.echoack]"), "{text}");
    assert!(text.contains("echo_ack_num: 4321"), "{text}");
}

#[test]
fn user_extensions_decode_upstream() {
    let Some(dir) = setup() else { return };

    let msg = user::UserMessage {
        instruction: vec![
            user::Instruction {
                keystroke: Some(user::Keystroke {
                    keys: Some(b"abc".to_vec()),
                }),
                resize: None,
            },
            user::Instruction {
                keystroke: None,
                resize: Some(user::ResizeMessage {
                    width: Some(132),
                    height: Some(43),
                }),
            },
        ],
    };

    let text = protoc_decode(
        &dir,
        "userinput.proto",
        "ClientBuffers.UserMessage",
        &msg.encode_to_vec(),
    );

    assert!(text.contains("[ClientBuffers.keystroke]"), "{text}");
    assert!(text.contains("keys: \"abc\""), "{text}");
    assert!(text.contains("[ClientBuffers.resize]"), "{text}");
    assert!(text.contains("width: 132"), "{text}");
}

#[test]
fn transport_instruction_decodes_upstream() {
    let Some(dir) = setup() else { return };

    let inst = transport::Instruction {
        protocol_version: Some(2),
        old_num: Some(10),
        new_num: Some(11),
        ack_num: Some(9),
        throwaway_num: Some(3),
        diff: Some(b"\x00\x01\x02".to_vec()),
        chaff: Some(vec![0xaa; 8]),
    };

    let text = protoc_decode(
        &dir,
        "transportinstruction.proto",
        "TransportBuffers.Instruction",
        &inst.encode_to_vec(),
    );

    assert!(text.contains("protocol_version: 2"), "{text}");
    assert!(text.contains("old_num: 10"), "{text}");
    assert!(text.contains("new_num: 11"), "{text}");
    assert!(text.contains("ack_num: 9"), "{text}");
    assert!(text.contains("throwaway_num: 3"), "{text}");
}

#[test]
fn empty_diff_is_distinguishable_from_absent_diff() {
    let Some(dir) = setup() else { return };

    // The transport relies on proto2's presence semantics: an empty diff means "nothing
    // changed", which is not the same as no diff field at all.
    let with_empty = transport::Instruction {
        diff: Some(Vec::new()),
        ..Default::default()
    };
    let text = protoc_decode(
        &dir,
        "transportinstruction.proto",
        "TransportBuffers.Instruction",
        &with_empty.encode_to_vec(),
    );
    assert!(text.contains("diff: \"\""), "empty diff was dropped: {text}");

    let without = transport::Instruction::default();
    assert!(without.encode_to_vec().is_empty());
}
