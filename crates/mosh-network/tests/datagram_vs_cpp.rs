//! Proves a datagram we build is readable by the C++ implementation.
//!
//! This is the substance of the backwards-compatibility requirement: a Rust endpoint
//! encrypts, frames, fragments and compresses exactly as a C++ endpoint expects, all the
//! way down. The harness decrypts with the C++ crypto, parses the C++ Packet, unwraps the
//! C++ Fragment and reassembles the C++ Instruction, printing each layer's fields.
//!
//! Skips unless the upstream submodule has been built; see `tests/cpp/build.sh`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mosh_crypto::{Base64Key, Session};
use mosh_network::fragment::Fragmenter;
use mosh_network::packet::{Direction, Packet};
use mosh_protobufs::transport::Instruction;

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
    let libs = [
        "src/network/libmoshnetwork.a",
        "src/crypto/libmoshcrypto.a",
        "src/protobufs/libmoshprotos.a",
        "src/util/libmoshutil.a",
    ];
    let source = root.join("tests/cpp/datagram-dump.cc");
    if !source.exists() || libs.iter().any(|l| !upstream.join(l).exists()) {
        return None;
    }

    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("datagram-dump-cpp");
    let mut cmd = Command::new("g++");
    // Modern protobuf headers require C++17.
    cmd.arg("-O2").arg("-std=c++17").arg("-I").arg(&upstream).arg("-o").arg(&out).arg(&source);
    for l in libs {
        cmd.arg(upstream.join(l));
    }
    // Upstream's generated headers include protobuf's own, which are not always where
    // the compiler looks by default.
    for package in ["protobuf-lite", "libcrypto"] {
        cmd.args(pkg_config("--cflags", package));
        cmd.args(pkg_config("--libs", package));
    }
    cmd.arg("-lz");

    cmd.status().ok()?.success().then_some(out)
}

fn pkg_config(flag: &str, package: &str) -> Vec<String> {
    let fallback = || vec![format!("-l{}", package.trim_start_matches("lib"))];
    let Some(out) = Command::new("pkg-config").arg(flag).arg(package).output().ok() else {
        return if flag == "--libs" { fallback() } else { Vec::new() };
    };
    if !out.status.success() {
        return if flag == "--libs" { fallback() } else { Vec::new() };
    }
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_string).collect()
}

/// Run the harness and parse its "key value" output into a lookup.
fn dump(tool: &Path, key: &str, datagram: &[u8]) -> std::collections::HashMap<String, String> {
    let mut child = Command::new(tool)
        .arg(key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn datagram-dump");
    child.stdin.as_mut().unwrap().write_all(datagram).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "C++ rejected our datagram: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Build one datagram the way the transport does, bottom to top.
fn build_datagram(
    session: &mut Session,
    fragmenter: &mut Fragmenter,
    inst: &Instruction,
    seq: u64,
    direction: Direction,
    timestamp: u16,
    timestamp_reply: u16,
) -> Vec<Vec<u8>> {
    fragmenter
        .make_fragments(inst, 500)
        .expect("fragmenting failed")
        .into_iter()
        .map(|frag| {
            let packet = Packet::new(
                seq,
                direction,
                timestamp,
                timestamp_reply,
                frag.to_bytes().expect("fragment header"),
            );
            session.encrypt(&packet.to_message()).expect("encrypt")
        })
        .collect()
}

fn instruction(diff: &[u8]) -> Instruction {
    Instruction {
        protocol_version: Some(mosh_network::packet::MOSH_PROTOCOL_VERSION),
        old_num: Some(11),
        new_num: Some(12),
        ack_num: Some(9),
        throwaway_num: Some(3),
        diff: Some(diff.to_vec()),
        chaff: Some(Vec::new()),
    }
}

#[test]
fn the_cpp_reads_every_layer_of_our_datagram() {
    let Some(cpp) = cpp_harness() else {
        eprintln!("skipping: C++ libraries not built");
        return;
    };

    let key = Base64Key::random();
    let printable = key.printable();
    let mut session = Session::new(&key);
    let mut fragmenter = Fragmenter::new();

    let inst = instruction(b"hello diff");
    let datagrams = build_datagram(
        &mut session,
        &mut fragmenter,
        &inst,
        0x0123_4567,
        Direction::ToClient,
        0xbeef,
        0xcafe,
    );
    assert_eq!(datagrams.len(), 1, "small instruction should be one datagram");

    let fields = dump(&cpp, &printable, &datagrams[0]);

    // Transport layer.
    assert_eq!(fields["seq"], "19088743"); // 0x01234567
    assert_eq!(fields["direction"], "1", "direction bit lost");
    assert_eq!(fields["timestamp"], "48879"); // 0xbeef
    assert_eq!(fields["timestamp_reply"], "51966"); // 0xcafe

    // Fragment layer.
    assert_eq!(fields["frag_num"], "0");
    assert_eq!(fields["frag_final"], "1");

    // Instruction layer, after decompression and protobuf parsing.
    assert_eq!(fields["protocol_version"], "2");
    assert_eq!(fields["old_num"], "11");
    assert_eq!(fields["new_num"], "12");
    assert_eq!(fields["ack_num"], "9");
    assert_eq!(fields["throwaway_num"], "3");
    assert_eq!(fields["diff"], "hello diff");
}

#[test]
fn direction_to_server_is_read_correctly() {
    let Some(cpp) = cpp_harness() else { return };

    let key = Base64Key::random();
    let mut session = Session::new(&key);
    let mut fragmenter = Fragmenter::new();

    let datagrams = build_datagram(
        &mut session,
        &mut fragmenter,
        &instruction(b"up"),
        1,
        Direction::ToServer,
        1,
        2,
    );
    let fields = dump(&cpp, &key.printable(), &datagrams[0]);
    assert_eq!(fields["direction"], "0");
}

#[test]
fn a_high_sequence_number_does_not_leak_into_the_direction_bit() {
    let Some(cpp) = cpp_harness() else { return };

    let key = Base64Key::random();
    let mut session = Session::new(&key);
    let mut fragmenter = Fragmenter::new();

    // Every bit below the direction bit set.
    let seq = (1u64 << 63) - 1;
    let datagrams = build_datagram(
        &mut session,
        &mut fragmenter,
        &instruction(b"x"),
        seq,
        Direction::ToServer,
        0,
        0,
    );
    let fields = dump(&cpp, &key.printable(), &datagrams[0]);
    assert_eq!(fields["direction"], "0", "sequence number set the direction bit");
    assert_eq!(fields["seq"], seq.to_string());
}

#[test]
fn a_fragmented_instruction_is_read_fragment_by_fragment() {
    let Some(cpp) = cpp_harness() else { return };

    let key = Base64Key::random();
    let mut session = Session::new(&key);
    let mut fragmenter = Fragmenter::new();

    // Incompressible enough to need several fragments.
    let big: Vec<u8> = (0..60_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    let datagrams = build_datagram(
        &mut session,
        &mut fragmenter,
        &instruction(&big),
        42,
        Direction::ToClient,
        7,
        8,
    );
    assert!(datagrams.len() > 1, "instruction was not fragmented");

    let mut ids = Vec::new();
    for (i, datagram) in datagrams.iter().enumerate() {
        let fields = dump(&cpp, &key.printable(), datagram);
        assert_eq!(fields["frag_num"], i.to_string(), "fragment numbering differs");
        let is_last = i == datagrams.len() - 1;
        assert_eq!(
            fields["frag_final"],
            if is_last { "1" } else { "0" },
            "final marker differs on fragment {i}"
        );
        ids.push(fields["frag_id"].clone());
    }
    // Every fragment of one instruction shares an id, which is how the peer groups them.
    assert!(ids.windows(2).all(|w| w[0] == w[1]), "fragment ids differ: {ids:?}");
}

#[test]
fn an_empty_diff_survives_the_round_trip() {
    let Some(cpp) = cpp_harness() else { return };

    let key = Base64Key::random();
    let mut session = Session::new(&key);
    let mut fragmenter = Fragmenter::new();

    let datagrams = build_datagram(
        &mut session,
        &mut fragmenter,
        &instruction(b""),
        3,
        Direction::ToClient,
        0,
        0,
    );
    let fields = dump(&cpp, &key.printable(), &datagrams[0]);
    assert_eq!(fields["diff"], "");
    assert_eq!(fields["new_num"], "12");
}
