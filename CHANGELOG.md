# Changelog

## v0.4.0 — first public release (2026-11-02)

The crate is published as `sio-serial`; the command is `sio`. Licensed MIT
OR Apache-2.0.

### Changed (breaking)

- **NDJSON protocol version 2.** Responses are typed: `end`, `side_effect`
  and error `code` take fixed values. A full `expect` window is now a result
  with `end: "buffer_limit"` and all its bytes, not an error.
- **Received bytes stay buffered until a command returns them.** `expect`
  returns data through the end of its match and leaves the rest for the next
  command; `close` reports `discarded_bytes`.
- **An `expect` or `readline` that times out leaves its bytes buffered**
  (`consumes_input: false`), so short waits in a loop still find a pattern
  or line split across two waits.
- **`list`, `close` and `drain` reject unknown fields**, like every other
  command.

### Added

- **Request ids.** Any request may carry `id` (a string up to 128 bytes or a
  non-negative integer); every response echoes it, including invalid
  commands.
- **`cancel`.** `{"cmd":"cancel","target":"<id>"}` stops a running `read`,
  `readline` or `expect` within about 100 ms (`end: "cancelled"`, bytes
  kept), ahead of queued requests. A cancel that stops nothing is answered
  `cancelled: false` at once.
- **Error codes** `port_not_found`, `port_busy`, `permission_denied`,
  `disconnected` and `queue_full`. A vanished device closes the port and
  reads end with `end: "disconnected"`.
- **`sio terminal --crlf`** shows bare LF from the device as CR LF.
- **tendr integration.** With tendr 0.5.0 or newer,
  `tendr start --stdin --exec-target ndjson … -- sio stdio` keeps a session
  alive between tool calls; see the embedded skill.

### Fixed

- A full `expect` buffer no longer makes every later `expect` fail.
- Writes get a timeout derived from baud rate and length instead of the
  last read's 1–50 ms driver timeout.
- The terminal shows bytes that arrived just before a disconnect.

### Limits

- Request lines up to 8 MiB. Behind a running request sio queues at most
  16 MiB of request lines and 1024 requests; beyond the byte budget a
  request is answered `queue_full`. Error messages are capped at 512 bytes.

### Known gaps

- Hardware flow control (RTS/CTS) cannot be selected; ports open without
  flow control.
- Tested against a Linux pseudo-terminal and the in-memory loopback device;
  not yet against a physical USB serial adapter.
