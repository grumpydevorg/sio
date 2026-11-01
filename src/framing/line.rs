//! Line-based framing (splits on newlines).

use super::{Deframer, Framer};

/// A deframer that splits on line endings (`\n` or `\r\n`).
///
/// Commonly used for:
/// - NMEA GPS sentences
/// - AT command responses
/// - Text-based protocols
///
/// # Example
///
/// ```
/// use sio::framing::{Deframer, LineDeframer};
///
/// let mut deframer = LineDeframer::new();
///
/// deframer.feed(b"$GPGGA,12");
/// assert!(deframer.next_frame().is_none()); // incomplete
///
/// deframer.feed(b"3456.00,*XX\r\n$GP");
/// let frame = deframer.next_frame().unwrap();
/// assert_eq!(frame, b"$GPGGA,123456.00,*XX");
///
/// // Partial next frame still buffered
/// assert!(!deframer.has_frame());
/// assert_eq!(deframer.buffered_bytes(), 3); // "$GP"
/// ```
pub struct LineDeframer {
    buffer: Vec<u8>,
    max_line_length: usize,
    include_newline: bool,
}

impl LineDeframer {
    /// Create a new line deframer with default settings.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
            max_line_length: 8192,
            include_newline: false,
        }
    }

    /// Set the maximum line length (default: 8192).
    ///
    /// Lines exceeding this length will be truncated.
    pub fn max_length(mut self, max: usize) -> Self {
        self.max_line_length = max;
        self
    }

    /// Include the newline character(s) in the returned frame (default: false).
    pub fn include_newline(mut self, include: bool) -> Self {
        self.include_newline = include;
        self
    }

    fn find_newline(&self) -> Option<usize> {
        self.buffer.iter().position(|&b| b == b'\n')
    }
}

impl Default for LineDeframer {
    fn default() -> Self {
        Self::new()
    }
}

impl Deframer for LineDeframer {
    fn feed(&mut self, bytes: &[u8]) {
        // Enforce max length by dropping old data if needed
        let available = self.max_line_length.saturating_sub(self.buffer.len());
        let take = std::cmp::min(bytes.len(), available);
        self.buffer.extend_from_slice(&bytes[..take]);
    }

    fn next_frame(&mut self) -> Option<Vec<u8>> {
        let newline_pos = self.find_newline()?;

        let mut end = newline_pos;
        if self.include_newline {
            end += 1;
        }

        // Handle \r\n
        let start_skip =
            if !self.include_newline && newline_pos > 0 && self.buffer[newline_pos - 1] == b'\r' {
                1
            } else {
                0
            };

        let frame = self.buffer[..end - start_skip].to_vec();
        self.buffer.drain(..newline_pos + 1);
        Some(frame)
    }

    fn has_frame(&self) -> bool {
        self.find_newline().is_some()
    }

    fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// A framer that appends a newline to payloads.
pub struct LineFramer {
    line_ending: Vec<u8>,
}

impl LineFramer {
    /// Create a line framer with `\r\n` ending.
    pub fn crlf() -> Self {
        Self {
            line_ending: b"\r\n".to_vec(),
        }
    }

    /// Create a line framer with `\n` ending.
    pub fn lf() -> Self {
        Self {
            line_ending: b"\n".to_vec(),
        }
    }
}

impl Default for LineFramer {
    fn default() -> Self {
        Self::crlf()
    }
}

impl Framer for LineFramer {
    fn frame(&self, payload: &[u8]) -> Vec<u8> {
        let mut result = Vec::with_capacity(payload.len() + self.line_ending.len());
        result.extend_from_slice(payload);
        result.extend_from_slice(&self.line_ending);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_deframer_basic() {
        let mut deframer = LineDeframer::new();

        deframer.feed(b"hello\nworld\n");

        assert_eq!(deframer.next_frame(), Some(b"hello".to_vec()));
        assert_eq!(deframer.next_frame(), Some(b"world".to_vec()));
        assert_eq!(deframer.next_frame(), None);
    }

    #[test]
    fn test_line_deframer_crlf() {
        let mut deframer = LineDeframer::new();

        deframer.feed(b"line1\r\nline2\r\n");

        assert_eq!(deframer.next_frame(), Some(b"line1".to_vec()));
        assert_eq!(deframer.next_frame(), Some(b"line2".to_vec()));
    }

    #[test]
    fn test_line_deframer_partial() {
        let mut deframer = LineDeframer::new();

        deframer.feed(b"hel");
        assert!(!deframer.has_frame());

        deframer.feed(b"lo\nwor");
        assert!(deframer.has_frame());

        assert_eq!(deframer.next_frame(), Some(b"hello".to_vec()));
        assert!(!deframer.has_frame());
        assert_eq!(deframer.buffered_bytes(), 3);
    }

    #[test]
    fn test_line_deframer_include_newline() {
        let mut deframer = LineDeframer::new().include_newline(true);

        deframer.feed(b"hello\n");
        assert_eq!(deframer.next_frame(), Some(b"hello\n".to_vec()));
    }

    #[test]
    fn test_line_framer_crlf() {
        let framer = LineFramer::crlf();
        assert_eq!(framer.frame(b"hello"), b"hello\r\n");
    }

    #[test]
    fn test_line_framer_lf() {
        let framer = LineFramer::lf();
        assert_eq!(framer.frame(b"hello"), b"hello\n");
    }
}
