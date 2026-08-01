//! The transport: one endpoint's view of both states, and the rules for advancing them.
//!
//! Transliterated from `src/network/networktransport-impl.h`.
//!
//! This is where an Instruction becomes idempotent. It names the state it moves from, so
//! a repeat is recognised and dropped here rather than being applied twice to a Diff
//! that is only meaningful once.

use mosh_protobufs::transport::Instruction;

use crate::connection::{Connection, RecvError};
use crate::fragment::{Fragment, FragmentAssembly};
use crate::packet::MOSH_PROTOCOL_VERSION;
use crate::sender::{SyncState, TransportSender};

/// Largest number of received states held before refusing to grow further.
const MAX_RECEIVED_STATES: usize = 1024;
/// How long to refuse growth once the queue has filled.
const QUENCH_INTERVAL: u64 = 15000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    ProtocolVersionMismatch,
    Recv(RecvError),
    Malformed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::ProtocolVersionMismatch => f.write_str("mosh protocol version mismatch"),
            TransportError::Recv(e) => write!(f, "{e}"),
            TransportError::Malformed => f.write_str("malformed instruction"),
        }
    }
}

impl std::error::Error for TransportError {}

#[derive(Debug, Clone)]
struct TimestampedState<S> {
    timestamp: u64,
    num: u64,
    state: S,
}

pub struct Transport<MyState: SyncState, RemoteState: SyncState> {
    pub connection: Connection,
    pub sender: TransportSender<MyState>,

    /// Every remote state we can still diff against, oldest first.
    received_states: Vec<TimestampedState<RemoteState>>,
    receiver_quench_timer: u64,
    last_receiver_state: RemoteState,
    fragments: FragmentAssembly,
    pub verbose: u32,
}

impl<MyState: SyncState, RemoteState: SyncState> Transport<MyState, RemoteState> {
    pub fn new(
        connection: Connection,
        initial_state: MyState,
        initial_remote: RemoteState,
        now: u64,
    ) -> Self {
        Transport {
            connection,
            sender: TransportSender::new(initial_state, now),
            received_states: vec![TimestampedState {
                timestamp: now,
                num: 0,
                state: initial_remote.clone(),
            }],
            receiver_quench_timer: 0,
            last_receiver_state: initial_remote,
            fragments: FragmentAssembly::new(),
            verbose: 0,
        }
    }

    pub fn remote_state_num(&self) -> u64 {
        self.received_states.last().expect("never empty").num
    }

    pub fn get_latest_remote_state(&self) -> &RemoteState {
        &self.received_states.last().expect("never empty").state
    }

    pub fn get_remote_state_timestamp(&self) -> u64 {
        self.received_states.last().expect("never empty").timestamp
    }

    /// Adopt the newest remote state as our reference, discarding older ones.
    pub fn get_remote_diff(&mut self) -> Vec<u8> {
        let diff = self
            .received_states
            .last()
            .expect("never empty")
            .state
            .diff_from(&self.last_receiver_state);
        self.last_receiver_state = self.received_states.last().expect("never empty").state.clone();
        diff
    }

    /// Receive and apply one datagram, if one is ready.
    pub fn recv(&mut self, now: u64) -> Option<Result<(), TransportError>> {
        let received = self.connection.recv(now)?;
        let payload = match received {
            Ok(r) => r.into_payload(),
            Err(e) => return Some(Err(TransportError::Recv(e))),
        };

        let frag = match Fragment::from_bytes(&payload) {
            Ok(f) => f,
            Err(_) => return Some(Err(TransportError::Malformed)),
        };

        if !self.fragments.add_fragment(frag) {
            return Some(Ok(())); // more fragments still to come
        }

        let inst = match self.fragments.get_assembly() {
            Ok(i) => i,
            Err(_) => return Some(Err(TransportError::Malformed)),
        };

        Some(self.apply_instruction(&inst, now))
    }

    /// The rules that decide whether an instruction may advance our state.
    pub fn apply_instruction(
        &mut self,
        inst: &Instruction,
        now: u64,
    ) -> Result<(), TransportError> {
        if inst.protocol_version.unwrap_or(0) != MOSH_PROTOCOL_VERSION {
            return Err(TransportError::ProtocolVersionMismatch);
        }

        let ack_num = inst.ack_num.unwrap_or(0);
        self.sender.process_acknowledgment_through(ack_num);

        // Tell the connection we have end-to-end connectivity, which is what stops the
        // client hopping ports needlessly.
        self.connection
            .set_last_roundtrip_success(self.sender.sent_state_acked_timestamp());

        let new_num = inst.new_num.unwrap_or(0);
        let old_num = inst.old_num.unwrap_or(0);

        // Already have it? Then this is a repeat, and applying its diff again would
        // corrupt our state. This is what makes an Instruction idempotent.
        if self.received_states.iter().any(|s| s.num == new_num) {
            return Ok(());
        }

        // Do we have the state it moves from? Without it the diff is meaningless.
        // Dropping here is security-sensitive: it is the other half of idempotency.
        let Some(reference) = self.received_states.iter().find(|s| s.num == old_num).cloned() else {
            return Ok(());
        };

        self.process_throwaway_until(inst.throwaway_num.unwrap_or(0));

        // Refuse to grow without bound. Better than dropping from the middle as the
        // sender does, because we must not acknowledge a state and then discard it.
        if self.received_states.len() > MAX_RECEIVED_STATES {
            if now < self.receiver_quench_timer {
                return Ok(());
            }
            self.receiver_quench_timer = now + QUENCH_INTERVAL;
        }

        let mut new_state = TimestampedState {
            timestamp: now,
            num: new_num,
            state: reference.state,
        };

        let diff = inst.diff.as_deref().unwrap_or(&[]);
        if !diff.is_empty() {
            new_state
                .state
                .apply_string(diff)
                .map_err(|_| TransportError::Malformed)?;
            self.sender.set_data_ack();
        }

        // Keep the queue ordered by state number.
        let pos = self
            .received_states
            .iter()
            .position(|s| s.num > new_num)
            .unwrap_or(self.received_states.len());
        self.received_states.insert(pos, new_state);

        self.sender.set_ack_num(self.remote_state_num());
        self.sender.remote_heard(now);
        Ok(())
    }

    /// Discard remote states the peer says it will never refer to again.
    fn process_throwaway_until(&mut self, throwaway_num: u64) {
        self.received_states.retain(|s| s.num >= throwaway_num);
        if self.received_states.is_empty() {
            // Never leave the queue empty; there must always be a reference state.
            unreachable!("throwaway must not empty the received-state queue");
        }
    }

    pub fn tick(&mut self, now: u64) {
        let _ = self.sender.tick(now, &mut self.connection);
    }

    pub fn wait_time(&mut self, now: u64) -> u64 {
        self.sender.wait_time(now, &self.connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StateError;

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct TestState(Vec<u8>);

    impl SyncState for TestState {
        fn subtract(&mut self, _p: &Self) {}
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

    fn transport() -> Transport<TestState, TestState> {
        let conn = Connection::new_server(Some("127.0.0.1"), 0, 0, 0)
            .expect("bind an ephemeral port");
        Transport::new(conn, TestState::default(), TestState::default(), 0)
    }

    fn inst(old: u64, new: u64, diff: &[u8]) -> Instruction {
        Instruction {
            protocol_version: Some(MOSH_PROTOCOL_VERSION),
            old_num: Some(old),
            new_num: Some(new),
            ack_num: Some(0),
            throwaway_num: Some(0),
            diff: Some(diff.to_vec()),
            chaff: Some(Vec::new()),
        }
    }

    #[test]
    fn a_wrong_protocol_version_is_refused() {
        let mut t = transport();
        let mut i = inst(0, 1, b"x");
        i.protocol_version = Some(MOSH_PROTOCOL_VERSION + 1);
        assert_eq!(
            t.apply_instruction(&i, 0),
            Err(TransportError::ProtocolVersionMismatch)
        );
    }

    #[test]
    fn an_instruction_advances_the_remote_state() {
        let mut t = transport();
        t.apply_instruction(&inst(0, 1, b"hello"), 0).unwrap();
        assert_eq!(t.remote_state_num(), 1);
        assert_eq!(t.get_latest_remote_state().0, b"hello");
    }

    #[test]
    fn a_repeated_instruction_is_dropped() {
        let mut t = transport();
        t.apply_instruction(&inst(0, 1, b"hello"), 0).unwrap();
        // The same instruction again. Its diff is only meaningful once, so applying it
        // twice would corrupt the state; the state number is what catches it.
        t.apply_instruction(&inst(0, 1, b"hello"), 0).unwrap();
        assert_eq!(t.get_latest_remote_state().0, b"hello");
        assert_eq!(t.remote_state_num(), 1);
    }

    #[test]
    fn an_instruction_from_an_unknown_state_is_dropped() {
        let mut t = transport();
        // We never saw state 7, so a diff from it cannot be applied to anything.
        t.apply_instruction(&inst(7, 8, b"orphan"), 0).unwrap();
        assert_eq!(t.remote_state_num(), 0);
        assert!(t.get_latest_remote_state().0.is_empty());
    }

    #[test]
    fn instructions_may_arrive_out_of_order() {
        let mut t = transport();
        t.apply_instruction(&inst(0, 1, b"a"), 0).unwrap();
        t.apply_instruction(&inst(1, 2, b"b"), 0).unwrap();
        assert_eq!(t.get_latest_remote_state().0, b"ab");

        // A late duplicate of an earlier one must not disturb the newest state.
        t.apply_instruction(&inst(0, 1, b"a"), 0).unwrap();
        assert_eq!(t.get_latest_remote_state().0, b"ab");
        assert_eq!(t.remote_state_num(), 2);
    }

    #[test]
    fn throwaway_discards_states_the_peer_has_finished_with() {
        let mut t = transport();
        t.apply_instruction(&inst(0, 1, b"a"), 0).unwrap();
        t.apply_instruction(&inst(1, 2, b"b"), 0).unwrap();

        let mut i = inst(2, 3, b"c");
        i.throwaway_num = Some(2);
        t.apply_instruction(&i, 0).unwrap();

        // States below the throwaway number are gone, but a reference always remains.
        assert!(t.received_states.iter().all(|s| s.num >= 2));
        assert!(!t.received_states.is_empty());
    }

    #[test]
    fn an_empty_diff_still_advances_the_state_number() {
        let mut t = transport();
        // An empty diff is how an acknowledgement travels; it must be accepted.
        t.apply_instruction(&inst(0, 1, b""), 0).unwrap();
        assert_eq!(t.remote_state_num(), 1);
        assert!(t.get_latest_remote_state().0.is_empty());
    }

    #[test]
    fn the_remote_diff_reports_only_what_is_new() {
        let mut t = transport();
        t.apply_instruction(&inst(0, 1, b"abc"), 0).unwrap();
        assert_eq!(t.get_remote_diff(), b"abc");
        // Asking again yields nothing; the reference has moved forward.
        assert!(t.get_remote_diff().is_empty());

        t.apply_instruction(&inst(1, 2, b"de"), 0).unwrap();
        assert_eq!(t.get_remote_diff(), b"de");
    }
}
