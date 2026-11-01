//! In-memory echo device: written bytes become readable bytes.

use super::{ModemStatus, PortConfig, SerialTransport, TransportError};
use std::time::Duration;

/// Echo device for dry runs and tests. Any port path is accepted.
#[derive(Debug, Default)]
pub struct LoopbackTransport {
    open: Option<Echo>,
}

#[derive(Debug)]
struct Echo {
    buffer: Vec<u8>,
    modem: ModemStatus,
}

impl LoopbackTransport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn echo(&mut self) -> Result<&mut Echo, TransportError> {
        self.open.as_mut().ok_or(TransportError::NotOpen)
    }
}

impl SerialTransport for LoopbackTransport {
    fn open(&mut self, _config: &PortConfig) -> Result<(), TransportError> {
        if self.open.is_some() {
            return Err(TransportError::AlreadyOpen);
        }
        self.open = Some(Echo {
            buffer: Vec::new(),
            modem: ModemStatus {
                dtr: Some(false),
                rts: Some(false),
                cts: Some(true),
                dsr: Some(true),
                dcd: Some(true),
            },
        });
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.open.take().map(drop).ok_or(TransportError::NotOpen)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.echo()?.buffer.extend_from_slice(data);
        Ok(())
    }

    fn read_into(
        &mut self,
        buf: &mut Vec<u8>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let echo = self.echo()?;
        if echo.buffer.is_empty() {
            // Nothing else can write to an echo device while we wait.
            std::thread::sleep(timeout);
            return Ok(());
        }
        let count = max_bytes.min(echo.buffer.len());
        buf.extend(echo.buffer.drain(..count));
        Ok(())
    }

    fn modem_control(
        &mut self,
        dtr: Option<bool>,
        rts: Option<bool>,
    ) -> Result<ModemStatus, TransportError> {
        let echo = self.echo()?;
        if let Some(level) = dtr {
            echo.modem.dtr = Some(level);
        }
        if let Some(level) = rts {
            echo.modem.rts = Some(level);
        }
        Ok(echo.modem)
    }

    fn send_break(&mut self, _duration: Duration) -> Result<(), TransportError> {
        self.echo().map(drop)
    }
}
