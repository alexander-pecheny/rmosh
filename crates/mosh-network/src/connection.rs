//! The datagram connection: sockets, roaming, replay rejection and round-trip timing.
//!
//! Transliterated from the `Connection` half of `third_party/mosh/src/network/network.cc`.
//!
//! The protocol logic is split out from the socket I/O. The C++ has them interleaved in
//! `recv_one`, which means none of the replay, timestamp or RTT handling can be tested
//! without a live socket. Here [`ConnectionState`] holds that logic and is exercised
//! directly; [`Connection`] adds the sockets around it.

use std::net::{SocketAddr, UdpSocket};

use mosh_crypto::{Base64Key, CryptoError, Message, Session};

use crate::packet::{timestamp16, timestamp_diff, Direction, Packet, TIMESTAMP_NONE};

/// Retransmission timeout bounds, in milliseconds.
const MIN_RTO: u64 = 50;
const MAX_RTO: u64 = 1000;

/// Penalty applied to a reported timestamp when the network signals congestion, which
/// gradually slows the peer down to the minimum frame rate.
const CONGESTION_TIMESTAMP_PENALTY: i32 = 500;

pub const PORT_RANGE_LOW: u16 = 60001;
pub const PORT_RANGE_HIGH: u16 = 60999;

/// How long the server waits before deciding the client has gone.
pub const SERVER_ASSOCIATION_TIMEOUT: u64 = 40000;
/// How often a client moves to a fresh source port.
pub const PORT_HOP_INTERVAL: u64 = 10000;

const MAX_PORTS_OPEN: usize = 10;
const MAX_OLD_SOCKET_AGE: u64 = 60000;

/// Application datagram MTU used before anything better is known.
pub const DEFAULT_SEND_MTU: usize = 500;
/// Conservative MTU that avoids fragmentation on mobile and tunnelled paths.
pub const DEFAULT_IPV4_MTU: usize = 1280;
pub const DEFAULT_IPV6_MTU: usize = 1280;

/// What a received datagram turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// A payload to hand upwards.
    Payload(Vec<u8>),
    /// Well-formed but too old to affect timing or targeting; still delivered.
    OutOfOrder(Vec<u8>),
}

impl Received {
    pub fn into_payload(self) -> Vec<u8> {
        match self {
            Received::Payload(p) | Received::OutOfOrder(p) => p,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Failed authentication, or was not a valid packet.
    Undecryptable,
    /// Sent in the direction we ourselves send: an attempt to play our own traffic back
    /// at us.
    WrongDirection,
}

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::Undecryptable => f.write_str("datagram failed to decrypt"),
            RecvError::WrongDirection => f.write_str("datagram was sent in our own direction"),
        }
    }
}

impl std::error::Error for RecvError {}

/// Protocol state, independent of any socket.
#[derive(Debug, Clone)]
pub struct ConnectionState {
    server: bool,
    direction: Direction,

    saved_timestamp: u16,
    saved_timestamp_received_at: u64,
    expected_receiver_seq: u64,

    pub last_heard: u64,

    rtt_hit: bool,
    srtt: f64,
    rttvar: f64,
}

impl ConnectionState {
    pub fn new(server: bool, now: u64) -> Self {
        ConnectionState {
            server,
            // We send in the opposite direction from the one we accept.
            direction: if server {
                Direction::ToClient
            } else {
                Direction::ToServer
            },
            saved_timestamp: TIMESTAMP_NONE,
            saved_timestamp_received_at: 0,
            expected_receiver_seq: 0,
            last_heard: now,
            rtt_hit: false,
            srtt: 1000.0,
            rttvar: 500.0,
        }
    }

    pub fn srtt(&self) -> f64 {
        self.srtt
    }

    /// Build the packet for an outgoing payload, echoing a recent peer timestamp.
    pub fn new_packet(&self, seq: u64, payload: Vec<u8>, now: u64) -> Packet {
        let mut outgoing_timestamp_reply = TIMESTAMP_NONE;
        // Only echo a timestamp we heard recently; a stale one would inflate the peer's
        // round-trip estimate.
        if now - self.saved_timestamp_received_at < 1000 {
            outgoing_timestamp_reply = self.saved_timestamp;
        }
        Packet::new(
            seq,
            self.direction,
            timestamp16(now),
            outgoing_timestamp_reply,
            payload,
        )
    }

    /// Validate and account for a decrypted datagram.
    pub fn process(
        &mut self,
        message: &Message,
        congestion_experienced: bool,
        now: u64,
    ) -> Result<Received, RecvError> {
        let p = Packet::from_message(message).map_err(|_| RecvError::Undecryptable)?;

        // Reject anything sent in the direction we send, so our own traffic cannot be
        // replayed at us.
        let expected = if self.server {
            Direction::ToServer
        } else {
            Direction::ToClient
        };
        if p.direction != expected {
            return Err(RecvError::WrongDirection);
        }

        // An out-of-order datagram is still delivered, but must not move the timestamp
        // or the peer address: a replay could otherwise corrupt timing and targeting.
        if p.seq < self.expected_receiver_seq {
            return Ok(Received::OutOfOrder(p.payload));
        }
        self.expected_receiver_seq = p.seq + 1;

        if p.timestamp != TIMESTAMP_NONE {
            self.saved_timestamp = p.timestamp;
            self.saved_timestamp_received_at = now;

            if congestion_experienced {
                // Tell the peer to slow down by pretending its packet took longer.
                self.saved_timestamp = self
                    .saved_timestamp
                    .wrapping_sub(CONGESTION_TIMESTAMP_PENALTY as u16);
            }
        }

        if p.timestamp_reply != TIMESTAMP_NONE {
            let r = timestamp_diff(timestamp16(now), p.timestamp_reply) as f64;
            // Ignore implausible values, e.g. the peer was suspended.
            if r < 5000.0 {
                if !self.rtt_hit {
                    self.srtt = r;
                    self.rttvar = r / 2.0;
                    self.rtt_hit = true;
                } else {
                    const ALPHA: f64 = 1.0 / 8.0;
                    const BETA: f64 = 1.0 / 4.0;
                    self.rttvar = (1.0 - BETA) * self.rttvar + BETA * (self.srtt - r).abs();
                    self.srtt = (1.0 - ALPHA) * self.srtt + ALPHA * r;
                }
            }
        }

        self.last_heard = now;
        Ok(Received::Payload(p.payload))
    }

    /// The retransmission timeout implied by the current round-trip estimate.
    pub fn timeout(&self) -> u64 {
        let rto = (self.srtt + 4.0 * self.rttvar).ceil() as i64;
        (rto.max(0) as u64).clamp(MIN_RTO, MAX_RTO)
    }
}

/// A connection over UDP.
pub struct Connection {
    /// Newest last; older sockets are kept briefly so replies to them still arrive.
    socks: Vec<UdpSocket>,
    remote_addr: Option<SocketAddr>,
    server: bool,
    mtu: usize,
    key: Base64Key,
    session: Session,
    state: ConnectionState,
    seq: mosh_crypto::Unique,
    last_port_choice: u64,
    last_roundtrip_success: u64,
    pub send_error: Option<String>,
}

impl Connection {
    /// Bind a server connection, choosing a port from the given range.
    pub fn new_server(
        desired_ip: Option<&str>,
        port_low: u16,
        port_high: u16,
        now: u64,
    ) -> std::io::Result<Self> {
        let ip = desired_ip.unwrap_or("0.0.0.0");
        let mut bound = None;
        // Try the range in order, as the C++ does, so a fixed port can be requested by
        // passing the same value for both bounds.
        for port in port_low..=port_high {
            match UdpSocket::bind((ip, port)) {
                Ok(s) => {
                    bound = Some(s);
                    break;
                }
                Err(_) => continue,
            }
        }
        let sock = bound.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "could not bind any port in range",
            )
        })?;
        sock.set_nonblocking(true)?;

        let key = Base64Key::random();
        let session = Session::new(&key);

        Ok(Connection {
            socks: vec![sock],
            remote_addr: None,
            server: true,
            mtu: DEFAULT_SEND_MTU,
            key,
            session,
            state: ConnectionState::new(true, now),
            seq: mosh_crypto::Unique::new(),
            last_port_choice: now,
            last_roundtrip_success: now,
            send_error: None,
        })
    }

    /// Open a client connection to a server that has already announced its key.
    pub fn new_client(key_str: &str, ip: &str, port: u16, now: u64) -> std::io::Result<Self> {
        let key = Base64Key::parse(key_str).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
        })?;
        let session = Session::new(&key);

        let remote: SocketAddr = format!("{ip}:{port}")
            .parse()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad address"))?;

        let bind_addr = if remote.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let sock = UdpSocket::bind(bind_addr)?;
        sock.set_nonblocking(true)?;

        let mtu = if remote.is_ipv6() {
            DEFAULT_IPV6_MTU
        } else {
            DEFAULT_IPV4_MTU
        };

        Ok(Connection {
            socks: vec![sock],
            remote_addr: Some(remote),
            server: false,
            mtu,
            key,
            session,
            state: ConnectionState::new(false, now),
            seq: mosh_crypto::Unique::new(),
            last_port_choice: now,
            last_roundtrip_success: now,
            send_error: None,
        })
    }

    pub fn key(&self) -> String {
        self.key.printable()
    }

    pub fn mtu(&self) -> usize {
        self.mtu
    }

    pub fn has_remote_addr(&self) -> bool {
        self.remote_addr.is_some()
    }

    pub fn srtt(&self) -> f64 {
        self.state.srtt()
    }

    pub fn timeout(&self) -> u64 {
        self.state.timeout()
    }

    pub fn set_last_roundtrip_success(&mut self, at: u64) {
        self.last_roundtrip_success = at;
    }

    /// The port we are listening on.
    pub fn port(&self) -> std::io::Result<u16> {
        Ok(self.current_sock().local_addr()?.port())
    }

    fn current_sock(&self) -> &UdpSocket {
        self.socks.last().expect("at least one socket")
    }

    /// Every socket we might receive on, for the caller's poll set.
    pub fn sockets(&self) -> Vec<&UdpSocket> {
        self.socks.iter().collect()
    }

    pub fn send(&mut self, payload: Vec<u8>, now: u64) -> Result<(), CryptoError> {
        let Some(remote) = self.remote_addr else {
            return Ok(()); // nothing to send to yet
        };

        let seq = self.seq.next_value()?;
        let packet = self.state.new_packet(seq, payload, now);
        let wire = self.session.encrypt(&packet.to_message())?;

        match self.current_sock().send_to(&wire, remote) {
            Ok(_) => self.send_error = None,
            Err(e) => {
                // Surfaced to the frontend rather than thrown, as in the C++.
                self.send_error = Some(format!("sendto: {e}"));
                if e.raw_os_error() == Some(libc_emsgsize()) {
                    self.mtu = DEFAULT_SEND_MTU;
                }
            }
        }

        self.housekeeping(now);
        Ok(())
    }

    fn housekeeping(&mut self, now: u64) {
        if self.server {
            if now.saturating_sub(self.state.last_heard) > SERVER_ASSOCIATION_TIMEOUT {
                self.remote_addr = None;
                eprintln!("Server now detached from client.");
            }
        } else if now.saturating_sub(self.last_port_choice) > PORT_HOP_INTERVAL
            && now.saturating_sub(self.last_roundtrip_success) > PORT_HOP_INTERVAL
        {
            let _ = self.hop_port(now);
        }
    }

    /// Move to a fresh source port, so a stateful middlebox cannot pin the session.
    fn hop_port(&mut self, now: u64) -> std::io::Result<()> {
        let remote = self.remote_addr.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "no remote address")
        })?;
        let bind_addr = if remote.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
        let sock = UdpSocket::bind(bind_addr)?;
        sock.set_nonblocking(true)?;
        self.socks.push(sock);
        self.last_port_choice = now;
        self.prune_sockets(now);
        Ok(())
    }

    fn prune_sockets(&mut self, now: u64) {
        // Once the newest socket has been working long enough, no reply can still be in
        // flight to the ones it superseded, so keep it alone.
        if self.socks.len() > 1 && now.saturating_sub(self.last_port_choice) > MAX_OLD_SOCKET_AGE {
            self.socks.drain(..self.socks.len() - 1);
        }
        // Never keep more than a bounded number, however recent.
        while self.socks.len() > MAX_PORTS_OPEN {
            self.socks.remove(0);
        }
    }

    /// Receive one datagram from whichever socket has one ready.
    pub fn recv(&mut self, now: u64) -> Option<Result<Received, RecvError>> {
        let mut buf = [0u8; mosh_crypto::RECEIVE_MTU];

        for i in 0..self.socks.len() {
            let (n, from) = match self.socks[i].recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => continue, // would block, or nothing here
            };

            let message = match self.session.decrypt(&buf[..n]) {
                Ok(m) => m,
                Err(_) => return Some(Err(RecvError::Undecryptable)),
            };

            // Explicit congestion notification needs recvmsg with control messages,
            // which std's UdpSocket does not expose. Treating every datagram as
            // uncongested is the same behaviour mosh has on platforms without it.
            let result = self.state.process(&message, false, now);

            if let Ok(Received::Payload(_)) = &result {
                // Only the client may roam; the server follows it.
                if self.server && self.remote_addr != Some(from) {
                    self.remote_addr = Some(from);
                    eprintln!("Server now attached to client at {from}");
                }
                self.prune_sockets(now);
            }

            return Some(result);
        }
        None
    }
}

/// EMSGSIZE, without pulling in libc for one constant.
fn libc_emsgsize() -> i32 {
    90
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosh_crypto::Nonce;

    fn message(seq: u64, direction: Direction, ts: u16, ts_reply: u16) -> Message {
        Packet::new(seq, direction, ts, ts_reply, b"payload".to_vec()).to_message()
    }

    #[test]
    fn a_server_rejects_traffic_sent_towards_the_client() {
        let mut state = ConnectionState::new(true, 0);
        // A server accepts ToServer and must refuse ToClient, which is what it sends.
        assert!(state
            .process(&message(0, Direction::ToServer, 1, TIMESTAMP_NONE), false, 0)
            .is_ok());
        assert_eq!(
            state.process(&message(1, Direction::ToClient, 1, TIMESTAMP_NONE), false, 0),
            Err(RecvError::WrongDirection)
        );
    }

    #[test]
    fn a_client_rejects_traffic_sent_towards_the_server() {
        let mut state = ConnectionState::new(false, 0);
        assert!(state
            .process(&message(0, Direction::ToClient, 1, TIMESTAMP_NONE), false, 0)
            .is_ok());
        assert_eq!(
            state.process(&message(1, Direction::ToServer, 1, TIMESTAMP_NONE), false, 0),
            Err(RecvError::WrongDirection)
        );
    }

    #[test]
    fn an_old_datagram_is_delivered_but_does_not_move_the_timestamp() {
        let mut state = ConnectionState::new(true, 0);
        state
            .process(&message(5, Direction::ToServer, 100, TIMESTAMP_NONE), false, 0)
            .unwrap();
        let saved = state.saved_timestamp;

        // A replayed older datagram must not be able to rewind our notion of the peer's
        // clock, which is what a replay attack would try to do.
        let result = state
            .process(&message(2, Direction::ToServer, 999, TIMESTAMP_NONE), false, 0)
            .unwrap();
        assert!(matches!(result, Received::OutOfOrder(_)));
        assert_eq!(state.saved_timestamp, saved);
    }

    #[test]
    fn an_out_of_order_datagram_is_still_handed_upwards() {
        let mut state = ConnectionState::new(true, 0);
        state
            .process(&message(5, Direction::ToServer, 1, TIMESTAMP_NONE), false, 0)
            .unwrap();
        let result = state
            .process(&message(1, Direction::ToServer, 1, TIMESTAMP_NONE), false, 0)
            .unwrap();
        // Losing it would drop real data; only timing and targeting are withheld.
        assert_eq!(result.into_payload(), b"payload");
    }

    #[test]
    fn the_first_round_trip_seeds_the_estimate() {
        let mut state = ConnectionState::new(true, 0);
        // The peer echoed a timestamp from 40ms ago.
        let now = 1000u64;
        let echoed = timestamp16(now - 40);
        state
            .process(&message(0, Direction::ToServer, 1, echoed), false, now)
            .unwrap();
        assert!((state.srtt() - 40.0).abs() < 1.0, "srtt was {}", state.srtt());
    }

    #[test]
    fn implausible_round_trips_are_ignored() {
        let mut state = ConnectionState::new(true, 0);
        let before = state.srtt();
        let now = 100_000u64;
        // A peer that was suspended reports an enormous round trip.
        let echoed = timestamp16(now - 20_000);
        state
            .process(&message(0, Direction::ToServer, 1, echoed), false, now)
            .unwrap();
        assert_eq!(state.srtt(), before, "an absurd sample moved the estimate");
    }

    #[test]
    fn the_timeout_is_clamped_to_its_bounds() {
        let mut state = ConnectionState::new(true, 0);
        state.srtt = 0.0;
        state.rttvar = 0.0;
        assert_eq!(state.timeout(), MIN_RTO);

        state.srtt = 100_000.0;
        state.rttvar = 100_000.0;
        assert_eq!(state.timeout(), MAX_RTO);
    }

    #[test]
    fn congestion_pulls_the_reported_timestamp_back() {
        let mut state = ConnectionState::new(true, 0);
        state
            .process(&message(0, Direction::ToServer, 1000, TIMESTAMP_NONE), false, 0)
            .unwrap();
        let uncongested = state.saved_timestamp;

        let mut state = ConnectionState::new(true, 0);
        state
            .process(&message(0, Direction::ToServer, 1000, TIMESTAMP_NONE), true, 0)
            .unwrap();
        // The peer is told its packet took longer, so it slows down.
        assert_eq!(
            state.saved_timestamp,
            uncongested.wrapping_sub(CONGESTION_TIMESTAMP_PENALTY as u16)
        );
    }

    #[test]
    fn a_stale_timestamp_is_not_echoed_back() {
        let mut state = ConnectionState::new(true, 0);
        state
            .process(&message(0, Direction::ToServer, 4242, TIMESTAMP_NONE), false, 0)
            .unwrap();

        // Fresh: echo it.
        let p = state.new_packet(0, vec![], 500);
        assert_eq!(p.timestamp_reply, 4242);

        // Stale: echoing it would inflate the peer's round-trip estimate.
        let p = state.new_packet(0, vec![], 5000);
        assert_eq!(p.timestamp_reply, TIMESTAMP_NONE);
    }

    #[test]
    fn an_undecryptable_message_is_reported_not_fatal() {
        let mut state = ConnectionState::new(true, 0);
        // Too short to hold the timestamps.
        let msg = Message::new(Nonce::new(0), vec![0; 2]);
        assert_eq!(
            state.process(&msg, false, 0),
            Err(RecvError::Undecryptable)
        );
    }

    #[test]
    fn a_server_and_client_can_exchange_a_datagram() {
        let now = 0;
        let mut server = Connection::new_server(Some("127.0.0.1"), 0, 0, now)
            .or_else(|_| Connection::new_server(Some("127.0.0.1"), 61234, 61999, now))
            .expect("bind server");
        let port = server.port().expect("port");
        let key = server.key();

        let mut client =
            Connection::new_client(&key, "127.0.0.1", port, now).expect("client");

        client.send(b"hello server".to_vec(), now).unwrap();

        // Give the datagram a moment to arrive on a loopback socket.
        let mut got = None;
        for _ in 0..100 {
            if let Some(Ok(r)) = server.recv(now) {
                got = Some(r.into_payload());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.as_deref(), Some(&b"hello server"[..]));
        assert!(server.has_remote_addr(), "server did not attach to client");

        // And back the other way.
        server.send(b"hello client".to_vec(), now).unwrap();
        let mut got = None;
        for _ in 0..100 {
            if let Some(Ok(r)) = client.recv(now) {
                got = Some(r.into_payload());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.as_deref(), Some(&b"hello client"[..]));
    }

    #[test]
    fn a_session_outlives_the_age_at_which_old_sockets_are_dropped() {
        let mut server = Connection::new_server(Some("127.0.0.1"), 0, 0, 0)
            .or_else(|_| Connection::new_server(Some("127.0.0.1"), 61234, 61999, 0))
            .expect("bind server");
        let port = server.port().expect("port");
        let mut client =
            Connection::new_client(&server.key(), "127.0.0.1", port, 0).expect("client");

        // A session that has never had to hop still has its original socket, however
        // old. Pruning it would leave nothing to receive on.
        let later = MAX_OLD_SOCKET_AGE + 1000;
        client.send(b"still here".to_vec(), later).unwrap();
        let mut got = None;
        for _ in 0..100 {
            if let Some(Ok(r)) = server.recv(later) {
                got = Some(r.into_payload());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got.as_deref(), Some(&b"still here"[..]));
        assert_eq!(server.sockets().len(), 1);
    }

    #[test]
    fn a_superseded_socket_is_dropped_once_the_newest_has_proved_itself() {
        let key = Base64Key::random().printable();
        let mut client = Connection::new_client(&key, "127.0.0.1", 60001, 0).expect("client");
        client.hop_port(1000).expect("hop");
        assert_eq!(client.sockets().len(), 2);

        client.prune_sockets(1000 + MAX_OLD_SOCKET_AGE);
        assert_eq!(client.sockets().len(), 2, "dropped while a reply could arrive");

        client.prune_sockets(1000 + MAX_OLD_SOCKET_AGE + 1);
        assert_eq!(client.sockets().len(), 1);
    }
}
