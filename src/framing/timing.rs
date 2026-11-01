//! Timing-based framing for protocols like Modbus RTU.
//!
//! Modbus RTU uses inter-frame gaps (silence) to delimit frames instead of
//! byte markers. A frame ends when there's a gap of 3.5 character times.
//!
//! At 9600 baud with 8N1 (10 bits/char), 3.5 chars = 3.646ms
//! At 115200 baud with 8N1, 3.5 chars = 0.304ms

use super::Deframer;
use std::time::{Duration, Instant};

/// A deframer that uses inter-byte timing to detect frame boundaries.
///
/// Used for Modbus RTU and similar protocols where frames are delimited
/// by silence (time gaps) rather than byte markers.
///
/// # How it works
///
/// 1. Bytes are fed with timestamps
/// 2. If the gap since the last byte exceeds the threshold, the current
///    buffer is considered a complete frame
/// 3. New bytes start a new frame
///
/// # Example
///
/// ```
/// use sio::framing::{Deframer, TimingDeframer};
/// use std::time::Duration;
///
/// // For Modbus RTU at 9600 baud: 3.5 char times ≈ 3.65ms
/// let mut deframer = TimingDeframer::new(Duration::from_micros(3650));
///
/// // Simulate receiving a frame
/// deframer.feed(b"\x01\x03\x00\x00");
/// // ... time passes (less than threshold) ...
/// deframer.feed(b"\x00\x0A");
///
/// // No frame yet (we're still within the timing window)
/// assert!(!deframer.has_frame());
///
/// // Mark end of reception (call with empty slice after timeout)
/// deframer.mark_idle();
///
/// // Now we have a complete frame
/// let frame = deframer.next_frame().unwrap();
/// assert_eq!(frame, b"\x01\x03\x00\x00\x00\x0A");
/// ```
pub struct TimingDeframer {
    buffer: Vec<u8>,
    last_byte_time: Option<Instant>,
    frame_ready: bool,
    inter_frame_gap: Duration,
    max_frame_size: usize,
}

impl TimingDeframer {
    /// Create a new timing deframer with the specified inter-frame gap.
    ///
    /// The gap should be the minimum silence time that indicates a frame
    /// boundary. For Modbus RTU, this is 3.5 character times.
    pub fn new(inter_frame_gap: Duration) -> Self {
        Self {
            buffer: Vec::with_capacity(256),
            last_byte_time: None,
            frame_ready: false,
            inter_frame_gap,
            max_frame_size: 256, // Modbus RTU max is 256
        }
    }

    /// Create a deframer configured for Modbus RTU at the given baud rate.
    ///
    /// Calculates the 3.5 character time gap based on baud rate.
    /// Assumes 8N1 (10 bits per character including start/stop).
    pub fn modbus_rtu(baud_rate: u32) -> Self {
        // 3.5 character times
        // Character time = 10 bits / baud_rate (for 8N1)
        // 3.5 chars in microseconds = 3.5 * 10 * 1_000_000 / baud_rate
        let gap_us = (35_000_000 / baud_rate) as u64;
        // Add some margin for timing jitter
        let gap_us = gap_us + (gap_us / 4); // +25%
        Self::new(Duration::from_micros(gap_us))
    }

    /// Set maximum frame size (default: 256 for Modbus RTU).
    pub fn max_frame_size(mut self, size: usize) -> Self {
        self.max_frame_size = size;
        self
    }

    /// Mark the channel as idle (no more bytes expected for now).
    ///
    /// This forces any buffered data to become a complete frame.
    /// Call this after a read timeout or when you know the device
    /// has stopped sending.
    pub fn mark_idle(&mut self) {
        if !self.buffer.is_empty() {
            self.frame_ready = true;
        }
    }

    /// Feed bytes with explicit timestamp.
    ///
    /// Use this when you have precise timing information.
    pub fn feed_at(&mut self, bytes: &[u8], timestamp: Instant) {
        if bytes.is_empty() {
            return;
        }

        // Check if this is a new frame (gap exceeded)
        if let Some(last_time) = self.last_byte_time {
            if timestamp.duration_since(last_time) >= self.inter_frame_gap
                && !self.buffer.is_empty()
            {
                // Gap detected - current buffer is complete
                self.frame_ready = true;
            }
        }

        // If we have a complete frame pending, don't add to it
        if self.frame_ready {
            return;
        }

        // Add bytes to buffer (enforce max size)
        let take = std::cmp::min(bytes.len(), self.max_frame_size - self.buffer.len());
        self.buffer.extend_from_slice(&bytes[..take]);
        self.last_byte_time = Some(timestamp);
    }

    /// Check if the inter-frame gap has elapsed since last byte.
    ///
    /// Call this periodically to detect frame completion based on timing.
    pub fn check_timeout(&mut self) {
        if let Some(last_time) = self.last_byte_time {
            if last_time.elapsed() >= self.inter_frame_gap && !self.buffer.is_empty() {
                self.frame_ready = true;
            }
        }
    }

    /// Get the configured inter-frame gap.
    pub fn inter_frame_gap(&self) -> Duration {
        self.inter_frame_gap
    }
}

impl Deframer for TimingDeframer {
    fn feed(&mut self, bytes: &[u8]) {
        self.feed_at(bytes, Instant::now());
    }

    fn next_frame(&mut self) -> Option<Vec<u8>> {
        // First check if timeout has elapsed
        self.check_timeout();

        if self.frame_ready && !self.buffer.is_empty() {
            self.frame_ready = false;
            self.last_byte_time = None;
            Some(std::mem::take(&mut self.buffer))
        } else {
            None
        }
    }

    fn has_frame(&self) -> bool {
        self.frame_ready && !self.buffer.is_empty()
    }

    fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.last_byte_time = None;
        self.frame_ready = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_timing_deframer_basic() {
        // Very short gap for testing
        let mut deframer = TimingDeframer::new(Duration::from_millis(10));

        deframer.feed(b"\x01\x02\x03");
        assert!(!deframer.has_frame());

        // Wait for gap
        sleep(Duration::from_millis(15));
        deframer.check_timeout();

        assert!(deframer.has_frame());
        assert_eq!(deframer.next_frame(), Some(vec![0x01, 0x02, 0x03]));
    }

    #[test]
    fn test_timing_deframer_mark_idle() {
        let mut deframer = TimingDeframer::new(Duration::from_secs(10)); // Long gap

        deframer.feed(b"\x01\x02\x03");
        assert!(!deframer.has_frame());

        // Force frame completion
        deframer.mark_idle();
        assert!(deframer.has_frame());
        assert_eq!(deframer.next_frame(), Some(vec![0x01, 0x02, 0x03]));
    }

    #[test]
    fn test_modbus_rtu_gap_calculation() {
        // At 9600 baud, 3.5 char times = 3.646ms
        let deframer = TimingDeframer::modbus_rtu(9600);
        let gap = deframer.inter_frame_gap();
        // With 25% margin: ~4.55ms
        assert!(gap.as_micros() > 4000 && gap.as_micros() < 5000);

        // At 115200 baud, 3.5 char times = 0.304ms
        let deframer = TimingDeframer::modbus_rtu(115200);
        let gap = deframer.inter_frame_gap();
        // With 25% margin: ~0.38ms
        assert!(gap.as_micros() > 300 && gap.as_micros() < 500);
    }
}
