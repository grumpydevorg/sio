//! Observer wrapper for recording serial traffic.
//!
//! Wraps any transport and records all traffic for debugging or test fixture generation.

use super::{ModemStatus, PortConfig, SerialTransport, TransportError};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Events that can be observed during serial communication.
#[derive(Debug, Clone)]
pub enum TrafficEvent {
    /// Port was opened
    Open {
        timestamp: Instant,
        config: PortConfig,
    },
    /// Port was closed
    Close { timestamp: Instant },
    /// Data was written
    Write {
        timestamp: Instant,
        data: Vec<u8>,
        bytes_written: usize,
    },
    /// Data was read
    Read {
        timestamp: Instant,
        data: Vec<u8>,
        timeout: Duration,
    },
    /// Modem control was called
    ModemControl {
        timestamp: Instant,
        dtr: Option<bool>,
        rts: Option<bool>,
        status: ModemStatus,
    },
    /// Break signal was sent
    Break {
        timestamp: Instant,
        duration: Duration,
    },
    /// An error occurred
    Error {
        timestamp: Instant,
        operation: String,
        error: String,
    },
}

/// Trait for observers that want to receive traffic events.
pub trait TrafficObserver: Send {
    /// Called for each traffic event.
    fn observe(&mut self, event: TrafficEvent);

    /// Called when observation ends (transport dropped or closed).
    fn flush(&mut self) {}
}

/// A simple observer that logs to a writer (file, stderr, etc).
pub struct LoggingObserver<W: Write + Send> {
    writer: W,
    start_time: Instant,
}

impl<W: Write + Send> LoggingObserver<W> {
    /// Create a new logging observer.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            start_time: Instant::now(),
        }
    }

    fn elapsed_ms(&self, timestamp: Instant) -> u128 {
        timestamp.duration_since(self.start_time).as_millis()
    }

    fn format_bytes(data: &[u8]) -> String {
        if data
            .iter()
            .all(|&b| b.is_ascii_graphic() || b.is_ascii_whitespace())
        {
            format!("{:?}", String::from_utf8_lossy(data))
        } else {
            format!("hex:{}", hex::encode(data))
        }
    }
}

impl<W: Write + Send> TrafficObserver for LoggingObserver<W> {
    fn observe(&mut self, event: TrafficEvent) {
        let line = match event {
            TrafficEvent::Open { timestamp, config } => {
                format!(
                    "[{:>8}ms] OPEN {} @ {} baud\n",
                    self.elapsed_ms(timestamp),
                    config.path,
                    config.baud
                )
            }
            TrafficEvent::Close { timestamp } => {
                format!("[{:>8}ms] CLOSE\n", self.elapsed_ms(timestamp))
            }
            TrafficEvent::Write {
                timestamp,
                data,
                bytes_written,
            } => {
                format!(
                    "[{:>8}ms] WRITE {} ({} bytes)\n",
                    self.elapsed_ms(timestamp),
                    Self::format_bytes(&data),
                    bytes_written
                )
            }
            TrafficEvent::Read {
                timestamp, data, ..
            } => {
                if data.is_empty() {
                    format!("[{:>8}ms] READ (empty)\n", self.elapsed_ms(timestamp))
                } else {
                    format!(
                        "[{:>8}ms] READ {} ({} bytes)\n",
                        self.elapsed_ms(timestamp),
                        Self::format_bytes(&data),
                        data.len()
                    )
                }
            }
            TrafficEvent::ModemControl {
                timestamp,
                dtr,
                rts,
                status,
            } => {
                format!(
                    "[{:>8}ms] MODEM dtr={:?} rts={:?} -> cts={:?} dsr={:?} dcd={:?}\n",
                    self.elapsed_ms(timestamp),
                    dtr,
                    rts,
                    status.cts,
                    status.dsr,
                    status.dcd
                )
            }
            TrafficEvent::Break {
                timestamp,
                duration,
            } => {
                format!(
                    "[{:>8}ms] BREAK {}ms\n",
                    self.elapsed_ms(timestamp),
                    duration.as_millis()
                )
            }
            TrafficEvent::Error {
                timestamp,
                operation,
                error,
            } => {
                format!(
                    "[{:>8}ms] ERROR in {}: {}\n",
                    self.elapsed_ms(timestamp),
                    operation,
                    error
                )
            }
        };
        let _ = self.writer.write_all(line.as_bytes());
    }

    fn flush(&mut self) {
        let _ = self.writer.flush();
    }
}

/// An observer that collects events into a vector.
#[cfg(test)]
#[derive(Default)]
pub struct CollectingObserver {
    events: Vec<TrafficEvent>,
}

#[cfg(test)]
impl CollectingObserver {
    /// Create a new collecting observer.
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Take ownership of collected events.
    pub fn into_events(self) -> Vec<TrafficEvent> {
        self.events
    }
}

#[cfg(test)]
impl TrafficObserver for CollectingObserver {
    fn observe(&mut self, event: TrafficEvent) {
        self.events.push(event);
    }
}

/// Wrapper for Arc<Mutex<O>> to make observers shareable.
impl<O: TrafficObserver> TrafficObserver for Arc<Mutex<O>> {
    fn observe(&mut self, event: TrafficEvent) {
        if let Ok(mut observer) = self.lock() {
            observer.observe(event);
        }
    }

    fn flush(&mut self) {
        if let Ok(mut observer) = self.lock() {
            observer.flush();
        }
    }
}

/// A transport wrapper that observes all traffic.
///
/// Wraps any `SerialTransport` and records all operations to an observer.
///
/// # Example
///
/// ```ignore
/// use sio::transport::*;
///
/// let transport = LocalTransport::new();
/// let observer = LoggingObserver::new(std::io::stderr());
/// let mut transport = ObservingTransport::new(transport, observer);
///
/// // All operations are now logged to stderr
/// transport.open(&PortConfig::new("/dev/ttyUSB0", 115200))?;
/// transport.write(b"AT\r\n")?;
/// ```
pub struct ObservingTransport<T: SerialTransport, O: TrafficObserver> {
    inner: T,
    observer: O,
}

impl<T: SerialTransport, O: TrafficObserver> ObservingTransport<T, O> {
    /// Create a new observing transport.
    pub fn new(inner: T, observer: O) -> Self {
        Self { inner, observer }
    }

    /// Get a reference to the inner transport.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Get a mutable reference to the inner transport.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Get a reference to the observer.
    pub fn observer(&self) -> &O {
        &self.observer
    }

    /// Get a mutable reference to the observer.
    pub fn observer_mut(&mut self) -> &mut O {
        &mut self.observer
    }

    /// Unwrap into inner transport and observer.
    pub fn into_parts(self) -> (T, O) {
        (self.inner, self.observer)
    }
}

impl<T: SerialTransport, O: TrafficObserver> SerialTransport for ObservingTransport<T, O> {
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError> {
        let result = self.inner.open(config);
        match &result {
            Ok(()) => {
                self.observer.observe(TrafficEvent::Open {
                    timestamp: Instant::now(),
                    config: config.clone(),
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "open".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let result = self.inner.close();
        match &result {
            Ok(()) => {
                self.observer.observe(TrafficEvent::Close {
                    timestamp: Instant::now(),
                });
                self.observer.flush();
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "close".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        let result = self.inner.write(data);
        match &result {
            Ok(bytes_written) => {
                self.observer.observe(TrafficEvent::Write {
                    timestamp: Instant::now(),
                    data: data.to_vec(),
                    bytes_written: *bytes_written,
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "write".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn read(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let result = self.inner.read(max_bytes, timeout);
        match &result {
            Ok(data) => {
                self.observer.observe(TrafficEvent::Read {
                    timestamp: Instant::now(),
                    data: data.clone(),
                    timeout,
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "read".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn drain(&mut self) -> Result<Vec<u8>, TransportError> {
        let result = self.inner.drain();
        match &result {
            Ok(data) => {
                self.observer.observe(TrafficEvent::Read {
                    timestamp: Instant::now(),
                    data: data.clone(),
                    timeout: Duration::ZERO,
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "drain".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        let result = self.inner.modem_control(dtr, rts);
        match &result {
            Ok(status) => {
                self.observer.observe(TrafficEvent::ModemControl {
                    timestamp: Instant::now(),
                    dtr,
                    rts,
                    status: status.clone(),
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "modem_control".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn send_break(&mut self, duration: Duration) -> Result<(), TransportError> {
        let result = self.inner.send_break(duration);
        match &result {
            Ok(()) => {
                self.observer.observe(TrafficEvent::Break {
                    timestamp: Instant::now(),
                    duration,
                });
            }
            Err(e) => {
                self.observer.observe(TrafficEvent::Error {
                    timestamp: Instant::now(),
                    operation: "send_break".to_string(),
                    error: e.to_string(),
                });
            }
        }
        result
    }

    fn port_path(&self) -> Option<&str> {
        self.inner.port_path()
    }
}

// Need hex for formatting
mod hex {
    pub fn encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::LoopbackTransport;

    #[test]
    fn test_observing_transport() {
        let inner = LoopbackTransport::new();
        let observer = CollectingObserver::new();
        let mut transport = ObservingTransport::new(inner, observer);

        transport.open(&PortConfig::new("loop://", 115200)).unwrap();
        transport.write(b"test").unwrap();
        transport.read(10, Duration::from_millis(100)).unwrap();
        transport.close().unwrap();

        let (_, observer) = transport.into_parts();
        let events = observer.into_events();

        assert_eq!(events.len(), 4); // open, write, read, close
    }
}
