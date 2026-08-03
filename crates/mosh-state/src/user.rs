//! The client's state: what the user has typed.
//!
//! Transliterated from `third_party/mosh/src/statesync/user.cc`.

use mosh_protobufs::{user as pb, Message};

/// One thing the user did.
///
/// The C++ carries both a keystroke and a resize in every event with a tag saying which
/// is live; an enum says the same thing without the dead field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEvent {
    Byte(u8),
    Resize { width: i32, height: i32 },
}

/// The ordered stream of everything the user has done.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserStream {
    actions: std::collections::VecDeque<UserEvent>,
}

impl UserStream {
    pub fn new() -> Self {
        UserStream::default()
    }

    pub fn push_byte(&mut self, c: u8) {
        self.actions.push_back(UserEvent::Byte(c));
    }

    pub fn push_resize(&mut self, width: i32, height: i32) {
        self.actions.push_back(UserEvent::Resize { width, height });
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn get(&self, i: usize) -> Option<UserEvent> {
        self.actions.get(i).copied()
    }

    /// Drop the events the peer has already acknowledged.
    pub fn subtract(&mut self, prefix: &UserStream) {
        // Subtracting a stream from itself clears it; the C++ checks pointer identity
        // for this, which we cannot do through a shared reference, so compare instead.
        if std::ptr::eq(self, prefix) {
            self.actions.clear();
            return;
        }
        for event in &prefix.actions {
            debug_assert_eq!(Some(event), self.actions.front());
            self.actions.pop_front();
        }
    }

    /// Encode everything in this stream that `existing` does not already have.
    pub fn diff_from(&self, existing: &UserStream) -> Vec<u8> {
        // `existing` is always a prefix of us; skip past it.
        let mut rest = self.actions.iter().skip(existing.actions.len());

        let mut output = pb::UserMessage::default();

        for event in &mut rest {
            match *event {
                UserEvent::Byte(c) => {
                    // Run consecutive keystrokes together into one instruction.
                    let appended = output
                        .instruction
                        .last_mut()
                        .and_then(|i| i.keystroke.as_mut())
                        .map(|k| {
                            k.keys.get_or_insert_with(Vec::new).push(c);
                        })
                        .is_some();
                    if !appended {
                        output.instruction.push(pb::Instruction {
                            keystroke: Some(pb::Keystroke {
                                keys: Some(vec![c]),
                            }),
                            resize: None,
                        });
                    }
                }
                UserEvent::Resize { width, height } => {
                    output.instruction.push(pb::Instruction {
                        keystroke: None,
                        resize: Some(pb::ResizeMessage {
                            width: Some(width),
                            height: Some(height),
                        }),
                    });
                }
            }
        }

        output.encode_to_vec()
    }

    pub fn init_diff(&self) -> Vec<u8> {
        self.diff_from(&UserStream::new())
    }

    /// Apply a diff received from the peer.
    pub fn apply_string(&mut self, diff: &[u8]) -> Result<(), ApplyError> {
        let input = pb::UserMessage::decode(diff).map_err(|_| ApplyError::Malformed)?;

        for inst in &input.instruction {
            if let Some(keystroke) = &inst.keystroke {
                for &b in keystroke.keys.as_deref().unwrap_or(&[]) {
                    self.actions.push_back(UserEvent::Byte(b));
                }
            } else if let Some(resize) = &inst.resize {
                self.actions.push_back(UserEvent::Resize {
                    width: resize.width.unwrap_or(-1),
                    height: resize.height.unwrap_or(-1),
                });
            }
        }
        Ok(())
    }
}

/// A diff that could not be applied.
///
/// The C++ calls `fatal_assert` here and dies. A peer can only produce a malformed diff
/// by being broken or hostile, and neither is a reason to abort the process, so this
/// returns instead and lets the caller drop the datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyError {
    Malformed,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("could not parse a state diff")
    }
}

impl std::error::Error for ApplyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_diff_encodes_to_nothing() {
        let s = UserStream::new();
        assert!(s.diff_from(&UserStream::new()).is_empty());
    }

    #[test]
    fn keystrokes_round_trip() {
        let mut s = UserStream::new();
        for c in b"hello" {
            s.push_byte(*c);
        }
        let diff = s.init_diff();

        let mut applied = UserStream::new();
        applied.apply_string(&diff).unwrap();
        assert_eq!(applied, s);
    }

    #[test]
    fn consecutive_keystrokes_share_one_instruction() {
        let mut s = UserStream::new();
        for c in b"abc" {
            s.push_byte(*c);
        }
        let diff = s.init_diff();
        let decoded = pb::UserMessage::decode(&diff[..]).unwrap();
        assert_eq!(decoded.instruction.len(), 1, "keystrokes were not coalesced");
        assert_eq!(
            decoded.instruction[0].keystroke.as_ref().unwrap().keys,
            Some(b"abc".to_vec())
        );
    }

    #[test]
    fn a_resize_breaks_the_keystroke_run() {
        let mut s = UserStream::new();
        s.push_byte(b'a');
        s.push_resize(80, 24);
        s.push_byte(b'b');

        let decoded = pb::UserMessage::decode(&s.init_diff()[..]).unwrap();
        assert_eq!(decoded.instruction.len(), 3);
        assert!(decoded.instruction[1].resize.is_some());
    }

    #[test]
    fn resize_round_trips() {
        let mut s = UserStream::new();
        s.push_resize(132, 43);
        let mut applied = UserStream::new();
        applied.apply_string(&s.init_diff()).unwrap();
        assert_eq!(applied.get(0), Some(UserEvent::Resize { width: 132, height: 43 }));
    }

    #[test]
    fn a_diff_carries_only_what_is_new() {
        let mut old = UserStream::new();
        old.push_byte(b'a');

        let mut new = old.clone();
        new.push_byte(b'b');

        let diff = new.diff_from(&old);
        let decoded = pb::UserMessage::decode(&diff[..]).unwrap();
        assert_eq!(
            decoded.instruction[0].keystroke.as_ref().unwrap().keys,
            Some(b"b".to_vec()),
            "the diff repeated an event the peer already had"
        );
    }

    #[test]
    fn applying_a_diff_reaches_the_same_state() {
        let mut old = UserStream::new();
        old.push_byte(b'a');
        let mut new = old.clone();
        new.push_byte(b'b');
        new.push_resize(10, 20);

        let diff = new.diff_from(&old);
        let mut target = old.clone();
        target.apply_string(&diff).unwrap();
        assert_eq!(target, new);
    }

    #[test]
    fn subtract_drops_the_acknowledged_prefix() {
        let mut s = UserStream::new();
        for c in b"abcd" {
            s.push_byte(*c);
        }
        let mut prefix = UserStream::new();
        prefix.push_byte(b'a');
        prefix.push_byte(b'b');

        s.subtract(&prefix);
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(0), Some(UserEvent::Byte(b'c')));
    }

    #[test]
    fn a_malformed_diff_is_reported_not_fatal() {
        let mut s = UserStream::new();
        // Field 1 declared as a length-delimited submessage, with a length that runs off
        // the end of the buffer.
        assert_eq!(s.apply_string(&[0x0a, 0x7f]), Err(ApplyError::Malformed));
    }

    #[test]
    fn applying_an_empty_diff_changes_nothing() {
        let mut s = UserStream::new();
        s.push_byte(b'x');
        let before = s.clone();
        s.apply_string(&[]).unwrap();
        assert_eq!(s, before);
    }
}
