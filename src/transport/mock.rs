//! Mock transport with scripted responses for unit testing.
//!
//! Allows defining expected writes and scripted read responses.

use super::{ModemStatus, PortConfig, SerialTransport, TransportError};
use std::collections::VecDeque;
use std::time::Duration;

/// An expectation for the mock transport.
#[derive(Debug, Clone)]
pub struct Expectation {
    /// What we expect to be written (None = any write)
    pub expect_write: Option<Vec<u8>>,
    /// Action to take when expectation is matched
    pub action: MockAction,
}

/// Action to take when an expectation is matched.
#[derive(Debug, Clone)]
pub enum MockAction {
    /// Respond with specific data
    Respond(Vec<u8>),
    /// Return an error
    Error(String),
    /// Do nothing (acknowledge write but no response)
    Ack,
    /// Simulate timeout (return empty)
    Timeout,
}

impl Expectation {
    /// Create an expectation that responds to any write.
    pub fn any_write(response: impl Into<Vec<u8>>) -> Self {
        Self {
            expect_write: None,
            action: MockAction::Respond(response.into()),
        }
    }

    /// Create an expectation for a specific write.
    pub fn on_write(write: impl Into<Vec<u8>>, response: impl Into<Vec<u8>>) -> Self {
        Self {
            expect_write: Some(write.into()),
            action: MockAction::Respond(response.into()),
        }
    }

    /// Create an expectation that returns an error.
    pub fn on_write_error(write: impl Into<Vec<u8>>, error: impl Into<String>) -> Self {
        Self {
            expect_write: Some(write.into()),
            action: MockAction::Error(error.into()),
        }
    }

    /// Create an expectation that simulates a timeout.
    pub fn on_write_timeout(write: impl Into<Vec<u8>>) -> Self {
        Self {
            expect_write: Some(write.into()),
            action: MockAction::Timeout,
        }
    }
}

/// A mock transport that follows a script of expectations.
///
/// Useful for unit testing protocol handlers without any I/O.
///
/// # Example
///
/// ```
/// use sio::transport::{Expectation, MockTransport, PortConfig, SerialTransport};
/// use std::time::Duration;
///
/// let mut transport = MockTransport::new(vec![
///     Expectation::on_write(b"AT\r\n".to_vec(), b"OK\r\n".to_vec()),
///     Expectation::on_write(b"AT+CSQ\r\n".to_vec(), b"+CSQ: 31,99\r\nOK\r\n".to_vec()),
/// ]);
///
/// transport.open(&PortConfig::new("mock://", 115200)).unwrap();
///
/// transport.write(b"AT\r\n").unwrap();
/// let response = transport.read(100, Duration::from_millis(100)).unwrap();
/// assert_eq!(response, b"OK\r\n");
/// ```
pub struct MockTransport {
    expectations: VecDeque<Expectation>,
    read_buffer: VecDeque<u8>,
    write_log: Vec<Vec<u8>>,
    is_open: bool,
    path: Option<String>,
    modem_status: ModemStatus,
    strict: bool,
}

impl MockTransport {
    /// Create a new mock transport with the given expectations.
    pub fn new(expectations: Vec<Expectation>) -> Self {
        Self {
            expectations: expectations.into(),
            read_buffer: VecDeque::new(),
            write_log: Vec::new(),
            is_open: false,
            path: None,
            modem_status: ModemStatus::default(),
            strict: true,
        }
    }

    /// Create an empty mock transport.
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// Set strict mode (default: true).
    ///
    /// In strict mode, unexpected writes cause an error.
    /// In non-strict mode, unexpected writes are logged but allowed.
    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Add an expectation.
    pub fn expect(mut self, expectation: Expectation) -> Self {
        self.expectations.push_back(expectation);
        self
    }

    /// Get all data that was written to this transport.
    pub fn write_log(&self) -> &[Vec<u8>] {
        &self.write_log
    }

    /// Check if all expectations were consumed.
    pub fn all_expectations_met(&self) -> bool {
        self.expectations.is_empty()
    }

    /// Get remaining expectations.
    pub fn remaining_expectations(&self) -> usize {
        self.expectations.len()
    }

    /// Inject data into the read buffer.
    pub fn inject(&mut self, data: &[u8]) {
        self.read_buffer.extend(data);
    }
}

impl SerialTransport for MockTransport {
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError> {
        if self.is_open {
            return Err(TransportError::AlreadyOpen);
        }
        self.is_open = true;
        self.path = Some(config.path.clone());
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        self.is_open = false;
        self.path = None;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.is_open
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }

        self.write_log.push(data.to_vec());

        // Find matching expectation
        let expectation = self.expectations.iter().position(|e| {
            e.expect_write.is_none() || e.expect_write.as_ref() == Some(&data.to_vec())
        });

        match expectation {
            Some(idx) => {
                let exp = self.expectations.remove(idx).unwrap();
                match exp.action {
                    MockAction::Respond(response) => {
                        self.read_buffer.extend(response);
                    }
                    MockAction::Error(msg) => {
                        return Err(TransportError::WriteFailed(msg));
                    }
                    MockAction::Ack => {
                        // No response
                    }
                    MockAction::Timeout => {
                        // No response (will timeout on read)
                    }
                }
            }
            None => {
                if self.strict {
                    return Err(TransportError::WriteFailed(format!(
                        "Unexpected write: {:?}",
                        String::from_utf8_lossy(data)
                    )));
                }
                // Non-strict: just log and continue
            }
        }

        Ok(data.len())
    }

    fn read(&mut self, max_bytes: usize, _timeout: Duration) -> Result<Vec<u8>, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }

        let take = std::cmp::min(max_bytes, self.read_buffer.len());
        let result: Vec<u8> = self.read_buffer.drain(..take).collect();
        Ok(result)
    }

    fn drain(&mut self) -> Result<Vec<u8>, TransportError> {
        if !self.is_open {
            return Err(TransportError::NotOpen);
        }
        let result: Vec<u8> = self.read_buffer.drain(..).collect();
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
    fn test_mock_scripted_response() {
        let mut transport = MockTransport::new(vec![Expectation::on_write(b"AT\r\n", b"OK\r\n")]);
        transport.open(&PortConfig::new("mock://", 115200)).unwrap();

        transport.write(b"AT\r\n").unwrap();
        let response = transport.read(100, Duration::from_millis(100)).unwrap();
        assert_eq!(response, b"OK\r\n");
        assert!(transport.all_expectations_met());
    }

    #[test]
    fn test_mock_any_write() {
        let mut transport = MockTransport::new(vec![Expectation::any_write(b"RESPONSE")]);
        transport.open(&PortConfig::new("mock://", 115200)).unwrap();

        transport.write(b"anything").unwrap();
        let response = transport.read(100, Duration::from_millis(100)).unwrap();
        assert_eq!(response, b"RESPONSE");
    }

    #[test]
    fn test_mock_strict_unexpected() {
        let mut transport = MockTransport::new(vec![]);
        transport.open(&PortConfig::new("mock://", 115200)).unwrap();

        let result = transport.write(b"unexpected");
        assert!(result.is_err());
    }

    #[test]
    fn test_mock_non_strict() {
        let mut transport = MockTransport::empty().strict(false);
        transport.open(&PortConfig::new("mock://", 115200)).unwrap();

        transport.write(b"unexpected").unwrap();
        assert_eq!(transport.write_log().len(), 1);
    }
}
