//! Property-based tests for transport invariants.
//!
//! These tests verify that the transport implementations maintain
//! their invariants regardless of the input.

#[cfg(test)]
mod tests {
    use crate::transport::{LoopbackTransport, PortConfig, SerialTransport};
    use proptest::prelude::*;
    use std::time::Duration;

    proptest! {
        /// Invariant: Write followed by drain returns exactly what was written.
        #[test]
        fn loopback_write_drain_identity(data in prop::collection::vec(any::<u8>(), 0..1024)) {
            let mut transport = LoopbackTransport::new();
            transport.open(&PortConfig::new("loop://", 115_200)).unwrap();

            if !data.is_empty() {
                let written = transport.write(&data).unwrap();
                prop_assert_eq!(written, data.len());
            }

            let drained = transport.drain().unwrap();
            prop_assert_eq!(drained, data);
        }

        /// Invariant: Multiple writes accumulate in buffer.
        #[test]
        fn loopback_writes_accumulate(
            data1 in prop::collection::vec(any::<u8>(), 0..512),
            data2 in prop::collection::vec(any::<u8>(), 0..512),
        ) {
            let mut transport = LoopbackTransport::new();
            transport.open(&PortConfig::new("loop://", 115_200)).unwrap();

            if !data1.is_empty() {
                transport.write(&data1).unwrap();
            }
            if !data2.is_empty() {
                transport.write(&data2).unwrap();
            }

            let drained = transport.drain().unwrap();

            let mut expected = data1;
            expected.extend(data2);
            prop_assert_eq!(drained, expected);
        }

        /// Invariant: Partial reads don't lose data.
        #[test]
        fn loopback_partial_read_preserves_data(
            data in prop::collection::vec(any::<u8>(), 1..1024),
            read_size in 1usize..512,
        ) {
            let mut transport = LoopbackTransport::new();
            transport.open(&PortConfig::new("loop://", 115_200)).unwrap();

            transport.write(&data).unwrap();

            let mut collected = Vec::new();
            loop {
                let chunk = transport.read(read_size, Duration::from_millis(10)).unwrap();
                if chunk.is_empty() {
                    break;
                }
                collected.extend(chunk);
            }

            prop_assert_eq!(collected, data);
        }

        /// Invariant: is_open reflects actual state.
        #[test]
        fn is_open_reflects_state(path in "[a-z]+://[a-z]+") {
            let mut transport = LoopbackTransport::new();

            // Initially closed
            prop_assert!(!transport.is_open());

            // After open
            transport.open(&PortConfig::new(&path, 115_200)).unwrap();
            prop_assert!(transport.is_open());
            prop_assert_eq!(transport.port_path(), Some(path.as_str()));

            // After close
            transport.close().unwrap();
            prop_assert!(!transport.is_open());
            prop_assert_eq!(transport.port_path(), None);
        }

        /// Invariant: Close on closed transport fails.
        #[test]
        fn close_on_closed_fails(_dummy in any::<u8>()) {
            let mut transport = LoopbackTransport::new();
            prop_assert!(transport.close().is_err());
        }

        /// Invariant: Open on open transport fails.
        #[test]
        fn open_on_open_fails(_dummy in any::<u8>()) {
            let mut transport = LoopbackTransport::new();
            transport.open(&PortConfig::new("loop://", 115_200)).unwrap();

            let result = transport.open(&PortConfig::new("loop://other", 9600));
            prop_assert!(result.is_err());
        }

        /// Invariant: Operations on closed transport fail.
        #[test]
        fn operations_on_closed_fail(data in prop::collection::vec(any::<u8>(), 1..100)) {
            let mut transport = LoopbackTransport::new();

            prop_assert!(transport.write(&data).is_err());
            prop_assert!(transport.read(10, Duration::from_millis(10)).is_err());
            prop_assert!(transport.drain().is_err());
            prop_assert!(transport.modem_control(None, None).is_err());
            prop_assert!(transport.send_break(Duration::from_millis(10)).is_err());
        }
    }

    // Non-proptest invariant tests

    #[test]
    fn drain_is_idempotent() {
        let mut transport = LoopbackTransport::new();
        transport
            .open(&PortConfig::new("loop://", 115_200))
            .unwrap();

        transport.write(b"data").unwrap();
        let first = transport.drain().unwrap();
        let second = transport.drain().unwrap();

        assert_eq!(first, b"data");
        assert!(second.is_empty(), "Second drain should be empty");
    }

    #[test]
    fn empty_write_succeeds() {
        let mut transport = LoopbackTransport::new();
        transport
            .open(&PortConfig::new("loop://", 115_200))
            .unwrap();

        let written = transport.write(&[]).unwrap();
        assert_eq!(written, 0);

        let drained = transport.drain().unwrap();
        assert!(drained.is_empty());
    }

    #[test]
    fn zero_byte_read_returns_empty() {
        let mut transport = LoopbackTransport::new();
        transport
            .open(&PortConfig::new("loop://", 115_200))
            .unwrap();

        transport.write(b"data").unwrap();
        let read = transport.read(0, Duration::from_millis(10)).unwrap();
        assert!(read.is_empty());

        // Data should still be there
        let drained = transport.drain().unwrap();
        assert_eq!(drained, b"data");
    }
}
