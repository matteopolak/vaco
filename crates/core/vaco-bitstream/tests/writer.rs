//! [`BitWriter`] and [`RbspWriter`] unit tests.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{BitReader, BitWriter, GolombRead, RbspWriter, annexb};
use vaco_limits::{Budget, Limits};

#[test]
fn put_is_msb_first() {
    let mut w = BitWriter::new();
    w.put(1, 1);
    w.put(3, 0b010);
    w.put(4, 0b1100);
    assert_eq!(w.bit_len(), 8);
    assert_eq!(w.finish(), vec![0b1010_1100]);
}

#[test]
fn put_masks_bits_above_n() {
    let mut w = BitWriter::new();
    w.put(4, 0xFFFF_FFFF);
    w.put(4, 0);
    assert_eq!(w.finish(), vec![0xF0]);
}

#[test]
fn put_long_spans_the_cache() {
    let mut w = BitWriter::new();
    w.put_long(64, 0x0123_4567_89AB_CDEF);
    assert_eq!(
        w.finish(),
        vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
    );

    let mut w = BitWriter::new();
    w.put_long(33, 0x1_0000_0001);
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.get_long(33), 0x1_0000_0001);
}

#[test]
fn alignment_helpers_pad_correctly() {
    let mut w = BitWriter::new();
    w.put(3, 0b101);
    w.align_zero();
    assert_eq!(w.finish(), vec![0b1010_0000]);

    let mut w = BitWriter::new();
    w.put(3, 0b101);
    w.align_one();
    assert_eq!(w.finish(), vec![0b1011_1111]);

    // Already aligned: a no-op, not an extra byte.
    let mut w = BitWriter::new();
    w.put(8, 0xAA);
    w.align_zero();
    w.align_one();
    assert_eq!(w.finish(), vec![0xAA]);
}

#[test]
fn rbsp_trailing_writes_a_stop_bit() {
    let mut w = BitWriter::new();
    w.put(3, 0b101);
    w.rbsp_trailing();
    assert_eq!(w.finish(), vec![0b1011_0000]);
}

#[test]
fn clear_keeps_the_allocation_and_reset_takes_it() {
    let mut w = BitWriter::new();
    w.put(8, 0xAA);
    let first = w.reset();
    assert_eq!(first, vec![0xAA]);
    assert_eq!(w.bit_len(), 0);

    w.put(8, 0xBB);
    assert_eq!(w.bytes(), &[0xBB]);
    w.clear();
    assert_eq!(w.bytes(), &[] as &[u8]);
    assert_eq!(w.bit_len(), 0);
}

#[test]
fn with_capacity_charges_the_budget() {
    let mut budget = Budget::new(Limits::strict());
    let w = BitWriter::with_capacity(&mut budget, 4096).unwrap();
    assert_eq!(w.bit_len(), 0);
    assert!(budget.committed() >= 4096);

    // Over budget is an error, not an abort.
    let mut tiny = Budget::new(Limits::tiny());
    assert!(BitWriter::with_capacity(&mut tiny, 1 << 30).is_err());
}

#[test]
fn golomb_writes_are_read_back() {
    let values = [0u32, 1, 2, 3, 100, 65_535, 1 << 20, u32::MAX - 1];
    let mut w = BitWriter::new();
    for &v in &values {
        w.ue(v);
    }
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    for &v in &values {
        assert_eq!(r.ue(), v, "value {v}");
    }
    assert!(!r.overrun());

    let signed = [0i32, 1, -1, 2, -2, 1000, -1000, i32::MAX, -i32::MAX];
    let mut w = BitWriter::new();
    for &v in &signed {
        w.se(v);
    }
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    for &v in &signed {
        assert_eq!(r.se(), v, "value {v}");
    }
    assert!(!r.overrun());
}

#[test]
fn put_zeros_does_not_loop_per_bit() {
    let mut w = BitWriter::new();
    w.put_zeros(1000);
    assert_eq!(w.bit_len(), 1000);
    assert!(w.finish().iter().all(|&b| b == 0));
}

#[test]
fn rbsp_writer_escapes_its_output() {
    let mut w = RbspWriter::new();
    // Three zero bytes in the payload must come out escaped.
    w.bits().put(8, 0);
    w.bits().put(8, 0);
    w.bits().put(8, 0);
    let ebsp = w.finish();
    assert!(!annexb::violates_ebsp_constraint(&ebsp));

    let mut scratch = Vec::new();
    let rbsp = annexb::to_rbsp(&ebsp, &mut scratch);
    // The payload plus rbsp_trailing_bits (0x80).
    assert_eq!(rbsp, &[0, 0, 0, 0x80]);
}

#[test]
fn rbsp_writer_produces_a_parseable_annexb_unit() {
    let mut w = RbspWriter::new();
    w.bits().put(8, 0x67);
    w.bits().ue(1234);
    let unit = w.finish_annexb();
    assert_eq!(&unit[..4], &[0, 0, 0, 1]);

    let units: Vec<&[u8]> = annexb::nal_units(&unit).collect();
    assert_eq!(units.len(), 1);
    let mut scratch = Vec::new();
    let rbsp = annexb::to_rbsp(units[0], &mut scratch);
    let mut r = BitReader::new(rbsp);
    assert_eq!(r.get(8), 0x67);
    assert_eq!(r.ue(), 1234);
    assert!(!r.overrun());
}
