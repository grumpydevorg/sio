//! `sio`: local serial I/O for people and agents.
//!
//! ```text
//! stdin NDJSON ──► protocol::parse_request ──► server::CommandHandler ──► transport::SerialTransport
//!                  (validates every field)     (pending-byte buffer,      ├── LocalTransport (hardware)
//!                                               side effects, results)    └── LoopbackTransport (echo)
//! ```
//!
//! [`protocol`] owns the wire format: requests are fully validated while they
//! are parsed, and responses are typed. [`server::CommandHandler`] runs one
//! serial session and keeps received bytes that no command has returned yet.
//! [`transport`] is the hardware boundary.

pub mod protocol;
pub mod server;
pub mod transport;
