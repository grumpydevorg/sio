---
name: sio
description: Use sio to list serial ports, identify USB UARTs, open UART or serial consoles, send and receive binary-safe data, automate AT commands, and diagnose ttyUSB, ttyACM, or cu.usbserial devices. Use when a user mentions a serial port, UART, serial console, modem, GPS serial stream, AT command, or serial adapter.
---

# sio — serial I/O

Use `sio` for local serial work. Prefer its persistent NDJSON interface for
agent automation and `sio terminal` for an interactive human console.

## Safety boundaries

- Treat `open`, `write`, `modem`, and `break` as hardware operations. Validate
  the port and framing first.
- Opening a port can toggle modem lines or reset attached hardware. DTR defaults
  to `preserve`, but an OS or USB adapter can still pulse it during open.
- Start unfamiliar devices with `sio stdio --read-only`. That mode rejects
  writes, breaks, and changes to DTR or RTS.
- Reads consume input. Do not retry a write merely because the following read
  timed out; the first write may already have reached the device.
- Use SSH to run `sio` on a remote host. `sio` deliberately has no unauthenticated
  TCP or HTTP server.

## Find the port

```bash
sio list
sio list --json
```

On macOS, prefer `/dev/cu.*` for initiating a connection. When the adapter has
a USB serial number, use `--serial SERIAL` instead of a path; `sio` resolves an
exact serial-number match and rejects ambiguity.

## Interactive console

```bash
sio terminal --serial BH00T16X --baud 115200
sio terminal --port /dev/ttyUSB0 --baud 115200
```

The terminal sends keyboard input and displays received bytes. Press `Ctrl+]`
to exit. `Ctrl+C` is forwarded to the serial device.

## One transaction

Provide exactly one payload encoding:

```bash
sio transact --port /dev/ttyUSB0 --text 'AT\r' --expect 'OK' --timeout-ms 5000
sio transact --serial BH00T16X --hex '00ff0d0a' --bytes 256 --json
sio transact --loopback --port loop:// --data-b64 'AP8=' --bytes 2 --json
```

`--expect` is a byte-oriented regular expression. The command reports whether
it matched, all received bytes as `data_b64`, valid UTF-8 as `text`, elapsed
time, and why the read ended. Text payloads interpret `\n`, `\r`, `\t`, `\\`,
and `\xHH`.

## Persistent agent session

Keep one `sio stdio` process alive. Separate invocations do not share an open
serial session. Send one JSON object per line and read exactly one response line
before sending the next command.

```bash
printf '%s\n' \
  '{"cmd":"open","port":"loop://","baud":115200}' \
  '{"cmd":"write","text":"ping"}' \
  '{"cmd":"read","bytes":4,"timeout_ms":1000}' \
  '{"cmd":"close"}' \
  | sio stdio --loopback
```

Commands are:

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

Unknown fields and invalid combinations are errors. In particular, specify one
of `port` or `serial`, and one of `text`, `hex`, or `data_b64` for a write.

Responses contain `protocol_version`, `session_id`, `command`, and `ok`, plus
either `result` or a structured `error` with `code`, `message`, and `retryable`.
Write and control responses describe their side effect and retry safety.
Diagnostics use stderr; stdout remains NDJSON only. Add `--fail-fast` when a
pipeline should exit nonzero after its first protocol error.

Limits: request line 64 KiB; one payload/read/expect buffer 1 MiB; ordinary
timeouts 1–60,000 ms; break duration 1–10,000 ms.

## Reproduce this skill from the binary

The binary embeds this exact file:

```bash
sio skill
sio skill --output ./SKILL.md
sio skill --output ./SKILL.md --force
sio skill --check ./SKILL.md
```

`sio skills` is an alias. A write refuses to replace a differing file unless
`--force` is present; `--check` exits nonzero when the file and binary differ.
