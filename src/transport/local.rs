//! Local hardware serial transport using the `serialport` crate.

use super::{
    LineState, ModemStatus, Parity, PortConfig, PortInfo, SerialTransport, TransportError,
};
use serialport::{DataBits, SerialPort, StopBits};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// Transport implementation for local hardware serial ports.
pub struct LocalTransport {
    port: Option<Box<dyn SerialPort>>,
    path: Option<String>,
}

impl LocalTransport {
    /// Create a new local transport (not yet connected).
    pub fn new() -> Self {
        Self {
            port: None,
            path: None,
        }
    }

    /// List available serial ports on the system.
    pub fn list_ports() -> Result<Vec<PortInfo>, TransportError> {
        let ports = serialport::available_ports()
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

        Ok(ports
            .into_iter()
            .map(|p| {
                let (port_type, vid, pid, serial, manufacturer, product) = match p.port_type {
                    serialport::SerialPortType::UsbPort(info) => (
                        "usb".to_string(),
                        Some(info.vid),
                        Some(info.pid),
                        info.serial_number,
                        info.manufacturer,
                        info.product,
                    ),
                    serialport::SerialPortType::PciPort => {
                        ("pci".to_string(), None, None, None, None, None)
                    }
                    serialport::SerialPortType::BluetoothPort => {
                        ("bluetooth".to_string(), None, None, None, None, None)
                    }
                    serialport::SerialPortType::Unknown => {
                        ("unknown".to_string(), None, None, None, None, None)
                    }
                };
                PortInfo {
                    path: p.port_name,
                    port_type,
                    vid,
                    pid,
                    serial,
                    manufacturer,
                    product,
                }
            })
            .collect())
    }
}

impl Default for LocalTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SerialTransport for LocalTransport {
    fn open(&mut self, config: &PortConfig) -> Result<(), TransportError> {
        if self.port.is_some() {
            return Err(TransportError::AlreadyOpen);
        }

        if !(1..=4_000_000).contains(&config.baud) {
            return Err(TransportError::InvalidConfig(
                "baud must be between 1 and 4000000".to_owned(),
            ));
        }

        let data_bits = match config.data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            8 => DataBits::Eight,
            _ => {
                return Err(TransportError::InvalidConfig(
                    "data bits must be 5, 6, 7, or 8".to_owned(),
                ));
            }
        };

        let stop_bits = match config.stop_bits {
            1 => StopBits::One,
            2 => StopBits::Two,
            _ => {
                return Err(TransportError::InvalidConfig(
                    "stop bits must be 1 or 2".to_owned(),
                ));
            }
        };

        let parity = match config.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        };

        let mut builder = serialport::new(&config.path, config.baud)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .timeout(Duration::from_millis(50));
        builder = match config.dtr_on_open {
            LineState::Preserve => builder.preserve_dtr_on_open(),
            LineState::On => builder.dtr_on_open(true),
            LineState::Off => builder.dtr_on_open(false),
        };

        let mut port = builder
            .open()
            .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?;

        match config.rts_on_open {
            LineState::Preserve => {}
            LineState::On => port
                .write_request_to_send(true)
                .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?,
            LineState::Off => port
                .write_request_to_send(false)
                .map_err(|e| TransportError::ConnectionFailed(e.to_string()))?,
        }

        self.port = Some(port);
        self.path = Some(config.path.clone());
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.port.is_none() {
            return Err(TransportError::NotOpen);
        }
        self.port = None;
        self.path = None;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.port.is_some()
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotOpen)?;
        let n = port
            .write(data)
            .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        port.flush()
            .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        Ok(n)
    }

    fn read(&mut self, max_bytes: usize, timeout: Duration) -> Result<Vec<u8>, TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotOpen)?;
        if max_bytes == 0 || timeout.is_zero() {
            return Ok(Vec::new());
        }

        let driver_timeout = timeout
            .min(Duration::from_millis(50))
            .max(Duration::from_millis(1));
        port.set_timeout(driver_timeout)
            .map_err(|e| TransportError::ReadFailed(e.to_string()))?;
        let start = Instant::now();
        let mut result = Vec::with_capacity(max_bytes);
        let mut buf = [0u8; 256];

        while result.len() < max_bytes && start.elapsed() < timeout {
            let wanted = buf.len().min(max_bytes - result.len());
            match port.read(&mut buf[..wanted]) {
                Ok(0) => continue,
                Ok(count) => {
                    result.extend_from_slice(&buf[..count]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    if !result.is_empty() {
                        break;
                    }
                }
                Err(e) => return Err(TransportError::ReadFailed(e.to_string())),
            }
        }

        Ok(result)
    }

    fn drain(&mut self) -> Result<Vec<u8>, TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotOpen)?;
        const MAX_DRAIN_BYTES: usize = 1024 * 1024;
        let mut result = Vec::new();
        let mut buf = [0u8; 1024];

        while result.len() < MAX_DRAIN_BYTES {
            let wanted = buf.len().min(MAX_DRAIN_BYTES - result.len());
            match port.read(&mut buf[..wanted]) {
                Ok(0) => break,
                Ok(count) => result.extend_from_slice(&buf[..count]),
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => return Err(TransportError::ReadFailed(e.to_string())),
            }
        }

        Ok(result)
    }

    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotOpen)?;

        if let Some(v) = dtr {
            port.write_data_terminal_ready(v)
                .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        }
        if let Some(v) = rts {
            port.write_request_to_send(v)
                .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        }

        Ok(ModemStatus {
            dtr,
            rts,
            cts: port.read_clear_to_send().ok(),
            dsr: port.read_data_set_ready().ok(),
            dcd: port.read_carrier_detect().ok(),
        })
    }

    fn send_break(&mut self, duration: Duration) -> Result<(), TransportError> {
        let port = self.port.as_mut().ok_or(TransportError::NotOpen)?;
        port.set_break()
            .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        std::thread::sleep(duration);
        port.clear_break()
            .map_err(|e| TransportError::WriteFailed(e.to_string()))?;
        Ok(())
    }

    fn port_path(&self) -> Option<&str> {
        self.path.as_deref()
    }
}
