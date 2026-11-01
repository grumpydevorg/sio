//! Serial transport boundary.
//!
//! [`SerialTransport`] has two implementations: [`LocalTransport`] drives an
//! operating-system serial port and [`LoopbackTransport`] is an in-memory echo
//! device for dry runs and tests. Port settings arrive already validated as
//! [`PortConfig`]; transports never re-check them.

mod local;
mod loopback;

pub use local::{LocalTransport, list_ports};
pub use loopback::LoopbackTransport;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

/// Errors raised by a transport.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("no serial port is open")]
    NotOpen,
    #[error("a serial port is already open; close it first")]
    AlreadyOpen,
    #[error("{path}: no such serial port")]
    PortNotFound { path: String },
    #[error("{path}: permission denied")]
    PermissionDenied { path: String },
    #[error("{path}: port is busy or unavailable; another program may have it open")]
    PortBusy { path: String },
    #[error("the serial device disconnected; the port is now closed")]
    Disconnected,
    #[error("write failed after {written} of {total} bytes: {source}")]
    Write {
        written: usize,
        total: usize,
        #[source]
        source: std::io::Error,
    },
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// Message for a setting outside its valid range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct InvalidSetting(pub &'static str);

/// Baud rate between 1 and [`BaudRate::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct BaudRate(u32);

impl BaudRate {
    pub const MAX: u32 = 4_000_000;

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for BaudRate {
    fn default() -> Self {
        Self(115_200)
    }
}

impl TryFrom<u32> for BaudRate {
    type Error = InvalidSetting;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if (1..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidSetting("baud must be between 1 and 4000000"))
        }
    }
}

impl From<BaudRate> for u32 {
    fn from(value: BaudRate) -> Self {
        value.0
    }
}

impl FromStr for BaudRate {
    type Err = InvalidSetting;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<u32>()
            .map_err(|_| InvalidSetting("baud must be between 1 and 4000000"))
            .and_then(Self::try_from)
    }
}

impl fmt::Display for BaudRate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Data bits per character.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum DataBits {
    Five,
    Six,
    Seven,
    #[default]
    Eight,
}

impl TryFrom<u8> for DataBits {
    type Error = InvalidSetting;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            5 => Ok(Self::Five),
            6 => Ok(Self::Six),
            7 => Ok(Self::Seven),
            8 => Ok(Self::Eight),
            _ => Err(InvalidSetting("bits must be 5, 6, 7, or 8")),
        }
    }
}

impl From<DataBits> for u8 {
    fn from(value: DataBits) -> Self {
        match value {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        }
    }
}

impl FromStr for DataBits {
    type Err = InvalidSetting;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<u8>()
            .map_err(|_| InvalidSetting("bits must be 5, 6, 7, or 8"))
            .and_then(Self::try_from)
    }
}

/// Stop bits per character.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum StopBits {
    #[default]
    One,
    Two,
}

impl TryFrom<u8> for StopBits {
    type Error = InvalidSetting;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::One),
            2 => Ok(Self::Two),
            _ => Err(InvalidSetting("stop must be 1 or 2")),
        }
    }
}

impl From<StopBits> for u8 {
    fn from(value: StopBits) -> Self {
        match value {
            StopBits::One => 1,
            StopBits::Two => 2,
        }
    }
}

impl FromStr for StopBits {
    type Err = InvalidSetting;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<u8>()
            .map_err(|_| InvalidSetting("stop must be 1 or 2"))
            .and_then(Self::try_from)
    }
}

/// Parity bit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

/// Requested state of an output modem line when the port opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LineState {
    /// Leave the line as the operating system and adapter set it.
    #[default]
    Preserve,
    On,
    Off,
}

/// Validated settings for opening a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConfig {
    pub path: String,
    pub baud: BaudRate,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub dtr: LineState,
    pub rts: LineState,
}

impl PortConfig {
    /// 8N1 at `baud`, modem lines preserved.
    #[must_use]
    pub fn new(path: impl Into<String>, baud: BaudRate) -> Self {
        Self {
            path: path.into(),
            baud,
            data_bits: DataBits::default(),
            parity: Parity::default(),
            stop_bits: StopBits::default(),
            dtr: LineState::default(),
            rts: LineState::default(),
        }
    }

    /// Time the line takes to carry `bytes` characters, including start,
    /// parity and stop bits.
    #[must_use]
    pub fn transmit_time(&self, bytes: usize) -> Duration {
        let bits_per_char = 1
            + u64::from(u8::from(self.data_bits))
            + u64::from(self.parity != Parity::None)
            + u64::from(u8::from(self.stop_bits));
        let bits = u64::try_from(bytes)
            .unwrap_or(u64::MAX)
            .saturating_mul(bits_per_char);
        let micros = bits.saturating_mul(1_000_000) / u64::from(self.baud.get());
        Duration::from_micros(micros)
    }
}

/// Modem-line state. `None` means the transport cannot report that line;
/// output lines (DTR, RTS) are known only after this session set them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ModemStatus {
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
    pub cts: Option<bool>,
    pub dsr: Option<bool>,
    pub dcd: Option<bool>,
}

/// A serial port as discovered on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortInfo {
    pub path: String,
    #[serde(rename = "type")]
    pub port_type: &'static str,
    /// False for macOS `/dev/tty.*` call-in devices, which block on open
    /// until carrier detect; use the paired `/dev/cu.*` path instead.
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

/// Byte-level access to one serial port at a time.
pub trait SerialTransport: Send {
    /// Open the port described by `config`.
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError>;

    /// Close the open port.
    fn close(&mut self) -> Result<(), TransportError>;

    /// Write all of `data` and wait until it has been transmitted.
    fn write(&mut self, data: &[u8]) -> Result<(), TransportError>;

    /// Append up to `max_bytes` received bytes to `buf`.
    ///
    /// Waits up to `timeout` for the first byte. Once data arrives, returns
    /// when the line goes idle, `max_bytes` have arrived, or `timeout` has
    /// elapsed. Bytes appended before an error stay in `buf`. Returns
    /// [`TransportError::Disconnected`] after closing the port when the device
    /// goes away.
    fn read_into(
        &mut self,
        buf: &mut Vec<u8>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<(), TransportError>;

    /// Set DTR and/or RTS when `Some`, then report all modem lines.
    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError>;

    /// Hold the line in a break condition for `duration`.
    fn send_break(&mut self, duration: Duration) -> Result<(), TransportError>;
}
