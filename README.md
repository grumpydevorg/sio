# sio

`sio` is a small serial terminal and an automation-safe NDJSON tool. It keeps
human terminal traffic separate from machine-readable output, preserves binary
data, validates commands before touching hardware, and has no network listener.
Run it on the machine with the port, over SSH or `tendr --host`.

## Quick start

```console
$ sio list
$ sio terminal --serial BH00T16X --baud 115200
$ sio transact --port /dev/cu.usbserial-BH00T16X --text 'AT\r' --expect OK
```

Interactive terminal mode forwards keyboard input to the device. Press
`Ctrl+]` to exit; `Ctrl+C` is sent to the serial peer. `--crlf` displays bare
LF from the device as CR LF.

For an agent or script, keep one `sio stdio` process alive so that its open
serial session survives across commands:

```console
$ printf '%s\n' \
  '{"id":1,"cmd":"open","port":"loop://"}' \
  '{"id":2,"cmd":"write","text":"ping"}' \
  '{"id":3,"cmd":"read","bytes":4,"timeout_ms":1000}' \
  '{"id":4,"cmd":"close"}' | sio stdio --loopback
```

Every stdout line is one response object and echoes the request's `id`.
Diagnostics go to stderr. Received bytes are always returned as Base64 in
`data_b64`; valid UTF-8 is also included as `text`. Bytes received but not yet
returned stay buffered for the next read, so nothing is dropped between
commands. The full protocol is in the embedded skill.

## With tendr

[tendr](https://github.com/grumpydevorg/tendr) keeps `sio stdio` running
between an agent's tool calls, locally or with `--host`. A tendr with the
`ndjson` exec target (`tendr guide ndjson`) sends one request per `tendr exec`
and returns the reply that carries the request's id, so a reply that arrives
after a client timeout is never mistaken for the next request's reply:

```console
$ tendr start --stdin --exec-target ndjson serial -- sio stdio --read-only
$ tendr exec serial -- '{"cmd":"open","serial":"BH00T16X"}'
$ tendr exec serial -- '{"cmd":"readline","timeout_ms":5000}'
```

`tendr start --pty con -- sio terminal …` gives a console that a person can
`tendr attach` to while tendr keeps a timestamped capture.

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

## Design

```text
stdin NDJSON → protocol (parse + validate) → server::CommandHandler → transport
                                             (pending-byte buffer)    ├ LocalTransport (serialport)
                                                                      └ LoopbackTransport (echo)
```

- Requests are validated while they are parsed: baud, data and stop bits,
  sizes and timeouts are range-checked types, so an invalid request never
  reaches a transport.
- Responses are typed; `end`, `side_effect` and error `code` values are enums.
- `LocalTransport` classifies failures: a missing path is `port_not_found`, a
  locked port `port_busy`, and a vanished device closes the port and reports
  `disconnected`. Bytes the operating system still held when the device hung
  up can be lost; `serialport` reports the hang-up before them.
- A command blocks the session until it returns; timeouts are at most 60 s.

## Build and check

```console
$ cargo fmt --all --check
$ cargo clippy --all-targets --all-features -- -D warnings   # pedantic via [lints]
$ cargo test
$ cargo deny check
$ cargo audit
$ nix build          # runs the tests and an install check
$ nix flake check
```

The toolchain is pinned in `rust-toolchain.toml` and matches `rust-version`.
The crate, executable, package, and project are all named `sio`.

## Safety model

- Opening a serial port can toggle modem lines or reset attached boards. DTR
  defaults to `preserve`; operating systems and USB adapters may still pulse it.
- Mutating commands report their side effect and whether retrying is safe.
- `sio stdio --read-only` rejects writes, breaks, and modem-line changes.
- Request lines, reads, timeouts, break durations, and the expect window have
  explicit limits.
- Reads consume bytes from the serial stream. A timeout is an ordinary result,
  not permission to retry a write.
- No command binds a TCP or HTTP socket.

## License

MIT or Apache-2.0, at your option.
