//! Live session: PTY + terminal model + timestamped event log.
//!
//! # Concurrency model (single-task pull)
//!
//! There is no background reader task. The evaluator owns the [`Session`]
//! and pulls output itself:
//!
//! - [`Session::drain`] — synchronously consume everything currently
//!   buffered in the PTY without blocking (nonblocking reads until
//!   `WouldBlock`).
//! - [`Session::wait_change`] — await the next output chunk (or child EOF)
//!   with a deadline; this powers event-driven `Wait` with zero polling.
//! - [`Session::pump`] — one non-greedy step: drain what's available, else
//!   yield to the reactor once with a zero timeout.
//!
//! Because all reads happen on the evaluator's task there are no locks; the
//! `watch` generation channel exists so auxiliary observers (progress UIs,
//! future streaming encoders) can be notified of state changes via
//! [`Session::subscribe`].

use std::io;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::pty::{ExitStatus, Pty};
use crate::snapshot::{SessionEvent, SessionEventKind};
use crate::term::Term;

/// Boundary-safe incremental UTF-8 decoder (ported from asciinema's
/// `src/util.rs`). Feeding byte chunks yields valid `String`s, holding back
/// incomplete trailing sequences until the next chunk completes them.
/// Invalid bytes decode to U+FFFD.
#[derive(Default)]
pub struct Utf8Decoder(Vec<u8>);

impl Utf8Decoder {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn feed(&mut self, input: &[u8]) -> String {
        let mut output = String::new();
        self.0.extend_from_slice(input);

        while !self.0.is_empty() {
            match std::str::from_utf8(&self.0) {
                Ok(valid_str) => {
                    output.push_str(valid_str);
                    self.0.clear();
                    break;
                }

                Err(e) => {
                    let n = e.valid_up_to();
                    let valid_bytes: Vec<u8> = self.0.drain(..n).collect();
                    // Safety: `valid_up_to` guarantees these bytes are valid UTF-8.
                    let valid_str = unsafe { std::str::from_utf8_unchecked(&valid_bytes) };
                    output.push_str(valid_str);

                    match e.error_len() {
                        Some(len) => {
                            self.0.drain(..len);
                            output.push('\u{fffd}');
                        }

                        None => break, // incomplete sequence: hold back
                    }
                }
            }
        }

        output
    }
}

const READ_BUF_SIZE: usize = 64 * 1024;

/// A running child on a PTY, mirrored into an offscreen terminal, with a
/// timestamped event log for later replay (GIF frames, `.cast` output).
pub struct Session {
    pty: Pty,
    term: Term,
    events: Vec<SessionEvent>,
    start: Instant,
    decoder: Utf8Decoder,
    generation: u64,
    gen_tx: watch::Sender<u64>,
    exited: bool,
}

impl Session {
    /// Spawns `command` on a fresh `cols × rows` PTY with `env` applied.
    /// Must be called within a tokio runtime.
    pub fn spawn(
        command: &[String],
        env: &[(String, String)],
        cols: usize,
        rows: usize,
    ) -> io::Result<Session> {
        let pty = Pty::spawn(command, env, (cols as u16, rows as u16))?;
        let (gen_tx, _) = watch::channel(0);

        Ok(Session {
            pty,
            term: Term::new(cols, rows),
            events: Vec::new(),
            start: Instant::now(),
            decoder: Utf8Decoder::new(),
            generation: 0,
            gen_tx,
            exited: false,
        })
    }

    /// Consumes everything currently readable from the PTY without blocking.
    /// Returns `true` if any state changed (output fed or EOF observed).
    pub fn drain(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; READ_BUF_SIZE];
        let mut changed = false;

        while !self.exited {
            match self.pty.try_read(&mut buf) {
                Ok(0) => {
                    self.note_exit();
                    changed = true;
                    break;
                }
                Ok(n) => {
                    self.ingest(&buf[..n]);
                    changed = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }

        Ok(changed)
    }

    /// Awaits the next output chunk (or child EOF), up to `deadline`.
    /// Returns `true` if state changed, `false` on timeout or if the child
    /// has already exited. Callers typically follow up with [`drain`] to
    /// slurp everything else that arrived, then re-check their predicate.
    ///
    /// [`drain`]: Session::drain
    pub async fn wait_change(&mut self, deadline: Duration) -> io::Result<bool> {
        if self.exited {
            return Ok(false);
        }

        let mut buf = [0u8; READ_BUF_SIZE];

        match tokio::time::timeout(deadline, self.pty.read(&mut buf)).await {
            Err(_elapsed) => Ok(false),
            Ok(Ok(0)) => {
                self.note_exit();
                Ok(true)
            }
            Ok(Ok(n)) => {
                self.ingest(&buf[..n]);
                Ok(true)
            }
            Ok(Err(e)) => Err(e),
        }
    }

    /// One pump step: drain available output; if nothing was pending, poll
    /// the PTY once through the reactor (zero timeout) so an in-flight chunk
    /// still lands. Returns `true` if state changed.
    pub async fn pump(&mut self) -> io::Result<bool> {
        if self.drain()? {
            return Ok(true);
        }

        self.wait_change(Duration::ZERO).await
    }

    /// Writes raw bytes (keystrokes) to the child's input.
    pub async fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.pty.write_all(bytes).await
    }

    /// Resizes both the PTY (child sees SIGWINCH) and the screen model, and
    /// records a Resize event.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.pty.resize(cols as u16, rows as u16);
        self.term.resize(cols, rows);
        self.push_event(SessionEventKind::Resize(cols, rows));
    }

    /// Records that the child exited (idempotent). Called automatically when
    /// a read hits EOF; also callable by the evaluator after `shutdown`.
    pub fn note_exit(&mut self) {
        if !self.exited {
            self.exited = true;
            self.push_event(SessionEventKind::Exit);
        }
    }

    /// Records a Hide/Show visibility toggle in the event log.
    pub fn note_visibility(&mut self, visible: bool) {
        self.push_event(SessionEventKind::Visibility(visible));
    }

    /// Whether child EOF has been observed.
    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Nonblocking child exit check (reaps on success); see [`Pty::try_wait`].
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.pty.try_wait()
    }

    /// Graceful teardown: drains pending output, then SIGTERM → SIGKILL and
    /// reaps the child. Records the Exit event.
    pub async fn shutdown(&mut self) -> io::Result<Option<ExitStatus>> {
        let _ = self.drain();
        let status = self.pty.shutdown().await?;
        self.note_exit();

        Ok(status)
    }

    /// The screen model.
    pub fn term(&self) -> &Term {
        &self.term
    }

    /// The recorded event log (replayed at encode time).
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// A receiver on the generation counter; bumped on every state change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.gen_tx.subscribe()
    }

    /// Time since the session started.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Application cursor keys mode (DECCKM), tracked by the emulator, so
    /// arrows encode correctly for vim/fzf/etc. (see [`crate::keys`]).
    pub fn application_cursor(&self) -> bool {
        self.term.application_cursor()
    }

    /// Decodes a raw chunk, feeds the terminal, and logs the Output event.
    fn ingest(&mut self, bytes: &[u8]) {
        let text = self.decoder.feed(bytes);

        if text.is_empty() {
            return; // held back mid-sequence; nothing changed yet
        }

        self.term.feed(&text);
        self.push_event(SessionEventKind::Output(text));
    }

    fn push_event(&mut self, kind: SessionEventKind) {
        self.events.push(SessionEvent {
            time: self.start.elapsed(),
            kind,
        });

        self.generation += 1;
        let _ = self.gen_tx.send(self.generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn utf8_decoder_boundaries() {
        let mut decoder = Utf8Decoder::new();

        assert_eq!(decoder.feed(b"czarna "), "czarna ");
        assert_eq!(decoder.feed(&[0xc5, 0xbc, 0xc3]), "\u{17c}");
        assert_eq!(decoder.feed(&[0xb3, 0xc5, 0x82]), "\u{f3}\u{142}");
        assert_eq!(decoder.feed(&[0xc4]), "");
        assert_eq!(decoder.feed(&[0x87, 0x21]), "\u{107}!");
        assert_eq!(decoder.feed(&[0x80]), "\u{fffd}");
        assert_eq!(decoder.feed(&[]), "");
        assert_eq!(decoder.feed(&[0x80, 0x81]), "\u{fffd}\u{fffd}");
        assert_eq!(decoder.feed(&[0x23]), "#");
    }

    #[tokio::test]
    async fn wait_change_and_drain_collect_output() {
        let mut session = Session::spawn(
            &cmd(&["/bin/sh", "-c", "printf 'abc'; sleep 0.2; printf 'def'"]),
            &[],
            80,
            24,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);

        while !session.term().text().contains("abcdef") {
            assert!(
                Instant::now() < deadline,
                "timed out; screen: {:?}",
                session.term().text()
            );

            session
                .wait_change(Duration::from_millis(250))
                .await
                .unwrap();
            session.drain().unwrap();
        }

        let outputs: Vec<&SessionEvent> = session
            .events()
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::Output(_)))
            .collect();

        assert!(outputs.len() >= 2, "got {} output events", outputs.len());
        assert!(
            outputs.windows(2).all(|w| w[0].time <= w[1].time),
            "event times must be monotonic"
        );
        // The 0.2s sleep separates the first and last chunk.
        assert!(outputs.last().unwrap().time > outputs.first().unwrap().time);

        // Replaying the logged output through a fresh Term reproduces the
        // screen (the invariant the encoders rely on).
        let mut replay = Term::new(80, 24);
        for event in session.events() {
            if let SessionEventKind::Output(s) = &event.kind {
                replay.feed(s);
            }
        }
        assert_eq!(replay.text(), session.term().text());

        session.shutdown().await.unwrap();
        assert!(session.exited());
        assert!(
            session
                .events()
                .iter()
                .any(|e| matches!(e.kind, SessionEventKind::Exit))
        );
    }

    #[tokio::test]
    async fn wait_change_times_out_quietly() {
        let mut session = Session::spawn(&cmd(&["/bin/sh", "-c", "sleep 5"]), &[], 80, 24).unwrap();

        // No output is coming; wait_change must return false, not hang.
        let changed = session
            .wait_change(Duration::from_millis(50))
            .await
            .unwrap();
        assert!(!changed);
        assert!(!session.exited());
    }

    #[tokio::test]
    async fn generation_counter_bumps_on_output() {
        let mut session =
            Session::spawn(&cmd(&["/bin/sh", "-c", "printf hi"]), &[], 80, 24).unwrap();
        let rx = session.subscribe();
        assert_eq!(*rx.borrow(), 0);

        let deadline = Instant::now() + Duration::from_secs(10);
        while !session.term().text().contains("hi") {
            assert!(Instant::now() < deadline, "timed out");
            session
                .wait_change(Duration::from_millis(250))
                .await
                .unwrap();
        }

        assert!(*rx.borrow() > 0);
    }

    #[tokio::test]
    async fn write_reaches_child_and_screen() {
        let mut session = Session::spawn(&cmd(&["/bin/cat"]), &[], 80, 24).unwrap();

        session.write(b"pingpong").await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while !session.term().text().contains("pingpong") {
            assert!(
                Instant::now() < deadline,
                "timed out; screen: {:?}",
                session.term().text()
            );
            session
                .wait_change(Duration::from_millis(250))
                .await
                .unwrap();
            session.drain().unwrap();
        }

        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn resize_logs_event_and_updates_term() {
        let mut session = Session::spawn(&cmd(&["/bin/sh", "-c", "sleep 1"]), &[], 80, 24).unwrap();

        session.resize(100, 30);

        assert_eq!(session.term().size(), (100, 30));
        assert!(
            session
                .events()
                .iter()
                .any(|e| matches!(e.kind, SessionEventKind::Resize(100, 30)))
        );
    }
}
