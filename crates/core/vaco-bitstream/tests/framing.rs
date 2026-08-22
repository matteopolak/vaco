//! Byte reader, Annex-B framing, RBSP escaping and length-prefixed units.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{ByteReader, annexb, avcc};

#[test]
fn byte_reader_is_endian_explicit() {
    let data = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let mut r = ByteReader::new(&data);
    assert_eq!(r.be16(), 0x0102);
    r.seek(0);
    assert_eq!(r.le16(), 0x0201);
    r.seek(0);
    assert_eq!(r.be24(), 0x0001_0203);
    r.seek(0);
    assert_eq!(r.le24(), 0x0003_0201);
    r.seek(0);
    assert_eq!(r.be32(), 0x0102_0304);
    r.seek(0);
    assert_eq!(r.le32(), 0x0403_0201);
    r.seek(0);
    assert_eq!(r.be64(), 0x0102_0304_0506_0708);
    r.seek(0);
    assert_eq!(r.le64(), 0x0807_0605_0403_0201);
    r.check().unwrap();
}

#[test]
fn byte_reader_floats_round_trip() {
    let v = std::f64::consts::PI;
    let bytes = v.to_be_bytes();
    let mut r = ByteReader::new(&bytes);
    assert!((r.f64_be() - v).abs() < f64::EPSILON);

    let bytes = 1.5f32.to_le_bytes();
    let mut r = ByteReader::new(&bytes);
    assert!((r.f32_le() - 1.5).abs() < f32::EPSILON);
}

#[test]
fn byte_reader_truncation_is_sticky_and_short() {
    let data = [1u8, 2, 3];
    let mut r = ByteReader::new(&data);
    assert_eq!(r.be32(), 0);
    assert!(r.overrun());
    assert_eq!(r.remaining(), 0);
    assert!(r.check().is_err());

    // A short `bytes` returns what was there rather than nothing.
    let mut r = ByteReader::new(&data);
    assert_eq!(r.bytes(10), &[1, 2, 3]);
    assert!(r.overrun());

    // Skip and seek past the end flag rather than panic.
    let mut r = ByteReader::new(&data);
    r.skip(usize::MAX);
    assert!(r.overrun());
    let mut r = ByteReader::new(&data);
    r.seek(99);
    assert!(r.overrun());
}

#[test]
fn byte_reader_sub_windows_cannot_see_past_themselves() {
    let data = [1u8, 2, 3, 4, 5, 6];
    let mut r = ByteReader::new(&data);
    let mut sub = r.sub(3);
    assert_eq!(sub.bytes(3), &[1, 2, 3]);
    assert_eq!(sub.u8(), 0);
    assert!(sub.overrun());
    // The parent is unaffected and advanced past the window.
    assert!(!r.overrun());
    assert_eq!(r.bytes(3), &[4, 5, 6]);
}

#[test]
fn start_code_scanning_finds_three_and_four_byte_codes() {
    assert_eq!(annexb::find_start_code(&[0, 0, 1], 0), Some(0));
    assert_eq!(annexb::find_start_code(&[0, 0, 0, 1], 0), Some(1));
    assert_eq!(annexb::find_start_code(&[9, 9, 0, 0, 1], 0), Some(2));
    assert_eq!(annexb::find_start_code(&[0, 0, 2], 0), None);
    assert_eq!(annexb::find_start_code(&[], 0), None);
    assert_eq!(annexb::find_start_code(&[0, 0], 0), None);

    // The word-skip fast path must agree with a naive scan on every offset.
    let mut data = vec![0xABu8; 200];
    for at in [0usize, 1, 7, 8, 9, 63, 64, 100, 196] {
        data.fill(0xAB);
        data[at] = 0;
        data[at + 1] = 0;
        data[at + 2] = 1;
        assert_eq!(annexb::find_start_code(&data, 0), Some(at), "at {at}");
    }

    // Zeros everywhere but never a start code.
    let data = [0u8; 128];
    assert_eq!(annexb::find_start_code(&data, 0), None);
}

#[test]
fn nal_iteration_partitions_the_stream() {
    let stream = [
        0, 0, 0, 1, 0x67, 0xAA, 0xBB, // four-byte start code
        0, 0, 1, 0x68, 0xCC, // three-byte start code
        0, 0, 0, 1, 0x65, // trailing unit
    ];
    let units: Vec<&[u8]> = annexb::nal_units(&stream).collect();
    assert_eq!(units.len(), 3);
    assert_eq!(units[0], &[0x67, 0xAA, 0xBB]);
    assert_eq!(units[1], &[0x68, 0xCC]);
    assert_eq!(units[2], &[0x65]);
}

#[test]
fn nal_iteration_terminates_on_pathological_input() {
    // Nothing but start codes: every unit is empty, and the iterator must end.
    let stream = [0u8, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1];
    assert_eq!(annexb::nal_units(&stream).count(), 0);

    // No start code at all.
    assert_eq!(annexb::nal_units(&[1u8, 2, 3, 4]).count(), 0);
    assert_eq!(annexb::nal_units(&[]).count(), 0);

    // Long runs of zeros.
    let stream = [0u8; 1024];
    assert_eq!(annexb::nal_units(&stream).count(), 0);

    // Trailing zeros are trimmed from a unit.
    let stream = [0, 0, 1, 0x67, 0x00, 0x00, 0x00, 0x00, 1, 0x68];
    let units: Vec<&[u8]> = annexb::nal_units(&stream).collect();
    assert_eq!(units, vec![&[0x67u8][..], &[0x68][..]]);
}

#[test]
fn rbsp_escaping_round_trips() {
    let cases: &[&[u8]] = &[
        &[],
        &[0, 0, 0],
        &[0, 0, 1],
        &[0, 0, 2],
        &[0, 0, 3],
        &[0, 0, 4],
        &[0, 0, 0, 0, 0, 0],
        &[1, 0, 0, 1, 0, 0, 2, 3],
        &[0; 32],
    ];
    let mut ebsp = Vec::new();
    let mut scratch = Vec::new();
    for &rbsp in cases {
        ebsp.clear();
        annexb::to_ebsp(rbsp, &mut ebsp);
        assert!(
            !annexb::violates_ebsp_constraint(&ebsp),
            "escaped output still violates the EBSP constraint: {rbsp:?} -> {ebsp:?}"
        );
        assert_eq!(annexb::to_rbsp(&ebsp, &mut scratch), rbsp, "case {rbsp:?}");
    }
}

#[test]
fn to_rbsp_drops_the_escape_byte() {
    let mut scratch = Vec::new();
    assert_eq!(annexb::to_rbsp(&[0, 0, 3, 0], &mut scratch), &[0, 0, 0]);
    assert_eq!(annexb::to_rbsp(&[0, 0, 3, 1], &mut scratch), &[0, 0, 1]);
    assert_eq!(annexb::to_rbsp(&[0, 0, 3, 3], &mut scratch), &[0, 0, 3]);
    // A 03 that is not preceded by two zeros is data.
    assert_eq!(annexb::to_rbsp(&[0, 3, 0], &mut scratch), &[0, 3, 0]);
    // Scratch is reused, not grown without bound.
    assert_eq!(annexb::to_rbsp(&[], &mut scratch), &[] as &[u8]);
}

#[test]
fn length_prefixed_units_stop_at_a_bad_length() {
    let sample = [0, 2, 0x67, 0xAA, 0, 1, 0x68];
    let units: Vec<&[u8]> = avcc::nal_units(&sample, 2).collect();
    assert_eq!(units, vec![&[0x67u8, 0xAA][..], &[0x68][..]]);

    // A length that overruns the sample yields nothing further.
    let sample = [0, 0, 0, 99, 0x67];
    assert_eq!(avcc::nal_units(&sample, 4).count(), 0);

    // A zero length terminates rather than spinning.
    let sample = [0u8, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(avcc::nal_units(&sample, 4).count(), 0);

    // An unsupported length size yields nothing.
    assert_eq!(avcc::nal_units(&[1, 0x67], 3).count(), 0);

    // One-byte prefixes.
    let sample = [1u8, 0x67, 2, 0x68, 0x69];
    let units: Vec<&[u8]> = avcc::nal_units(&sample, 1).collect();
    assert_eq!(units, vec![&[0x67u8][..], &[0x68, 0x69][..]]);
}
