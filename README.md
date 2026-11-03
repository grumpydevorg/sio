# sio

`sio` is a serial terminal for people and an NDJSON session protocol for
agents and scripts. It keeps terminal traffic apart from machine-readable
output, preserves binary data, validates every request before touching
hardware, and opens no network socket: run it on the machine with the port,
directly, over SSH or with `tendr --host`.

## Install

```console
$ cargo binstall sio-serial        # prebuilt binary from the GitHub release
$ cargo install sio-serial         # build from crates.io
$ nix run github:grumpydevorg/sio -- --version
```

The crate is `sio-serial` because `sio` was taken on crates.io; the command
and library are `sio`. Each [release](https://github.com/grumpydevorg/sio/releases)
has `sio-<target>.tar.gz` archives for Linux (glibc 2.35 or newer), macOS and
Windows on x86-64 and ARM64, each with a SHA-256 file and a build
attestation: `gh attestation verify <archive> --repo grumpydevorg/sio`.
Building on Linux needs `libudev` and `pkg-config`.

## Quick start

```console
$ sio list
$ sio terminal --serial BH00T16X --baud 115200
$ sio transact --port /dev/ttyUSB0 --text 'AT\r' --expect OK
```

`sio terminal` sends keyboard input to the device. `Ctrl+]` exits; `Ctrl+C`
goes to the device. `--crlf` shows bare LF from the device as CR LF.

For an agent or script, keep one `sio stdio` process alive so its open port
survives across requests:

```console
$ printf '%s\n' \
  '{"id":1,"cmd":"open","port":"loop://"}' \
  '{"id":2,"cmd":"write","text":"ping"}' \
  '{"id":3,"cmd":"read","bytes":4,"timeout_ms":1000}' \
  '{"id":4,"cmd":"close"}' | sio stdio --loopback
```

Each stdout line is one response and echoes its request's `id`; diagnostics
go to stderr. Received bytes come back as Base64 in `data_b64`, plus `text`
when they are valid UTF-8. Bytes not yet returned stay buffered for the next
read, and an `expect` or `readline` that times out keeps its bytes, so short
waits in a loop lose nothing. `{"cmd":"cancel","target":"<id>"}` stops a
running wait within about 100 ms. The full protocol is in the
[embedded skill](skills/sio/SKILL.md).

## Ports on each platform

| Platform | Port names | Notes |
|---|---|---|
| Linux | `/dev/ttyUSB0`, `/dev/ttyACM0` | Without access to the device (usually the `dialout` or `uucp` group) `open` fails with `permission_denied`. |
| macOS | `/dev/cu.usbserial-…` | Each adapter also appears as `/dev/tty.…`, which can block on open; `sio list` shows only the `cu.` name. |
| Windows | `COM3` | A busy port and a missing port are told apart by whether Windows lists it. |

`--serial <number>` (or `"serial"` in `open`) selects an adapter by its USB
serial number on every platform and needs exactly one match.

## With tendr

A shell tool call ends its processes, so an agent runs `sio stdio` under
[tendr](https://github.com/grumpydevorg/tendr), locally or with `--host`. With
tendr 0.5.0 or newer, the `ndjson` exec target sends one request per
`tendr exec` and returns the reply carrying that request's id, so a late reply
to a timed-out request is never taken for the next one:

```console
$ tendr start --stdin --exec-target ndjson serial -- sio stdio --read-only
$ tendr exec serial -- '{"cmd":"open","serial":"BH00T16X"}'
$ tendr exec serial -- '{"cmd":"readline","timeout_ms":5000}'
```

`tendr start --pty con -- sio terminal …` runs a console that a person can
`tendr attach` to while tendr keeps a timestamped log. With tendr 0.7.0 or
newer, `tendr attach --read-only con` watches it without taking control.

## Safety model

- Opening a port can toggle modem lines and reset attached boards. DTR
  defaults to `preserve`, but an operating system or USB adapter may still
  pulse it.
- `sio stdio --read-only` rejects writes, breaks and modem-line changes.
- Mutating results report their side effect and whether a retry is safe.
- Reads consume the serial stream. A timeout is an ordinary result, not
  permission to retry a write.
- Request lines, payloads, reads, timeouts, break durations and the expect
  window all have explicit limits.

## Status

CI builds and tests every change on Linux, macOS and Windows. The tests drive
an in-memory loopback device; a Linux pseudo-terminal has also been used by
hand. sio has not yet been tested with a physical USB serial adapter, so
opening real hardware, unplug handling and busy-port detection are unproven
on every platform. Bytes the operating system still held when a device hung
up can be lost, because `serialport` reports the hang-up first. There is no
option for RTS/CTS flow control yet. Changes are listed in `CHANGELOG.md`.

## Agent skill

The agent skill is embedded in the binary:

```console
$ sio skill                         # print it
$ sio skill --output ./SKILL.md     # write it; refuses to replace a different file
$ sio skill --output ./SKILL.md --force
$ sio skill --check ./SKILL.md      # exit nonzero if the file and binary differ
```

`sio skills` is an alias. `skills/sio/SKILL.md` is compiled into the binary
and a test checks the round trip exactly.

## Design

```text
stdin NDJSON → protocol (parse + validate) → server::CommandHandler → transport
                                             (pending-byte buffer)    ├ LocalTransport (serialport)
                                                                      └ LoopbackTransport (echo)
```

- Requests are validated while they are parsed: baud, framing, sizes and
  timeouts are range-checked types, so an invalid request never reaches a
  transport. Responses are typed; `end`, `side_effect` and error `code` are
  enums.
- `LocalTransport` classifies failures as `port_not_found`, `port_busy`,
  `permission_denied` or `disconnected`; a vanished device closes the port.
- `sio stdio` reads stdin on its own thread so a `cancel` can reach a running
  wait. Requests run one at a time and reply in order, except that a `cancel`
  which stops nothing replies at once. Waits poll in 100 ms slices.
- The queue behind a running request is bounded: past 16 MiB of request
  lines a request is answered `queue_full`, and at 1024 queued requests stdin
  is not read until the queue drains.

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
Pushing a `v*` tag runs `.github/workflows/release.yml`; run `cargo publish`
only after its `publish` job is green, because `cargo binstall` fetches from
the GitHub release.

## License

MIT or Apache-2.0, at your option.
