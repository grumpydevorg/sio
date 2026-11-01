---
name: sio
description: Use sio to list serial ports, identify USB UARTs, open UART or serial consoles, send and receive binary-safe data, automate AT commands, and diagnose ttyUSB, ttyACM, or cu.usbserial devices. Use when a user mentions a serial port, UART, serial console, modem, GPS serial stream, AT command, or serial adapter.
---

# sio — serial I/O

Use `sio` for local serial work: `sio stdio` for agent automation, kept alive
by tendr when available, and `sio terminal` for an interactive human console.

## Safety boundaries

- Treat `open`, `write`, `modem`, and `break` as hardware operations. Validate
  the port and framing first.
- Opening a port can toggle modem lines or reset attached hardware. DTR defaults
  to `preserve`, but an OS or USB adapter can still pulse it during open.
- Start unfamiliar devices with `sio stdio --read-only`. That mode rejects
  writes, breaks, and changes to DTR or RTS.
- Reads consume input. Do not retry a write because a later read timed out; the
  write may already have reached the device. `retry_safe` on a result and
  `retryable` on an error say whether repeating that exact request is harmless.
- Run `sio` on the machine with the port (over SSH or `tendr --host`). `sio`
  has no network listener.

## Find the port

```bash
sio list
sio list --json
```

On macOS, prefer `/dev/cu.*`. When the adapter has a USB serial number, use
`--serial SERIAL` instead of a path; `sio` requires exactly one match.

## Interactive console

```bash
sio terminal --serial BH00T16X --baud 115200
sio terminal --port /dev/ttyUSB0 --baud 115200 --crlf
```

Keyboard input goes to the device; `Ctrl+]` exits and `Ctrl+C` is forwarded.
`--crlf` displays bare LF from the device as CR LF. The terminal exits nonzero
when the device disconnects.

## One transaction

Provide exactly one payload encoding:

```bash
sio transact --port /dev/ttyUSB0 --text 'AT\r' --expect 'OK' --timeout-ms 5000
sio transact --serial BH00T16X --hex '00ff0d0a' --bytes 256 --json
sio transact --loopback --port loop:// --data-b64 'AP8=' --bytes 2 --json
```

Text payloads interpret `\n`, `\r`, `\t`, `\\`, and `\xHH`. Each transaction
opens and closes the port, which may reset the device; use a session for
repeated work. The command exits nonzero when the pattern does not arrive.

## Session protocol (`sio stdio`)

One process holds one open port across commands. Send one JSON object per
line; each produces exactly one response line on stdout. Diagnostics go to
stderr. `--fail-fast` exits nonzero after the first error response.

```json
{"cmd":"list"}
{"cmd":"open","port":"/dev/ttyUSB0","baud":115200,"bits":8,"parity":"none","stop":1,"dtr":"preserve","rts":"preserve"}
{"cmd":"write","text":"AT\r"}
{"cmd":"write","hex":"00ff"}
{"cmd":"write","data_b64":"AP8="}
{"cmd":"read","bytes":256,"timeout_ms":1000}
{"cmd":"readline","timeout_ms":1000}
{"cmd":"expect","pattern":"OK|ERROR","timeout_ms":5000}
{"cmd":"drain"}
{"cmd":"modem"}
{"cmd":"modem","dtr":false,"rts":false}
{"cmd":"break","duration_ms":250}
{"cmd":"close"}
```

Use `"serial":"BH00T16X"` instead of `"port"` to open by USB serial number.
Unknown fields, out-of-range values and invalid combinations are rejected
before any hardware is touched.

**Correlation.** Add `"id"` (a string up to 128 bytes, or a non-negative
integer) to any request; the response echoes it, including when the command is
invalid. Lines that are not JSON objects get a response without `id`.

**Responses** hold `protocol_version` (2), `id` when given, `session_id`,
`command`, `ok`, and either `result` or `error` (`code`, `message`,
`retryable`). Error codes: `invalid_request`, `read_only`, `not_open`,
`already_open`, `port_not_found`, `ambiguous_port`, `port_busy`,
`permission_denied`, `disconnected`, `transport`.

**Received data.** `read`, `readline`, `expect`, and `drain` return
`data_b64`, `text` (only when the bytes are valid UTF-8), `bytes`,
`elapsed_ms`, and `end`:

| `end` | Meaning |
|---|---|
| `byte_limit` | `read` got the requested count, or a 1 MiB window filled |
| `idle` | data arrived, then the line went quiet |
| `timeout` | the timeout elapsed; any partial data is returned |
| `newline` | `readline` returned one line including `\n` |
| `match` | `expect` returned everything up to the end of the match |
| `buffer_limit` | `expect` received 1 MiB without a match; all of it is returned |
| `disconnected` | the device went away; the port is closed; reopen it |

Bytes received but not yet returned stay buffered for the next read-type
command, so `expect` followed by `readline` sees the bytes after the match.
`expect` adds `matched`, and `matched_b64`/`matched_text` when it matched.
`close` reports `discarded_bytes` still buffered.

Limits: request line 64 KiB; payload, read, and expect window 1 MiB; timeouts
1–60000 ms; break 1–10000 ms. A command blocks the session until it returns.

## Keep a session alive with tendr

A shell tool call ends its processes, so a plain `sio stdio` dies between
calls. Run it under tendr instead.

With a tendr that has the `ndjson` exec target (`tendr guide ndjson`), `exec`
sends one request and returns the reply carrying its own id; replies to an
earlier timed-out exec appear in `unmatched`, never as your reply:

```bash
tendr start --stdin --exec-target ndjson serial -- sio stdio --read-only
tendr exec serial -- '{"cmd":"open","serial":"BH00T16X","baud":115200}'
tendr exec serial -- '{"cmd":"expect","pattern":"login:","timeout_ms":10000}' \
  | jq -r '.stdout | fromjson | .result.text'
tendr exec serial -- '{"cmd":"close"}'
tendr kill serial
```

`exec` exits 0 for `"ok":true`, 1 for an error response, 124 when tendr stops
waiting. Give `tendr exec --timeout` more than the request's `timeout_ms`.
Without that target, use `printf '%s\n' '{"id":"r1",…}' | tendr push serial`
and find the response with `"id":"r1"` in `tendr log serial`. For a human
console that an agent can also drive, use
`tendr start --pty con -- sio terminal …` with `tendr attach con` and
`tendr push con`; tendr's log keeps a timestamped capture.

## Reproduce this skill from the binary

```bash
sio skill
sio skill --output ./SKILL.md
sio skill --output ./SKILL.md --force
sio skill --check ./SKILL.md
```

`sio skills` is an alias. A write refuses to replace a differing file unless
`--force` is present; `--check` exits nonzero when the file and binary differ.
