//! Property tests.
//!
//! Bit I/O is prime property-testing territory: the invariants are exact
//! (write-then-read is the identity), the input space is huge, and hand-written
//! cases only ever cover the boundaries someone already thought of.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_bitstream::{BitReader, BitWriter, GolombRead, Padded, annexb, avcc};

/// A sequence of `(width, value)` writes, widths biased towards the boundaries
/// where shift bugs live: 0, 1, 7, 8, 31, 32.
fn write_script() -> impl Strategy<Value = Vec<(u32, u32)>> {
    proptest::collection::vec(
        (
            prop_oneof![
                5 => Just(0u32),
                5 => Just(1u32),
                5 => Just(7u32),
                5 => Just(8u32),
                5 => Just(31u32),
                5 => Just(32u32),
                20 => 0u32..=32,
            ],
            any::<u32>(),
        ),
        0..64,
    )
}

fn read_widths() -> impl Strategy<Value = Vec<u32>> {
    proptest::collection::vec(
        prop_oneof![
            1 => Just(0u32),
            1 => Just(1u32),
            1 => Just(32u32),
            6 => 0u32..=32,
        ],
        0..96,
    )
}

proptest! {
    /// The single most valuable property in the crate.
    #[test]
    fn write_then_read_is_the_identity(script in write_script()) {
        let mut w = BitWriter::new();
        for &(n, v) in &script {
            w.put(n, v);
        }
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        for &(n, v) in &script {
            let expect = if n == 0 { 0 } else if n == 32 { v } else { v & ((1u32 << n) - 1) };
            prop_assert_eq!(r.get(n), expect, "width {}", n);
        }
        prop_assert!(!r.overrun());
    }

    #[test]
    fn write_then_read_is_the_identity_for_wide_fields(
        script in proptest::collection::vec((0u32..=64, any::<u64>()), 0..32)
    ) {
        let mut w = BitWriter::new();
        for &(n, v) in &script {
            w.put_long(n, v);
        }
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        for &(n, v) in &script {
            let expect = if n == 0 { 0 } else if n == 64 { v } else { v & ((1u64 << n) - 1) };
            prop_assert_eq!(r.get_long(n), expect, "width {}", n);
        }
        prop_assert!(!r.overrun());
    }

    #[test]
    fn signed_fields_round_trip(
        script in proptest::collection::vec((1u32..=32, any::<i32>()), 0..32)
    ) {
        let mut w = BitWriter::new();
        for &(n, v) in &script {
            w.put_signed(n, v);
        }
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        for &(n, v) in &script {
            // Sign-extend the low n bits of v, which is what an n-bit field holds.
            let shift = 32 - n;
            let expect = (v << shift) >> shift;
            prop_assert_eq!(r.get_signed(n), expect, "width {}", n);
        }
    }

    /// Exp-Golomb over the whole representable range.
    #[test]
    fn ue_round_trips(values in proptest::collection::vec(0u32..u32::MAX, 0..64)) {
        let mut w = BitWriter::new();
        for &v in &values {
            w.ue(v);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &v in &values {
            prop_assert_eq!(r.ue(), v);
        }
        prop_assert!(!r.overrun());
    }

    #[test]
    fn se_round_trips(values in proptest::collection::vec(-i32::MAX..=i32::MAX, 0..64)) {
        let mut w = BitWriter::new();
        for &v in &values {
            w.se(v);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &v in &values {
            prop_assert_eq!(r.se(), v);
        }
        prop_assert!(!r.overrun());
    }

    #[test]
    fn ue_long_round_trips_via_ue(values in proptest::collection::vec(0u32..u32::MAX, 0..32)) {
        let mut w = BitWriter::new();
        for &v in &values {
            w.ue(v);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for &v in &values {
            prop_assert_eq!(r.ue_long(), u64::from(v));
        }
        prop_assert!(!r.overrun());
    }

    /// F9's fast path must be indistinguishable from the slow one — values *and*
    /// overrun *and* position, well past the end.
    #[test]
    fn padded_and_unpadded_readers_agree(
        data in proptest::collection::vec(any::<u8>(), 0..128),
        widths in read_widths(),
    ) {
        let mut scratch = Vec::new();
        let padded = Padded::from_slice_copying(&data, &mut scratch);
        let mut a = BitReader::new(&data);
        let mut b = BitReader::new_padded(padded);
        for &n in &widths {
            prop_assert_eq!(a.get(n), b.get(n));
            prop_assert_eq!(a.overrun(), b.overrun());
            prop_assert_eq!(a.bit_pos(), b.bit_pos());
        }
        prop_assert_eq!(a.check().is_ok(), b.check().is_ok());
    }

    /// Truncating the buffer changes nothing before the truncation point.
    #[test]
    fn truncation_is_monotone(
        data in proptest::collection::vec(any::<u8>(), 1..64),
        cut in 0usize..64,
        widths in read_widths(),
    ) {
        let cut = cut.min(data.len());
        let mut full = BitReader::new(&data);
        let mut short = BitReader::new(&data[..cut]);
        let cut_bits = (cut as u64) * 8;
        for &n in &widths {
            let before = full.bit_pos();
            let a = full.get(n);
            let b = short.get(n);
            if before + u64::from(n) <= cut_bits {
                prop_assert_eq!(a, b, "at bit {}", before);
                prop_assert!(!short.overrun());
            } else {
                // Past the cut the truncated reader yields zeros and says so.
                prop_assert!(short.overrun());
            }
        }
    }

    /// `mark`/`restore` must be exactly a position save.
    #[test]
    fn mark_and_restore_replay(
        data in proptest::collection::vec(any::<u8>(), 0..64),
        widths in read_widths(),
        skip in 0u32..64,
    ) {
        let mut r = BitReader::new(&data);
        r.skip(skip);
        let m = r.mark();
        let first: Vec<u32> = widths.iter().map(|&n| r.get(n)).collect();
        let end = r.mark();
        r.restore(m);
        let second: Vec<u32> = widths.iter().map(|&n| r.get(n)).collect();
        prop_assert_eq!(first, second);
        prop_assert_eq!(r.mark(), end);
    }

    /// Escaping is invertible and establishes the EBSP constraint.
    #[test]
    fn rbsp_escaping_round_trips(rbsp in proptest::collection::vec(0u8..=4, 0..256)) {
        let mut ebsp = Vec::new();
        annexb::to_ebsp(&rbsp, &mut ebsp);
        prop_assert!(!annexb::violates_ebsp_constraint(&ebsp));
        let mut scratch = Vec::new();
        prop_assert_eq!(annexb::to_rbsp(&ebsp, &mut scratch), &rbsp[..]);
    }

    /// A NAL iterator over arbitrary bytes must terminate, and every unit must
    /// be a contiguous, non-empty, start-code-free piece of the input.
    #[test]
    fn nal_iteration_is_well_behaved(data in proptest::collection::vec(0u8..=3, 0..512)) {
        let mut last_end = 0usize;
        let mut count = 0usize;
        for unit in annexb::nal_units(&data) {
            count += 1;
            prop_assert!(count <= data.len() + 1, "iterator did not terminate");
            prop_assert!(!unit.is_empty());
            // Units appear in order and never overlap.
            let offset = unit.as_ptr() as usize - data.as_ptr() as usize;
            prop_assert!(offset >= last_end);
            last_end = offset + unit.len();
            prop_assert!(last_end <= data.len());
            // A unit never contains a start code.
            prop_assert_eq!(annexb::find_start_code(unit, 0), None);
        }
    }

    /// Length-prefixed iteration must terminate and stay inside the sample.
    #[test]
    fn length_prefixed_iteration_is_well_behaved(
        data in proptest::collection::vec(any::<u8>(), 0..256),
        size in prop_oneof![Just(1u8), Just(2), Just(4), any::<u8>()],
    ) {
        let mut total = 0usize;
        let mut count = 0usize;
        for unit in avcc::nal_units(&data, size) {
            count += 1;
            prop_assert!(count <= data.len() + 1, "iterator did not terminate");
            total += unit.len();
            prop_assert!(total <= data.len());
        }
    }

    /// Arbitrary bytes, arbitrary read script: no panic, and overrun is set
    /// exactly when the position has passed the end.
    #[test]
    fn reads_never_panic_and_overrun_is_exact(
        data in proptest::collection::vec(any::<u8>(), 0..64),
        widths in read_widths(),
    ) {
        let mut r = BitReader::new(&data);
        let logical = (data.len() as u64) * 8;
        for &n in &widths {
            r.get(n);
            prop_assert_eq!(r.overrun(), r.bit_pos() > logical);
        }
        prop_assert_eq!(r.bits_left(), logical.saturating_sub(r.bit_pos()));
    }
}
