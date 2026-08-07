//! The server's state: the screen, plus how much of the user's typing it has echoed.
//!
//! Transliterated from `third_party/mosh/src/statesync/completeterminal.cc`.

use mosh_protobufs::{host as pb, Message};
use mosh_terminal::display::Display;
use mosh_terminal::emulator::Emulator;
use mosh_terminal::framebuffer::Framebuffer;

use crate::user::ApplyError;

/// How long to wait before acknowledging a frame of input, so that an echo the client
/// already predicted is not confirmed twice.
const ECHO_TIMEOUT: u64 = 50;

/// A complete screen state, as one endpoint believes it to be.
pub struct Complete {
    terminal: Emulator,
    display: Display,
    /// Frame numbers paired with the time they arrived, oldest first.
    input_history: std::collections::VecDeque<(u64, u64)>,
    echo_ack: u64,
}

impl Clone for Complete {
    fn clone(&self) -> Self {
        Complete {
            terminal: self.terminal.clone(),
            // Display holds only terminal capabilities, which are the same for every
            // Complete on this side; rebuilding avoids making Display cloneable.
            display: Display::new(false).expect("no environment needed"),
            input_history: self.input_history.clone(),
            echo_ack: self.echo_ack,
        }
    }
}

impl Complete {
    pub fn new(width: i32, height: i32) -> Self {
        Complete {
            terminal: Emulator::new(width, height),
            display: Display::new(false).expect("no environment needed"),
            input_history: std::collections::VecDeque::new(),
            echo_ack: 0,
        }
    }

    /// Feed output from the host, returning whatever the terminal owes it in reply.
    pub fn act(&mut self, bytes: &[u8]) -> String {
        self.terminal.input(bytes);
        self.terminal.read_octets_to_host()
    }

    /// Forget the colour queries, which the client has by now been told about.
    ///
    /// They accumulate until then: an application writes more than one batch of output
    /// before we transmit, and dropping the earlier batch's queries would lose them.
    pub fn clear_color_queries(&mut self) {
        self.terminal.clear_color_queries();
    }

    /// Apply a resize.
    pub fn act_resize(&mut self, width: i32, height: i32) -> String {
        self.terminal.resize(width, height);
        self.terminal.read_octets_to_host()
    }

    pub fn fb(&self) -> &Framebuffer {
        self.terminal.fb()
    }

    pub fn echo_ack(&self) -> u64 {
        self.echo_ack
    }

    /// Encode the change from `existing` to us.
    pub fn diff_from(&self, existing: &Complete) -> Vec<u8> {
        let mut output = pb::HostMessage::default();

        if existing.echo_ack != self.echo_ack {
            debug_assert!(self.echo_ack >= existing.echo_ack);
            output.instruction.push(pb::Instruction {
                echoack: Some(pb::EchoAck {
                    echo_ack_num: Some(self.echo_ack),
                }),
                ..Default::default()
            });
        }

        if existing.fb() != self.fb() {
            if existing.fb().ds.width() != self.fb().ds.width()
                || existing.fb().ds.height() != self.fb().ds.height()
            {
                output.instruction.push(pb::Instruction {
                    resize: Some(pb::ResizeMessage {
                        width: Some(self.fb().ds.width()),
                        height: Some(self.fb().ds.height()),
                    }),
                    ..Default::default()
                });
            }
            let update = self.display.new_frame(true, existing.fb(), self.fb());
            if !update.is_empty() {
                output.instruction.push(pb::Instruction {
                    hostbytes: Some(pb::HostBytes {
                        hoststring: Some(update.into_bytes()),
                    }),
                    ..Default::default()
                });
            }
        }

        output.encode_to_vec()
    }

    pub fn init_diff(&self) -> Vec<u8> {
        self.diff_from(&Complete::new(self.fb().ds.width(), self.fb().ds.height()))
    }

    /// Apply a diff received from the peer.
    pub fn apply_string(&mut self, diff: &[u8]) -> Result<(), ApplyError> {
        let input = pb::HostMessage::decode(diff).map_err(|_| ApplyError::Malformed)?;

        for inst in &input.instruction {
            if let Some(hostbytes) = &inst.hostbytes {
                // An honest server never interrogates our terminal, so this is normally
                // empty. A hostile one can send a device-attributes query and get a
                // reply, which is simply dropped: a state update is not a channel back.
                let _reply = self.act(hostbytes.hoststring.as_deref().unwrap_or(&[]));
            } else if let Some(resize) = &inst.resize {
                self.act_resize(resize.width.unwrap_or(0), resize.height.unwrap_or(0));
            } else if let Some(echoack) = &inst.echoack {
                let n = echoack.echo_ack_num.unwrap_or(0);
                // An acknowledgement can only move forward.
                if n >= self.echo_ack {
                    self.echo_ack = n;
                }
            }
        }
        Ok(())
    }

    /// Advance the echo acknowledgement to cover every frame older than the timeout.
    ///
    /// Returns whether it moved.
    pub fn set_echo_ack(&mut self, now: u64) -> bool {
        let mut newest_echo_ack = 0;
        for &(frame, at) in &self.input_history {
            if at <= now.saturating_sub(ECHO_TIMEOUT) {
                newest_echo_ack = frame;
            }
        }

        self.input_history
            .retain(|&(frame, _)| frame >= newest_echo_ack);

        let moved = self.echo_ack != newest_echo_ack;
        self.echo_ack = newest_echo_ack;
        moved
    }

    pub fn register_input_frame(&mut self, n: u64, now: u64) {
        self.input_history.push_back((n, now));
    }

    /// How long until [`set_echo_ack`] would have something to do.
    pub fn wait_time(&self, now: u64) -> i32 {
        if self.input_history.len() < 2 {
            return i32::MAX;
        }
        let (_, at) = self.input_history[1];
        let next_echo_ack_time = at + ECHO_TIMEOUT;
        if next_echo_ack_time <= now {
            return 0;
        }
        (next_echo_ack_time - now) as i32
    }

    /// Report differences against another state, for the round-trip self-check.
    pub fn compare(&self, other: &Complete) -> bool {
        let mut ret = false;
        let (fb, other_fb) = (self.fb(), other.fb());

        if fb.ds.height() != other_fb.ds.height() || fb.ds.width() != other_fb.ds.width() {
            eprintln!(
                "Framebuffer size ({}x{}, {}x{}) differs.",
                fb.ds.width(),
                fb.ds.height(),
                other_fb.ds.width(),
                other_fb.ds.height()
            );
            return true;
        }

        for y in 0..fb.ds.height() {
            for x in 0..fb.ds.width() {
                let (a, b) = (fb.cell(y, x), other_fb.cell(y, x));
                if let (Some(a), Some(b)) = (a, b) {
                    let (mut ga, mut gb) = (String::new(), String::new());
                    a.print_grapheme(&mut ga);
                    b.print_grapheme(&mut gb);
                    if ga != gb || a.wide() != b.wide() || a.renditions() != b.renditions() {
                        eprintln!("Cell ({y}, {x}) differs.");
                        ret = true;
                    }
                }
            }
        }

        if fb.ds.cursor_row() != other_fb.ds.cursor_row()
            || fb.ds.cursor_col() != other_fb.ds.cursor_col()
        {
            eprintln!(
                "Cursor mismatch: ({}, {}) vs. ({}, {}).",
                fb.ds.cursor_row(),
                fb.ds.cursor_col(),
                other_fb.ds.cursor_row(),
                other_fb.ds.cursor_col()
            );
            ret = true;
        }

        ret
    }
}

impl PartialEq for Complete {
    fn eq(&self, other: &Self) -> bool {
        self.terminal == other.terminal && self.echo_ack == other.echo_ack
    }
}

impl Eq for Complete {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_state_produces_an_empty_diff() {
        let c = Complete::new(20, 4);
        assert!(c.diff_from(&c.clone()).is_empty());
    }

    #[test]
    fn host_output_reaches_the_screen() {
        let mut c = Complete::new(20, 4);
        c.act(b"hello");
        let mut line = String::new();
        for x in 0..5 {
            c.fb().cell(0, x).unwrap().print_grapheme(&mut line);
        }
        assert_eq!(line, "hello");
    }

    #[test]
    fn a_diff_applied_to_the_old_state_reproduces_the_new_one() {
        let mut old = Complete::new(20, 4);
        old.act(b"hello");

        let mut new = old.clone();
        new.act(b" world");

        let diff = new.diff_from(&old);
        assert!(!diff.is_empty());

        let mut target = old.clone();
        target.apply_string(&diff).unwrap();

        // Compare what is displayed, since row identity cannot survive a round trip
        // through the wire.
        assert!(
            !target.compare(&new),
            "screens differ after applying the diff"
        );
    }

    #[test]
    fn colour_queries_outlive_the_batch_that_raised_them() {
        let mut c = Complete::new(20, 4);
        c.act(b"\x1b]11;?\x1b\\");
        c.act(b"more output");
        assert_eq!(c.fb().color_queries(), "\x1b]11;?\x1b\\");

        c.clear_color_queries();
        assert!(c.fb().color_queries().is_empty());
    }

    #[test]
    fn a_resize_is_carried_in_the_diff() {
        let mut old = Complete::new(20, 4);
        old.act(b"hi");
        let mut new = old.clone();
        new.act_resize(40, 8);

        let diff = new.diff_from(&old);
        let decoded = pb::HostMessage::decode(&diff[..]).unwrap();
        let resize = decoded
            .instruction
            .iter()
            .find_map(|i| i.resize.as_ref())
            .expect("no resize in the diff");
        assert_eq!(resize.width, Some(40));
        assert_eq!(resize.height, Some(8));

        let mut target = old.clone();
        target.apply_string(&diff).unwrap();
        assert_eq!(target.fb().ds.width(), 40);
        assert_eq!(target.fb().ds.height(), 8);
    }

    #[test]
    fn an_echo_acknowledgement_is_carried_in_the_diff() {
        let old = Complete::new(20, 4);
        let mut new = old.clone();
        new.register_input_frame(7, 0);
        new.register_input_frame(8, 1000);
        assert!(new.set_echo_ack(1000));
        assert_eq!(new.echo_ack(), 7);

        let diff = new.diff_from(&old);
        let mut target = old.clone();
        target.apply_string(&diff).unwrap();
        assert_eq!(target.echo_ack(), 7);
    }

    #[test]
    fn echo_acknowledgement_only_covers_frames_past_the_timeout() {
        let mut c = Complete::new(20, 4);
        c.register_input_frame(1, 100);
        // Too recent: the client may still be showing its own prediction.
        assert!(!c.set_echo_ack(100));
        assert_eq!(c.echo_ack(), 0);

        // Once the timeout has passed, it is acknowledged.
        assert!(c.set_echo_ack(100 + ECHO_TIMEOUT));
        assert_eq!(c.echo_ack(), 1);
    }

    #[test]
    fn wait_time_is_unbounded_until_there_is_something_to_wait_for() {
        let mut c = Complete::new(20, 4);
        assert_eq!(c.wait_time(0), i32::MAX);
        c.register_input_frame(1, 0);
        assert_eq!(c.wait_time(0), i32::MAX);

        c.register_input_frame(2, 10);
        assert_eq!(c.wait_time(10), ECHO_TIMEOUT as i32);
        assert_eq!(c.wait_time(10 + ECHO_TIMEOUT), 0);
    }

    #[test]
    fn init_diff_rebuilds_the_screen_from_nothing() {
        let mut c = Complete::new(20, 4);
        c.act(b"hello\r\nworld");

        let diff = c.init_diff();
        let mut target = Complete::new(20, 4);
        target.apply_string(&diff).unwrap();
        assert!(
            !target.compare(&c),
            "init diff did not reproduce the screen"
        );
    }

    #[test]
    fn a_resize_the_peer_chose_cannot_kill_us() {
        // Both ends take the size straight off the wire: the client from a resize in the
        // host message, the server from a UserEvent::Resize. Every one of these used to
        // abort the process, and the large ones used to allocate until the kernel did.
        for (w, h) in [
            (0, 24),
            (80, 0),
            (-1, -1),
            (i32::MIN, i32::MIN),
            (i32::MAX, 1),
            (i32::MAX, i32::MAX),
        ] {
            let mut c = Complete::new(80, 24);
            c.act_resize(w, h);
            assert!(c.fb().ds.width() >= 1 && c.fb().ds.height() >= 1);
            // Still a working terminal afterwards, not a wedged one.
            c.act(b"hello");
        }
    }

    #[test]
    fn a_hostile_server_asking_our_terminal_a_question_is_not_fatal() {
        // A device-attributes query in a state update makes the emulator produce a reply
        // the client has nowhere to send. Dropping it must not be an assertion.
        let mut c = Complete::new(80, 24);
        let hostile = pb::HostMessage {
            instruction: vec![pb::Instruction {
                hostbytes: Some(pb::HostBytes {
                    hoststring: Some(b"\x1b[c".to_vec()),
                }),
                ..Default::default()
            }],
        }
        .encode_to_vec();
        assert!(c.apply_string(&hostile).is_ok());
    }

    #[test]
    fn a_malformed_diff_is_reported_not_fatal() {
        let mut c = Complete::new(20, 4);
        assert_eq!(c.apply_string(&[0x0a, 0x7f]), Err(ApplyError::Malformed));
    }

    #[test]
    fn a_diff_is_a_transition_not_an_idempotent_operation() {
        let mut old = Complete::new(20, 4);
        old.act(b"abc");
        let mut new = old.clone();
        new.act(b"def");
        let diff = new.diff_from(&old);

        let mut target = old.clone();
        target.apply_string(&diff).unwrap();
        assert_eq!(screen(&target)[0].trim_end(), "abcdef");

        // Applying it again appends a second time. This is not a defect: a diff carries
        // the change between two specific states, so it is only meaningful applied to
        // the state it was computed from. Idempotence is the transport's job -- an
        // Instruction names the state it moves from, and a repeated one is dropped
        // before it ever reaches here.
        target.apply_string(&diff).unwrap();
        assert_eq!(screen(&target)[0].trim_end(), "abcdefdef");
    }

    fn screen(c: &Complete) -> Vec<String> {
        let fb = c.fb();
        (0..fb.ds.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..fb.ds.width() {
                    fb.cell(y, x).unwrap().print_grapheme(&mut line);
                }
                line
            })
            .collect()
    }
}
