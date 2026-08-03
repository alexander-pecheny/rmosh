//! Splitting an instruction across datagrams, and putting it back together.
//!
//! Transliterated from `third_party/mosh/src/network/transportfragment.cc`.
//!
//! The fragment header is on the wire, so its layout is contractual: an 8-byte
//! instruction id and a 2-byte fragment number, both big-endian, with the top bit of the
//! fragment number marking the last fragment.

use mosh_protobufs::{transport::Instruction, Message};

use crate::compressor;

/// Bytes of header in front of every fragment.
pub const FRAG_HEADER_LEN: usize = 10;

/// The top bit of the fragment number marks the final fragment, which leaves 15 bits of
/// fragment number and so an effective limit on how large one instruction may be.
const FINAL_BIT: u16 = 0x8000;
const FRAGMENT_NUM_MASK: u16 = 0x7fff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub id: u64,
    pub fragment_num: u16,
    pub final_fragment: bool,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentError {
    /// Shorter than the header.
    TooShort,
    /// More fragments than the numbering allows.
    TooManyFragments,
    /// The reassembled payload was not a valid instruction.
    Malformed,
}

impl std::fmt::Display for FragmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FragmentError::TooShort => f.write_str("datagram is shorter than a fragment header"),
            FragmentError::TooManyFragments => f.write_str("instruction needs too many fragments"),
            FragmentError::Malformed => f.write_str("could not parse reassembled instruction"),
        }
    }
}

impl std::error::Error for FragmentError {}

impl Fragment {
    pub fn new(id: u64, fragment_num: u16, final_fragment: bool, contents: Vec<u8>) -> Self {
        Fragment {
            id,
            fragment_num,
            final_fragment,
            contents,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, FragmentError> {
        // The fragment number must leave the top bit free for the final marker.
        if self.fragment_num & FINAL_BIT != 0 {
            return Err(FragmentError::TooManyFragments);
        }
        let combined = if self.final_fragment {
            FINAL_BIT | self.fragment_num
        } else {
            self.fragment_num
        };

        let mut out = Vec::with_capacity(FRAG_HEADER_LEN + self.contents.len());
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&combined.to_be_bytes());
        debug_assert_eq!(out.len(), FRAG_HEADER_LEN);
        out.extend_from_slice(&self.contents);
        Ok(out)
    }

    pub fn from_bytes(x: &[u8]) -> Result<Self, FragmentError> {
        if x.len() < FRAG_HEADER_LEN {
            return Err(FragmentError::TooShort);
        }
        let id = u64::from_be_bytes(x[0..8].try_into().expect("8 bytes"));
        let combined = u16::from_be_bytes(x[8..10].try_into().expect("2 bytes"));
        Ok(Fragment {
            id,
            fragment_num: combined & FRAGMENT_NUM_MASK,
            final_fragment: combined & FINAL_BIT != 0,
            contents: x[FRAG_HEADER_LEN..].to_vec(),
        })
    }
}

/// Collects the fragments of one instruction until it is complete.
#[derive(Debug, Default)]
pub struct FragmentAssembly {
    fragments: Vec<Option<Fragment>>,
    fragments_arrived: usize,
    /// None until the final fragment tells us how many there are.
    fragments_total: Option<usize>,
    current_id: Option<u64>,
}

impl FragmentAssembly {
    pub fn new() -> Self {
        FragmentAssembly::default()
    }

    /// Add a fragment. Returns whether the instruction is now complete.
    pub fn add_fragment(&mut self, frag: Fragment) -> bool {
        let index = frag.fragment_num as usize;

        if self.current_id != Some(frag.id) {
            // A fragment of a different instruction supersedes whatever we were
            // collecting; the old one can never complete now.
            self.fragments.clear();
            self.fragments.resize(index + 1, None);
            self.fragments_arrived = 1;
            self.fragments_total = None;
            self.current_id = Some(frag.id);
            let final_fragment = frag.final_fragment;
            self.fragments[index] = Some(frag);
            if final_fragment {
                self.fragments_total = Some(index + 1);
                self.fragments.truncate(index + 1);
            }
        } else {
            if self.fragments.len() <= index {
                self.fragments.resize(index + 1, None);
            }
            // A duplicate is ignored rather than asserted on: the network is allowed to
            // deliver the same datagram twice.
            if self.fragments[index].is_none() {
                let final_fragment = frag.final_fragment;
                self.fragments[index] = Some(frag);
                self.fragments_arrived += 1;
                if final_fragment {
                    self.fragments_total = Some(index + 1);
                    self.fragments.truncate(index + 1);
                }
            }
        }

        Some(self.fragments_arrived) == self.fragments_total
    }

    /// Reassemble, decompress and parse the completed instruction.
    pub fn get_assembly(&mut self) -> Result<Instruction, FragmentError> {
        let mut encoded = Vec::new();
        for frag in &self.fragments {
            match frag {
                Some(f) => encoded.extend_from_slice(&f.contents),
                None => return Err(FragmentError::Malformed),
            }
        }

        let plain = compressor::uncompress(&encoded).map_err(|_| FragmentError::Malformed)?;
        let inst = Instruction::decode(&plain[..]).map_err(|_| FragmentError::Malformed)?;

        self.fragments.clear();
        self.fragments_arrived = 0;
        self.fragments_total = None;

        Ok(inst)
    }
}

/// Splits instructions into fragments, giving each instruction an id.
#[derive(Debug, Default)]
pub struct Fragmenter {
    next_instruction_id: u64,
    last_instruction: Option<Instruction>,
    last_mtu: Option<usize>,
}

impl Fragmenter {
    pub fn new() -> Self {
        Fragmenter::default()
    }

    pub fn last_ack_sent(&self) -> u64 {
        self.last_instruction
            .as_ref()
            .and_then(|i| i.ack_num)
            .unwrap_or(0)
    }

    pub fn make_fragments(
        &mut self,
        inst: &Instruction,
        mtu: usize,
    ) -> Result<Vec<Fragment>, FragmentError> {
        let mtu = mtu.saturating_sub(FRAG_HEADER_LEN).max(1);

        // A new id whenever anything but the diff changed, so the receiver can tell one
        // instruction's fragments from another's.
        let changed = match &self.last_instruction {
            None => true,
            Some(last) => {
                inst.old_num != last.old_num
                    || inst.new_num != last.new_num
                    || inst.ack_num != last.ack_num
                    || inst.throwaway_num != last.throwaway_num
                    || inst.chaff != last.chaff
                    || inst.protocol_version != last.protocol_version
                    || self.last_mtu != Some(mtu)
            }
        };
        if changed {
            self.next_instruction_id += 1;
        }

        self.last_instruction = Some(inst.clone());
        self.last_mtu = Some(mtu);

        let serialized = inst.encode_to_vec();
        let payload = compressor::compress(&serialized).map_err(|_| FragmentError::Malformed)?;

        let mut ret = Vec::new();
        let mut fragment_num: u16 = 0;
        let mut rest = &payload[..];

        loop {
            let take = rest.len().min(mtu);
            let final_fragment = take == rest.len();
            if fragment_num & FINAL_BIT != 0 {
                return Err(FragmentError::TooManyFragments);
            }
            ret.push(Fragment::new(
                self.next_instruction_id,
                fragment_num,
                final_fragment,
                rest[..take].to_vec(),
            ));
            rest = &rest[take..];
            fragment_num += 1;
            if final_fragment {
                break;
            }
        }

        Ok(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(diff: &[u8]) -> Instruction {
        Instruction {
            protocol_version: Some(2),
            old_num: Some(1),
            new_num: Some(2),
            ack_num: Some(0),
            throwaway_num: Some(0),
            diff: Some(diff.to_vec()),
            chaff: Some(Vec::new()),
        }
    }

    #[test]
    fn header_layout_is_ten_bytes_big_endian() {
        let f = Fragment::new(0x0102_0304_0506_0708, 3, true, b"body".to_vec());
        let bytes = f.to_bytes().unwrap();
        assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        // Final bit set, fragment number 3.
        assert_eq!(&bytes[8..10], &[0x80, 0x03]);
        assert_eq!(&bytes[10..], b"body");
    }

    #[test]
    fn a_fragment_round_trips_through_its_wire_form() {
        for (num, is_final) in [(0u16, false), (1, true), (0x7fff, false), (0x7fff, true)] {
            let f = Fragment::new(42, num, is_final, b"contents".to_vec());
            let back = Fragment::from_bytes(&f.to_bytes().unwrap()).unwrap();
            assert_eq!(back, f);
        }
    }

    #[test]
    fn a_short_datagram_is_rejected() {
        assert_eq!(
            Fragment::from_bytes(&[0; FRAG_HEADER_LEN - 1]),
            Err(FragmentError::TooShort)
        );
        // Exactly a header with no body is legitimate.
        assert!(Fragment::from_bytes(&[0; FRAG_HEADER_LEN]).is_ok());
    }

    #[test]
    fn a_small_instruction_makes_one_final_fragment() {
        let mut f = Fragmenter::new();
        let frags = f.make_fragments(&instruction(b"hi"), 500).unwrap();
        assert_eq!(frags.len(), 1);
        assert!(frags[0].final_fragment);
        assert_eq!(frags[0].fragment_num, 0);
    }

    #[test]
    fn a_large_instruction_is_split_and_reassembled() {
        let mut f = Fragmenter::new();
        // Random-ish content so it does not simply compress away.
        let big: Vec<u8> = (0..40_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let inst = instruction(&big);
        let frags = f.make_fragments(&inst, 500).unwrap();
        assert!(frags.len() > 1, "large instruction was not fragmented");
        assert!(frags.last().unwrap().final_fragment);
        assert!(frags[..frags.len() - 1].iter().all(|f| !f.final_fragment));

        let mut asm = FragmentAssembly::new();
        let mut complete = false;
        for frag in frags {
            complete = asm.add_fragment(frag);
        }
        assert!(complete);
        assert_eq!(asm.get_assembly().unwrap(), inst);
    }

    #[test]
    fn fragments_may_arrive_out_of_order() {
        let mut f = Fragmenter::new();
        let big: Vec<u8> = (0..40_000u32)
            .map(|i| (i.wrapping_mul(40503) >> 8) as u8)
            .collect();
        let inst = instruction(&big);
        let mut frags = f.make_fragments(&inst, 500).unwrap();
        frags.reverse();

        let mut asm = FragmentAssembly::new();
        let mut complete = false;
        for frag in frags {
            complete = asm.add_fragment(frag);
        }
        assert!(complete, "reversed fragments never completed");
        assert_eq!(asm.get_assembly().unwrap(), inst);
    }

    #[test]
    fn a_duplicated_fragment_is_ignored() {
        let mut f = Fragmenter::new();
        let big: Vec<u8> = (0..30_000u32)
            .map(|i| (i.wrapping_mul(2246822519) >> 16) as u8)
            .collect();
        let inst = instruction(&big);
        let frags = f.make_fragments(&inst, 500).unwrap();
        assert!(frags.len() > 2);

        let mut asm = FragmentAssembly::new();
        let mut complete = false;
        for frag in &frags {
            // Deliver every fragment twice; the network is allowed to do this.
            asm.add_fragment(frag.clone());
            complete = asm.add_fragment(frag.clone());
        }
        assert!(complete);
        assert_eq!(asm.get_assembly().unwrap(), inst);
    }

    #[test]
    fn a_new_instruction_supersedes_a_partial_one() {
        let mut f = Fragmenter::new();
        let big: Vec<u8> = (0..30_000u32)
            .map(|i| (i.wrapping_mul(7919) >> 8) as u8)
            .collect();
        let first = f.make_fragments(&instruction(&big), 500).unwrap();

        let mut asm = FragmentAssembly::new();
        asm.add_fragment(first[0].clone()); // partial

        // A different instruction arrives; the incomplete one can never finish.
        let second_inst = Instruction {
            new_num: Some(99),
            ..instruction(b"small")
        };
        let second = f.make_fragments(&second_inst, 500).unwrap();
        assert_eq!(second.len(), 1);
        assert!(asm.add_fragment(second[0].clone()));
        assert_eq!(asm.get_assembly().unwrap(), second_inst);
    }

    #[test]
    fn instruction_ids_advance_only_when_something_changed() {
        let mut f = Fragmenter::new();
        let inst = instruction(b"hi");
        let a = f.make_fragments(&inst, 500).unwrap()[0].id;
        // Resending the identical instruction must reuse the id, so the receiver treats
        // it as the same instruction rather than a new one.
        let b = f.make_fragments(&inst, 500).unwrap()[0].id;
        assert_eq!(a, b);

        let changed = Instruction {
            new_num: Some(3),
            ..inst
        };
        let c = f.make_fragments(&changed, 500).unwrap()[0].id;
        assert_ne!(b, c);
    }

    #[test]
    fn changing_the_mtu_starts_a_new_instruction() {
        let mut f = Fragmenter::new();
        let inst = instruction(b"hi");
        let a = f.make_fragments(&inst, 500).unwrap()[0].id;
        // Fragment boundaries move, so the receiver must not mix the two.
        let b = f.make_fragments(&inst, 300).unwrap()[0].id;
        assert_ne!(a, b);
    }

    #[test]
    fn a_corrupt_payload_is_reported_not_fatal() {
        let mut asm = FragmentAssembly::new();
        asm.add_fragment(Fragment::new(1, 0, true, b"not a zlib stream".to_vec()));
        assert_eq!(asm.get_assembly(), Err(FragmentError::Malformed));
    }
}
