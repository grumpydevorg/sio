//! Framing abstractions for extracting messages from byte streams.
//!
//! The core abstractions are:
//! - [`Deframer`] - Extracts complete frames from incoming bytes
//! - [`Framer`] - Wraps payloads into wire format
//!
//! Built-in implementations:
//! - [`LineDeframer`] - Line-based framing (splits on `\n` or `\r\n`)
//! - [`NullDeframer`] - Pass-through (no framing)

mod line;
mod timing;

pub use line::{LineDeframer, LineFramer};
pub use timing::TimingDeframer;

/// Extracts complete frames from a byte stream.
///
/// Implementations buffer incoming bytes and emit complete frames
/// when delimiters or length requirements are met.
pub trait Deframer: Send {
    /// Feed raw bytes into the deframer.
    ///
    /// Bytes are buffered internally until a complete frame can be extracted.
    fn feed(&mut self, bytes: &[u8]);

    /// Extract the next complete frame, if available.
    ///
    /// Returns `None` if no complete frame is buffered.
    fn next_frame(&mut self) -> Option<Vec<u8>>;

    /// Check if there's a complete frame available without consuming it.
    fn has_frame(&self) -> bool;

    /// Get the number of bytes currently buffered.
    fn buffered_bytes(&self) -> usize;

    /// Reset the deframer state, discarding any buffered data.
    fn reset(&mut self);
}

/// Wraps payloads into wire format.
///
/// The inverse of [`Deframer`] - takes a payload and produces
/// bytes suitable for transmission.
pub trait Framer: Send {
    /// Wrap a payload into wire format.
    fn frame(&self, payload: &[u8]) -> Vec<u8>;
}

/// A pass-through deframer that treats each feed as a complete frame.
///
/// Useful when no framing is needed (raw byte mode).
#[derive(Default)]
pub struct NullDeframer {
    buffer: Vec<u8>,
}

impl NullDeframer {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }
}

impl Deframer for NullDeframer {
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buffer))
        }
    }

    fn has_frame(&self) -> bool {
        !self.buffer.is_empty()
    }

    fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// A pass-through framer that returns payloads unchanged.
#[derive(Default)]
pub struct NullFramer;

impl NullFramer {
    pub fn new() -> Self {
        Self
    }
}

impl Framer for NullFramer {
    fn frame(&self, payload: &[u8]) -> Vec<u8> {
        payload.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_deframer() {
        let mut deframer = NullDeframer::new();

        deframer.feed(b"hello");
        assert!(deframer.has_frame());

        let frame = deframer.next_frame().unwrap();
        assert_eq!(frame, b"hello");

        assert!(!deframer.has_frame());
        assert!(deframer.next_frame().is_none());
    }

    #[test]
    fn test_null_framer() {
        let framer = NullFramer::new();
        assert_eq!(framer.frame(b"test"), b"test");
    }
}
