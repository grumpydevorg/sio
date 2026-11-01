//! Versioned NDJSON protocol shared by `sio stdio`, `sio transact` and
//! `sio list --json`.
//!
//! Requests are validated entirely during parsing: a [`Command`] that exists
//! holds only in-range values. Responses are typed, so field names and enum
//! values on the wire are fixed by the compiler.

use crate::transport::{
    BaudRate, DataBits, InvalidSetting, LineState, ModemStatus, Parity, PortConfig, PortInfo,
    StopBits, list_ports,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::Args;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::ops::Range;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

pub const PROTOCOL_VERSION: u8 = 2;
/// Longest accepted request line, excluding the newline.
pub const MAX_LINE_BYTES: usize = 64 * 1024;
/// Largest payload, read, or expect window.
pub const MAX_IO_BYTES: usize = 1024 * 1024;
/// Longest accepted string request id.
pub const MAX_ID_BYTES: usize = 128;

macro_rules! bounded_u64 {
    ($(#[$meta:meta])* $name:ident, $min:expr, $max:expr, $default:expr, $message:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "u64", into = "u64")]
        pub struct $name(u64);

        impl $name {
            pub const MAX: u64 = $max;

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self($default)
            }
        }

        impl TryFrom<u64> for $name {
            type Error = InvalidSetting;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                if ($min..=$max).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(InvalidSetting($message))
                }
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidSetting;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value
                    .parse::<u64>()
                    .map_err(|_| InvalidSetting($message))
                    .and_then(Self::try_from)
            }
        }
    };
}

bounded_u64!(
    /// How long a read, readline or expect may wait, in milliseconds.
    TimeoutMs,
    1,
    60_000,
    5_000,
    "timeout_ms must be between 1 and 60000"
);

bounded_u64!(
    /// Most bytes one read may return.
    ReadSize,
    1,
    1_048_576,
    256,
    "bytes must be between 1 and 1048576"
);

bounded_u64!(
    /// Break duration in milliseconds.
    BreakMs,
    1,
    10_000,
    500,
    "duration_ms must be between 1 and 10000"
);

impl TimeoutMs {
    #[must_use]
    pub const fn duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

impl ReadSize {
    #[must_use]
    pub fn bytes(self) -> usize {
        // Bounded by MAX_IO_BYTES, which fits every supported usize.
        usize::try_from(self.0).unwrap_or(MAX_IO_BYTES)
    }
}

/// Where and how to open a port. Shared by the CLI flags and the `open`
/// command, so both accept exactly the same settings.
#[derive(Debug, Clone, Default, Args, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenParams {
    /// Serial device path.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "serial",
        required_unless_present = "serial"
    )]
    #[serde(default)]
    pub port: Option<String>,
    /// Exact USB serial number; resolves to one device path.
    #[arg(long, value_name = "SERIAL")]
    #[serde(default)]
    pub serial: Option<String>,
    /// Baud rate, 1-4000000.
    #[arg(short, long, default_value = "115200")]
    #[serde(default)]
    pub baud: BaudRate,
    /// Data bits: 5, 6, 7 or 8.
    #[arg(long, default_value = "8")]
    #[serde(default)]
    pub bits: DataBits,
    /// Parity.
    #[arg(long, value_enum, default_value = "none")]
    #[serde(default)]
    pub parity: Parity,
    /// Stop bits: 1 or 2.
    #[arg(long, default_value = "1")]
    #[serde(default)]
    pub stop: StopBits,
    /// DTR while opening; adapters may still pulse it.
    #[arg(long, value_enum, default_value = "preserve")]
    #[serde(default)]
    pub dtr: LineState,
    /// RTS immediately after opening.
    #[arg(long, value_enum, default_value = "preserve")]
    #[serde(default)]
    pub rts: LineState,
}

impl OpenParams {
    /// Resolve the device path; a USB serial number must match exactly one port.
    pub fn resolve(&self) -> Result<PortConfig, CommandError> {
        let path = match (self.port.as_deref(), self.serial.as_deref()) {
            (Some(path), None) if !path.is_empty() => path.to_owned(),
            (None, Some(serial)) if !serial.is_empty() => {
                let ports =
                    list_ports().map_err(|error| CommandError::from_transport(&error, false))?;
                let mut matches = ports
                    .into_iter()
                    .filter(|port| port.serial.as_deref() == Some(serial));
                match (matches.next(), matches.next()) {
                    (Some(port), None) => port.path,
                    (None, _) => {
                        return Err(CommandError::new(
                            ErrorCode::PortNotFound,
                            format!("no serial port has USB serial number {serial:?}"),
                            false,
                        ));
                    }
                    (Some(_), Some(_)) => {
                        return Err(CommandError::new(
                            ErrorCode::AmbiguousPort,
                            format!("more than one serial port has USB serial number {serial:?}"),
                            false,
                        ));
                    }
                }
            }
            _ => {
                return Err(CommandError::invalid(
                    "specify exactly one non-empty port or serial",
                ));
            }
        };
        Ok(PortConfig {
            path,
            baud: self.baud,
            data_bits: self.bits,
            parity: self.parity,
            stop_bits: self.stop,
            dtr: self.dtr,
            rts: self.rts,
        })
    }
}

/// Bytes to write, in exactly one encoding.
#[derive(Debug, Clone, Default, Args, Deserialize)]
#[group(required = true, multiple = false)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    /// Text with \n, \r, \t, \\ and \xHH escapes.
    #[arg(long)]
    #[serde(default)]
    pub text: Option<String>,
    /// Hexadecimal bytes; ASCII whitespace is ignored.
    #[arg(long)]
    #[serde(default)]
    pub hex: Option<String>,
    /// Base64 bytes.
    #[arg(long, value_name = "BASE64")]
    #[serde(default)]
    pub data_b64: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Text,
    Hex,
    Base64,
}

impl Payload {
    pub fn decode(&self) -> Result<(Encoding, Vec<u8>), CommandError> {
        let (encoding, bytes) = match (&self.text, &self.hex, &self.data_b64) {
            (Some(text), None, None) => (Encoding::Text, decode_text_escapes(text)?),
            (None, Some(hex), None) => (Encoding::Hex, decode_hex(hex)?),
            (None, None, Some(data)) => (
                Encoding::Base64,
                BASE64.decode(data).map_err(|error| {
                    CommandError::invalid(format!("invalid Base64 payload: {error}"))
                })?,
            ),
            _ => {
                return Err(CommandError::invalid(
                    "specify exactly one of text, hex, or data_b64",
                ));
            }
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
}

fn decode_hex(value: &str) -> Result<Vec<u8>, CommandError> {
    let digits: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .map(hex_digit)
        .collect::<Option<_>>()
        .ok_or_else(|| CommandError::invalid("hex contains a non-hexadecimal byte"))?;
    let (pairs, odd) = digits.as_chunks::<2>();
    if !odd.is_empty() {
        return Err(CommandError::invalid(
            "hex must contain complete byte pairs",
        ));
    }
    Ok(pairs.iter().map(|[high, low]| (high << 4) | low).collect())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode `\n`, `\r`, `\t`, `\\` and `\xHH`; other characters are UTF-8.
pub fn decode_text_escapes(value: &str) -> Result<Vec<u8>, CommandError> {
    let mut output = Vec::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut encoded = [0_u8; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => output.push(b'\n'),
            Some('r') => output.push(b'\r'),
            Some('t') => output.push(b'\t'),
            Some('\\') => output.push(b'\\'),
            Some('x') => {
                let mut digit = || {
                    chars
                        .next()
                        .and_then(|character| u8::try_from(character).ok())
                        .and_then(hex_digit)
                        .ok_or_else(|| {
                            CommandError::invalid("\\x must be followed by two hex digits")
                        })
                };
                let high = digit()?;
                let low = digit()?;
                output.push((high << 4) | low);
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

/// One request's operation.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase", deny_unknown_fields)]
pub enum Command {
    // Empty struct variants, not unit variants: serde ignores extra fields
    // on internally tagged unit variants even with `deny_unknown_fields`.
    List {},
    Open(OpenParams),
    Close {},
    Write(Payload),
    Read {
        bytes: ReadSize,
        #[serde(default)]
        timeout_ms: TimeoutMs,
    },
    Readline {
        #[serde(default)]
        timeout_ms: TimeoutMs,
    },
    Expect {
        pattern: String,
        #[serde(default)]
        timeout_ms: TimeoutMs,
    },
    Drain {},
    Modem {
        #[serde(default)]
        dtr: Option<bool>,
        #[serde(default)]
        rts: Option<bool>,
    },
    Break {
        #[serde(default)]
        duration_ms: BreakMs,
    },
}

impl Command {
    const NAMES: [&'static str; 10] = [
        "list", "open", "close", "write", "read", "readline", "expect", "drain", "modem", "break",
    ];

    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List {} => "list",
            Self::Open(_) => "open",
            Self::Close {} => "close",
            Self::Write(_) => "write",
            Self::Read { .. } => "read",
            Self::Readline { .. } => "readline",
            Self::Expect { .. } => "expect",
            Self::Drain {} => "drain",
            Self::Modem { .. } => "modem",
            Self::Break { .. } => "break",
        }
    }

    fn lookup(name: &str) -> Option<&'static str> {
        Self::NAMES.into_iter().find(|known| *known == name)
    }
}

/// Caller-chosen correlation value, echoed verbatim in the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    Text(String),
}

#[derive(Debug)]
pub struct Request {
    pub id: Option<RequestId>,
    pub command: Command,
}

/// A request line that could not become a [`Request`].
#[derive(Debug)]
pub struct Rejected {
    pub id: Option<RequestId>,
    pub command: &'static str,
    pub error: CommandError,
}

/// Parse one request line. The id is recovered before the command is
/// validated, so an invalid command still answers with the caller's id.
pub fn parse_request(line: &[u8]) -> Result<Request, Rejected> {
    let reject = |id, command, message: String| Rejected {
        id,
        command,
        error: CommandError::invalid(message),
    };
    let mut object: serde_json::Map<String, Value> = serde_json::from_slice(line)
        .map_err(|error| reject(None, "parse", format!("invalid command JSON: {error}")))?;
    let id = match object.remove("id") {
        None => None,
        Some(value) => match serde_json::from_value::<RequestId>(value) {
            Ok(RequestId::Text(text)) if text.len() > MAX_ID_BYTES => {
                return Err(reject(
                    None,
                    "parse",
                    format!("id exceeds {MAX_ID_BYTES} bytes"),
                ));
            }
            Ok(id) => Some(id),
            Err(_) => {
                return Err(reject(
                    None,
                    "parse",
                    "id must be a string or a non-negative integer".to_owned(),
                ));
            }
        },
    };
    let command_name = object
        .get("cmd")
        .and_then(Value::as_str)
        .and_then(Command::lookup)
        .unwrap_or("parse");
    match serde_json::from_value::<Command>(Value::Object(object)) {
        Ok(command) => Ok(Request { id, command }),
        Err(error) => Err(reject(
            id,
            command_name,
            format!("invalid command: {error}"),
        )),
    }
}

/// One response line.
#[derive(Debug)]
pub struct Response {
    pub id: Option<RequestId>,
    pub session_id: Arc<str>,
    pub command: &'static str,
    pub outcome: Result<Outcome, CommandError>,
}

impl Response {
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.outcome.is_ok()
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("protocol_version", &PROTOCOL_VERSION)?;
        if let Some(id) = &self.id {
            map.serialize_entry("id", id)?;
        }
        map.serialize_entry("session_id", &*self.session_id)?;
        map.serialize_entry("command", self.command)?;
        map.serialize_entry("ok", &self.outcome.is_ok())?;
        match &self.outcome {
            Ok(result) => map.serialize_entry("result", result)?,
            Err(error) => map.serialize_entry("error", error)?,
        }
        map.end()
    }
}

/// What a mutating command did to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    None,
    SerialPortOpened,
    SerialPortClosed,
    SerialBytesWritten,
    ModemLinesChanged,
    SerialBreakSent,
}

/// A side effect and whether repeating the command is harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Effect {
    side_effect: SideEffect,
    retry_safe: bool,
}

impl From<SideEffect> for Effect {
    fn from(side_effect: SideEffect) -> Self {
        Self {
            side_effect,
            retry_safe: matches!(side_effect, SideEffect::None | SideEffect::SerialPortClosed),
        }
    }
}

/// Why a read-type command returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum End {
    /// The requested byte count arrived.
    ByteLimit,
    /// The line went quiet after data arrived.
    Idle,
    /// The timeout elapsed.
    Timeout,
    /// `readline` received a newline.
    Newline,
    /// `expect` found its pattern.
    Match,
    /// `expect` filled its window without a match.
    BufferLimit,
    /// The device went away; the port is closed.
    Disconnected,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Ports {
        ports: Vec<PortInfo>,
    },
    Opened {
        port: String,
        baud: BaudRate,
        bits: DataBits,
        parity: Parity,
        stop: StopBits,
        dtr: LineState,
        rts: LineState,
        #[serde(flatten)]
        effect: Effect,
        warning: &'static str,
    },
    Closed {
        /// Received bytes that no command had returned yet.
        discarded_bytes: usize,
        #[serde(flatten)]
        effect: Effect,
    },
    Written {
        encoding: Encoding,
        bytes: usize,
        #[serde(flatten)]
        effect: Effect,
    },
    Received(Received),
    Modem {
        #[serde(flatten)]
        status: ModemStatus,
        #[serde(flatten)]
        effect: Effect,
    },
    Break {
        duration_ms: BreakMs,
        #[serde(flatten)]
        effect: Effect,
    },
}

/// Bytes returned (and consumed) by `read`, `readline`, `expect` or `drain`.
#[derive(Debug)]
pub struct Received {
    pub data: Vec<u8>,
    pub elapsed: Duration,
    pub end: End,
    /// `Some` for `expect`: the matched byte range within `data`, if any.
    pub expect: Option<Option<Range<usize>>>,
}

impl Serialize for Received {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("data_b64", &BASE64.encode(&self.data))?;
        if let Ok(text) = std::str::from_utf8(&self.data) {
            map.serialize_entry("text", text)?;
        }
        map.serialize_entry("bytes", &self.data.len())?;
        map.serialize_entry(
            "elapsed_ms",
            &u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        )?;
        map.serialize_entry("end", &self.end)?;
        map.serialize_entry("consumes_input", &true)?;
        if let Some(matched) = &self.expect {
            map.serialize_entry("matched", &matched.is_some())?;
            if let Some(range) = matched {
                let bytes = &self.data[range.clone()];
                map.serialize_entry("matched_b64", &BASE64.encode(bytes))?;
                if let Ok(text) = std::str::from_utf8(bytes) {
                    map.serialize_entry("matched_text", text)?;
                }
            }
        }
        map.end()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    ReadOnly,
    NotOpen,
    AlreadyOpen,
    PortNotFound,
    AmbiguousPort,
    PortBusy,
    PermissionDenied,
    Disconnected,
    Transport,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ReadOnly => "read_only",
            Self::NotOpen => "not_open",
            Self::AlreadyOpen => "already_open",
            Self::PortNotFound => "port_not_found",
            Self::AmbiguousPort => "ambiguous_port",
            Self::PortBusy => "port_busy",
            Self::PermissionDenied => "permission_denied",
            Self::Disconnected => "disconnected",
            Self::Transport => "transport",
        }
    }
}

/// A failed command. `retryable` is true only when sending the same request
/// again cannot repeat a side effect.
#[derive(Debug, Serialize, thiserror::Error)]
#[error("{}: {message}", code.as_str())]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl CommandError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message, false)
    }

    /// Map a transport failure. Failures of commands with side effects are
    /// never retryable: the device may already have seen part of the effect.
    #[must_use]
    pub fn from_transport(error: &crate::transport::TransportError, has_effect: bool) -> Self {
        use crate::transport::TransportError as E;
        let (code, retryable) = match error {
            E::NotOpen => (ErrorCode::NotOpen, false),
            E::AlreadyOpen => (ErrorCode::AlreadyOpen, false),
            E::PortNotFound { .. } => (ErrorCode::PortNotFound, false),
            E::PermissionDenied { .. } => (ErrorCode::PermissionDenied, false),
            E::PortBusy { .. } => (ErrorCode::PortBusy, false),
            E::Disconnected => (ErrorCode::Disconnected, false),
            E::Write { .. } | E::Io { .. } => (ErrorCode::Transport, !has_effect),
        };
        Self::new(code, error.to_string(), retryable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fmt::Write as _;

    fn rejected(line: &str) -> Rejected {
        parse_request(line.as_bytes()).expect_err("request should be rejected")
    }

    #[test]
    fn id_survives_an_invalid_command() {
        let rejection = rejected(r#"{"id":"r1","cmd":"open","port":"/dev/x","bits":9}"#);
        assert_eq!(rejection.id, Some(RequestId::Text("r1".into())));
        assert_eq!(rejection.command, "open");
        assert!(
            rejection
                .error
                .message
                .contains("bits must be 5, 6, 7, or 8")
        );
    }

    #[test]
    fn invalid_ids_are_rejected_without_echo() {
        for line in [
            r#"{"id":{"a":1},"cmd":"list"}"#,
            r#"{"id":-1,"cmd":"list"}"#,
            &format!(
                r#"{{"id":"{}","cmd":"list"}}"#,
                "x".repeat(MAX_ID_BYTES + 1)
            ),
        ] {
            let rejection = rejected(line);
            assert_eq!(rejection.id, None, "{line}");
            assert_eq!(rejection.error.code, ErrorCode::InvalidRequest);
        }
    }

    #[test]
    fn unknown_fields_are_rejected_for_every_command_shape() {
        for line in [
            r#"{"cmd":"close","extra":1}"#,
            r#"{"cmd":"open","port":"loop://","extra":1}"#,
            r#"{"cmd":"write","text":"x","extra":1}"#,
            r#"{"cmd":"read","bytes":1,"extra":1}"#,
        ] {
            assert!(rejected(line).error.message.contains("extra"), "{line}");
        }
    }

    #[test]
    fn limits_are_enforced_at_parse_time() {
        for line in [
            r#"{"cmd":"read","bytes":0}"#,
            r#"{"cmd":"read","bytes":1048577}"#,
            r#"{"cmd":"readline","timeout_ms":60001}"#,
            r#"{"cmd":"break","duration_ms":0}"#,
            r#"{"cmd":"open","port":"loop://","baud":4000001}"#,
            r#"{"cmd":"open","port":"loop://","stop":3}"#,
        ] {
            assert_eq!(
                rejected(line).error.code,
                ErrorCode::InvalidRequest,
                "{line}"
            );
        }
    }

    #[test]
    fn payload_requires_exactly_one_nonempty_encoding() {
        assert!(Payload::default().decode().is_err());
        let both = Payload {
            text: Some("x".into()),
            hex: Some("78".into()),
            data_b64: None,
        };
        assert!(both.decode().is_err());
        let empty = Payload {
            hex: Some(" ".into()),
            ..Payload::default()
        };
        assert!(empty.decode().is_err());
        let odd = Payload {
            hex: Some("abc".into()),
            ..Payload::default()
        };
        assert!(odd.decode().is_err());
    }

    #[test]
    fn text_escape_errors() {
        assert!(decode_text_escapes("\\x4").is_err());
        assert!(decode_text_escapes("\\xg0").is_err());
        assert!(decode_text_escapes("\\q").is_err());
        assert!(decode_text_escapes("end\\").is_err());
        assert_eq!(
            decode_text_escapes("AT\\r\\n€").unwrap(),
            "AT\r\n€".as_bytes()
        );
    }

    proptest! {
        #[test]
        fn hex_payload_round_trips(bytes in prop::collection::vec(any::<u8>(), 1..512)) {
            let spaced = bytes.iter().fold(String::new(), |mut text, byte| {
                let _ = write!(text, "{byte:02X} ");
                text
            });
            let payload = Payload { hex: Some(spaced), ..Payload::default() };
            prop_assert_eq!(payload.decode().unwrap().1, bytes);
        }

        #[test]
        fn escaped_text_round_trips(bytes in prop::collection::vec(any::<u8>(), 1..512)) {
            let escaped = bytes.iter().fold(String::new(), |mut text, byte| {
                let _ = write!(text, "\\x{byte:02x}");
                text
            });
            prop_assert_eq!(decode_text_escapes(&escaped).unwrap(), bytes);
        }
    }
}
