//! `sio` command-line interface.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Args, Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use sio::protocol::{Command, CommandError, LineState, MAX_LINE_BYTES, Parity, Response};
use sio::server::{CommandHandler, discover_ports, resolve_port, validated_config};
use sio::transport::{LocalTransport, LoopbackTransport, SerialTransport};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const EMBEDDED_SKILL: &str = include_str!("../skills/sio/SKILL.md");

#[derive(Parser)]
#[command(
    name = "sio",
    version,
    about = "Safe serial I/O for terminals and automation"
)]
#[command(after_help = "Examples:
  sio list
  sio terminal --serial BH00T16X --baud 115200
  sio transact --port /dev/ttyUSB0 --text 'AT\\r' --expect OK
  sio stdio --read-only
  sio skill --output ./SKILL.md")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// List available serial ports.
    List {
        /// Emit a versioned JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Open a bidirectional interactive terminal. Press Ctrl+] to exit.
    Terminal {
        #[command(flatten)]
        connection: ConnectionArgs,
    },
    /// Write once and optionally read or expect a response.
    Transact {
        #[command(flatten)]
        connection: ConnectionArgs,
        #[command(flatten)]
        payload: PayloadArgs,
        /// Wait for a byte-oriented regular expression.
        #[arg(long, conflicts_with = "bytes")]
        expect: Option<String>,
        /// Read at most this many bytes after writing.
        #[arg(long, value_name = "COUNT")]
        bytes: Option<usize>,
        /// Overall read or expect timeout.
        #[arg(long, default_value = "5000")]
        timeout_ms: u64,
        /// Emit the final versioned JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Process one JSON command per stdin line and emit one response per line.
    Stdio {
        /// Use an in-memory echo device instead of hardware.
        #[arg(long)]
        loopback: bool,
        /// Reject writes, breaks, and modem-line changes.
        #[arg(long)]
        read_only: bool,
        /// Exit nonzero after emitting the first error response.
        #[arg(long)]
        fail_fast: bool,
    },
    /// Print, write, or verify the exact embedded agent skill.
    #[command(alias = "skills")]
    Skill {
        /// Write the skill atomically to this file.
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        output: Option<PathBuf>,
        /// Verify that this file exactly matches the embedded skill.
        #[arg(long, value_name = "PATH", conflicts_with = "output")]
        check: Option<PathBuf>,
        /// Replace a differing output file.
        #[arg(long, requires = "output")]
        force: bool,
    },
}

#[derive(Args, Clone)]
struct ConnectionArgs {
    /// Serial device path.
    #[arg(long, value_name = "PATH", conflicts_with = "serial")]
    port: Option<String>,
    /// Exact USB serial number; resolves to one preferred device path.
    #[arg(long, value_name = "SERIAL", conflicts_with = "port")]
    serial: Option<String>,
    /// Baud rate.
    #[arg(short, long, default_value = "115200")]
    baud: u32,
    /// Data bits: 5, 6, 7, or 8.
    #[arg(long, default_value = "8")]
    bits: u8,
    /// Parity.
    #[arg(long, value_enum, default_value = "none")]
    parity: Parity,
    /// Stop bits: 1 or 2.
    #[arg(long, default_value = "1")]
    stop: u8,
    /// DTR behavior while opening the port.
    #[arg(long, value_enum, default_value = "preserve")]
    dtr: LineState,
    /// RTS state immediately after opening the port.
    #[arg(long, value_enum, default_value = "preserve")]
    rts: LineState,
    /// Use an in-memory echo device instead of hardware.
    #[arg(long)]
    loopback: bool,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct PayloadArgs {
    /// Text payload with \n, \r, \t, \\, and \xHH escapes.
    #[arg(long)]
    text: Option<String>,
    /// Hexadecimal byte payload; ASCII whitespace is ignored.
    #[arg(long)]
    hex: Option<String>,
    /// Base64-encoded byte payload.
    #[arg(long, value_name = "BASE64")]
    data_b64: Option<String>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        None | Some(Commands::List { json: false }) => run_list(false),
        Some(Commands::List { json: true }) => run_list(true),
        Some(Commands::Terminal { connection }) => run_terminal(&connection),
        Some(Commands::Transact {
            connection,
            payload,
            expect,
            bytes,
            timeout_ms,
            json,
        }) => run_transact(&connection, payload, expect, bytes, timeout_ms, json),
        Some(Commands::Stdio {
            loopback,
            read_only,
            fail_fast,
        }) => run_stdio(loopback, read_only, fail_fast),
        Some(Commands::Skill {
            output,
            check,
            force,
        }) => run_skill(output.as_deref(), check.as_deref(), force),
    }
}

fn run_list(json_output: bool) -> Result<()> {
    if json_output {
        let mut handler = CommandHandler::new(Box::new(LocalTransport::new()), true);
        let response = handler.execute(Command::List);
        println!("{}", serde_json::to_string_pretty(&response)?);
        if !response.ok {
            bail!(response_message(&response));
        }
        return Ok(());
    }

    let ports = discover_ports()?;
    if ports.is_empty() {
        println!("No serial ports found");
        return Ok(());
    }
    for port in ports {
        let description = port
            .product
            .as_deref()
            .or(port.manufacturer.as_deref())
            .unwrap_or(&port.port_type);
        match port.serial {
            Some(serial) => println!("{:<32} {:<24} serial={serial}", port.path, description),
            None => println!("{:<32} {description}", port.path),
        }
    }
    Ok(())
}

fn run_transact(
    connection: &ConnectionArgs,
    payload: PayloadArgs,
    expect: Option<String>,
    bytes: Option<usize>,
    timeout_ms: u64,
    json_output: bool,
) -> Result<()> {
    let transport: Box<dyn SerialTransport> = if connection.loopback {
        Box::new(LoopbackTransport::new())
    } else {
        Box::new(LocalTransport::new())
    };
    let mut handler = CommandHandler::new(transport, false);

    let open = handler.execute(open_command(connection));
    ensure_response(&open, json_output)?;

    let write = handler.execute(Command::Write {
        text: payload.text,
        hex: payload.hex,
        data_b64: payload.data_b64,
    });
    if !write.ok {
        let _ = handler.execute(Command::Close);
        ensure_response(&write, json_output)?;
    }

    let final_response = if let Some(pattern) = expect {
        handler.execute(Command::Expect {
            pattern,
            timeout_ms,
        })
    } else if let Some(bytes) = bytes {
        handler.execute(Command::Read { bytes, timeout_ms })
    } else {
        write
    };
    let _ = handler.execute(Command::Close);

    emit_transaction_response(&final_response, json_output)?;
    if !final_response.ok {
        bail!(response_message(&final_response));
    }
    if final_response.command == "expect"
        && final_response
            .result
            .as_ref()
            .and_then(|result| result.get("matched"))
            .and_then(serde_json::Value::as_bool)
            == Some(false)
    {
        bail!("timeout waiting for expected pattern");
    }
    Ok(())
}

fn open_command(connection: &ConnectionArgs) -> Command {
    Command::Open {
        port: connection.port.clone(),
        serial: connection.serial.clone(),
        baud: connection.baud,
        bits: connection.bits,
        parity: connection.parity,
        stop: connection.stop,
        dtr: connection.dtr,
        rts: connection.rts,
    }
}

fn ensure_response(response: &Response, json_output: bool) -> Result<()> {
    if response.ok {
        return Ok(());
    }
    if json_output {
        println!("{}", serde_json::to_string(response)?);
    }
    bail!(response_message(response))
}

fn emit_transaction_response(response: &Response, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(response)?);
        return Ok(());
    }
    let Some(result) = response.result.as_ref() else {
        return Ok(());
    };
    if let Some(data_b64) = result.get("data_b64").and_then(serde_json::Value::as_str) {
        let bytes = BASE64
            .decode(data_b64)
            .context("response contained invalid Base64")?;
        io::stdout().write_all(&bytes)?;
        io::stdout().flush()?;
    } else if let Some(bytes) = result.get("bytes").and_then(serde_json::Value::as_u64) {
        println!("wrote {bytes} bytes");
    }
    Ok(())
}

fn response_message(response: &Response) -> String {
    response
        .error
        .as_ref()
        .map(|error| format!("{}: {}", error.code, error.message))
        .unwrap_or_else(|| "command failed".to_owned())
}

fn run_stdio(loopback: bool, read_only: bool, fail_fast: bool) -> Result<()> {
    let transport: Box<dyn SerialTransport> = if loopback {
        Box::new(LoopbackTransport::new())
    } else {
        Box::new(LocalTransport::new())
    };
    let mut handler = CommandHandler::new(transport, read_only);
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    let mut line = Vec::with_capacity(4096);

    while let Some(too_long) = read_limited_line(&mut reader, &mut line)? {
        trim_line_ending(&mut line);
        if line.is_empty() && !too_long {
            continue;
        }
        let response = if too_long {
            Response::failure(
                handler.session_id(),
                "parse",
                CommandError::invalid(format!("request line exceeds {MAX_LINE_BYTES} bytes")),
            )
        } else {
            match serde_json::from_slice::<Command>(&line) {
                Ok(command) => handler.execute(command),
                Err(error) => Response::failure(
                    handler.session_id(),
                    "parse",
                    CommandError::invalid(format!("invalid command JSON: {error}")),
                ),
            }
        };
        let failed = !response.ok;
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        if failed && fail_fast {
            bail!("stdio stopped after protocol error");
        }
    }
    Ok(())
}

fn read_limited_line<R: BufRead>(reader: &mut R, output: &mut Vec<u8>) -> io::Result<Option<bool>> {
    output.clear();
    let mut too_long = false;
    let mut saw_input = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(saw_input.then_some(too_long));
        }
        saw_input = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_bytes = newline.unwrap_or(available.len());
        if output.len() < MAX_LINE_BYTES + 1 {
            let keep = content_bytes.min(MAX_LINE_BYTES + 1 - output.len());
            output.extend_from_slice(&available[..keep]);
        }
        if output.len() > MAX_LINE_BYTES {
            too_long = true;
        }
        reader.consume(content_bytes + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(Some(too_long));
        }
    }
}

fn trim_line_ending(line: &mut Vec<u8>) {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
}

fn run_terminal(connection: &ConnectionArgs) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("terminal mode requires terminal stdin and stdout; use `sio stdio` for automation");
    }
    let path = resolve_port(connection.port.as_deref(), connection.serial.as_deref())?;
    let config = validated_config(
        &path,
        connection.baud,
        connection.bits,
        connection.parity.into(),
        connection.stop,
        connection.dtr,
        connection.rts,
    )?;
    let mut transport: Box<dyn SerialTransport> = if connection.loopback {
        Box::new(LoopbackTransport::new())
    } else {
        Box::new(LocalTransport::new())
    };
    transport.open(&config)?;
    eprintln!(
        "Connected to {path} at {} baud (Ctrl+] exits; opening may pulse DTR/RTS)",
        connection.baud
    );

    let _raw_mode = RawModeGuard::enter()?;
    let mut stdout = io::stdout().lock();
    loop {
        let received = transport.read(4096, Duration::from_millis(10))?;
        if !received.is_empty() {
            stdout.write_all(&received)?;
            stdout.flush()?;
        }
        if event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) => match terminal_key(key) {
                    TerminalAction::Exit => break,
                    TerminalAction::Bytes(bytes) if !bytes.is_empty() => {
                        transport.write(&bytes)?;
                    }
                    TerminalAction::Bytes(_) => {}
                },
                Event::Paste(text) => {
                    transport.write(text.as_bytes())?;
                }
                _ => {}
            }
        }
    }
    drop(_raw_mode);
    transport.close()?;
    eprintln!("\nDisconnected");
    Ok(())
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

enum TerminalAction {
    Exit,
    Bytes(Vec<u8>),
}

fn terminal_key(key: KeyEvent) -> TerminalAction {
    if key.kind == KeyEventKind::Release {
        return TerminalAction::Bytes(Vec::new());
    }
    if key.code == KeyCode::Char('\u{1d}')
        || (key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::CONTROL))
        || (key.code == KeyCode::Char('5') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        return TerminalAction::Exit;
    }

    let mut bytes = Vec::new();
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if ('4'..='7').contains(&character) {
                bytes.push(0x1c + character as u8 - b'4');
            } else if character.is_ascii() {
                bytes.push((character.to_ascii_uppercase() as u8) & 0x1f);
            }
        }
        KeyCode::Char(character) => {
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => bytes.extend_from_slice(b"\x1b[2~"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        _ => {}
    }
    TerminalAction::Bytes(bytes)
}

fn run_skill(output: Option<&Path>, check: Option<&Path>, force: bool) -> Result<()> {
    if let Some(path) = check {
        let actual =
            fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
        if actual != EMBEDDED_SKILL.as_bytes() {
            bail!("{} differs from the skill embedded in sio", path.display());
        }
        println!(
            "{} matches sio {}",
            path.display(),
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if let Some(path) = output {
        return write_skill(path, force);
    }
    io::stdout().write_all(EMBEDDED_SKILL.as_bytes())?;
    Ok(())
}

fn write_skill(path: &Path, force: bool) -> Result<()> {
    match fs::read(path) {
        Ok(existing) => {
            if existing == EMBEDDED_SKILL.as_bytes() {
                return Ok(());
            }
            if !force {
                bail!(
                    "{} exists and differs; pass --force to replace it",
                    path.display()
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("SKILL.md");
    let temporary = parent.join(format!(".{filename}.sio-{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("could not create {}", temporary.display()))?;
        file.write_all(EMBEDDED_SKILL.as_bytes())?;
        file.sync_all()?;
        if force {
            fs::rename(&temporary, path)
                .with_context(|| format!("could not replace {}", path.display()))?;
        } else {
            fs::hard_link(&temporary, path).with_context(|| {
                format!("could not create {}; it may already exist", path.display())
            })?;
            fs::remove_file(&temporary)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_reader_discards_an_oversized_line() {
        let input = format!(
            "{}\n{{\"cmd\":\"list\"}}\n",
            "x".repeat(MAX_LINE_BYTES + 20)
        );
        let mut reader = BufReader::new(input.as_bytes());
        let mut line = Vec::new();
        assert_eq!(
            read_limited_line(&mut reader, &mut line).unwrap(),
            Some(true)
        );
        assert_eq!(
            read_limited_line(&mut reader, &mut line).unwrap(),
            Some(false)
        );
        trim_line_ending(&mut line);
        assert_eq!(line, br#"{"cmd":"list"}"#);
    }

    #[test]
    fn limited_reader_accepts_exactly_the_documented_limit() {
        let input = format!("{}\n", "x".repeat(MAX_LINE_BYTES));
        let mut reader = BufReader::new(input.as_bytes());
        let mut line = Vec::new();
        assert_eq!(
            read_limited_line(&mut reader, &mut line).unwrap(),
            Some(false)
        );
        assert_eq!(line.len(), MAX_LINE_BYTES);
    }

    #[test]
    fn ctrl_bracket_exits_but_ctrl_c_is_forwarded() {
        assert!(matches!(
            terminal_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
            TerminalAction::Exit
        ));
        assert!(matches!(
            terminal_key(KeyEvent::new(KeyCode::Char('\u{1d}'), KeyModifiers::NONE)),
            TerminalAction::Exit
        ));
        assert!(matches!(
            terminal_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL)),
            TerminalAction::Exit
        ));
        match terminal_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)) {
            TerminalAction::Bytes(bytes) => assert_eq!(bytes, vec![3]),
            TerminalAction::Exit => panic!("Ctrl+C must be forwarded"),
        }
    }
}
