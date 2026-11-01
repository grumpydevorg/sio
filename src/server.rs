//! Stateful command handling for the local NDJSON interface.

use crate::protocol::{
    Command, CommandError, LineState as ProtocolLineState, MAX_BREAK_MS, MAX_IO_BYTES,
    MAX_TIMEOUT_MS, PortInfo as ProtocolPortInfo, Response,
};
use crate::transport::{LineState, PortConfig, SerialTransport, TransportError, list_ports};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use regex::bytes::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct CommandHandler {
    transport: Box<dyn SerialTransport>,
    expect_buffer: Vec<u8>,
    read_only: bool,
    session_id: String,
}

impl CommandHandler {
    #[must_use]
    pub fn new(transport: Box<dyn SerialTransport>, read_only: bool) -> Self {
        let epoch_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self {
            transport,
            expect_buffer: Vec::with_capacity(4096),
            read_only,
            session_id: format!("{}-{epoch_nanos}", std::process::id()),
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn execute(&mut self, command: Command) -> Response {
        let name = command.name();
        match self.execute_inner(command) {
            Ok(result) => Response::success(&self.session_id, name, result),
            Err(error) => Response::failure(&self.session_id, name, error),
        }
    }

    fn execute_inner(&mut self, command: Command) -> Result<Value, CommandError> {
        match command {
            Command::List => Ok(json!({ "ports": discover_ports()? })),
            Command::Open {
                port,
                serial,
                baud,
                bits,
                parity,
                stop,
                dtr,
                rts,
            } => {
                let path = resolve_port(port.as_deref(), serial.as_deref())?;
                let config = validated_config(&path, baud, bits, parity.into(), stop, dtr, rts)?;
                self.transport.open(&config).map_err(map_effect_error)?;
                self.expect_buffer.clear();
                Ok(json!({
                    "port": path,
                    "baud": baud,
                    "bits": bits,
                    "parity": format!("{parity:?}").to_lowercase(),
                    "stop": stop,
                    "dtr": line_state_name(dtr),
                    "rts": line_state_name(rts),
                    "side_effect": "serial_port_opened",
                    "retry_safe": false,
                    "warning": "Opening a serial port may pulse DTR or RTS on some operating systems and adapters"
                }))
            }
            Command::Close => {
                self.transport.close().map_err(map_transport_error)?;
                self.expect_buffer.clear();
                Ok(json!({ "side_effect": "serial_port_closed", "retry_safe": true }))
            }
            Command::Write {
                text,
                hex,
                data_b64,
            } => {
                self.ensure_writable("write")?;
                let (encoding, bytes) = decode_payload(text, hex, data_b64)?;
                let written = self.transport.write(&bytes).map_err(map_effect_error)?;
                if written != bytes.len() {
                    return Err(CommandError::new(
                        "short_write",
                        format!("wrote {written} of {} bytes", bytes.len()),
                        false,
                    ));
                }
                Ok(json!({
                    "encoding": encoding,
                    "bytes": written,
                    "side_effect": "serial_bytes_written",
                    "retry_safe": false
                }))
            }
            Command::Read { bytes, timeout_ms } => {
                validate_io_size(bytes)?;
                validate_timeout(timeout_ms)?;
                let start = Instant::now();
                let data = self
                    .transport
                    .read(bytes, Duration::from_millis(timeout_ms))
                    .map_err(map_transport_error)?;
                let end = if data.len() == bytes {
                    "byte_limit"
                } else if data.is_empty() {
                    "timeout"
                } else {
                    "idle"
                };
                Ok(read_result(&data, start.elapsed(), end))
            }
            Command::Readline { timeout_ms } => self.readline(timeout_ms),
            Command::Expect {
                pattern,
                timeout_ms,
            } => self.expect(&pattern, timeout_ms),
            Command::Drain => {
                let data = self.transport.drain().map_err(map_transport_error)?;
                let end = if data.len() == MAX_IO_BYTES {
                    "byte_limit"
                } else {
                    "idle"
                };
                Ok(read_result(&data, Duration::ZERO, end))
            }
            Command::Modem { dtr, rts } => {
                if dtr.is_some() || rts.is_some() {
                    self.ensure_writable("modem-line change")?;
                }
                let status = self.transport.modem_control(dtr, rts).map_err(|error| {
                    if dtr.is_some() || rts.is_some() {
                        map_effect_error(error)
                    } else {
                        map_transport_error(error)
                    }
                })?;
                Ok(json!({
                    "dtr": status.dtr,
                    "rts": status.rts,
                    "cts": status.cts,
                    "dsr": status.dsr,
                    "dcd": status.dcd,
                    "side_effect": if dtr.is_some() || rts.is_some() { "modem_lines_changed" } else { "none" },
                    "retry_safe": dtr.is_none() && rts.is_none()
                }))
            }
            Command::Break { duration_ms } => {
                self.ensure_writable("break")?;
                if !(1..=MAX_BREAK_MS).contains(&duration_ms) {
                    return Err(CommandError::invalid(format!(
                        "duration_ms must be between 1 and {MAX_BREAK_MS}"
                    )));
                }
                self.transport
                    .send_break(Duration::from_millis(duration_ms))
                    .map_err(map_effect_error)?;
                Ok(json!({
                    "duration_ms": duration_ms,
                    "side_effect": "serial_break_sent",
                    "retry_safe": false
                }))
            }
        }
    }

    fn ensure_writable(&self, operation: &str) -> Result<(), CommandError> {
        if self.read_only {
            Err(CommandError::new(
                "read_only",
                format!("{operation} is disabled by --read-only"),
                false,
            ))
        } else {
            Ok(())
        }
    }

    fn readline(&mut self, timeout_ms: u64) -> Result<Value, CommandError> {
        validate_timeout(timeout_ms)?;
        let start = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);
        let mut line = Vec::new();

        while start.elapsed() < timeout && line.len() < MAX_IO_BYTES {
            let remaining = timeout.saturating_sub(start.elapsed());
            let data = self
                .transport
                .read(1, remaining)
                .map_err(map_transport_error)?;
            if data.is_empty() {
                break;
            }
            line.push(data[0]);
            if data[0] == b'\n' {
                break;
            }
        }

        let end = if line.last() == Some(&b'\n') {
            "newline"
        } else if line.len() == MAX_IO_BYTES {
            "byte_limit"
        } else if line.is_empty() {
            "timeout"
        } else {
            "idle"
        };
        Ok(read_result(&line, start.elapsed(), end))
    }

    fn expect(&mut self, pattern: &str, timeout_ms: u64) -> Result<Value, CommandError> {
        validate_timeout(timeout_ms)?;
        if pattern.len() > MAX_IO_BYTES {
            return Err(CommandError::invalid(format!(
                "pattern exceeds {MAX_IO_BYTES} bytes"
            )));
        }
        let regex = Regex::new(pattern)
            .map_err(|error| CommandError::invalid(format!("invalid byte regex: {error}")))?;
        let start = Instant::now();
        let timeout = Duration::from_millis(timeout_ms);

        while start.elapsed() < timeout {
            if let Some(found) = regex.find(&self.expect_buffer) {
                return Ok(self.finish_expect(Some(found.range()), start.elapsed(), "match"));
            }
            if self.expect_buffer.len() == MAX_IO_BYTES {
                return Err(CommandError::new(
                    "buffer_limit",
                    format!("expect buffer reached {MAX_IO_BYTES} bytes"),
                    false,
                ));
            }

            let remaining = timeout.saturating_sub(start.elapsed());
            let room = MAX_IO_BYTES - self.expect_buffer.len();
            let data = self
                .transport
                .read(room.min(4096), remaining.min(Duration::from_millis(100)))
                .map_err(map_transport_error)?;
            if data.is_empty() {
                continue;
            }
            self.expect_buffer.extend_from_slice(&data);
            if let Some(range) = regex.find(&self.expect_buffer).map(|found| found.range()) {
                return Ok(self.finish_expect(Some(range), start.elapsed(), "match"));
            }
        }

        Ok(self.finish_expect(None, start.elapsed(), "timeout"))
    }

    fn finish_expect(
        &mut self,
        matched_range: Option<std::ops::Range<usize>>,
        elapsed: Duration,
        end: &str,
    ) -> Value {
        let data = std::mem::take(&mut self.expect_buffer);
        let mut result = read_result(&data, elapsed, end);
        let object = result.as_object_mut().expect("read result is an object");
        object.insert("matched".to_owned(), json!(matched_range.is_some()));
        if let Some(range) = matched_range {
            let matched = &data[range];
            object.insert("matched_b64".to_owned(), json!(BASE64.encode(matched)));
            if let Ok(text) = std::str::from_utf8(matched) {
                object.insert("matched_text".to_owned(), json!(text));
            }
        }
        result
    }
}

pub fn discover_ports() -> Result<Vec<ProtocolPortInfo>, CommandError> {
    let mut ports: BTreeMap<String, ProtocolPortInfo> = BTreeMap::new();
    for port in list_ports().map_err(map_transport_error)? {
        let recommended = !cfg!(target_os = "macos") || port.path.starts_with("/dev/cu.");
        let key = macos_pair_key(&port.path);
        let candidate = ProtocolPortInfo {
            path: port.path,
            port_type: port.port_type,
            recommended_for_open: recommended,
            vid: port.vid,
            pid: port.pid,
            serial: port.serial,
            manufacturer: port.manufacturer,
            product: port.product,
        };
        match ports.get(&key) {
            Some(existing) if existing.recommended_for_open || !candidate.recommended_for_open => {}
            _ => {
                ports.insert(key, candidate);
            }
        }
    }
    Ok(ports.into_values().collect())
}

fn macos_pair_key(path: &str) -> String {
    if cfg!(target_os = "macos") {
        path.strip_prefix("/dev/cu.")
            .or_else(|| path.strip_prefix("/dev/tty."))
            .unwrap_or(path)
            .to_owned()
    } else {
        path.to_owned()
    }
}

pub fn resolve_port(port: Option<&str>, serial: Option<&str>) -> Result<String, CommandError> {
    match (port, serial) {
        (Some(path), None) if !path.is_empty() => Ok(path.to_owned()),
        (None, Some(serial)) if !serial.is_empty() => {
            let matches: Vec<_> = discover_ports()?
                .into_iter()
                .filter(|port| port.serial.as_deref() == Some(serial))
                .collect();
            match matches.as_slice() {
                [] => Err(CommandError::new(
                    "port_not_found",
                    format!("no serial port has USB serial number {serial:?}"),
                    false,
                )),
                [port] => Ok(port.path.clone()),
                _ => Err(CommandError::new(
                    "ambiguous_port",
                    format!("more than one serial port has USB serial number {serial:?}"),
                    false,
                )),
            }
        }
        (Some(_), Some(_)) => Err(CommandError::invalid(
            "specify exactly one of port or serial",
        )),
        _ => Err(CommandError::invalid(
            "specify exactly one non-empty port or serial",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn validated_config(
    path: &str,
    baud: u32,
    bits: u8,
    parity: crate::transport::Parity,
    stop: u8,
    dtr: ProtocolLineState,
    rts: ProtocolLineState,
) -> Result<PortConfig, CommandError> {
    if path.is_empty() {
        return Err(CommandError::invalid("port cannot be empty"));
    }
    if !(1..=4_000_000).contains(&baud) {
        return Err(CommandError::invalid("baud must be between 1 and 4000000"));
    }
    if !(5..=8).contains(&bits) {
        return Err(CommandError::invalid("bits must be 5, 6, 7, or 8"));
    }
    if !matches!(stop, 1 | 2) {
        return Err(CommandError::invalid("stop must be 1 or 2"));
    }
    Ok(PortConfig::new(path, baud)
        .data_bits(bits)
        .parity(parity)
        .stop_bits(stop)
        .dtr_on_open(LineState::from(dtr))
        .rts_on_open(LineState::from(rts)))
}

pub fn decode_payload(
    text: Option<String>,
    hex: Option<String>,
    data_b64: Option<String>,
) -> Result<(&'static str, Vec<u8>), CommandError> {
    let supplied =
        usize::from(text.is_some()) + usize::from(hex.is_some()) + usize::from(data_b64.is_some());
    if supplied != 1 {
        return Err(CommandError::invalid(
            "specify exactly one of text, hex, or data_b64",
        ));
    }

    let (encoding, bytes) = if let Some(value) = text {
        ("text", decode_text_escapes(&value)?)
    } else if let Some(value) = hex {
        let compact: String = value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        if !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CommandError::invalid("hex contains a non-hexadecimal byte"));
        }
        if compact.len() % 2 != 0 {
            return Err(CommandError::invalid(
                "hex must contain complete byte pairs",
            ));
        }
        let decoded = (0..compact.len())
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&compact[index..index + 2], 16)
                    .map_err(|_| CommandError::invalid("hex contains a non-hexadecimal byte"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        ("hex", decoded)
    } else {
        let value = data_b64.expect("one payload variant is present");
        let decoded = BASE64
            .decode(value)
            .map_err(|error| CommandError::invalid(format!("invalid Base64 payload: {error}")))?;
        ("base64", decoded)
    };

    if bytes.is_empty() {
        return Err(CommandError::invalid("payload cannot be empty"));
    }
    if bytes.len() > MAX_IO_BYTES {
        return Err(CommandError::invalid(format!(
            "payload exceeds {MAX_IO_BYTES} bytes"
        )));
    }
    Ok((encoding, bytes))
}

pub fn decode_text_escapes(value: &str) -> Result<Vec<u8>, CommandError> {
    let mut output = Vec::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut bytes = [0_u8; 4];
            output.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => output.push(b'\n'),
            Some('r') => output.push(b'\r'),
            Some('t') => output.push(b'\t'),
            Some('\\') => output.push(b'\\'),
            Some('x') => {
                let high = chars
                    .next()
                    .and_then(|value| value.to_digit(16))
                    .ok_or_else(|| {
                        CommandError::invalid("\\x must be followed by two hex digits")
                    })?;
                let low = chars
                    .next()
                    .and_then(|value| value.to_digit(16))
                    .ok_or_else(|| {
                        CommandError::invalid("\\x must be followed by two hex digits")
                    })?;
                output.push((high * 16 + low) as u8);
            }
            Some(other) => {
                return Err(CommandError::invalid(format!(
                    "unsupported text escape \\{other}"
                )));
            }
            None => return Err(CommandError::invalid("text ends with an incomplete escape")),
        }
    }
    Ok(output)
}

fn validate_io_size(bytes: usize) -> Result<(), CommandError> {
    if !(1..=MAX_IO_BYTES).contains(&bytes) {
        Err(CommandError::invalid(format!(
            "bytes must be between 1 and {MAX_IO_BYTES}"
        )))
    } else {
        Ok(())
    }
}

fn validate_timeout(timeout_ms: u64) -> Result<(), CommandError> {
    if !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        Err(CommandError::invalid(format!(
            "timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"
        )))
    } else {
        Ok(())
    }
}

fn read_result(data: &[u8], elapsed: Duration, end: &str) -> Value {
    let mut result = json!({
        "data_b64": BASE64.encode(data),
        "bytes": data.len(),
        "elapsed_ms": u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        "end": end,
        "consumes_input": true
    });
    if let Ok(text) = std::str::from_utf8(data) {
        result
            .as_object_mut()
            .expect("read result is an object")
            .insert("text".to_owned(), json!(text));
    }
    result
}

fn line_state_name(state: ProtocolLineState) -> &'static str {
    match state {
        ProtocolLineState::Preserve => "preserve",
        ProtocolLineState::On => "on",
        ProtocolLineState::Off => "off",
    }
}

fn map_transport_error(error: TransportError) -> CommandError {
    match error {
        TransportError::NotOpen => CommandError::new("not_open", error.to_string(), false),
        TransportError::AlreadyOpen => CommandError::new("already_open", error.to_string(), false),
        TransportError::InvalidConfig(_) => {
            CommandError::new("invalid_request", error.to_string(), false)
        }
        TransportError::NotSupported(_) => {
            CommandError::new("not_supported", error.to_string(), false)
        }
        TransportError::Timeout => CommandError::new("timeout", error.to_string(), true),
        TransportError::ConnectionFailed(_)
        | TransportError::WriteFailed(_)
        | TransportError::ReadFailed(_)
        | TransportError::Io(_) => CommandError::new("transport", error.to_string(), true),
    }
}

fn map_effect_error(error: TransportError) -> CommandError {
    let code = match &error {
        TransportError::NotOpen => "not_open",
        TransportError::AlreadyOpen => "already_open",
        TransportError::InvalidConfig(_) => "invalid_request",
        TransportError::NotSupported(_) => "not_supported",
        TransportError::Timeout => "timeout",
        TransportError::ConnectionFailed(_)
        | TransportError::WriteFailed(_)
        | TransportError::ReadFailed(_)
        | TransportError::Io(_) => "transport",
    };
    CommandError::new(code, error.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::LoopbackTransport;

    #[test]
    fn payload_requires_exactly_one_encoding() {
        assert!(decode_payload(None, None, None).is_err());
        assert!(decode_payload(Some("x".into()), Some("78".into()), None).is_err());
        assert!(decode_payload(None, Some("€€".into()), None).is_err());
    }

    #[test]
    fn loopback_preserves_binary_bytes() {
        let mut handler = CommandHandler::new(Box::new(LoopbackTransport::new()), false);
        assert!(
            handler
                .execute(Command::Open {
                    port: Some("loop://".into()),
                    serial: None,
                    baud: 115_200,
                    bits: 8,
                    parity: crate::protocol::Parity::None,
                    stop: 1,
                    dtr: ProtocolLineState::Preserve,
                    rts: ProtocolLineState::Preserve,
                })
                .ok
        );
        assert!(
            handler
                .execute(Command::Write {
                    text: None,
                    hex: Some("00ff".into()),
                    data_b64: None,
                })
                .ok
        );
        let response = handler.execute(Command::Read {
            bytes: 2,
            timeout_ms: 100,
        });
        assert_eq!(response.result.unwrap()["data_b64"], "AP8=");
    }
}
