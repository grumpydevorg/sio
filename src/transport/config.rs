//! Strongly-typed configuration values.
//!
//! Instead of using raw primitives like `u32` for baud rate, we use
//! newtypes that enforce validity at construction time.

use std::time::Duration;

/// A validated baud rate.
///
/// Standard baud rates are: 300, 1200, 2400, 4800, 9600, 19200, 38400,
/// 57600, 115200, 230400, 460800, 921600.
///
/// # Example
///
/// ```
/// use sio::transport::config::BaudRate;
///
/// let baud = BaudRate::new(115_200).expect("valid baud rate");
/// assert_eq!(baud.as_u32(), 115_200);
///
/// // Standard rates have convenience constructors
/// let baud = BaudRate::B115200;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaudRate(u32);

impl BaudRate {
    // Standard baud rates as constants
    pub const B300: Self = Self(300);
    pub const B1200: Self = Self(1200);
    pub const B2400: Self = Self(2400);
    pub const B4800: Self = Self(4800);
    pub const B9600: Self = Self(9600);
    pub const B19200: Self = Self(19_200);
    pub const B38400: Self = Self(38_400);
    pub const B57600: Self = Self(57_600);
    pub const B115200: Self = Self(115_200);
    pub const B230400: Self = Self(230_400);
    pub const B460800: Self = Self(460_800);
    pub const B921600: Self = Self(921_600);

    /// Create a custom baud rate.
    ///
    /// Returns `None` if the baud rate is zero.
    #[must_use]
    pub const fn new(rate: u32) -> Option<Self> {
        if rate == 0 { None } else { Some(Self(rate)) }
    }

    /// Create a baud rate without validation.
    ///
    /// # Safety
    ///
    /// The rate must be non-zero.
    #[must_use]
    pub const unsafe fn new_unchecked(rate: u32) -> Self {
        Self(rate)
    }

    /// Get the raw baud rate value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Calculate the duration of one character at this baud rate.
    ///
    /// Assumes 10 bits per character (8N1: 1 start + 8 data + 1 stop).
    #[must_use]
    pub fn char_time(&self) -> Duration {
        // 10 bits per char, time = 10 / baud_rate seconds
        Duration::from_nanos(10_000_000_000 / u64::from(self.0))
    }

    /// Calculate the inter-frame gap for Modbus RTU (3.5 character times).
    #[must_use]
    pub fn modbus_rtu_gap(&self) -> Duration {
        // 3.5 chars = 35 bits / baud_rate seconds
        Duration::from_nanos(35_000_000_000 / u64::from(self.0))
    }
}

impl Default for BaudRate {
    fn default() -> Self {
        Self::B115200
    }
}

impl From<BaudRate> for u32 {
    fn from(rate: BaudRate) -> Self {
        rate.0
    }
}

/// Data bits configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataBits {
    Five = 5,
    Six = 6,
    Seven = 7,
    #[default]
    Eight = 8,
}

impl DataBits {
    /// Create from raw value.
    #[must_use]
    pub const fn from_u8(bits: u8) -> Option<Self> {
        match bits {
            5 => Some(Self::Five),
            6 => Some(Self::Six),
            7 => Some(Self::Seven),
            8 => Some(Self::Eight),
            _ => None,
        }
    }

    /// Get the raw value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl From<DataBits> for u8 {
    fn from(bits: DataBits) -> Self {
        bits as Self
    }
}

/// Stop bits configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopBits {
    #[default]
    One = 1,
    Two = 2,
}

impl StopBits {
    /// Create from raw value.
    #[must_use]
    pub const fn from_u8(bits: u8) -> Option<Self> {
        match bits {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            _ => None,
        }
    }

    /// Get the raw value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl From<StopBits> for u8 {
    fn from(bits: StopBits) -> Self {
        bits as Self
    }
}

/// A timeout duration that is guaranteed to be non-zero.
///
/// This prevents accidental zero timeouts which would cause immediate timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout(Duration);

impl Timeout {
    /// Create a timeout from milliseconds.
    ///
    /// Returns `None` if the timeout is zero.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Option<Self> {
        if ms == 0 {
            None
        } else {
            Some(Self(Duration::from_millis(ms)))
        }
    }

    /// Create a timeout from a Duration.
    ///
    /// Returns `None` if the timeout is zero.
    #[must_use]
    pub fn from_duration(d: Duration) -> Option<Self> {
        if d.is_zero() { None } else { Some(Self(d)) }
    }

    /// Get the timeout as a Duration.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// Get the timeout in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u128 {
        self.0.as_millis()
    }

    /// Common timeout values.
    pub const ONE_SECOND: Self = Self(Duration::from_secs(1));
    pub const FIVE_SECONDS: Self = Self(Duration::from_secs(5));
    pub const TEN_SECONDS: Self = Self(Duration::from_secs(10));
}

impl Default for Timeout {
    fn default() -> Self {
        Self::FIVE_SECONDS
    }
}

impl From<Timeout> for Duration {
    fn from(t: Timeout) -> Self {
        t.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_baud_rate_validation() {
        assert!(BaudRate::new(0).is_none());
        assert!(BaudRate::new(115_200).is_some());
    }

    #[test]
    fn test_baud_rate_char_time() {
        let baud = BaudRate::B9600;
        let char_time = baud.char_time();
        // At 9600 baud, one char (10 bits) takes ~1.04ms
        assert!(char_time.as_micros() > 1000 && char_time.as_micros() < 1100);
    }

    #[test]
    fn test_baud_rate_modbus_gap() {
        let baud = BaudRate::B9600;
        let gap = baud.modbus_rtu_gap();
        // At 9600 baud, 3.5 chars takes ~3.65ms
        assert!(gap.as_micros() > 3600 && gap.as_micros() < 3700);
    }

    #[test]
    fn test_timeout_validation() {
        assert!(Timeout::from_millis(0).is_none());
        assert!(Timeout::from_millis(1000).is_some());
        assert!(Timeout::from_duration(Duration::ZERO).is_none());
    }

    #[test]
    fn test_data_bits_roundtrip() {
        for bits in [5, 6, 7, 8] {
            let db = DataBits::from_u8(bits).unwrap();
            assert_eq!(db.as_u8(), bits);
        }
        assert!(DataBits::from_u8(4).is_none());
        assert!(DataBits::from_u8(9).is_none());
    }
}
