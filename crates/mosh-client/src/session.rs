//! The client session loop, with byte channels in place of a terminal.
//!
//! Transliterated from `stmclient.cc`. What the C++ reads from a tty fd and writes back
//! to it arrives here as [`Event`]s and leaves through the callback given to
//! [`Session::run`], so the same loop serves the `mosh-client` binary and an embedder
//! that owns no terminal at all.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;

use mosh_network::connection::Connection;
use mosh_network::transport::Transport;
use mosh_state::{Complete, UserStream};
use mosh_sys::pty;
use mosh_terminal::display::Display;
use mosh_terminal::framebuffer::Framebuffer;

use crate::prediction::{DisplayPreference, PredictionEngine};

/// Bytes in one read above which the input is taken to be a paste, not typing.
const PASTE_THRESHOLD: usize = 100;

/// Everything the session must react to besides the network.
pub enum Event {
    /// What the person typed.
    Bytes(Vec<u8>),
    Resize(i32, i32),
    /// Draw the whole screen again rather than a difference against it.
    Repaint,
    /// The terminal was taken away and handed back: reopen it, then repaint.
    Reopen,
    /// Ask the server to end the session.
    Quit,
    /// No more input will arrive.
    Eof,
}

/// Feeds [`Event`]s to a running [`Session`] from any thread.
#[derive(Clone)]
pub struct Handle {
    events: Sender<Event>,
    waker: Arc<UnixStream>,
}

impl Handle {
    pub fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            let _ = (&*self.waker).write(&[0]);
        }
    }
}

pub struct Options {
    pub cols: i32,
    pub rows: i32,
    pub predict: DisplayPreference,
    pub predict_overwrite: bool,
    /// Read `$TERM` and the terminfo database. False assumes a capable terminal and
    /// never switches to the alternate screen, which is what an embedder wants.
    pub terminfo: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            cols: 80,
            rows: 24,
            predict: DisplayPreference::Adaptive,
            predict_overwrite: false,
            terminfo: true,
        }
    }
}

pub struct Session {
    transport: Transport<UserStream, Complete>,
    predictions: PredictionEngine,
    display: Display,
    framebuffer: Framebuffer,
    events: Receiver<Event>,
    handle: Handle,
    wakeup: UnixStream,
}

impl Session {
    pub fn new(key: &str, ip: &str, port: u16, options: Options) -> std::io::Result<Self> {
        let (cols, rows) = (options.cols.max(1), options.rows.max(1));
        let display = Display::new(options.terminfo).map_err(std::io::Error::other)?;

        let now = pty::now_ms();
        let connection = Connection::new_client(key, ip, port, now)?;
        let mut transport = Transport::new(
            connection,
            UserStream::new(),
            Complete::new(cols, rows),
            now,
        );
        // Keystrokes should leave as soon as possible; batching them adds visible latency.
        transport.sender.set_send_delay(1);
        // Tell the server how big our terminal is before anything else. A terminal
        // reflows its own screen when it is resized, so a client that slept through the
        // change goes on painting differences against a screen that has moved underneath
        // it, and the leftovers never wash out.
        transport.sender.current_state_mut().push_resize(cols, rows);

        let mut predictions = PredictionEngine::new();
        predictions.set_display_preference(options.predict);
        predictions.set_predict_overwrite(options.predict_overwrite);

        let (wakeup, waker) = UnixStream::pair()?;
        wakeup.set_nonblocking(true)?;
        let (sender, events) = mpsc::channel();

        Ok(Session {
            transport,
            predictions,
            display,
            framebuffer: Framebuffer::new(cols, rows),
            events,
            handle: Handle {
                events: sender,
                waker: Arc::new(waker),
            },
            wakeup,
        })
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Run until the session ends, handing every frame to `frames`.
    pub fn run(
        mut self,
        mut frames: impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        // Paint the initial screen, so what we diff against is what the terminal shows.
        frames(self.display.open().as_bytes())?;
        let opening = self
            .display
            .new_frame(false, &self.framebuffer, &self.framebuffer);
        frames(opening.as_bytes())?;

        let mut repaint = false;
        loop {
            let now = pty::now_ms();
            let wait = self
                .transport
                .wait_time(now)
                .min(self.predictions.wait_time())
                .min(100) as i32;

            let mut fds: Vec<RawFd> = self
                .transport
                .connection
                .sockets()
                .iter()
                .map(|s| s.as_raw_fd())
                .collect();
            let network = fds.len();
            fds.push(self.wakeup.as_raw_fd());

            let ready = pty::poll_readable(&fds, wait.max(0))?;
            let now = pty::now_ms();

            if ready.iter().any(|&i| i < network) {
                while let Some(result) = self.transport.recv(now) {
                    let _ = result;
                }
                // Tell the prediction engine what the server has confirmed, which is what
                // lets it retire guesses and judge whether it is guessing well.
                self.predictions
                    .set_local_frame_acked(self.transport.sender.sent_state_acked());
                self.predictions.set_local_frame_late_acked(
                    self.transport.get_latest_remote_state().echo_ack(),
                );
                self.predictions.set_send_interval(
                    self.transport
                        .sender
                        .send_interval(self.transport.connection.srtt()),
                );
            }

            if ready.iter().any(|&i| i >= network) {
                self.drain_wakeup();
            }

            let mut eof = false;
            loop {
                match self.events.try_recv() {
                    Ok(Event::Bytes(bytes)) => self.typed(&bytes, now),
                    Ok(Event::Resize(cols, rows)) => self.resized(cols.max(1), rows.max(1)),
                    Ok(Event::Repaint) => repaint = true,
                    Ok(Event::Reopen) => {
                        frames(self.display.open().as_bytes())?;
                        repaint = true;
                    }
                    Ok(Event::Quit) => self.transport.sender.start_shutdown(now),
                    Ok(Event::Eof) => {
                        eof = true;
                        break;
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
            if eof {
                break;
            }

            // Draw: the server's screen, with our guesses laid over it.
            let mut new_state = self.transport.get_latest_remote_state().fb().clone();
            self.predictions.cull(&new_state, now);
            self.predictions.apply(&mut new_state);

            let diff = self
                .display
                .new_frame(!repaint, &self.framebuffer, &new_state);
            if !diff.is_empty() {
                frames(diff.as_bytes())?;
            }
            repaint = false;
            self.framebuffer = new_state;

            // The server signals it is leaving by moving to the shutdown state number. The
            // client has to notice and shut down too, or it waits for a peer that is gone.
            if self.transport.remote_state_num() == u64::MAX
                && !self.transport.sender.shutdown_in_progress()
            {
                self.transport.sender.start_shutdown(now);
            }

            self.transport.tick(now);

            if self.transport.sender.shutdown_in_progress()
                && (self.transport.sender.shutdown_acknowledged()
                    || self.transport.sender.counterparty_shutdown_acknowledged()
                    || self.transport.sender.shutdown_ack_timed_out(now))
            {
                break;
            }
        }

        frames(self.display.close().as_bytes())?;
        Ok(())
    }

    fn typed(&mut self, bytes: &[u8], now: u64) {
        if self.transport.sender.shutdown_in_progress() {
            return;
        }
        // Arriving faster than anyone types, so this is a paste rather than a keystroke,
        // and what an application does with one is not worth guessing at.
        let paste = bytes.len() > PASTE_THRESHOLD;
        if paste {
            self.predictions.reset();
        }
        // Before predicting, not after: these bytes ride the frame after the last one
        // sent, and a guess dated a frame early is judged against a screen that cannot
        // show it yet, which throws away roughly every other prediction.
        self.predictions
            .set_local_frame_sent(self.transport.sender.sent_state_last());
        for &b in bytes {
            if !paste {
                self.predictions.new_user_byte(b, &self.framebuffer, now);
            }
            self.transport.sender.current_state_mut().push_byte(b);
        }
    }

    fn resized(&mut self, cols: i32, rows: i32) {
        if !self.transport.sender.shutdown_in_progress() {
            self.transport
                .sender
                .current_state_mut()
                .push_resize(cols, rows);
        }
        // Every guess was placed against the old geometry.
        self.predictions.reset();
    }

    fn drain_wakeup(&mut self) {
        let mut scratch = [0u8; 256];
        while let Ok(n) = self.wakeup.read(&mut scratch) {
            if n == 0 {
                break;
            }
        }
    }
}
