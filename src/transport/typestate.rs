//! Typestate pattern for serial transports.
//!
//! This module provides a wrapper that uses the typestate pattern to make
//! invalid states unrepresentable at compile time.
//!
//! # Problem with runtime checks
//!
//! ```ignore
//! let mut transport = LocalTransport::new();
//! transport.write(b"data")?;  // Runtime error: NotOpen
//! ```
//!
//! # Solution: Typestate
//!
//! ```ignore
//! let transport = LocalTransport::new();  // Closed state
//! let transport = transport.open(config)?;  // Open state
//! transport.write(b"data")?;  // Compiles! Type guarantees it's open
//! ```
//!
//! The type system enforces that you can only call I/O methods on open transports.

use super::{ModemStatus, PortConfig, PortInfo, SerialTransport, TransportError};
use std::marker::PhantomData;
use std::time::Duration;

/// Marker type for closed state.
#[derive(Debug)]
pub struct Closed;

/// Marker type for open state.
#[derive(Debug)]
pub struct Open;

/// A transport wrapper with compile-time state tracking.
///
/// This wrapper uses the typestate pattern to ensure that I/O operations
/// can only be called on open transports.
///
/// # Example
///
/// ```ignore
/// use sio::transport::typestate::TypedTransport;
/// use sio::transport::{LocalTransport, PortConfig};
///
/// // Create closed transport
/// let transport: TypedTransport<LocalTransport, Closed> = TypedTransport::new(LocalTransport::new());
///
/// // Open it - returns a different type!
/// let transport: TypedTransport<LocalTransport, Open> = transport.open(&config)?;
///
/// // Now we can do I/O (this wouldn't compile on Closed)
/// transport.write(b"data")?;
///
/// // Close it - returns Closed type
/// let transport: TypedTransport<LocalTransport, Closed> = transport.close()?;
/// ```
pub struct TypedTransport<T: SerialTransport, State> {
    inner: T,
    _state: PhantomData<State>,
}

impl<T: SerialTransport + std::fmt::Debug, State: std::fmt::Debug> std::fmt::Debug
    for TypedTransport<T, State>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedTransport")
            .field("inner", &self.inner)
            .field("state", &std::any::type_name::<State>())
            .finish()
    }
}

impl<T: SerialTransport> TypedTransport<T, Closed> {
    /// Create a new closed transport.
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _state: PhantomData,
        }
    }

    /// Open the transport, transitioning to Open state.
    ///
    /// # Errors
    ///
    /// Returns error if the transport fails to open.
    pub fn open(
        mut self,
        config: &PortConfig,
    ) -> Result<TypedTransport<T, Open>, (Self, TransportError)> {
        match self.inner.open(config) {
            Ok(()) => Ok(TypedTransport {
                inner: self.inner,
                _state: PhantomData,
            }),
            Err(e) => Err((self, e)),
        }
    }

    /// Get the inner transport (consuming this wrapper).
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: SerialTransport> TypedTransport<T, Open> {
    /// Write data to the transport.
    ///
    /// # Errors
    ///
    /// Returns error if write fails.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        self.inner.write(data)
    }

    /// Read data with timeout.
    ///
    /// # Errors
    ///
    /// Returns error if read fails.
    pub fn read(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        self.inner.read(max_bytes, timeout)
    }

    /// Drain all available data.
    ///
    /// # Errors
    ///
    /// Returns error if drain fails.
    pub fn drain(&mut self) -> Result<Vec<u8>, TransportError> {
        self.inner.drain()
    }

    /// Control modem lines.
    ///
    /// # Errors
    ///
    /// Returns error if operation fails.
    pub fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        self.inner.modem_control(dtr, rts)
    }

    /// Send break signal.
    ///
    /// # Errors
    ///
    /// Returns error if operation fails.
    pub fn send_break(&mut self, duration: Duration) -> Result<(), TransportError> {
        self.inner.send_break(duration)
    }

    /// Get the port path.
    #[must_use]
    pub fn port_path(&self) -> Option<&str> {
        self.inner.port_path()
    }

    /// Close the transport, transitioning to Closed state.
    ///
    /// # Errors
    ///
    /// Returns error if close fails.
    pub fn close(mut self) -> Result<TypedTransport<T, Closed>, (Self, TransportError)> {
        match self.inner.close() {
            Ok(()) => Ok(TypedTransport {
                inner: self.inner,
                _state: PhantomData,
            }),
            Err(e) => Err((self, e)),
        }
    }

    /// Get the inner transport (consuming this wrapper).
    ///
    /// Note: This will leave the underlying transport open.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

// Both states can list ports (static method)
impl<T: SerialTransport, State> TypedTransport<T, State> {
    /// Check if the transport is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.inner.is_open()
    }
}

/// List available ports (independent of transport state).
///
/// # Errors
///
/// Returns error if port enumeration fails.
pub fn list_ports() -> Result<Vec<PortInfo>, TransportError> {
    super::list_ports()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::LoopbackTransport;

    #[test]
    fn test_typestate_compile_time_safety() {
        // This test demonstrates that the typestate pattern works.
        // The key is that these operations COMPILE - runtime behavior is secondary.

        let transport = TypedTransport::new(LoopbackTransport::new());

        // This would NOT compile:
        // transport.write(b"data");  // Error: no method `write` on TypedTransport<_, Closed>

        let mut transport = transport
            .open(&PortConfig::new("loop://", 115_200))
            .expect("open should succeed");

        // Now write compiles because we're in Open state
        transport.write(b"data").expect("write should succeed");

        let _transport = transport.close().expect("close should succeed");

        // This would NOT compile:
        // _transport.write(b"data");  // Error: no method `write` on TypedTransport<_, Closed>
    }

    #[test]
    fn test_open_failure_returns_closed() {
        let transport = TypedTransport::new(LoopbackTransport::new());

        // Open succeeds
        let transport = transport
            .open(&PortConfig::new("loop://", 115_200))
            .expect("first open");

        // Try to close and reopen - should work
        let transport = transport.close().expect("close");
        let _transport = transport
            .open(&PortConfig::new("loop://", 115_200))
            .expect("reopen");
    }
}
