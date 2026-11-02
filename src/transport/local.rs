//! Operating-system serial ports through the `serialport` crate.

use super::{
    DataBits, LineState, ModemStatus, Parity, PortConfig, PortInfo, SerialTransport, StopBits,
    TransportError,
};
use serialport::{SerialPort, SerialPortType};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// Shortest silence that ends a read once data has arrived.
const MIN_IDLE_GAP: Duration = Duration::from_millis(50);
/// Silence measured in characters at the configured line rate.
const IDLE_GAP_CHARS: usize = 3;
/// Slack added to the computed transmit time of a write.
const WRITE_SLACK: Duration = Duration::from_secs(1);

/// A hardware serial port.
#[derive(Default)]
pub struct LocalTransport {
    open: Option<OpenPort>,
}

struct OpenPort {
    port: Box<dyn SerialPort>,
    config: PortConfig,
}

impl LocalTransport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn port(&mut self) -> Result<&mut OpenPort, TransportError> {
        self.open.as_mut().ok_or(TransportError::NotOpen)
    }

    /// Classify an I/O failure; a vanished device closes the port.
    fn io_failure(&mut self, operation: &'static str, error: io::Error) -> TransportError {
        let path = self.open.as_ref().map(|open| open.config.path.as_str());
        if is_disconnect(&error, path) {
            self.open = None;
            TransportError::Disconnected
        } else {
            TransportError::Io {
                operation,
                source: error,
            }
        }
    }
}

impl SerialTransport for LocalTransport {
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError> {
        if self.open.is_some() {
            return Err(TransportError::AlreadyOpen);
        }
        let builder = serialport::new(&config.path, config.baud.get())
            .data_bits(match config.data_bits {
                DataBits::Five => serialport::DataBits::Five,
                DataBits::Six => serialport::DataBits::Six,
                DataBits::Seven => serialport::DataBits::Seven,
                DataBits::Eight => serialport::DataBits::Eight,
            })
            .parity(match config.parity {
                Parity::None => serialport::Parity::None,
                Parity::Odd => serialport::Parity::Odd,
                Parity::Even => serialport::Parity::Even,
            })
            .stop_bits(match config.stop_bits {
                StopBits::One => serialport::StopBits::One,
                StopBits::Two => serialport::StopBits::Two,
            });
        let builder = match config.dtr {
            LineState::Preserve => builder.preserve_dtr_on_open(),
            LineState::On => builder.dtr_on_open(true),
            LineState::Off => builder.dtr_on_open(false),
        };
        let mut port = builder
            .open()
            .map_err(|error| open_failure(&config.path, error))?;
        match config.rts {
            LineState::Preserve => {}
            LineState::On | LineState::Off => port
                .write_request_to_send(config.rts == LineState::On)
                .map_err(|error| TransportError::Io {
                    operation: "set RTS",
                    source: error.into(),
                })?,
        }
        self.open = Some(OpenPort {
            port,
            config: config.clone(),
        });
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.open.take().map(drop).ok_or(TransportError::NotOpen)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let open = self.port()?;
        let budget = open.config.transmit_time(data.len()).saturating_mul(2) + WRITE_SLACK;
        let deadline = Instant::now() + budget;
        let mut written = 0;
        let failure = loop {
            if written == data.len() {
                break open.port.flush().err();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break Some(io::Error::new(ErrorKind::TimedOut, "write timed out"));
            }
            if let Err(error) = open.port.set_timeout(remaining) {
                break Some(error.into());
            }
            match open.port.write(&data[written..]) {
                Ok(0) => break Some(io::Error::from(ErrorKind::WriteZero)),
                Ok(count) => written += count,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => break Some(error),
            }
        };
        let Some(error) = failure else {
            return Ok(());
        };
        match self.io_failure("write", error) {
            TransportError::Io { source, .. } => Err(TransportError::Write {
                written,
                total: data.len(),
                source,
            }),
            other => Err(other),
        }
    }

    fn read_into(
        &mut self,
        buf: &mut Vec<u8>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let open = self.port()?;
        let start = Instant::now();
        let idle_gap = open.config.transmit_time(IDLE_GAP_CHARS).max(MIN_IDLE_GAP);
        let target = buf.len().saturating_add(max_bytes);
        let mut chunk = [0_u8; 4096];
        let mut received = false;
        let failure = loop {
            if buf.len() >= target {
                break None;
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            let wait = if received {
                remaining.min(idle_gap)
            } else {
                remaining
            };
            if wait.is_zero() {
                break None;
            }
            if let Err(error) = open.port.set_timeout(wait) {
                break Some(error.into());
            }
            let wanted = chunk.len().min(target - buf.len());
            match open.port.read(&mut chunk[..wanted]) {
                // A ready descriptor that yields no bytes is a hang-up.
                Ok(0) => break Some(io::Error::from(ErrorKind::UnexpectedEof)),
                Ok(count) => {
                    buf.extend_from_slice(&chunk[..count]);
                    received = true;
                }
                Err(error) if error.kind() == ErrorKind::TimedOut => {
                    if received {
                        break None;
                    }
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => break Some(error),
            }
        };
        match failure {
            None => Ok(()),
            Some(error) => Err(self.io_failure("read", error)),
        }
    }

    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        let open = self.port()?;
        let result = (|| -> serialport::Result<ModemStatus> {
            if let Some(level) = dtr {
                open.port.write_data_terminal_ready(level)?;
            }
            if let Some(level) = rts {
                open.port.write_request_to_send(level)?;
            }
            Ok(ModemStatus {
                dtr,
                rts,
                cts: Some(open.port.read_clear_to_send()?),
                dsr: Some(open.port.read_data_set_ready()?),
                dcd: Some(open.port.read_carrier_detect()?),
            })
        })();
        result.map_err(|error| self.io_failure("modem control", error.into()))
    }

    fn send_break(&mut self, duration: Duration) -> Result<(), TransportError> {
        let open = self.port()?;
        let result = open.port.set_break().and_then(|()| {
            std::thread::sleep(duration);
            open.port.clear_break()
        });
        result.map_err(|error| self.io_failure("break", error.into()))
    }
}

fn is_listed(path: &str) -> bool {
    serialport::available_ports().is_ok_and(|ports| {
        ports
            .iter()
            .any(|port| port.port_name.eq_ignore_ascii_case(path))
    })
}

fn open_failure(path: &str, error: serialport::Error) -> TransportError {
    let path = path.to_owned();
    match error.kind() {
        // Windows reports a missing port, a busy port and access denied all
        // as NoDevice; a port the system does not list is the missing one.
        serialport::ErrorKind::NoDevice if cfg!(windows) && !is_listed(&path) => {
            TransportError::PortNotFound { path }
        }
        serialport::ErrorKind::NoDevice => TransportError::PortBusy { path },
        serialport::ErrorKind::Io(ErrorKind::NotFound) => TransportError::PortNotFound { path },
        serialport::ErrorKind::Io(ErrorKind::PermissionDenied) => {
            TransportError::PermissionDenied { path }
        }
        _ => TransportError::Io {
            operation: "open",
            source: error.into(),
        },
    }
}

fn is_disconnect(error: &io::Error, path: Option<&str>) -> bool {
    if matches!(
        error.kind(),
        ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::NotConnected
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
    ) {
        return true;
    }
    // A USB adapter's device node disappears when it is unplugged; the error
    // itself is then an unspecific EIO or ENXIO.
    cfg!(unix) && path.is_some_and(|path| !std::path::Path::new(path).exists())
}

/// List serial ports, collapsing each macOS `cu.`/`tty.` pair to one entry
/// that prefers the `cu.` device.
pub fn list_ports() -> Result<Vec<PortInfo>, TransportError> {
    let ports = serialport::available_ports().map_err(|error| TransportError::Io {
        operation: "list ports",
        source: error.into(),
    })?;
    let mut unique: BTreeMap<String, PortInfo> = BTreeMap::new();
    for port in ports {
        let candidate = port_info(port);
        let key = macos_pair_key(&candidate.path).to_owned();
        match unique.get(&key) {
            Some(existing) if existing.recommended_for_open || !candidate.recommended_for_open => {}
            _ => {
                unique.insert(key, candidate);
            }
        }
    }
    Ok(unique.into_values().collect())
}

fn port_info(port: serialport::SerialPortInfo) -> PortInfo {
    let recommended_for_open = !cfg!(target_os = "macos") || port.port_name.starts_with("/dev/cu.");
    let mut info = PortInfo {
        path: port.port_name,
        port_type: "unknown",
        recommended_for_open,
        vid: None,
        pid: None,
        serial: None,
        manufacturer: None,
        product: None,
    };
    match port.port_type {
        SerialPortType::UsbPort(usb) => {
            info.port_type = "usb";
            info.vid = Some(usb.vid);
            info.pid = Some(usb.pid);
            info.serial = usb.serial_number;
            info.manufacturer = usb.manufacturer;
            info.product = usb.product;
        }
        SerialPortType::PciPort => info.port_type = "pci",
        SerialPortType::BluetoothPort => info.port_type = "bluetooth",
        SerialPortType::Unknown => {}
    }
    info
}

fn macos_pair_key(path: &str) -> &str {
    if cfg!(target_os = "macos") {
        path.strip_prefix("/dev/cu.")
            .or_else(|| path.strip_prefix("/dev/tty."))
            .unwrap_or(path)
    } else {
        path
    }
}
