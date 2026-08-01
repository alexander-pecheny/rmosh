//! A whole session between two Rust endpoints over a real socket.
//!
//! Every layer built so far participates: terminal, statesync, protobuf, compression,
//! fragmentation, packet framing, crypto, connection and transport. Nothing here is
//! mocked, so this is the first test that exercises them together rather than one at a
//! time.

use mosh_network::connection::Connection;
use mosh_network::transport::Transport;
use mosh_state::{Complete, UserStream};

/// Pump both ends until a condition holds, or give up.
fn pump<F>(
    server: &mut Transport<Complete, UserStream>,
    client: &mut Transport<UserStream, Complete>,
    clock: &mut u64,
    mut done: F,
) -> bool
where
    F: FnMut(&Transport<Complete, UserStream>, &Transport<UserStream, Complete>) -> bool,
{
    for _ in 0..400 {
        // Advance a notional clock faster than real time so the senders' rate limits
        // are satisfied without the test actually sleeping for seconds.
        *clock += 25;
        server.tick(*clock);
        client.tick(*clock);
        std::thread::sleep(std::time::Duration::from_millis(2));
        while server.recv(*clock).is_some() {}
        while client.recv(*clock).is_some() {}
        if done(server, client) {
            return true;
        }
    }
    false
}

fn session() -> (
    Transport<Complete, UserStream>,
    Transport<UserStream, Complete>,
    u64,
) {
    let now = 0u64;
    let server_conn = Connection::new_server(Some("127.0.0.1"), 0, 0, now).expect("bind server");
    let port = server_conn.port().expect("port");
    let key = server_conn.key();

    let server = Transport::new(server_conn, Complete::new(80, 24), UserStream::new(), now);

    let client_conn = Connection::new_client(&key, "127.0.0.1", port, now).expect("client");
    let client = Transport::new(client_conn, UserStream::new(), Complete::new(80, 24), now);

    (server, client, now)
}

fn screen_line(state: &Complete, row: i32) -> String {
    let fb = state.fb();
    let mut line = String::new();
    for x in 0..fb.ds.width() {
        fb.cell(row, x).unwrap().print_grapheme(&mut line);
    }
    line.trim_end().to_string()
}

#[test]
fn the_client_learns_what_the_server_put_on_the_screen() {
    let (mut server, mut client, mut clock) = session();

    // The client must speak first so the server learns where it is.
    client.sender.current_state_mut().push_byte(b'x');
    assert!(
        pump(&mut server, &mut client, &mut clock, |s, _| s
            .connection
            .has_remote_addr()),
        "server never heard from the client"
    );

    server.sender.current_state_mut().act(b"hello from the host");

    assert!(
        pump(&mut server, &mut client, &mut clock, |_, c| {
            screen_line(c.get_latest_remote_state(), 0) == "hello from the host"
        }),
        "client screen never caught up; it shows {:?}",
        screen_line(client.get_latest_remote_state(), 0)
    );
}

#[test]
fn the_server_learns_what_the_user_typed() {
    let (mut server, mut client, mut clock) = session();

    for c in b"ls -l\r" {
        client.sender.current_state_mut().push_byte(*c);
    }

    assert!(
        pump(&mut server, &mut client, &mut clock, |s, _| {
            s.get_latest_remote_state().len() >= 6
        }),
        "server never received the keystrokes"
    );

    let typed: Vec<u8> = (0..server.get_latest_remote_state().len())
        .filter_map(|i| match server.get_latest_remote_state().get(i) {
            Some(mosh_state::UserEvent::Byte(b)) => Some(b),
            _ => None,
        })
        .collect();
    assert_eq!(typed, b"ls -l\r");
}

#[test]
fn successive_updates_all_arrive() {
    let (mut server, mut client, mut clock) = session();

    client.sender.current_state_mut().push_byte(b'x');
    assert!(pump(&mut server, &mut client, &mut clock, |s, _| s
        .connection
        .has_remote_addr()));

    // Several updates in a row, each building on the last. Pumping between them lets
    // some arrive as separate instructions rather than being coalesced into one, which
    // is what makes this exercise the sequencing rather than a single diff.
    for line in ["first", "second", "third"] {
        server
            .sender
            .current_state_mut()
            .act(format!("\r\n{line}").as_bytes());
        pump(&mut server, &mut client, &mut clock, |_, _| false);
    }

    assert!(
        pump(&mut server, &mut client, &mut clock, |_, c| {
            screen_line(c.get_latest_remote_state(), 3) == "third"
        }),
        "client never saw the last update; row 3 shows {:?}",
        screen_line(client.get_latest_remote_state(), 3)
    );

    // Every intermediate line survived, so no update was lost or applied twice.
    let remote = client.get_latest_remote_state();
    assert_eq!(screen_line(remote, 1), "first");
    assert_eq!(screen_line(remote, 2), "second");
    assert_eq!(screen_line(remote, 3), "third");
}

#[test]
fn a_large_update_survives_fragmentation() {
    let (mut server, mut client, mut clock) = session();

    client.sender.current_state_mut().push_byte(b'x');
    assert!(pump(&mut server, &mut client, &mut clock, |s, _| s
        .connection
        .has_remote_addr()));

    // Fill the screen with varied content so the diff cannot compress into one datagram.
    let mut payload = String::new();
    for row in 0..24 {
        payload.push_str(&format!("\x1b[{};1H", row + 1));
        for col in 0..79 {
            payload.push((b'!' + ((row * 79 + col) % 90) as u8) as char);
        }
    }
    server.sender.current_state_mut().act(payload.as_bytes());
    let expected = screen_line(server.sender.current_state(), 23);

    assert!(
        pump(&mut server, &mut client, &mut clock, |_, c| {
            screen_line(c.get_latest_remote_state(), 23) == expected
        }),
        "a fragmented update never fully arrived; last row shows {:?}",
        screen_line(client.get_latest_remote_state(), 23)
    );
}

#[test]
fn both_directions_work_at_once() {
    let (mut server, mut client, mut clock) = session();

    for c in b"typing" {
        client.sender.current_state_mut().push_byte(*c);
    }
    server.sender.current_state_mut().act(b"output");

    assert!(
        pump(&mut server, &mut client, &mut clock, |s, c| {
            s.get_latest_remote_state().len() >= 6
                && screen_line(c.get_latest_remote_state(), 0) == "output"
        }),
        "the two directions did not both complete"
    );
}
