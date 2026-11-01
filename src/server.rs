//! Stateful command execution for one serial session.
//!
//! Received bytes that a command has not returned yet stay in one pending
//! buffer, so every read-type command sees them in arrival order and a
//! transport error never discards data. A `read`, `readline` or `expect` that
//! times out or is cancelled leaves its bytes buffered.

use crate::protocol::{
    Command, CommandError, End, ErrorCode, MAX_IO_BYTES, Outcome, Received, Rejected, Request,
    RequestId, Response, SideEffect, TimeoutMs,
};
use crate::transport::{SerialTransport, TransportError, list_ports};
use regex::bytes::Regex;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Longest single wait inside `read`, `readline` and `expect`; bounds how
/// long a cancel takes to stop them.
const WAIT_SLICE: Duration = Duration::from_millis(100);
/// How long `drain` waits for bytes already in flight.
const DRAIN_WAIT: Duration = Duration::from_millis(50);
/// Largest single transport read.
const CHUNK: usize = 4096;

const OPEN_WARNING: &str =
    "Opening a serial port may pulse DTR or RTS on some operating systems and adapters";

pub struct CommandHandler {
    transport: Box<dyn SerialTransport>,
    /// Received, not yet returned. Never longer than `MAX_IO_BYTES`.
    pending: Vec<u8>,
    read_only: bool,
    session_id: Arc<str>,
    gate: Arc<CancelGate>,
}

impl CommandHandler {
    #[must_use]
    pub fn new(transport: Box<dyn SerialTransport>, read_only: bool) -> Self {
        let epoch_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self {
            transport,
            pending: Vec::new(),
            read_only,
            session_id: format!("{}-{epoch_nanos}", std::process::id()).into(),
            gate: Arc::default(),
        }
    }

    /// The gate a concurrent stdin reader uses to cancel running waits.
    #[must_use]
    pub fn cancel_gate(&self) -> Arc<CancelGate> {
        Arc::clone(&self.gate)
    }

    pub fn execute(&mut self, request: Request) -> Response {
        let command = request.command.name();
        let outcome = if matches!(
            request.command,
            Command::Read { .. } | Command::Readline { .. } | Command::Expect { .. }
        ) {
            self.gate.begin(request.id.clone());
            let outcome = self.run(request.command);
            self.gate.end(matches!(
                &outcome,
                Ok(Outcome::Received(received)) if received.end == End::Cancelled
            ));
            outcome
        } else {
            self.run(request.command)
        };
        self.respond(request.id, command, outcome)
    }

    /// Answer a `cancel` whose [`CancelGate::request`] ticket was taken when
    /// its line arrived.
    #[must_use]
    pub fn finish_cancel(&self, id: Option<RequestId>, ticket: u64) -> Response {
        let cancelled = self.gate.fired(ticket);
        self.respond(id, "cancel", Ok(Outcome::Cancel { cancelled }))
    }

    /// Answer a request line that failed to parse.
    #[must_use]
    pub fn reject(&self, rejected: Rejected) -> Response {
        self.respond(rejected.id, rejected.command, Err(rejected.error))
    }

    fn respond(
        &self,
        id: Option<RequestId>,
        command: &'static str,
        outcome: Result<Outcome, CommandError>,
    ) -> Response {
        Response {
            id,
            session_id: Arc::clone(&self.session_id),
            command,
            outcome,
        }
    }

    fn run(&mut self, command: Command) -> Result<Outcome, CommandError> {
        match command {
            Command::List {} => Ok(Outcome::Ports {
                ports: list_ports().map_err(|error| CommandError::from_transport(&error, false))?,
            }),
            Command::Open(params) => {
                let config = params.resolve()?;
                self.transport
                    .open(&config)
                    .map_err(|error| CommandError::from_transport(&error, true))?;
                self.pending.clear();
                Ok(Outcome::Opened {
                    port: config.path,
                    baud: config.baud,
                    bits: config.data_bits,
                    parity: config.parity,
                    stop: config.stop_bits,
                    dtr: config.dtr,
                    rts: config.rts,
                    effect: SideEffect::SerialPortOpened.into(),
                    warning: OPEN_WARNING,
                })
            }
            Command::Close {} => {
                self.transport
                    .close()
                    .map_err(|error| CommandError::from_transport(&error, false))?;
                let discarded_bytes = self.pending.len();
                self.pending.clear();
                Ok(Outcome::Closed {
                    discarded_bytes,
                    effect: SideEffect::SerialPortClosed.into(),
                })
            }
            Command::Write(payload) => {
                self.ensure_writable("write")?;
                let (encoding, bytes) = payload.decode()?;
                self.transport
                    .write(&bytes)
                    .map_err(|error| CommandError::from_transport(&error, true))?;
                Ok(Outcome::Written {
                    encoding,
                    bytes: bytes.len(),
                    effect: SideEffect::SerialBytesWritten.into(),
                })
            }
            Command::Read { bytes, timeout_ms } => self.read(bytes.bytes(), timeout_ms),
            Command::Readline { timeout_ms } => self.readline(timeout_ms),
            Command::Expect {
                pattern,
                timeout_ms,
            } => self.expect(&pattern, timeout_ms),
            Command::Drain {} => self.drain(),
            Command::Modem { dtr, rts } => {
                let changes = dtr.is_some() || rts.is_some();
                if changes {
                    self.ensure_writable("modem-line change")?;
                }
                let status = self
                    .transport
                    .modem_control(dtr, rts)
                    .map_err(|error| CommandError::from_transport(&error, changes))?;
                let effect = if changes {
                    SideEffect::ModemLinesChanged
                } else {
                    SideEffect::None
                };
                Ok(Outcome::Modem {
                    status,
                    effect: effect.into(),
                })
            }
            Command::Break { duration_ms } => {
                self.ensure_writable("break")?;
                self.transport
                    .send_break(Duration::from_millis(duration_ms.get()))
                    .map_err(|error| CommandError::from_transport(&error, true))?;
                Ok(Outcome::Break {
                    duration_ms,
                    effect: SideEffect::SerialBreakSent.into(),
                })
            }
            // Reached only without a concurrent reader, so no wait is running.
            Command::Cancel { target } => {
                let ticket = self.gate.request(target.as_ref());
                Ok(Outcome::Cancel {
                    cancelled: self.gate.fired(ticket),
                })
            }
        }
    }

    fn ensure_writable(&self, operation: &str) -> Result<(), CommandError> {
        if self.read_only {
            Err(CommandError::new(
                ErrorCode::ReadOnly,
                format!("{operation} is disabled by --read-only"),
                false,
            ))
        } else {
            Ok(())
        }
    }

    /// Receive into `pending`, leaving room for at most `limit` buffered bytes.
    /// Waits at most [`WAIT_SLICE`] so a cancel is noticed promptly.
    /// `Ok(false)` means the device disconnected.
    fn receive(&mut self, limit: usize, wait: Duration) -> Result<bool, CommandError> {
        let room = limit.saturating_sub(self.pending.len()).min(CHUNK);
        match self
            .transport
            .read_into(&mut self.pending, room, wait.min(WAIT_SLICE))
        {
            Ok(()) => Ok(true),
            Err(TransportError::Disconnected) => Ok(false),
            Err(error) => Err(CommandError::from_transport(&error, false)),
        }
    }

    /// Return and consume the first `count` pending bytes.
    fn take(&mut self, count: usize, start: Instant, end: End) -> Received {
        let rest = self.pending.split_off(count.min(self.pending.len()));
        Received {
            data: std::mem::replace(&mut self.pending, rest),
            elapsed: start.elapsed(),
            end,
            consumed: true,
            expect: None,
        }
    }

    /// Report the pending bytes without consuming them.
    fn peek(&self, start: Instant, end: End) -> Received {
        Received {
            data: self.pending.clone(),
            elapsed: start.elapsed(),
            end,
            consumed: false,
            expect: None,
        }
    }

    fn read(&mut self, bytes: usize, timeout_ms: TimeoutMs) -> Result<Outcome, CommandError> {
        let start = Instant::now();
        let timeout = timeout_ms.duration();
        let end = loop {
            if self.pending.len() >= bytes {
                break End::ByteLimit;
            }
            if self.gate.requested() {
                return Ok(Outcome::Received(self.peek(start, End::Cancelled)));
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break if self.pending.is_empty() {
                    End::Timeout
                } else {
                    End::Idle
                };
            }
            let before = self.pending.len();
            // Once data has arrived, a short silence ends the read.
            let wait = if before == 0 {
                remaining
            } else {
                remaining.min(DRAIN_WAIT)
            };
            if !self.receive(bytes, wait)? {
                break End::Disconnected;
            }
            if before > 0 && self.pending.len() == before {
                break End::Idle;
            }
        };
        Ok(Outcome::Received(self.take(bytes, start, end)))
    }

    fn readline(&mut self, timeout_ms: TimeoutMs) -> Result<Outcome, CommandError> {
        let start = Instant::now();
        let timeout = timeout_ms.duration();
        let mut scanned = 0;
        let end = loop {
            if let Some(offset) = memchr_newline(&self.pending[scanned..]) {
                let count = scanned + offset + 1;
                return Ok(Outcome::Received(self.take(count, start, End::Newline)));
            }
            scanned = self.pending.len();
            if self.pending.len() >= MAX_IO_BYTES {
                break End::ByteLimit;
            }
            if self.gate.requested() {
                return Ok(Outcome::Received(self.peek(start, End::Cancelled)));
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                // A partial line stays buffered for the next readline.
                return Ok(Outcome::Received(self.peek(start, End::Timeout)));
            }
            if !self.receive(MAX_IO_BYTES, remaining)? {
                break End::Disconnected;
            }
        };
        Ok(Outcome::Received(self.take(MAX_IO_BYTES, start, end)))
    }

    fn expect(&mut self, pattern: &str, timeout_ms: TimeoutMs) -> Result<Outcome, CommandError> {
        if pattern.len() > MAX_IO_BYTES {
            return Err(CommandError::invalid(format!(
                "pattern exceeds {MAX_IO_BYTES} bytes"
            )));
        }
        let regex = Regex::new(pattern)
            .map_err(|error| CommandError::invalid(format!("invalid byte regex: {error}")))?;
        let start = Instant::now();
        let timeout = timeout_ms.duration();
        let (count, end, matched) = loop {
            if let Some(found) = regex.find(&self.pending) {
                let range: Range<usize> = found.range();
                break (range.end, End::Match, Some(range));
            }
            if self.pending.len() >= MAX_IO_BYTES {
                break (MAX_IO_BYTES, End::BufferLimit, None);
            }
            let unfinished = if self.gate.requested() {
                Some(End::Cancelled)
            } else if timeout.saturating_sub(start.elapsed()).is_zero() {
                Some(End::Timeout)
            } else {
                None
            };
            if let Some(end) = unfinished {
                // Keep the bytes: a pattern split across two waits still matches.
                let mut received = self.peek(start, end);
                received.expect = Some(None);
                return Ok(Outcome::Received(received));
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if !self.receive(MAX_IO_BYTES, remaining)? {
                if let Some(found) = regex.find(&self.pending) {
                    let range = found.range();
                    break (range.end, End::Match, Some(range));
                }
                break (MAX_IO_BYTES, End::Disconnected, None);
            }
        };
        let mut received = self.take(count, start, end);
        received.expect = Some(matched);
        Ok(Outcome::Received(received))
    }

    fn drain(&mut self) -> Result<Outcome, CommandError> {
        let start = Instant::now();
        let mut end = End::Idle;
        while self.pending.len() < MAX_IO_BYTES {
            let before = self.pending.len();
            if !self.receive(MAX_IO_BYTES, DRAIN_WAIT)? {
                end = End::Disconnected;
                break;
            }
            if self.pending.len() == before {
                break;
            }
        }
        if end == End::Idle && self.pending.len() >= MAX_IO_BYTES {
            end = End::ByteLimit;
        }
        Ok(Outcome::Received(self.take(MAX_IO_BYTES, start, end)))
    }
}

/// Lets a `cancel` request, read while another request is running, stop
/// that request's wait.
///
/// The stdin reader calls [`CancelGate::request`] the moment a cancel line
/// arrives; the command thread checks [`CancelGate::requested`] between
/// receive slices and answers the cancel, in queue order, with
/// [`CommandHandler::finish_cancel`].
#[derive(Debug, Default)]
pub struct CancelGate(Mutex<GateState>);

#[derive(Debug, Default)]
struct GateState {
    running: Option<RunningWait>,
    next_ticket: u64,
    /// Tickets whose wait ended `cancelled`, not yet answered.
    fired: Vec<u64>,
}

#[derive(Debug)]
struct RunningWait {
    id: Option<RequestId>,
    ticket: Option<u64>,
}

impl CancelGate {
    fn state(&self) -> MutexGuard<'_, GateState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask the running wait to stop: any wait without `target`, otherwise
    /// only the wait whose request id equals `target`. A wait that is not
    /// running yet is never affected. Returns the ticket that
    /// [`CommandHandler::finish_cancel`] later reports on.
    pub fn request(&self, target: Option<&RequestId>) -> u64 {
        let mut state = self.state();
        let ticket = state.next_ticket;
        state.next_ticket += 1;
        if let Some(wait) = &mut state.running
            && wait.ticket.is_none()
            && target.is_none_or(|target| wait.id.as_ref() == Some(target))
        {
            wait.ticket = Some(ticket);
        }
        ticket
    }

    fn begin(&self, id: Option<RequestId>) {
        self.state().running = Some(RunningWait { id, ticket: None });
    }

    fn requested(&self) -> bool {
        self.state()
            .running
            .as_ref()
            .is_some_and(|wait| wait.ticket.is_some())
    }

    fn end(&self, cancelled: bool) {
        let mut state = self.state();
        if let Some(RunningWait {
            ticket: Some(ticket),
            ..
        }) = state.running.take()
            && cancelled
        {
            state.fired.push(ticket);
        }
    }

    fn fired(&self, ticket: u64) -> bool {
        let mut state = self.state();
        match state.fired.iter().position(|fired| *fired == ticket) {
            Some(index) => {
                state.fired.swap_remove(index);
                true
            }
            None => false,
        }
    }
}

fn memchr_newline(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|byte| *byte == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::parse_request;
    use crate::transport::LoopbackTransport;
    use serde_json::Value;

    fn run(handler: &mut CommandHandler, line: &str) -> Value {
        let response = match parse_request(line.as_bytes()) {
            Ok(request) => handler.execute(request),
            Err(rejected) => handler.reject(rejected),
        };
        serde_json::to_value(&response).unwrap()
    }

    fn open_loopback() -> CommandHandler {
        let mut handler = CommandHandler::new(Box::new(LoopbackTransport::new()), false);
        assert_eq!(
            run(&mut handler, r#"{"cmd":"open","port":"loop://"}"#)["ok"],
            true
        );
        handler
    }

    #[test]
    fn full_expect_window_returns_every_byte_and_recovers() {
        let mut handler = open_loopback();
        let chunk = "QUFB".repeat(10_000); // 30 000 bytes of 'A'
        for _ in 0..36 {
            let line = format!(r#"{{"cmd":"write","data_b64":"{chunk}"}}"#);
            assert_eq!(run(&mut handler, &line)["ok"], true);
        }
        let full = run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"ZZZ","timeout_ms":5000}"#,
        );
        assert_eq!(full["ok"], true, "{full}");
        assert_eq!(full["result"]["end"], "buffer_limit");
        assert_eq!(full["result"]["matched"], false);
        assert_eq!(full["result"]["bytes"], MAX_IO_BYTES);

        assert_eq!(
            run(&mut handler, r#"{"cmd":"write","text":"ZZZ"}"#)["ok"],
            true
        );
        let next = run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"ZZZ","timeout_ms":1000}"#,
        );
        assert_eq!(next["result"]["end"], "match", "{next}");
        // 36 * 30 000 - 1 MiB leftover bytes, then the match.
        assert_eq!(next["result"]["bytes"], 36 * 30_000 - MAX_IO_BYTES + 3);
    }

    #[test]
    fn expect_leaves_bytes_after_the_match_for_the_next_command() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"OK\r\nNEXT\n"}"#);
        let matched = run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"OK\\r\\n","timeout_ms":500}"#,
        );
        assert_eq!(matched["result"]["text"], "OK\r\n");
        assert_eq!(matched["result"]["matched_text"], "OK\r\n");
        let line = run(&mut handler, r#"{"cmd":"readline","timeout_ms":500}"#);
        assert_eq!(line["result"]["text"], "NEXT\n");
        assert_eq!(line["result"]["end"], "newline");
    }

    #[test]
    fn readline_returns_one_line_at_a_time() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"a\nb\npartial"}"#);
        for expected in ["a\n", "b\n"] {
            let line = run(&mut handler, r#"{"cmd":"readline","timeout_ms":200}"#);
            assert_eq!(line["result"]["text"], expected);
        }
        let rest = run(&mut handler, r#"{"cmd":"readline","timeout_ms":50}"#);
        assert_eq!(rest["result"]["text"], "partial");
        assert_eq!(rest["result"]["end"], "timeout");
    }

    #[test]
    fn read_returns_buffered_bytes_before_new_ones() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","hex":"00ff10"}"#);
        let first = run(&mut handler, r#"{"cmd":"read","bytes":2,"timeout_ms":200}"#);
        assert_eq!(first["result"]["data_b64"], "AP8=");
        assert_eq!(first["result"]["end"], "byte_limit");
        assert!(first["result"].get("text").is_none());
        let rest = run(&mut handler, r#"{"cmd":"drain"}"#);
        assert_eq!(rest["result"]["data_b64"], "EA==");
        let close = run(&mut handler, r#"{"cmd":"close"}"#);
        assert_eq!(close["result"]["discarded_bytes"], 0);
    }

    #[test]
    fn close_reports_bytes_it_discards() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"abcdef"}"#);
        run(&mut handler, r#"{"cmd":"read","bytes":2,"timeout_ms":200}"#);
        // The loopback hands over everything requested; leave bytes pending
        // through a match that ends early.
        run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"c","timeout_ms":200}"#,
        );
        let close = run(&mut handler, r#"{"cmd":"close"}"#);
        assert_eq!(close["result"]["discarded_bytes"], 3);
    }

    #[test]
    fn read_only_rejects_every_mutation() {
        let mut handler = CommandHandler::new(Box::new(LoopbackTransport::new()), true);
        run(&mut handler, r#"{"cmd":"open","port":"loop://"}"#);
        for line in [
            r#"{"cmd":"write","text":"x"}"#,
            r#"{"cmd":"break"}"#,
            r#"{"cmd":"modem","dtr":true}"#,
        ] {
            assert_eq!(
                run(&mut handler, line)["error"]["code"],
                "read_only",
                "{line}"
            );
        }
        assert_eq!(run(&mut handler, r#"{"cmd":"modem"}"#)["ok"], true);
    }

    #[test]
    fn a_pattern_split_across_timed_out_expects_still_matches() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"log: ZZ"}"#);
        let early = run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"ZZZ","timeout_ms":150}"#,
        );
        assert_eq!(early["result"]["end"], "timeout");
        assert_eq!(early["result"]["consumes_input"], false);
        assert_eq!(early["result"]["text"], "log: ZZ");
        run(&mut handler, r#"{"cmd":"write","text":"Z!"}"#);
        let matched = run(
            &mut handler,
            r#"{"cmd":"expect","pattern":"ZZZ","timeout_ms":500}"#,
        );
        assert_eq!(matched["result"]["end"], "match");
        assert_eq!(matched["result"]["consumes_input"], true);
        assert_eq!(matched["result"]["text"], "log: ZZZ");
        let rest = run(&mut handler, r#"{"cmd":"drain"}"#);
        assert_eq!(rest["result"]["text"], "!");
    }

    #[test]
    fn a_partial_line_survives_a_readline_timeout() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"hal"}"#);
        let early = run(&mut handler, r#"{"cmd":"readline","timeout_ms":150}"#);
        assert_eq!(early["result"]["end"], "timeout");
        assert_eq!(early["result"]["consumes_input"], false);
        run(&mut handler, r#"{"cmd":"write","text":"f\n"}"#);
        let line = run(&mut handler, r#"{"cmd":"readline","timeout_ms":500}"#);
        assert_eq!(line["result"]["text"], "half\n");
    }

    /// Run `line` while another thread sends a cancel for `target` after
    /// 200 ms; returns the request's response, the cancel's response, and
    /// how long the request took.
    fn cancel_during(
        handler: &mut CommandHandler,
        line: &str,
        target: Option<&str>,
    ) -> (Value, Value, Duration) {
        let gate = handler.cancel_gate();
        let target = target.map(|id| RequestId::Text(id.to_owned()));
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            gate.request(target.as_ref())
        });
        let start = Instant::now();
        let response = run(handler, line);
        let elapsed = start.elapsed();
        let ticket = canceller.join().unwrap();
        let cancel = serde_json::to_value(handler.finish_cancel(None, ticket)).unwrap();
        (response, cancel, elapsed)
    }

    #[test]
    fn targeted_cancel_stops_its_wait_and_keeps_the_bytes() {
        let mut handler = open_loopback();
        run(&mut handler, r#"{"cmd":"write","text":"booting"}"#);
        let (expect, cancel, elapsed) = cancel_during(
            &mut handler,
            r#"{"id":"a","cmd":"expect","pattern":"login:","timeout_ms":10000}"#,
            Some("a"),
        );
        assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
        assert_eq!(expect["id"], "a");
        assert_eq!(expect["result"]["end"], "cancelled");
        assert_eq!(expect["result"]["consumes_input"], false);
        assert_eq!(cancel["result"]["cancelled"], true);
        let rest = run(&mut handler, r#"{"cmd":"drain"}"#);
        assert_eq!(rest["result"]["text"], "booting");
    }

    #[test]
    fn untargeted_cancel_stops_any_running_wait() {
        let mut handler = open_loopback();
        let (read, cancel, elapsed) = cancel_during(
            &mut handler,
            r#"{"cmd":"read","bytes":10,"timeout_ms":10000}"#,
            None,
        );
        assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
        assert_eq!(read["result"]["end"], "cancelled");
        assert_eq!(cancel["result"]["cancelled"], true);
    }

    #[test]
    fn cancel_for_another_request_leaves_the_running_wait_alone() {
        // A timed out and finished; B is running when A's cancel arrives.
        let mut handler = open_loopback();
        let (b, cancel, elapsed) = cancel_during(
            &mut handler,
            r#"{"id":"b","cmd":"readline","timeout_ms":600}"#,
            Some("a"),
        );
        assert!(elapsed >= Duration::from_millis(600), "took {elapsed:?}");
        assert_eq!(b["result"]["end"], "timeout");
        assert_eq!(cancel["result"]["cancelled"], false);
    }

    #[test]
    fn cancel_with_nothing_running_reports_false() {
        let mut handler = open_loopback();
        let cancel = run(&mut handler, r#"{"cmd":"cancel"}"#);
        assert_eq!(cancel["result"]["cancelled"], false);
        // It is not armed for the next wait either.
        let start = Instant::now();
        let line = run(&mut handler, r#"{"cmd":"readline","timeout_ms":300}"#);
        assert_eq!(line["result"]["end"], "timeout");
        assert!(start.elapsed() >= Duration::from_millis(300));
    }
}
