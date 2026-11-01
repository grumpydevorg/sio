//! `sio` command-line interface.

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use sio::protocol::{
    Command, CommandError, End, MAX_LINE_BYTES, OpenParams, Outcome, Payload, ReadSize, Rejected,
    Request, Response, TimeoutMs, parse_request,
};
use sio::server::CommandHandler;
use sio::transport::{LocalTransport, LoopbackTransport, SerialTransport, list_ports};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const EMBEDDED_SKILL: &str = include_str!("../skills/sio/SKILL.md");

/// How long the terminal waits for device bytes before checking the keyboard.
const TERMINAL_POLL: Duration = Duration::from_millis(10);

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
        /// Emit one versioned JSON response.
        #[arg(long)]
        json: bool,
    },
    /// Open a bidirectional interactive terminal. Press Ctrl+] to exit.
    Terminal {
        #[command(flatten)]
        connection: ConnectionArgs,
        /// Display each received LF as CR LF, for devices that send bare LF.
        #[arg(long)]
        crlf: bool,
    },
    /// Write once and optionally read or expect a response.
    Transact {
        #[command(flatten)]
        connection: ConnectionArgs,
        #[command(flatten)]
        payload: Payload,
        /// Wait for a byte-oriented regular expression.
        #[arg(long, conflicts_with = "bytes")]
        expect: Option<String>,
        /// Read at most this many bytes after writing.
        #[arg(long, value_name = "COUNT")]
        bytes: Option<ReadSize>,
        /// Read or expect timeout, 1-60000 ms.
        #[arg(long, default_value = "5000")]
        timeout_ms: TimeoutMs,
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

#[derive(Args)]
struct ConnectionArgs {
    #[command(flatten)]
    open: OpenParams,
    /// Use an in-memory echo device instead of hardware.
    #[arg(long)]
    loopback: bool,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        None => run_list(false),
        Some(Commands::List { json }) => run_list(json),
        Some(Commands::Terminal { connection, crlf }) => run_terminal(&connection, crlf),
        Some(Commands::Transact {
            connection,
            payload,
            expect,
            bytes,
            timeout_ms,
            json,
        }) => run_transact(connection, payload, expect, bytes, timeout_ms, json),
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

fn transport(loopback: bool) -> Box<dyn SerialTransport> {
    if loopback {
        Box::new(LoopbackTransport::new())
    } else {
        Box::new(LocalTransport::new())
    }
}

const fn request(command: Command) -> Request {
    Request { id: None, command }
}

fn run_list(json_output: bool) -> Result<()> {
    if json_output {
        let mut handler = CommandHandler::new(transport(false), true);
        let response = handler.execute(request(Command::List {}));
        println!("{}", serde_json::to_string(&response)?);
        if let Err(error) = &response.outcome {
            bail!("{error}");
        }
        return Ok(());
    }

    let ports = list_ports()?;
    if ports.is_empty() {
        println!("No serial ports found");
    }
    for port in ports {
        let description = port
            .product
            .as_deref()
            .or(port.manufacturer.as_deref())
            .unwrap_or(port.port_type);
        match port.serial {
            Some(serial) => println!("{:<32} {description:<24} serial={serial}", port.path),
            None => println!("{:<32} {description}", port.path),
        }
    }
    Ok(())
}

fn run_transact(
    connection: ConnectionArgs,
    payload: Payload,
    expect: Option<String>,
    bytes: Option<ReadSize>,
    timeout_ms: TimeoutMs,
    json_output: bool,
) -> Result<()> {
    let mut handler = CommandHandler::new(transport(connection.loopback), false);
    let open = handler.execute(request(Command::Open(connection.open)));
    if !open.is_ok() {
        return finish_transaction(&open, json_output);
    }
    let write = handler.execute(request(Command::Write(payload)));
    let last = if !write.is_ok() {
        write
    } else if let Some(pattern) = expect {
        handler.execute(request(Command::Expect {
            pattern,
            timeout_ms,
        }))
    } else if let Some(bytes) = bytes {
        handler.execute(request(Command::Read { bytes, timeout_ms }))
    } else {
        write
    };
    // Close failures cannot change the outcome; the process exits next.
    let _ = handler.execute(request(Command::Close {}));
    finish_transaction(&last, json_output)
}

fn finish_transaction(response: &Response, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(response)?);
    }
    match &response.outcome {
        Err(error) => bail!("{error}"),
        Ok(Outcome::Received(received)) => {
            if !json_output {
                let mut stdout = io::stdout().lock();
                stdout.write_all(&received.data)?;
                stdout.flush()?;
            }
            let unmatched = received.expect == Some(None);
            match received.end {
                End::Disconnected => bail!("the serial device disconnected"),
                End::Timeout if unmatched => bail!("timed out waiting for the expected pattern"),
                End::BufferLimit => {
                    bail!(
                        "received {} bytes without the expected pattern",
                        received.data.len()
                    )
                }
                _ => {}
            }
        }
        Ok(Outcome::Written { bytes, .. }) if !json_output => println!("wrote {bytes} bytes"),
        Ok(_) => {}
    }
    Ok(())
}

fn run_stdio(loopback: bool, read_only: bool, fail_fast: bool) -> Result<()> {
    let mut handler = CommandHandler::new(transport(loopback), read_only);
    let mut reader = BufReader::new(io::stdin().lock());
    let mut stdout = io::stdout().lock();
    let mut line = Vec::with_capacity(4096);

    while let Some(too_long) = read_limited_line(&mut reader, &mut line)? {
        trim_line_ending(&mut line);
        if line.is_empty() && !too_long {
            continue;
        }
        let response = if too_long {
            handler.reject(Rejected {
                id: None,
                command: "parse",
                error: CommandError::invalid(format!(
                    "request line exceeds {MAX_LINE_BYTES} bytes"
                )),
            })
        } else {
            match parse_request(&line) {
                Ok(request) => handler.execute(request),
                Err(rejected) => handler.reject(rejected),
            }
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        if fail_fast && !response.is_ok() {
            bail!("stdio stopped after an error response");
        }
    }
    Ok(())
}

/// Read one line into `output`, keeping at most `MAX_LINE_BYTES + 1` bytes.
/// Returns `None` at end of input, otherwise whether the line was too long.
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
        if output.len() <= MAX_LINE_BYTES {
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

fn run_terminal(connection: &ConnectionArgs, crlf: bool) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("terminal mode requires terminal stdin and stdout; use `sio stdio` for automation");
    }
    let config = connection.open.resolve()?;
    let mut transport = transport(connection.loopback);
    transport.open(&config)?;
    eprintln!(
        "Connected to {} at {} baud (Ctrl+] exits; opening may pulse DTR/RTS)",
        config.path, config.baud
    );
    let raw_mode = RawModeGuard::enter()?;
    let result = terminal_loop(transport.as_mut(), crlf);
    drop(raw_mode);
    // The port is already closed when the device disconnected.
    let _ = transport.close();
    result?;
    eprintln!("Disconnected");
    Ok(())
}

fn terminal_loop(transport: &mut dyn SerialTransport, crlf: bool) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let mut received = Vec::with_capacity(4096);
    let mut display = Vec::with_capacity(8192);
    let mut keys = Vec::with_capacity(16);
    loop {
        received.clear();
        transport.read_into(&mut received, 4096, TERMINAL_POLL)?;
        if !received.is_empty() {
            if crlf {
                display.clear();
                for &byte in &received {
                    if byte == b'\n' {
                        display.push(b'\r');
                    }
                    display.push(byte);
                }
                stdout.write_all(&display)?;
            } else {
                stdout.write_all(&received)?;
            }
            stdout.flush()?;
        }
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                keys.clear();
                if !terminal_key(key, &mut keys) {
                    return Ok(());
                }
                if !keys.is_empty() {
                    transport.write(&keys)?;
                }
            }
        }
    }
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

/// Append the bytes `key` sends to the device. Returns `false` for the exit
/// key (Ctrl+], which some terminals report as Ctrl+5).
fn terminal_key(key: KeyEvent, out: &mut Vec<u8>) -> bool {
    if key.kind == KeyEventKind::Release {
        return true;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if key.code == KeyCode::Char('\u{1d}')
        || (control && matches!(key.code, KeyCode::Char(']' | '5')))
    {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        out.push(0x1b);
    }
    let sequence: &[u8] = match key.code {
        KeyCode::Char(character) if control => {
            if let Ok(byte) = u8::try_from(character)
                && byte.is_ascii()
            {
                out.push(match byte {
                    b'4'..=b'7' => 0x1c + byte - b'4',
                    _ => byte.to_ascii_uppercase() & 0x1f,
                });
            }
            &[]
        }
        KeyCode::Char(character) => {
            let mut encoded = [0_u8; 4];
            out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            &[]
        }
        KeyCode::Enter => b"\r",
        KeyCode::Tab => b"\t",
        KeyCode::BackTab => b"\x1b[Z",
        KeyCode::Backspace => b"\x7f",
        KeyCode::Esc => b"\x1b",
        KeyCode::Up => b"\x1b[A",
        KeyCode::Down => b"\x1b[B",
        KeyCode::Right => b"\x1b[C",
        KeyCode::Left => b"\x1b[D",
        KeyCode::Home => b"\x1b[H",
        KeyCode::End => b"\x1b[F",
        KeyCode::Delete => b"\x1b[3~",
        KeyCode::Insert => b"\x1b[2~",
        KeyCode::PageUp => b"\x1b[5~",
        KeyCode::PageDown => b"\x1b[6~",
        _ => &[],
    };
    out.extend_from_slice(sequence);
    true
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

    fn key_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        terminal_key(KeyEvent::new(code, modifiers), &mut out).then_some(out)
    }

    #[test]
    fn ctrl_bracket_exits_but_ctrl_c_is_forwarded() {
        assert_eq!(key_bytes(KeyCode::Char(']'), KeyModifiers::CONTROL), None);
        assert_eq!(key_bytes(KeyCode::Char('\u{1d}'), KeyModifiers::NONE), None);
        assert_eq!(key_bytes(KeyCode::Char('5'), KeyModifiers::CONTROL), None);
        assert_eq!(
            key_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(KeyCode::Char('x'), KeyModifiers::ALT),
            Some(b"\x1bx".to_vec())
        );
    }
}
