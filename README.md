# sio

`sio` is a small serial terminal and an automation-safe NDJSON tool. It keeps
human terminal traffic separate from machine-readable output, preserves binary
data, validates commands before touching hardware, and has no network listener.
Use SSH when the serial port is on another machine.

## Quick start

```console
$ sio list
$ sio terminal --serial BH00T16X --baud 115200
$ sio transact --port /dev/cu.usbserial-BH00T16X --text 'AT\r' --expect OK
```

Interactive terminal mode forwards keyboard input to the device. Press
`Ctrl+]` to exit; `Ctrl+C` is sent to the serial peer.

For an agent or script, keep one `sio stdio` process alive so that its open
serial session survives across commands:

```console
$ printf '%s\n' \
  '{"cmd":"open","port":"loop://"}' \
  '{"cmd":"write","text":"ping"}' \
  '{"cmd":"read","bytes":4,"timeout_ms":1000}' \
  '{"cmd":"close"}' | sio stdio --loopback
```

Every stdout line is one response object. Diagnostics go to stderr. Received
bytes are always returned as Base64 in `data_b64`; valid UTF-8 is also included
as `text`.

## Self-documenting agent skill

The canonical agent skill is embedded in the binary:

```console
$ sio skill                         # print it
$ sio skill --output ./SKILL.md     # write it without clobbering
$ sio skill --output ./SKILL.md --force
$ sio skill --check ./SKILL.md      # detect version drift
```

`sio skills` is an alias for `sio skill`. The checked-in
`skills/sio/SKILL.md` is the source embedded at compile time and tested for an
exact round trip.

## Build

```console
$ cargo test
$ cargo clippy --all-targets --all-features -- -D warnings
$ nix build
$ nix flake check
```

The crate, executable, package, and project are all named `sio`.

## Safety model

- Opening a serial port can toggle modem lines or reset attached boards. DTR
  defaults to `preserve`; operating systems and USB adapters may still pulse it.
- Mutating commands report their side effect and whether retrying is safe.
- `sio stdio --read-only` rejects writes, breaks, and modem-line changes.
- Request lines, reads, timeouts, break durations, and accumulated expect data
  have explicit limits.
- Reads consume bytes from the serial stream. A timeout is an ordinary result,
  not permission to retry a write.
- No command binds a TCP or HTTP socket.
