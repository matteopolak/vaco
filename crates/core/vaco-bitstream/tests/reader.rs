//! Unit tests for [`BitReader`].
//!
//! The workspace denies indexing and `unwrap` because a panic reachable from
//! untrusted input is a decoder vulnerability. In a test a panic *is* the
//! failure report, so both are allowed here.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_bitstream::{BitReader, Padded};

/// The definition of an MSB-first bit read, written the slow obvious way.
///
/// Zero past the logical end, exactly as the reader promises. Every fast-path
/// assertion below is against this.
fn reference(data: &[u8], bit_pos: u64, n: u32) -> u64 {
    let mut v = 0u64;
    for i in 0..u64::from(n) {
        let p = bit_pos + i;
        let byte = data.get((p >> 3) as usize).copied().unwrap_or(0);
        let bit = (byte >> (7 - (p & 7))) & 1;
        v = (v << 1) | u64::from(bit);
    }
    v
}

fn sample() -> Vec<u8> {
    // Deterministic pseudo-random bytes: a fixed LCG, so the corpus is the same
    // on every machine and a failure is reproducible from the seed alone.
    let mut s = 0x1234_5678_9ABC_DEF0u64;
    (0..64)
        .map(|_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (s >> 33) as u8
        })
        .collect()
}

#[test]
fn get_matches_the_definition_at_every_width_and_offset() {
    let data = sample();
    for offset in 0..8u32 {
        for n in 0..=32u32 {
            let mut r = BitReader::new(&data);
            r.skip(offset);
            let got = r.get(n);
            let want = reference(&data, u64::from(offset), n) as u32;
            assert_eq!(got, want, "offset {offset}, n {n}");
            assert_eq!(r.bit_pos(), u64::from(offset + n));
        }
    }
}

#[test]
fn get_long_matches_the_definition_at_every_width_and_offset() {
    let data = sample();
    for offset in 0..8u32 {
        for n in 0..=64u32 {
            let mut r = BitReader::new(&data);
            r.skip(offset);
            let got = r.get_long(n);
            let want = reference(&data, u64::from(offset), n);
            assert_eq!(got, want, "offset {offset}, n {n}");
            assert_eq!(r.bit_pos(), u64::from(offset + n));
        }
    }
}

#[test]
fn peek_does_not_consume_and_agrees_with_get() {
    let data = sample();
    for offset in 0..8u32 {
        for n in 1..=32u32 {
            let mut r = BitReader::new(&data);
            r.skip(offset);
            let peeked = r.peek(n);
            assert_eq!(r.bit_pos(), u64::from(offset));
            assert_eq!(peeked, r.get(n), "offset {offset}, n {n}");
        }
    }
}

#[test]
fn get_signed_sign_extends() {
    // 1111 = -1 in 4 bits, 0111 = 7 in 4 bits.
    let mut r = BitReader::new(&[0b1111_0111]);
    assert_eq!(r.get_signed(4), -1);
    assert_eq!(r.get_signed(4), 7);

    let mut r = BitReader::new(&[0x80, 0x00, 0x00, 0x00]);
    assert_eq!(r.get_signed(32), i32::MIN);

    // n == 0 is a no-op that reads nothing.
    let mut r = BitReader::new(&[0xFF]);
    assert_eq!(r.get_signed(0), 0);
    assert_eq!(r.bit_pos(), 0);
}

#[test]
fn shift_edges_do_not_misbehave() {
    let data = [0xFFu8; 16];
    // n == 0 must not shift by 64.
    let mut r = BitReader::new(&data);
    assert_eq!(r.get(0), 0);
    assert_eq!(r.peek(0), 0);
    r.skip(0);
    assert_eq!(r.bit_pos(), 0);

    // n == 64 must not shift by 64 either.
    let mut r = BitReader::new(&data);
    assert_eq!(r.get_long(64), u64::MAX);
    assert_eq!(r.bit_pos(), 64);

    // n == 32 twice, straddling the cache.
    let mut r = BitReader::new(&data);
    assert_eq!(r.get(32), u32::MAX);
    assert_eq!(r.get(32), u32::MAX);

    // A full cache consumed exactly.
    let mut r = BitReader::new(&data);
    r.get_long(64);
    assert!(!r.overrun());
    assert_eq!(r.bits_left(), 64);
}

#[test]
fn refill_boundaries_are_crossed_correctly() {
    let data = sample();
    // Read 1 bit at a time across several refills and compare with the
    // definition; this is the test that catches a broken idempotent OR.
    let mut r = BitReader::new(&data);
    for i in 0..(data.len() as u64 * 8) {
        assert_eq!(u64::from(r.get_bit()), reference(&data, i, 1), "bit {i}");
    }
    assert!(!r.overrun());

    // Widths that are coprime with 8 and with 64, so every alignment occurs.
    for width in [3u32, 5, 7, 11, 13, 17, 23, 29, 31] {
        let mut r = BitReader::new(&data);
        let mut pos = 0u64;
        while pos + u64::from(width) <= data.len() as u64 * 8 {
            assert_eq!(
                u64::from(r.get(width)),
                reference(&data, pos, width),
                "width {width}, pos {pos}"
            );
            pos += u64::from(width);
        }
    }
}

#[test]
fn skip_is_constant_time_and_exact() {
    let data = sample();
    for skip in [0u32, 1, 7, 8, 63, 64, 65, 127, 200, 511] {
        let mut a = BitReader::new(&data);
        a.skip(skip);
        let mut b = BitReader::new(&data);
        for _ in 0..skip {
            b.get_bit();
        }
        assert_eq!(a.bit_pos(), b.bit_pos(), "skip {skip}");
        assert_eq!(a.get(8), b.get(8), "skip {skip}");
    }

    // A skip far past the end must not loop and must flag.
    let mut r = BitReader::new(&data);
    r.skip(u32::MAX);
    assert!(r.overrun());
    assert_eq!(r.bits_left(), 0);
    assert_eq!(r.get(8), 0);

    let mut r = BitReader::new(&data);
    r.skip_long(u64::MAX);
    assert!(r.overrun());
}

#[test]
fn align_reaches_the_next_byte_boundary() {
    let data = sample();
    for offset in 0..24u32 {
        let mut r = BitReader::new(&data);
        r.skip(offset);
        r.align();
        assert_eq!(r.bit_pos() % 8, 0, "offset {offset}");
        assert!(r.is_aligned());
        assert!(r.bit_pos() >= u64::from(offset));
        assert!(r.bit_pos() - u64::from(offset) < 8);
    }
}

#[test]
fn mark_and_restore_are_exact() {
    let data = sample();
    let mut r = BitReader::new(&data);
    r.skip(13);
    let m = r.mark();
    let expect = r.get(19);
    r.get(31);
    r.restore(m);
    assert_eq!(r.get(19), expect);
}

#[test]
fn overrun_is_sticky_and_reads_zero_past_the_end() {
    let mut r = BitReader::new(&[0xAB, 0xCD]);
    assert_eq!(r.get(16), 0xABCD);
    assert!(!r.overrun());
    r.check().unwrap();

    assert_eq!(r.get(8), 0);
    assert!(r.overrun());
    assert!(r.check().is_err());
    // Still zero, still flagged, still no panic.
    assert_eq!(r.get(32), 0);
    assert_eq!(r.get_long(64), 0);
    assert!(r.finish().is_err());

    // An empty buffer overruns on the first bit and never panics.
    let mut r = BitReader::new(&[]);
    assert_eq!(r.get_bit(), 0);
    assert!(r.overrun());
}

#[test]
fn try_get_refuses_rather_than_zero_filling() {
    let mut r = BitReader::new(&[0xFF]);
    assert_eq!(r.try_get(8).unwrap(), 0xFF);
    assert!(r.try_get(1).is_err());
    // Nothing was consumed by the failed read.
    assert_eq!(r.bit_pos(), 8);
    assert!(!r.overrun());
}

#[test]
fn with_logical_len_hides_the_bytes_after_the_window() {
    let data = [0xAAu8, 0xBB, 0xCC, 0xDD];
    let mut r = BitReader::with_logical_len(&data, 2);
    assert_eq!(r.get(16), 0xAABB);
    assert!(!r.overrun());
    // The bytes are there but are not ours to read.
    assert_eq!(r.get(8), 0xCC);
    assert!(r.overrun());
    assert_eq!(r.logical_bits(), 16);
}

#[test]
fn padded_construction_verifies_the_padding() {
    let mut scratch = Vec::new();
    let p = Padded::from_slice_copying(&[1, 2, 3], &mut scratch);
    assert_eq!(p.logical_len(), 3);
    assert_eq!(p.as_bytes().len(), 3 + Padded::PAD);
    assert_eq!(p.logical_bytes(), &[1, 2, 3]);

    let mut buf = vec![0u8; 3 + Padded::PAD];
    buf[0] = 9;
    assert!(Padded::new(&buf, 3).is_some());
    // Too short.
    assert!(Padded::new(&buf, 4).is_none());
    // Non-zero padding.
    buf[3] = 1;
    assert!(Padded::new(&buf, 3).is_none());
}

#[test]
fn padded_and_unpadded_readers_agree() {
    let data = sample();
    let mut scratch = Vec::new();
    for len in 0..data.len() {
        let src = &data[..len];
        let padded = Padded::from_slice_copying(src, &mut scratch);
        let mut a = BitReader::new(src);
        let mut b = BitReader::new_padded(padded);
        // Read well past the end so the tail path and the padding both engage.
        for i in 0..(len * 8 + 200) {
            let width = (i % 33) as u32;
            assert_eq!(a.get(width), b.get(width), "len {len}, step {i}");
            assert_eq!(a.overrun(), b.overrun(), "len {len}, step {i}");
            assert_eq!(a.bit_pos(), b.bit_pos(), "len {len}, step {i}");
        }
    }
}

#[test]
fn remaining_bytes_starts_on_a_boundary() {
    let data = [1u8, 2, 3, 4];
    let mut r = BitReader::new(&data);
    assert_eq!(r.remaining_bytes(), &data[..]);
    r.get(8);
    assert_eq!(r.remaining_bytes(), &data[1..]);
    r.get(4);
    // Unaligned: the partial byte is skipped.
    assert_eq!(r.remaining_bytes(), &data[2..]);
    r.skip(100);
    assert_eq!(r.remaining_bytes(), &[] as &[u8]);
}
