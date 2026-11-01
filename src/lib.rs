//! `sio` - local serial I/O with a versioned NDJSON protocol.
//!
//! Designed for safe human and agent interaction through a local process.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Protocol Layer                           │
//! │  NDJSON commands over stdio → protocol::Command             │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//! ┌────────────────────────────▼────────────────────────────────┐
//! │                    Framing Layer                            │
//! │  framing::Deframer / framing::Framer                        │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//! ┌────────────────────────────▼────────────────────────────────┐
//! │                    Transport Layer                          │
//! │  transport::SerialTransport trait                           │
//! │  ├── LocalTransport (real hardware)                         │
//! │  ├── LoopbackTransport (testing)                            │
//! │  ├── MockTransport (unit tests)                             │
//! │  └── ObservingTransport (traffic recording)                 │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use sio::transport::*;
//! use std::time::Duration;
//!
//! // Real hardware
//! let mut transport = LocalTransport::new();
//! transport.open(&PortConfig::new("/dev/ttyUSB0", 115200))?;
//! transport.write(b"AT\r\n")?;
//! let response = transport.read(100, Duration::from_secs(1))?;
//!
//! // With observation (debugging)
//! let observer = LoggingObserver::new(std::io::stderr());
//! let mut transport = ObservingTransport::new(transport, observer);
//!
//! // For testing
//! let mut transport = LoopbackTransport::new();
//! transport.open(&PortConfig::new("loop://", 115200))?;
//! ```

pub mod framing;
pub mod protocol;
pub mod server;
pub mod transport;

// Re-export commonly used types at crate root
pub use transport::{
    LineState, LocalTransport, LoopbackTransport, MockTransport, ObservingTransport, PortConfig,
    PortInfo, SerialTransport, TransportError, list_ports,
};
