//! Deciding what to send and when.
//!
//! Transliterated from `third_party/mosh/src/network/transportsender-impl.h`.
//!
//! The sender keeps every state it has sent but not had acknowledged, so it can always
//! diff against something the peer is known or assumed to hold. The C++ tracks the
//! assumed receiver state with a list iterator; an index into a deque says the same
//! thing without the iterator-invalidation hazard when the queue is trimmed.

use mosh_protobufs::transport::Instruction;

use crate::connection::Connection;
use crate::fragment::{FragmentError, Fragmenter};
use crate::packet::MOSH_PROTOCOL_VERSION;

/// Milliseconds between frames.
pub const SEND_INTERVAL_MIN: u64 = 20;
pub const SEND_INTERVAL_MAX: u64 = 250;
/// Milliseconds between empty acknowledgements.
pub const ACK_INTERVAL: u64 = 3000;
/// Milliseconds before a delayed acknowledgement.
pub const ACK_DELAY: u64 = 100;
/// Shutdown datagrams to send before giving up.
pub const SHUTDOWN_RETRIES: u32 = 16;
/// How long to keep retrying at the frame rate after last hearing from the peer.
pub const ACTIVE_RETRY_TIMEOUT: u64 = 10000;

/// The state number meaning "shutting down".
const SHUTDOWN_NUM: u64 = u64::MAX;

/// Largest number of sent states kept before thinning the middle of the queue.
const MAX_SENT_STATES: usize = 32;

/// What the transport needs of a state to carry it.
pub trait SyncState: Clone + PartialEq {
    /// Drop the part of this state the peer has already acknowledged.
    fn subtract(&mut self, prefix: &Self);
    /// Encode the change from `existing` to self.
    fn diff_from(&self, existing: &Self) -> Vec<u8>;
    /// Encode this state from nothing.
    fn init_diff(&self) -> Vec<u8>;
    /// Apply a peer's diff.
    fn apply_string(&mut self, diff: &[u8]) -> Result<(), crate::StateError>;
    /// Discard partially-parsed input, used when a state is adopted wholesale.
    fn reset_input(&mut self) {}
    /// Report differences, for the verbose round-trip self-check.
    fn compare(&self, _other: &Self) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
struct TimestampedState<S> {
    timestamp: u64,
    num: u64,
    state: S,
}

pub struct TransportSender<S: SyncState> {
    current_state: S,
    /// Oldest first. The front is the last acknowledged state; the back is the most
    /// recently sent.
    sent_states: std::collections::VecDeque<TimestampedState<S>>,
    /// Index into `sent_states` of the state we believe the peer holds.
    assumed_receiver_state: usize,

    fragmenter: Fragmenter,

    next_ack_time: u64,
    next_send_time: u64,

    pub verbose: u32,
    shutdown_in_progress: bool,
    shutdown_tries: u32,
    shutdown_start: u64,

    ack_num: u64,
    pending_data_ack: bool,

    /// Milliseconds to spend collecting input before sending.
    send_mindelay: u64,
    last_heard: u64,
    /// When the current state first differed from the last sent one.
    mindelay_clock: Option<u64>,
}

impl<S: SyncState> TransportSender<S> {
    pub fn new(initial_state: S, now: u64) -> Self {
        let mut sent_states = std::collections::VecDeque::new();
        sent_states.push_back(TimestampedState {
            timestamp: now,
            num: 0,
            state: initial_state.clone(),
        });
        TransportSender {
            current_state: initial_state,
            sent_states,
            assumed_receiver_state: 0,
            fragmenter: Fragmenter::new(),
            next_ack_time: now,
            next_send_time: now,
            verbose: 0,
            shutdown_in_progress: false,
            shutdown_tries: 0,
            shutdown_start: 0,
            ack_num: 0,
            pending_data_ack: false,
            send_mindelay: 8,
            last_heard: 0,
            mindelay_clock: None,
        }
    }

    pub fn current_state(&self) -> &S {
        &self.current_state
    }

    pub fn current_state_mut(&mut self) -> &mut S {
        debug_assert!(!self.shutdown_in_progress);
        &mut self.current_state
    }

    pub fn set_current_state(&mut self, x: S) {
        debug_assert!(!self.shutdown_in_progress);
        self.current_state = x;
        self.current_state.reset_input();
    }

    pub fn set_ack_num(&mut self, n: u64) {
        self.ack_num = n;
    }

    pub fn set_data_ack(&mut self) {
        self.pending_data_ack = true;
    }

    pub fn remote_heard(&mut self, ts: u64) {
        self.last_heard = ts;
    }

    pub fn set_send_delay(&mut self, d: u64) {
        self.send_mindelay = d;
    }

    pub fn start_shutdown(&mut self, now: u64) {
        if !self.shutdown_in_progress {
            self.shutdown_start = now;
            self.shutdown_in_progress = true;
        }
    }

    pub fn shutdown_in_progress(&self) -> bool {
        self.shutdown_in_progress
    }

    pub fn shutdown_acknowledged(&self) -> bool {
        self.sent_states[0].num == SHUTDOWN_NUM
    }

    pub fn counterparty_shutdown_acknowledged(&self) -> bool {
        self.fragmenter.last_ack_sent() == SHUTDOWN_NUM
    }

    pub fn sent_state_acked_timestamp(&self) -> u64 {
        self.sent_states[0].timestamp
    }

    pub fn sent_state_acked(&self) -> u64 {
        self.sent_states[0].num
    }

    pub fn sent_state_last(&self) -> u64 {
        self.sent_states.back().expect("never empty").num
    }

    pub fn shutdown_ack_timed_out(&self, now: u64) -> bool {
        self.shutdown_in_progress
            && (self.shutdown_tries >= SHUTDOWN_RETRIES
                || now.saturating_sub(self.shutdown_start) >= ACTIVE_RETRY_TIMEOUT)
    }

    /// Aim for roughly two frames per round trip, within the frame-rate limits.
    pub fn send_interval(&self, srtt: f64) -> u64 {
        let interval = ((srtt / 2.0).ceil() as i64).max(0) as u64;
        interval.clamp(SEND_INTERVAL_MIN, SEND_INTERVAL_MAX)
    }

    /// Give the benefit of the doubt to states sent recently enough that an
    /// acknowledgement could still be in flight.
    fn update_assumed_receiver_state(&mut self, now: u64, timeout: u64) {
        self.assumed_receiver_state = 0;
        for i in 1..self.sent_states.len() {
            if now.saturating_sub(self.sent_states[i].timestamp) < timeout + ACK_DELAY {
                self.assumed_receiver_state = i;
            } else {
                return;
            }
        }
    }

    /// Drop the prefix every state shares, so diffs stay small.
    fn rationalize_states(&mut self) {
        let known = self.sent_states[0].state.clone();
        self.current_state.subtract(&known);
        for s in self.sent_states.iter_mut() {
            s.state.subtract(&known);
        }
    }

    fn calculate_timers(&mut self, now: u64, srtt: f64, timeout: u64) {
        self.update_assumed_receiver_state(now, timeout);
        self.rationalize_states();

        if self.pending_data_ack && self.next_ack_time > now + ACK_DELAY {
            self.next_ack_time = now + ACK_DELAY;
        }

        let back = self.sent_states.back().expect("never empty");
        let assumed = &self.sent_states[self.assumed_receiver_state];

        if self.current_state != back.state {
            let clock = *self.mindelay_clock.get_or_insert(now);
            self.next_send_time =
                (clock + self.send_mindelay).max(back.timestamp + self.send_interval(srtt));
        } else if self.current_state != assumed.state
            && self.last_heard + ACTIVE_RETRY_TIMEOUT > now
        {
            let mut t = back.timestamp + self.send_interval(srtt);
            if let Some(clock) = self.mindelay_clock {
                t = t.max(clock + self.send_mindelay);
            }
            self.next_send_time = t;
        } else if self.current_state != self.sent_states[0].state
            && self.last_heard + ACTIVE_RETRY_TIMEOUT > now
        {
            self.next_send_time = back.timestamp + timeout + ACK_DELAY;
        } else {
            self.next_send_time = u64::MAX;
        }

        // Hurry the shutdown handshake along.
        if self.shutdown_in_progress || self.ack_num == SHUTDOWN_NUM {
            self.next_ack_time =
                self.sent_states.back().expect("never empty").timestamp + self.send_interval(srtt);
        }
    }

    /// Milliseconds until something needs doing.
    pub fn wait_time(&mut self, now: u64, connection: &Connection) -> u64 {
        self.calculate_timers(now, connection.srtt(), connection.timeout());
        if !connection.has_remote_addr() {
            return u64::MAX;
        }
        let next = self.next_ack_time.min(self.next_send_time);
        next.saturating_sub(now)
    }

    /// Send a diff or an acknowledgement if one is due.
    pub fn tick(&mut self, now: u64, connection: &mut Connection) -> Result<(), FragmentError> {
        self.calculate_timers(now, connection.srtt(), connection.timeout());

        if !connection.has_remote_addr() {
            return Ok(());
        }
        if now < self.next_ack_time && now < self.next_send_time {
            return Ok(());
        }

        let mut diff = self
            .current_state
            .diff_from(&self.sent_states[self.assumed_receiver_state].state);

        self.attempt_prospective_resend_optimization(&mut diff);

        if self.verbose != 0 {
            self.verify_round_trip(&diff);
        }

        if diff.is_empty() {
            if now >= self.next_ack_time {
                self.send_empty_ack(now, connection)?;
                self.mindelay_clock = None;
            }
            if now >= self.next_send_time {
                self.next_send_time = u64::MAX;
                self.mindelay_clock = None;
            }
        } else if now >= self.next_send_time || now >= self.next_ack_time {
            self.send_to_receiver(diff, now, connection)?;
            self.mindelay_clock = None;
        }
        Ok(())
    }

    /// Check that applying our own diff reproduces our own state.
    ///
    /// A mismatch means we are about to tell the peer something that does not describe
    /// the screen we have, which would desynchronise the session silently.
    fn verify_round_trip(&self, diff: &[u8]) {
        let mut newstate = self.sent_states[self.assumed_receiver_state].state.clone();
        if newstate.apply_string(diff).is_err() {
            eprintln!("Warning, round-trip Instruction verification failed!");
            return;
        }
        if self.current_state.compare(&newstate) {
            eprintln!("Warning, round-trip Instruction verification failed!");
        }
        if self.current_state.init_diff() != newstate.init_diff() {
            eprintln!("Warning, target state Instruction verification failed!");
        }
    }

    fn send_empty_ack(&mut self, now: u64, connection: &mut Connection) -> Result<(), FragmentError> {
        let new_num = if self.shutdown_in_progress {
            SHUTDOWN_NUM
        } else {
            self.sent_states.back().expect("never empty").num + 1
        };

        let state = self.current_state.clone();
        self.add_sent_state(now, new_num, state);
        self.send_in_fragments(Vec::new(), new_num, now, connection)?;

        self.next_ack_time = now + ACK_INTERVAL;
        self.next_send_time = u64::MAX;
        Ok(())
    }

    fn add_sent_state(&mut self, timestamp: u64, num: u64, state: S) {
        self.sent_states.push_back(TimestampedState {
            timestamp,
            num,
            state,
        });
        if self.sent_states.len() > MAX_SENT_STATES {
            // Thin from the middle rather than the ends: the oldest is the acknowledged
            // baseline and the newest are the likely diff targets.
            let victim = self.sent_states.len() - 16;
            self.sent_states.remove(victim);
            if self.assumed_receiver_state >= victim && self.assumed_receiver_state > 0 {
                self.assumed_receiver_state -= 1;
            }
        }
    }

    fn send_to_receiver(
        &mut self,
        diff: Vec<u8>,
        now: u64,
        connection: &mut Connection,
    ) -> Result<(), FragmentError> {
        let back_num = self.sent_states.back().expect("never empty").num;
        let back_state_matches = self.current_state == self.sent_states.back().expect("never empty").state;

        let new_num = if self.shutdown_in_progress {
            SHUTDOWN_NUM
        } else if back_state_matches {
            back_num
        } else {
            back_num + 1
        };

        if new_num == back_num {
            self.sent_states.back_mut().expect("never empty").timestamp = now;
        } else {
            let state = self.current_state.clone();
            self.add_sent_state(now, new_num, state);
        }

        self.send_in_fragments(diff, new_num, now, connection)?;

        self.assumed_receiver_state = self.sent_states.len() - 1;
        self.next_ack_time = now + ACK_INTERVAL;
        self.next_send_time = u64::MAX;
        Ok(())
    }

    /// Random padding, so an eavesdropper cannot read the instruction's length.
    fn make_chaff(&self) -> Vec<u8> {
        const CHAFF_MAX: usize = 16;
        let mut len_byte = [0u8; 1];
        getrandom_fill(&mut len_byte);
        let chaff_len = len_byte[0] as usize % (CHAFF_MAX + 1);
        let mut chaff = vec![0u8; chaff_len];
        getrandom_fill(&mut chaff);
        chaff
    }

    fn send_in_fragments(
        &mut self,
        diff: Vec<u8>,
        new_num: u64,
        now: u64,
        connection: &mut Connection,
    ) -> Result<(), FragmentError> {
        let inst = Instruction {
            protocol_version: Some(MOSH_PROTOCOL_VERSION),
            old_num: Some(self.sent_states[self.assumed_receiver_state].num),
            new_num: Some(new_num),
            ack_num: Some(self.ack_num),
            throwaway_num: Some(self.sent_states[0].num),
            diff: Some(diff),
            chaff: Some(self.make_chaff()),
        };

        if new_num == SHUTDOWN_NUM {
            self.shutdown_tries += 1;
        }

        let mtu = connection
            .mtu()
            .saturating_sub(crate::packet::ADDED_BYTES)
            .saturating_sub(mosh_crypto::ADDED_BYTES);

        for frag in self.fragmenter.make_fragments(&inst, mtu)? {
            let bytes = frag.to_bytes()?;
            let _ = connection.send(bytes, now);
        }

        self.pending_data_ack = false;
        Ok(())
    }

    /// Drop states the peer has acknowledged past.
    pub fn process_acknowledgment_through(&mut self, ack_num: u64) {
        // Ignore an acknowledgement of a state we have already culled.
        if self.sent_states.iter().any(|s| s.num == ack_num) {
            self.sent_states.retain(|s| s.num >= ack_num);
        }
        debug_assert!(!self.sent_states.is_empty());
        if self.assumed_receiver_state >= self.sent_states.len() {
            self.assumed_receiver_state = self.sent_states.len() - 1;
        }
    }

    /// Consider diffing against the acknowledged state instead of the assumed one.
    ///
    /// Resending from a state the peer definitely has costs nothing when the diff is no
    /// larger, and recovers immediately if our assumption was wrong.
    fn attempt_prospective_resend_optimization(&mut self, proposed_diff: &mut Vec<u8>) {
        if self.assumed_receiver_state == 0 {
            return;
        }
        let resend_diff = self.current_state.diff_from(&self.sent_states[0].state);

        if resend_diff.len() <= proposed_diff.len()
            || (resend_diff.len() < 1000
                && resend_diff.len().saturating_sub(proposed_diff.len()) < 100)
        {
            self.assumed_receiver_state = 0;
            *proposed_diff = resend_diff;
        }
    }
}

fn getrandom_fill(buf: &mut [u8]) {
    getrandom::fill(buf).expect("system randomness unavailable");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StateError;

    /// A minimal state: a string that only ever grows.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct TestState(Vec<u8>);

    impl SyncState for TestState {
        fn subtract(&mut self, _prefix: &Self) {}
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

    fn sender() -> TransportSender<TestState> {
        TransportSender::new(TestState::default(), 0)
    }

    #[test]
    fn the_send_interval_tracks_half_the_round_trip() {
        let s = sender();
        assert_eq!(s.send_interval(100.0), 50);
        // Clamped at both ends so the frame rate stays sane.
        assert_eq!(s.send_interval(0.0), SEND_INTERVAL_MIN);
        assert_eq!(s.send_interval(10_000.0), SEND_INTERVAL_MAX);
    }

    #[test]
    fn a_fresh_sender_holds_one_acknowledged_state() {
        let s = sender();
        assert_eq!(s.sent_state_acked(), 0);
        assert_eq!(s.sent_state_last(), 0);
    }

    #[test]
    fn acknowledgement_drops_older_states_only() {
        let mut s = sender();
        for i in 1..=4u64 {
            s.add_sent_state(i * 10, i, TestState(vec![b'x'; i as usize]));
        }
        assert_eq!(s.sent_state_acked(), 0);

        s.process_acknowledgment_through(2);
        assert_eq!(s.sent_state_acked(), 2, "did not advance to the acked state");
        assert_eq!(s.sent_state_last(), 4, "dropped states newer than the ack");
    }

    #[test]
    fn an_acknowledgement_of_a_culled_state_is_ignored() {
        let mut s = sender();
        s.add_sent_state(10, 1, TestState(b"a".to_vec()));
        s.process_acknowledgment_through(1);
        let before = s.sent_states.len();
        // State 0 is gone; acknowledging it again must not empty the queue.
        s.process_acknowledgment_through(0);
        assert_eq!(s.sent_states.len(), before);
        assert!(!s.sent_states.is_empty());
    }

    #[test]
    fn the_state_queue_is_thinned_from_the_middle() {
        let mut s = sender();
        for i in 1..=40u64 {
            s.add_sent_state(i, i, TestState(vec![b'x'; i as usize]));
        }
        assert!(s.sent_states.len() <= MAX_SENT_STATES);
        // The acknowledged baseline and the newest state must both survive.
        assert_eq!(s.sent_state_acked(), 0);
        assert_eq!(s.sent_state_last(), 40);
    }

    #[test]
    fn the_assumed_receiver_state_advances_only_for_recent_sends() {
        let mut s = sender();
        s.add_sent_state(100, 1, TestState(b"a".to_vec()));
        s.add_sent_state(200, 2, TestState(b"ab".to_vec()));

        // Both were sent recently enough that an ack could still be in flight.
        s.update_assumed_receiver_state(250, 500);
        assert_eq!(s.assumed_receiver_state, 2);

        // Long enough ago that we should assume they were lost.
        s.update_assumed_receiver_state(10_000, 50);
        assert_eq!(s.assumed_receiver_state, 0);
    }

    #[test]
    fn a_prophylactic_resend_is_taken_when_it_costs_nothing() {
        let mut s = sender();
        s.current_state = TestState(b"hello".to_vec());
        s.add_sent_state(10, 1, TestState(b"hel".to_vec()));
        s.assumed_receiver_state = 1;

        // Diffing from the assumed state gives "lo"; from the acknowledged state,
        // "hello". That is longer, but under both thresholds, so it is worth the
        // certainty of diffing against a state the peer definitely holds.
        let mut diff = b"lo".to_vec();
        s.attempt_prospective_resend_optimization(&mut diff);
        assert_eq!(s.assumed_receiver_state, 0);
        assert_eq!(diff, b"hello");
    }

    #[test]
    fn a_prophylactic_resend_is_refused_when_far_too_large() {
        let mut s = sender();
        s.current_state = TestState(vec![b'x'; 5000]);
        s.add_sent_state(10, 1, TestState(vec![b'x'; 4999]));
        s.assumed_receiver_state = 1;

        let mut diff = b"x".to_vec();
        s.attempt_prospective_resend_optimization(&mut diff);
        // Resending 5000 bytes to save one is not worth it.
        assert_eq!(s.assumed_receiver_state, 1);
        assert_eq!(diff, b"x");
    }

    #[test]
    fn shutdown_times_out_after_enough_tries() {
        let mut s = sender();
        assert!(!s.shutdown_ack_timed_out(0));
        s.start_shutdown(0);
        assert!(!s.shutdown_ack_timed_out(0));

        s.shutdown_tries = SHUTDOWN_RETRIES;
        assert!(s.shutdown_ack_timed_out(0));
    }

    #[test]
    fn shutdown_times_out_after_long_enough() {
        let mut s = sender();
        s.start_shutdown(1000);
        assert!(!s.shutdown_ack_timed_out(1000));
        assert!(s.shutdown_ack_timed_out(1000 + ACTIVE_RETRY_TIMEOUT));
    }

    #[test]
    fn chaff_is_bounded_and_varies() {
        let s = sender();
        let mut lengths = std::collections::HashSet::new();
        for _ in 0..200 {
            let c = s.make_chaff();
            assert!(c.len() <= 16, "chaff was {} bytes", c.len());
            lengths.insert(c.len());
        }
        // Constant-length padding would not disguise anything.
        assert!(lengths.len() > 1, "chaff length never varied");
    }

    #[test]
    fn a_pending_data_ack_brings_the_ack_time_forward() {
        let mut s = sender();
        s.next_ack_time = 100_000;
        s.set_data_ack();
        s.calculate_timers(0, 100.0, 100);
        assert!(s.next_ack_time <= ACK_DELAY);
    }

    #[test]
    fn nothing_to_say_means_no_send_is_scheduled() {
        let mut s = sender();
        // Current state equals every sent state, so there is nothing to send.
        s.calculate_timers(0, 100.0, 100);
        assert_eq!(s.next_send_time, u64::MAX);
    }

    #[test]
    fn a_changed_state_schedules_a_send() {
        let mut s = sender();
        s.current_state = TestState(b"new".to_vec());
        s.calculate_timers(1000, 100.0, 100);
        assert!(s.next_send_time < u64::MAX, "a change did not schedule a send");
    }
}
