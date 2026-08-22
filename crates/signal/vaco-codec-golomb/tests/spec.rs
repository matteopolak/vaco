//! Checked against ITU-T H.264 clause 9.1 directly, not only against our own
//! writer.
//!
//! A round-trip test proves the reader and the writer agree with each other,
//! which they would also do if both were wrong the same way. These tests use
//! the bit patterns and the `(codeNum, value)` pairs the standard tabulates, so
//! a shared misreading of the specification fails here rather than shipping.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]

use vaco_bitstream::{BitReader, BitWriter, GolombRead};
use vaco_codec_golomb::{
    ChromaArrayType, GolombDecode, GolombEncode, MbPartPredMode, cbp_from_code_num, map,
};

/// Turn a string of `'0'`/`'1'` into bytes, MSB first, zero-padded.
fn bits(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cur = 0u8;
    let mut n = 0u32;
    for c in s.chars().filter(|c| *c == '0' || *c == '1') {
        cur = (cur << 1) | u8::from(c == '1');
        n += 1;
        if n.is_multiple_of(8) {
            out.push(cur);
            cur = 0;
        }
    }
    if !n.is_multiple_of(8) {
        out.push(cur << (8 - (n % 8)));
    }
    // Room for the reader to peek past the end without leaving the slice.
    out.resize(out.len() + 8, 0);
    out
}

// --------------------------------------------------- clause 9.1, the codewords

/// H.264 clause 9.1 gives the bit-string-to-`codeNum` correspondence explicitly:
/// `1` is 0, `010` is 1, `011` is 2, `00100` is 3, and so on.
#[test]
fn clause_9_1_codeword_table() {
    let cases: &[(&str, u32)] = &[
        ("1", 0),
        ("010", 1),
        ("011", 2),
        ("00100", 3),
        ("00101", 4),
        ("00110", 5),
        ("00111", 6),
        ("0001000", 7),
        ("0001001", 8),
        ("0001010", 9),
        ("0001011", 10),
        ("0001100", 11),
        ("0001101", 12),
        ("0001110", 13),
        ("0001111", 14),
        ("000010000", 15),
    ];
    for &(pattern, want) in cases {
        let data = bits(pattern);
        let mut r = BitReader::with_logical_len(&data, data.len() - 8);
        assert_eq!(r.ue_v(), want, "decoding {pattern}");
        assert!(!r.overrun(), "{pattern} should not overrun");

        let mut w = BitWriter::new();
        w.put_ue_v(want);
        let encoded = w.finish();
        let expect = &bits(pattern)[..pattern.len().div_ceil(8)];
        assert_eq!(&encoded[..], expect, "encoding {want}");
    }
}

/// Clause 9.1.1, Table 9-3: the `codeNum` to `se(v)` mapping.
#[test]
fn table_9_3_signed_mapping() {
    let table: &[(u32, i32)] = &[
        (0, 0),
        (1, 1),
        (2, -1),
        (3, 2),
        (4, -2),
        (5, 3),
        (6, -3),
        (7, 4),
        (8, -4),
        (9, 5),
    ];
    for &(code_num, value) in table {
        assert_eq!(map::se_value(code_num), value, "se_value({code_num})");
        assert_eq!(map::se_code_num(value), code_num, "se_code_num({value})");
    }
}

/// Reading `se(v)` straight out of the codewords clause 9.1 lists, so the
/// mapping and the bit reader are checked together rather than separately.
#[test]
fn se_v_from_codewords() {
    let cases: &[(&str, i32)] = &[
        ("1", 0),
        ("010", 1),
        ("011", -1),
        ("00100", 2),
        ("00101", -2),
        ("00110", 3),
        ("00111", -3),
    ];
    for &(pattern, want) in cases {
        let data = bits(pattern);
        let mut r = BitReader::new(&data);
        assert_eq!(r.se_v(), want, "decoding {pattern}");
    }
}

/// Clause 9.1.1: with `cMax == 1`, `te(v)` reads one bit and the value is its
/// **inverse**. Getting this backwards is the classic `te(v)` bug.
#[test]
fn te_v_single_bit_is_inverted() {
    let data = bits("10");
    let mut r = BitReader::new(&data);
    assert_eq!(r.te_v(1), 0, "bit 1 means value 0");
    assert_eq!(r.te_v(1), 1, "bit 0 means value 1");

    // cMax > 1 is plain ue(v).
    let data = bits("00100");
    let mut r = BitReader::new(&data);
    assert_eq!(r.te_v(7), 3);
}

#[test]
fn te_v_round_trips_at_both_ceilings() {
    for c_max in [1u32, 2, 7, 255] {
        for v in 0..=c_max.min(16) {
            let mut w = BitWriter::new();
            w.put_te_v(c_max, v);
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            assert_eq!(r.te_v(c_max), v, "c_max={c_max} v={v}");
        }
    }
}

/// Clause 9.1.2, Table 9-4: a few rows read out of the table by hand, including
/// the first row of each column, which is where an off-by-one shows first.
#[test]
fn table_9_4_spot_checks() {
    use ChromaArrayType::{Monochrome, WithChroma};
    use MbPartPredMode::{Inter, Intra};

    assert_eq!(cbp_from_code_num(0, WithChroma, Intra), Some(47));
    assert_eq!(cbp_from_code_num(0, WithChroma, Inter), Some(0));
    assert_eq!(cbp_from_code_num(3, WithChroma, Intra), Some(0));
    assert_eq!(cbp_from_code_num(1, WithChroma, Inter), Some(16));
    assert_eq!(cbp_from_code_num(47, WithChroma, Intra), Some(41));
    assert_eq!(cbp_from_code_num(47, WithChroma, Inter), Some(41));

    assert_eq!(cbp_from_code_num(0, Monochrome, Intra), Some(15));
    assert_eq!(cbp_from_code_num(0, Monochrome, Inter), Some(0));
    assert_eq!(cbp_from_code_num(15, Monochrome, Intra), Some(9));
    assert_eq!(cbp_from_code_num(15, Monochrome, Inter), Some(9));
}

#[test]
fn me_v_reads_through_the_table() {
    // codeNum 3 is '00100'; with ChromaArrayType 1 and intra prediction Table
    // 9-4 maps it to coded_block_pattern 0.
    let data = bits("00100");
    let mut r = BitReader::new(&data);
    assert_eq!(
        r.me_v(ChromaArrayType::WithChroma, MbPartPredMode::Intra),
        0
    );
    assert!(!r.overrun());
}

#[test]
fn me_v_out_of_table_flags_rather_than_panics() {
    // codeNum 48 has no row in the 48-entry column.
    let mut w = BitWriter::new();
    w.put_ue_v(48);
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    assert_eq!(
        r.me_v(ChromaArrayType::WithChroma, MbPartPredMode::Intra),
        0
    );
    assert!(r.overrun(), "an out-of-table code number must flag");

    let mut r = BitReader::new(&bytes);
    assert!(
        r.me_v_checked(ChromaArrayType::WithChroma, MbPartPredMode::Intra)
            .is_err()
    );
}

// ------------------------------------------------------------- order-k coding

/// Order-`k` Exp-Golomb, worked by hand from the definition in clause 9.1:
/// `lz` zeros, a one, then `lz + k` bits, value `(2^lz − 1)·2^k + suffix`.
#[test]
fn order_k_worked_examples() {
    // k = 1. lz = 0: codeword is '1' then 1 bit. Values 0 and 1.
    let data = bits("10 11");
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_k(1), 0);
    assert_eq!(r.ue_k(1), 1);

    // k = 1, lz = 1: '01' then 2 bits. Value = (2^1 − 1)·2 + suffix = 2 + s.
    let data = bits("01 00");
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_k(1), 2);
    let data = bits("01 11");
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_k(1), 5);

    // k = 3, lz = 0: '1' then 3 bits, values 0..=7.
    for s in 0..8u32 {
        let pattern = format!("1{s:03b}");
        let data = bits(&pattern);
        let mut r = BitReader::new(&data);
        assert_eq!(r.ue_k(3), s, "pattern {pattern}");
    }
}

#[test]
fn order_zero_is_ue_v() {
    for v in [0u32, 1, 2, 3, 100, 65534, 65535, 1 << 20] {
        let mut w = BitWriter::new();
        w.put_ue_k(0, v);
        let a = w.finish();
        let mut w = BitWriter::new();
        w.put_ue_v(v);
        let b = w.finish();
        assert_eq!(a, b, "order 0 must be ue(v) for {v}");
    }
}

// -------------------------------------------------------- termination and caps

/// The property that stops a fuzz hang: an all-zero buffer must be rejected in
/// constant time, not looped on.
#[test]
fn all_zeros_is_rejected_not_looped() {
    let data = [0u8; 256];
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_v(), 0);
    assert!(r.overrun());

    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_v64(), 0);
    assert!(r.overrun());

    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_k(3), 0);
    assert!(r.overrun());
}

#[test]
fn empty_buffer_never_panics() {
    let data: [u8; 0] = [];
    let mut r = BitReader::new(&data);
    assert_eq!(r.ue_v(), 0);
    assert_eq!(r.se_v(), 0);
    assert_eq!(r.te_v(1), 1);
    assert_eq!(r.ue_k(4), 0);
    assert_eq!(r.ue_v64(), 0);
    assert!(r.overrun());
}

#[test]
fn ue_v64_reaches_past_the_32_bit_ceiling() {
    for v in [
        0u64,
        1,
        u64::from(u32::MAX) - 1,
        u64::from(u32::MAX),
        1u64 << 40,
        (1u64 << 62) - 1,
    ] {
        let mut w = BitWriter::new();
        w.put_ue_v64(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue_v64(), v, "value {v}");
        assert!(!r.overrun());
    }
}

#[test]
fn max_value_at_the_32_bit_boundary() {
    // codeNum 2^32 − 2 is the largest ue(v) with a 31-zero prefix.
    let v = u32::MAX - 1;
    let mut w = BitWriter::new();
    w.put_ue_v(v);
    let bytes = w.finish();
    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_v(), v);
    assert!(!r.overrun());
    assert_eq!(map::ue_bit_len(v), 63);
}

// --------------------------------------------------------------- bounded reads

#[test]
fn ue_v_max_rejects_above_the_ceiling() {
    let mut w = BitWriter::new();
    w.put_ue_v(300);
    let bytes = w.finish();

    let mut r = BitReader::new(&bytes);
    assert_eq!(r.ue_v_max(1024), Ok(300));

    let mut r = BitReader::new(&bytes);
    assert!(matches!(
        r.ue_v_max(255),
        Err(vaco_bitstream::BitstreamError::ValueTooLarge { value: 300, .. })
    ));
}

#[test]
fn se_v_range_rejects_outside() {
    let mut w = BitWriter::new();
    w.put_se_v(-40);
    let bytes = w.finish();

    let mut r = BitReader::new(&bytes);
    assert_eq!(r.se_v_range(-64, 63), Ok(-40));

    let mut r = BitReader::new(&bytes);
    assert!(r.se_v_range(-10, 10).is_err());
}

#[test]
fn bounded_golomb_runs_out_of_fuel_rather_than_forever() {
    use vaco_bitstream::BitWriter;
    use vaco_codec_golomb::BoundedGolomb;
    use vaco_limits::{Budget, Limits};

    let mut w = BitWriter::new();
    for _ in 0..1000 {
        w.put_ue_v(1);
    }
    let bytes = w.finish();

    let mut reader = BitReader::new(&bytes);
    let mut budget = Budget::new(Limits::strict().with_fuel(10));
    let mut g = BoundedGolomb::new(&mut reader, &mut budget);

    let mut reads = 0;
    while g.ue_v(u32::MAX).is_ok() {
        reads += 1;
        assert!(reads <= 10, "fuel must stop this loop");
    }
    assert_eq!(reads, 10);
}

#[test]
fn bounded_counted_loop_refuses_an_implausible_count() {
    use vaco_codec_golomb::BoundedGolomb;
    use vaco_limits::{Budget, Limits};

    let mut w = BitWriter::new();
    w.put_ue_v(1_000_000); // a declared count nothing in the buffer backs up
    let bytes = w.finish();

    let mut reader = BitReader::new(&bytes);
    let mut budget = Budget::new(Limits::tiny());
    let mut g = BoundedGolomb::new(&mut reader, &mut budget);
    assert!(g.ue_v_counted(u32::MAX, u32::MAX).is_err());
}

// ------------------------------------------------- agreement with vaco-bitstream

/// The two `ue(v)` implementations in this workspace must be indistinguishable.
///
/// This crate's is a different shape (see `GolombDecode::ue_v`), and parsers
/// already written against `vaco-bitstream` must keep decoding identically —
/// so the interesting test is not "does mine work" but "do they ever differ".
#[test]
fn agrees_with_vaco_bitstream_over_a_dense_corpus() {
    let mut w = BitWriter::new();
    let mut values = Vec::new();
    for v in (0u32..4096).chain((0..32).map(|k| (1u32 << k).wrapping_sub(1))) {
        values.push(v);
        w.put_ue_v(v);
    }
    let bytes = w.finish();

    let mut a = BitReader::new(&bytes);
    let mut b = BitReader::new(&bytes);
    for &want in &values {
        let mine = GolombDecode::ue_v(&mut a);
        let theirs = GolombRead::ue(&mut b);
        assert_eq!(mine, theirs, "disagreement on {want}");
        assert_eq!(mine, want);
    }
    assert_eq!(a.bit_pos(), b.bit_pos());
    assert_eq!(a.overrun(), b.overrun());
}

/// The same agreement on *garbage*, which is the case that actually matters:
/// two implementations that agree on valid input and diverge on malformed input
/// are a differential bug waiting for an attacker.
#[test]
fn agrees_with_vaco_bitstream_on_garbage() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    for _ in 0..2000 {
        let mut data = Vec::new();
        for _ in 0..16 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.push(state as u8);
        }
        let mut a = BitReader::new(&data);
        let mut b = BitReader::new(&data);
        for _ in 0..24 {
            assert_eq!(GolombDecode::ue_v(&mut a), GolombRead::ue(&mut b));
            assert_eq!(a.bit_pos(), b.bit_pos());
            assert_eq!(a.overrun(), b.overrun());
        }
    }
}

// --------------------------------------------------------------- cost functions

#[test]
fn bit_lengths_match_what_the_writer_produces() {
    for v in (0u32..2048).chain([65535, 65536, 1 << 20, u32::MAX - 1]) {
        let mut w = BitWriter::new();
        w.put_ue_v(v);
        assert_eq!(w.bit_len(), u64::from(map::ue_bit_len(v)), "ue({v})");
    }
    for v in (-1024i32..1024).chain([i32::MAX, -i32::MAX]) {
        let mut w = BitWriter::new();
        w.put_se_v(v);
        assert_eq!(w.bit_len(), u64::from(map::se_bit_len(v)), "se({v})");
    }
    for k in 0..8u32 {
        for v in (0u32..512).chain([1 << 16, 1 << 20]) {
            let mut w = BitWriter::new();
            w.put_ue_k(k, v);
            assert_eq!(
                w.bit_len(),
                u64::from(map::ue_k_bit_len(v, k)),
                "ue_k({v}, {k})"
            );
        }
    }
}

#[test]
fn batch_cost_matches_the_scalar_one() {
    let values: Vec<u32> = (0..1000).map(|i| i * i).collect();
    let expect: u64 = values.iter().map(|&v| u64::from(map::ue_bit_len(v))).sum();
    assert_eq!(map::ue_bits_total(&values), expect);

    let mut out = vec![0u32; values.len()];
    assert_eq!(map::ue_bit_len_batch(&values, &mut out), values.len());
    assert_eq!(out[0], map::ue_bit_len(values[0]));

    // A short destination truncates rather than panicking.
    let mut short = vec![0u32; 3];
    assert_eq!(map::ue_bit_len_batch(&values, &mut short), 3);
}
