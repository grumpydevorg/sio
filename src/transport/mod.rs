//! Transport layer abstractions for serial communication.
//!
//! The core abstraction is [`SerialTransport`], which provides byte-level I/O
//! with serial-like semantics. Various backends implement this trait:
//!
//! - [`LocalTransport`] - Real hardware via OS serial ports
//! - [`LoopbackTransport`] - Echo transport for testing
//! - [`MockTransport`] - Scripted responses for unit tests
//! - [`ObservingTransport`] - Wrapper that records traffic
//!
//! # Example
//!
//! ```ignore
//! // Real hardware
//! let mut transport = LocalTransport::new();
//! transport.open(&PortConfig::new("/dev/ttyUSB0", 115200))?;
//!
//! // With observation
//! let observer = FileObserver::new("traffic.log");
//! let mut transport = ObservingTransport::new(transport, observer);
//! ```

pub mod config;
#[cfg(test)]
mod invariants;
mod local;
mod loopback;
mod mock;
mod observer;
pub mod typestate;

pub use config::{BaudRate, DataBits, StopBits, Timeout};
pub use local::LocalTransport;
pub use loopback::LoopbackTransport;
pub use mock::{Expectation, MockAction, MockTransport};
pub use observer::{LoggingObserver, ObservingTransport, TrafficEvent, TrafficObserver};
pub use typestate::{Closed, Open, TypedTransport};

use std::time::Duration;
use thiserror::Error;

/// Errors that can occur during transport operations.
#[derive(Error, Debug)]
pub enum TransportError {
    #[error("Transport not open")]
    NotOpen,

    #[error("Transport already open")]
    AlreadyOpen,

    #[error("Invalid serial configuration: {0}")]
    InvalidConfig(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Write failed: {0}")]
    WriteFailed(String),

    #[error("Read failed: {0}")]
    ReadFailed(String),

    #[error("Operation timed out")]
    Timeout,

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration for opening a serial port.
#[derive(Debug, Clone)]
pub struct PortConfig {
    /// Port path (e.g., "/dev/ttyUSB0", "COM3", or "loop://")
    pub path: String,
    /// Baud rate
    pub baud: u32,
    /// Data bits (5, 6, 7, 8)
    pub data_bits: u8,
    /// Parity
    pub parity: Parity,
    /// Stop bits (1, 2)
    pub stop_bits: u8,
    /// Desired DTR behavior while opening the port.
    pub dtr_on_open: LineState,
    /// Desired RTS state immediately after opening the port.
    pub rts_on_open: LineState,
}

impl PortConfig {
    /// Create a new port configuration with common defaults (8N1).
    pub fn new(path: impl Into<String>, baud: u32) -> Self {
        Self {
            path: path.into(),
            baud,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            dtr_on_open: LineState::Preserve,
            rts_on_open: LineState::Preserve,
        }
    }

    /// Set data bits.
    pub fn data_bits(mut self, bits: u8) -> Self {
        self.data_bits = bits;
        self
    }

    /// Set parity.
    pub fn parity(mut self, parity: Parity) -> Self {
        self.parity = parity;
        self
    }

    /// Set stop bits.
    pub fn stop_bits(mut self, stop: u8) -> Self {
        self.stop_bits = stop;
        self
    }

    /// Set DTR behavior while opening the port.
    pub fn dtr_on_open(mut self, state: LineState) -> Self {
        self.dtr_on_open = state;
        self
    }

    /// Set RTS immediately after opening the port.
    pub fn rts_on_open(mut self, state: LineState) -> Self {
        self.rts_on_open = state;
        self
    }
}

/// Parity setting for serial communication.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

/// Desired output-line behavior when a serial port is opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineState {
    #[default]
    Preserve,
    On,
    Off,
}

/// Modem line status.
#[derive(Debug, Clone, Default)]
pub struct ModemStatus {
    /// Data Terminal Ready (output)
    pub dtr: Option<bool>,
    /// Request To Send (output)
    pub rts: Option<bool>,
    /// Clear To Send (input)
    pub cts: Option<bool>,
    /// Data Set Ready (input)
    pub dsr: Option<bool>,
    /// Data Carrier Detect (input)
    pub dcd: Option<bool>,
}

/// Core trait for serial transport backends.
///
/// This trait abstracts over different ways to send/receive bytes with
/// serial-like semantics. Implementations include real hardware, network
/// hardware ports, loopback for testing, and mocks.
pub trait SerialTransport: Send {
    /// Open connection using the given configuration.
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError>;

    /// Close the connection.
    fn close(&mut self) -> Result<(), TransportError>;

    /// Check if the transport is currently open.
    fn is_open(&self) -> bool;

    /// Write bytes to the transport.
    ///
    /// Returns the number of bytes written.
    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError>;

    /// Read bytes with a timeout.
    ///
    /// Reads up to `max_bytes` bytes, waiting up to `timeout` for data.
    /// Returns the bytes read (may be empty if timeout expires with no data).
    fn read(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, TransportError>;

    /// Read all currently available bytes without blocking.
    fn drain(&mut self) -> Result<Vec<u8>, TransportError>;

    /// Control and query modem lines.
    ///
    /// Pass `Some(value)` to set DTR/RTS, or `None` to leave unchanged.
    /// Returns the current status of all modem lines.
    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError>;

    /// Send a break signal for the specified duration.
    fn send_break(&mut self, duration: Duration) -> Result<(), TransportError>;

    /// Get the port path/identifier (if open).
    fn port_path(&self) -> Option<&str>;
}

/// Information about an available serial port.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub path: String,
    pub port_type: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// List available serial ports on the system.
pub fn list_ports() -> Result<Vec<PortInfo>, TransportError> {
    LocalTransport::list_ports()
}
