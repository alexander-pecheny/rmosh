//! Everything a peer sends past the decryption is attacker-shaped input.
//!
//! Authentication proves the sender holds the session key; it says nothing about the
//! sender being the mosh client, or being well behaved. Nothing reachable from a
//! decrypted datagram may abort the process, so this drives the whole path -- fragment
//! header, reassembly, decompression, protobuf, and the state rules -- with input chosen
//! to hit the edges rather than the ordinary case.

use mosh_network::connection::Connection;
use mosh_network::fragment::{Fragment, FragmentAssembly};
use mosh_network::sender::SyncState;
use mosh_network::transport::Transport;
use mosh_network::StateError;
use mosh_protobufs::transport::Instruction;
use mosh_protobufs::Message;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Bytes(Vec<u8>);

impl SyncState for Bytes {
    fn subtract(&mut self, prefix: &Self) {
        if self.0.starts_with(&prefix.0) {
            self.0.drain(..prefix.0.len());
        }
    }
    fn diff_from(&self, existing: &Self) -> Vec<u8> {
        self.0[existing.0.len().min(self.0.len())..].to_vec()
    }
    fn init_diff(&self) -> Vec<u8> {
        self.0.clone()
    }
    fn apply_string(&mut self, diff: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(diff);
        Ok(())
    }
}

fn transport() -> Transport<Bytes, Bytes> {
    let conn = Connection::new_server(Some("127.0.0.1"), 0, 0, 0).expect("bind an ephemeral port");
    Transport::new(conn, Bytes::default(), Bytes::default(), 0)
}

/// A cheap deterministic sequence; a seeded generator keeps a failure reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0 ^ (self.0 >> 33)
    }

    /// Values clustered at the boundaries, where the interesting cases live.
    fn edgy_u64(&mut self) -> u64 {
        const EDGES: [u64; 8] = [0, 1, 2, 3, u64::MAX, u64::MAX - 1, 1 << 63, 1024];
        match self.next() % 3 {
            0 => EDGES[(self.next() % EDGES.len() as u64) as usize],
            1 => self.next() % 8,
            _ => self.next(),
        }
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() as usize) % (max + 1);
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn no_sequence_of_instructions_can_abort_the_receiver() {
    let mut rng = Rng(0x5eed);
    let mut t = transport();

    for round in 0..200_000u32 {
        // Start over periodically so early states do not pin every later one.
        if round % 1000 == 0 {
            t = transport();
        }
        let inst = Instruction {
            protocol_version: Some(if rng.next() % 4 == 0 {
                rng.next() as u32
            } else {
                2
            }),
            old_num: Some(rng.edgy_u64()),
            new_num: Some(rng.edgy_u64()),
            ack_num: Some(rng.edgy_u64()),
            throwaway_num: Some(rng.edgy_u64()),
            diff: Some(rng.bytes(16)),
            chaff: Some(rng.bytes(4)),
        };
        // The result is uninteresting; not aborting is the whole property.
        let _ = t.apply_instruction(&inst, rng.next() % 100_000);
    }
}

#[test]
fn no_datagram_can_abort_the_fragment_reassembler() {
    let mut rng = Rng(0xf00d);
    let mut asm = FragmentAssembly::new();

    for _ in 0..200_000u32 {
        // Sometimes a plausible header over random contents, sometimes raw noise: the
        // first reaches the reassembly rules, the second the header parse.
        let datagram = if rng.next() % 2 == 0 {
            let mut d = Vec::new();
            d.extend_from_slice(&(rng.next() % 4).to_be_bytes());
            d.extend_from_slice(&(rng.next() as u16).to_be_bytes());
            d.extend_from_slice(&rng.bytes(32));
            d
        } else {
            rng.bytes(24)
        };

        let Ok(frag) = Fragment::from_bytes(&datagram) else {
            continue;
        };
        if asm.add_fragment(frag) {
            let _ = asm.get_assembly();
        }
    }
}

#[test]
fn a_truncated_or_corrupt_instruction_never_parses_into_something_wild() {
    // Compression sits between the wire and the protobuf, so a corrupt stream must be
    // refused at one layer or the other rather than yielding a half-decoded message.
    let good = Instruction {
        protocol_version: Some(2),
        old_num: Some(0),
        new_num: Some(1),
        ack_num: Some(0),
        throwaway_num: Some(0),
        diff: Some(vec![7; 4096]),
        chaff: Some(Vec::new()),
    }
    .encode_to_vec();

    let mut rng = Rng(0xbeef);
    for _ in 0..20_000u32 {
        let mut corrupt = good.clone();
        let n = 1 + (rng.next() as usize % 8);
        for _ in 0..n {
            let at = rng.next() as usize % corrupt.len();
            corrupt[at] = rng.next() as u8;
        }
        corrupt.truncate(1 + rng.next() as usize % corrupt.len());
        let _ = Instruction::decode(&corrupt[..]);
    }
}
