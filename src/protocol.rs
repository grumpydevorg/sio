//! Versioned NDJSON protocol used by `sio stdio`.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const MAX_IO_BYTES: usize = 1024 * 1024;
pub const MAX_TIMEOUT_MS: u64 = 60_000;
pub const MAX_BREAK_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase", deny_unknown_fields)]
pub enum Command {
    List,
    Open {
        #[serde(default)]
        port: Option<String>,
        #[serde(default)]
        serial: Option<String>,
        #[serde(default = "default_baud")]
        baud: u32,
        #[serde(default = "default_bits")]
        bits: u8,
        #[serde(default)]
        parity: Parity,
        #[serde(default = "default_stop")]
        stop: u8,
        #[serde(default)]
        dtr: LineState,
        #[serde(default)]
        rts: LineState,
    },
    Close,
    Write {
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        hex: Option<String>,
        #[serde(default)]
        data_b64: Option<String>,
    },
    Read {
        bytes: usize,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    Readline {
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    Expect {
        pattern: String,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    Drain,
    Modem {
        #[serde(default)]
        dtr: Option<bool>,
        #[serde(default)]
        rts: Option<bool>,
    },
    Break {
        #[serde(default = "default_break_duration")]
        duration_ms: u64,
    },
}

impl Command {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Open { .. } => "open",
            Self::Close => "close",
            Self::Write { .. } => "write",
            Self::Read { .. } => "read",
            Self::Readline { .. } => "readline",
            Self::Expect { .. } => "expect",
            Self::Drain => "drain",
            Self::Modem { .. } => "modem",
            Self::Break { .. } => "break",
        }
    }
}

const fn default_baud() -> u32 {
    115_200
}

const fn default_bits() -> u8 {
    8
}

const fn default_stop() -> u8 {
    1
}

const fn default_timeout() -> u64 {
    5_000
}

const fn default_break_duration() -> u64 {
    500
}

#[derive(Debug, Default, Deserialize, Clone, Copy, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

impl From<Parity> for crate::transport::Parity {
    fn from(value: Parity) -> Self {
        match value {
            Parity::None => Self::None,
            Parity::Odd => Self::Odd,
            Parity::Even => Self::Even,
        }
    }
}

#[derive(Debug, Default, Deserialize, Clone, Copy, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LineState {
    #[default]
    Preserve,
    On,
    Off,
}

impl From<LineState> for crate::transport::LineState {
    fn from(value: LineState) -> Self {
        match value {
            LineState::Preserve => Self::Preserve,
            LineState::On => Self::On,
            LineState::Off => Self::Off,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub protocol_version: u8,
    pub session_id: String,
    pub command: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    #[must_use]
    pub fn success(session_id: &str, command: &str, result: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_owned(),
            command: command.to_owned(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn failure(session_id: &str, command: &str, error: CommandError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            session_id: session_id.to_owned(),
            command: command.to_owned(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl CommandError {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message, false)
    }
}

impl From<CommandError> for ErrorBody {
    fn from(value: CommandError) -> Self {
        Self {
            code: value.code.to_owned(),
            message: value.message,
            retryable: value.retryable,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PortInfo {
    pub path: String,
    #[serde(rename = "type")]
    pub port_type: String,
    pub recommended_for_open: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}
