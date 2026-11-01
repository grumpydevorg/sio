//! Loopback transport that echoes written data back to reads.
//!
//! Useful for testing protocol implementations without hardware.

use super::{ModemStatus, PortConfig, SerialTransport, TransportError};
use std::collections::VecDeque;
use std::time::Duration;

/// A transport that echoes all written data back to reads.
///
/// This is useful for testing:
/// - Protocol framing logic
/// - Timeout handling
/// - Basic I/O flow
///
/// # Example
///
/// ```
/// use sio::transport::{LoopbackTransport, PortConfig, SerialTransport};
/// use std::time::Duration;
///
/// let mut transport = LoopbackTransport::new();
/// transport.open(&PortConfig::new("loop://", 115200)).unwrap();
///
/// transport.write(b"Hello").unwrap();
/// let data = transport.read(10, Duration::from_millis(100)).unwrap();
/// assert_eq!(data, b"Hello");
/// ```
#[derive(Debug)]
pub struct LoopbackTransport {
    buffer: VecDeque<u8>,
    is_open: bool,
    path: Option<String>,
    modem_status: ModemStatus,
}

impl LoopbackTransport {
    /// Create a new loopback transport.
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(4096),
            is_open: false,
            path: None,
            modem_status: ModemStatus {
                dtr: Some(false),
                rts: Some(false),
                cts: Some(true),
                dsr: Some(true),
                dcd: Some(true),
            },
        }
    }

    /// Get the number of bytes currently in the buffer.
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Inject data into the read buffer (simulating received data).
    ///
    /// This is useful for testing scenarios where you want to simulate
    /// incoming data that wasn't written by the same transport.
    pub fn inject(&mut self, data: &[u8]) {
        self.buffer.extend(data);
    }
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SerialTransport for LoopbackTransport {
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError> {
        if self.is_open {
            return Err(TransportError::AlreadyOpen);
        }
        self.is_open = true;
        self.path = Some(config.path.clone());
        self.buffer.clear();
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        self.is_open = false;
        self.path = None;
        self.buffer.clear();
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        // Echo: written data goes into the read buffer
        self.buffer.extend(data);
        Ok(data.len())
    }

    fn read(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        if self.buffer.is_empty() && !timeout.is_zero() {
            std::thread::sleep(timeout);
        }
        let take = std::cmp::min(max_bytes, self.buffer.len());
        let result: Vec<u8> = self.buffer.drain(..take).collect();
        Ok(result)
    }

    fn drain(&mut self) -> Result<Vec<u8>, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        let result: Vec<u8> = self.buffer.drain(..).collect();
        Ok(result)
    }

    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        if let Some(v) = dtr {
            self.modem_status.dtr = Some(v);
        }
        if let Some(v) = rts {
            self.modem_status.rts = Some(v);
        }
        Ok(self.modem_status.clone())
    }

    fn send_break(&mut self, _duration: Duration) -> Result<(), TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        // Loopback just ignores break signals
        Ok(())
    }

    fn port_path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loopback_echo() {
        let mut transport = LoopbackTransport::new();
        transport.open(&PortConfig::new("loop://", 115200)).unwrap();

        transport.write(b"Hello, World!").unwrap();
        let data = transport.read(20, Duration::from_millis(100)).unwrap();
        assert_eq!(data, b"Hello, World!");
    }

    #[test]
    fn test_loopback_partial_read() {
        let mut transport = LoopbackTransport::new();
        transport.open(&PortConfig::new("loop://", 115200)).unwrap();

        transport.write(b"ABCDEFGH").unwrap();

        let data1 = transport.read(4, Duration::from_millis(100)).unwrap();
        assert_eq!(data1, b"ABCD");

        let data2 = transport.read(4, Duration::from_millis(100)).unwrap();
        assert_eq!(data2, b"EFGH");
    }

    #[test]
    fn test_loopback_inject() {
        let mut transport = LoopbackTransport::new();
        transport.open(&PortConfig::new("loop://", 115200)).unwrap();

        transport.inject(b"Injected data");
        let data = transport.drain().unwrap();
        assert_eq!(data, b"Injected data");
    }

    #[test]
    fn test_loopback_not_open() {
        let mut transport = LoopbackTransport::new();
        assert!(transport.write(b"test").is_err());
        assert!(transport.read(10, Duration::from_millis(100)).is_err());
    }
}
