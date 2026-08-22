//! The sample-format numeric contract, pinned against measured values.

#![allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::excessive_precision,
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    clippy::cast_possible_wrap,
    clippy::drop_non_drop,
    clippy::field_reassign_with_default,
    clippy::redundant_closure_for_method_calls,
    clippy::collapsible_if,
    reason = "test and benchmark code; a panic here is a failing test, which is the point"
)]

use vaco_resample::convert::{Elem, convert, convert_elems, elem};
use vaco_resample::{AudioMut, AudioRef};
use vaco_sampfmt::SampleFmt;

// ---------------------------------------------------------------------------
// Integer narrowing is a shift, not a rounding. Plan 17 §B.3.2 says otherwise.
// ---------------------------------------------------------------------------

#[test]
fn s32_to_s16_is_an_arithmetic_shift() {
    // MEASURED, ffmpeg 8.1: these exact pairs.
    for (input, want) in [
        (-32769i32, -1i16),
        (-32768, -1),
        (-32767, -1),
        (-1, -1),
        (0, 0),
        (1, 0),
        (32767, 0),
        (32768, 0),
        (65535, 0),
        (65536, 1),
        (-65536, -1),
        (-65537, -2),
        (i32::MAX, 32767),
        (i32::MIN, -32768),
    ] {
        assert_eq!(elem::i32_to_i16(input), want, "s32 {input} -> s16");
    }
}

#[test]
fn u8_is_offset_binary_with_a_128_bias() {
    for (input, want) in [
        (0u8, -32768i16),
        (1, -32512),
        (127, -256),
        (128, 0),
        (129, 256),
        (255, 32512),
        (64, -16384),
        (192, 16384),
    ] {
        assert_eq!(elem::u8_to_i16(input), want);
    }
    for (input, want) in [
        (0i16, 128u8),
        (1, 128),
        (-1, 127),
        (255, 128),
        (256, 129),
        (32767, 255),
        (-32768, 0),
        (16384, 192),
        (-16384, 64),
        (-129, 127),
    ] {
        assert_eq!(elem::i16_to_u8(input), want, "s16 {input} -> u8");
    }
}

#[test]
fn integer_to_float_divides_by_a_power_of_two() {
    assert_eq!(elem::i16_to_f32(-32768), -1.0);
    assert_eq!(elem::i16_to_f32(32767), 0.999_969_482_421_875);
    assert_eq!(elem::i16_to_f32(16384), 0.5);
    assert_eq!(elem::i32_to_f32(i32::MIN), -1.0);
    // i32::MAX rounds up to exactly 1.0 in f32; in f64 it does not.
    assert_eq!(elem::i32_to_f32(i32::MAX), 1.0);
    assert_eq!(elem::i32_to_f64(i32::MAX), 0.999_999_999_534_338_7);
    assert_eq!(elem::u8_to_f32(0), -1.0);
    assert_eq!(elem::u8_to_f32(128), 0.0);
    assert_eq!(elem::u8_to_f32(255), 0.992_187_5);
}

// ---------------------------------------------------------------------------
// Float to integer: two rounding modes, both measured.
// ---------------------------------------------------------------------------

#[test]
fn f32_to_s16_rounds_half_toward_positive_infinity() {
    // 65 536 exact half-LSB ties were probed; all of them round this way in the
    // reference's vector kernel. See `convert::F32_TO_S16_TAIL_DIVERGENCE`.
    for k in [-49i32, -8, -2, -1, 0, 1, 2, 7, 8, 100, 32000] {
        let x = (k as f32 + 0.5) / 32768.0;
        assert_eq!(
            elem::f32_to_i16(x),
            (k + 1) as i16,
            "tie at {k}.5 must round up"
        );
    }
    // Non-ties round to nearest either way.
    assert_eq!(elem::f32_to_i16(0.9 / 32768.0), 1);
    assert_eq!(elem::f32_to_i16(-0.9 / 32768.0), -1);
}

#[test]
fn every_other_float_to_integer_rounds_ties_to_even() {
    for k in [-5i64, -4, -3, -2, -1, 0, 1, 2, 3, 4] {
        let q = k as f64 + 0.5;
        let want = if k % 2 == 0 { k } else { k + 1 } as i16;
        assert_eq!(elem::f64_to_i16(q / 32768.0), want, "f64 tie at {q}");
    }
    assert_eq!(elem::f32_to_i32(0.5 / 2_147_483_648.0), 0);
    assert_eq!(elem::f32_to_i32(1.5 / 2_147_483_648.0), 2);
}

/// The reference clips `u8` and `s16` through a 32-bit truncation and `s32`
/// through a 64-bit clamp. That produces different answers for absurd inputs,
/// and both were measured directly.
#[test]
fn out_of_range_floats_reproduce_the_reference() {
    assert_eq!(elem::f32_to_i16(1.0), 32767);
    assert_eq!(elem::f32_to_i16(-1.0), -32768);
    assert_eq!(elem::f32_to_i16(2.0), 32767);
    assert_eq!(elem::f32_to_i16(-2.0), -32768);
    assert_eq!(elem::f32_to_i16(1e30), -1);
    assert_eq!(elem::f32_to_i16(-1e30), 0);
    assert_eq!(elem::f32_to_i16(f32::INFINITY), -1);
    assert_eq!(elem::f32_to_i16(f32::NEG_INFINITY), 0);
    assert_eq!(elem::f32_to_i16(f32::NAN), 0);

    assert_eq!(elem::f32_to_u8(1e30), 127);
    assert_eq!(elem::f32_to_u8(-1e30), 128);
    assert_eq!(elem::f32_to_u8(f32::NAN), 128);
    assert_eq!(elem::f32_to_u8(2.0), 255);

    // s32 clamps rather than wrapping.
    assert_eq!(elem::f32_to_i32(1e30), i32::MAX);
    assert_eq!(elem::f32_to_i32(-1e30), i32::MIN);
    assert_eq!(elem::f32_to_i32(f32::INFINITY), i32::MAX);
    assert_eq!(elem::f32_to_i32(f32::NAN), 0);

    assert_eq!(elem::f64_to_i16(1e300), -1);
    assert_eq!(elem::f64_to_i16(f64::NAN), 0);
}

// ---------------------------------------------------------------------------
// Round trips
// ---------------------------------------------------------------------------

#[test]
fn s16_round_trips_through_f32_exactly() {
    for k in i16::MIN..=i16::MAX {
        assert_eq!(elem::f32_to_i16(elem::i16_to_f32(k)), k, "s16 {k}");
    }
}

#[test]
fn s32_round_trips_through_f64_exactly() {
    for k in [
        i32::MIN,
        i32::MIN + 1,
        -1,
        0,
        1,
        12345,
        i32::MAX - 1,
        i32::MAX,
    ] {
        assert_eq!(elem::f64_to_i32(elem::i32_to_f64(k)), k, "s32 {k}");
    }
}

#[test]
fn u8_round_trips_through_f32_exactly() {
    for k in 0u8..=255 {
        assert_eq!(elem::f32_to_u8(elem::u8_to_f32(k)), k, "u8 {k}");
    }
}

// ---------------------------------------------------------------------------
// Layout: packed <-> planar
// ---------------------------------------------------------------------------

#[test]
fn packed_to_planar_deinterleaves() {
    let src: Vec<i16> = (0..12).collect();
    let bytes: Vec<u8> = src.iter().flat_map(|v| v.to_le_bytes()).collect();
    let a = AudioRef::packed(SampleFmt::S16, 3, &bytes).unwrap();
    assert_eq!(a.samples(), 4);

    let mut p0 = vec![0u8; 8];
    let mut p1 = vec![0u8; 8];
    let mut p2 = vec![0u8; 8];
    let mut planes: [&mut [u8]; 3] = [&mut p0, &mut p1, &mut p2];
    {
        let mut dst = AudioMut::planar(SampleFmt::S16P, &mut planes).unwrap();
        assert_eq!(convert(a, &mut dst).unwrap(), 4);
    }
    let read = |p: &[u8]| -> Vec<i16> {
        p.as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect()
    };
    assert_eq!(read(&p0), vec![0, 3, 6, 9]);
    assert_eq!(read(&p1), vec![1, 4, 7, 10]);
    assert_eq!(read(&p2), vec![2, 5, 8, 11]);
}

#[test]
fn planar_to_packed_interleaves_and_converts() {
    let a: Vec<u8> = [-32768i16, 0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let b: Vec<u8> = [16384i16, -16384]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let planes: [&[u8]; 2] = [&a, &b];
    let src = AudioRef::planar(SampleFmt::S16P, &planes).unwrap();
    let mut out = vec![0u8; 4 * 4];
    let mut dst = AudioMut::packed(SampleFmt::F32, 2, &mut out).unwrap();
    assert_eq!(convert(src, &mut dst).unwrap(), 2);
    drop(dst);
    let got: Vec<f32> = out
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    assert_eq!(got, vec![-1.0, 0.5, 0.0, -0.5]);
}

#[test]
fn identity_conversion_copies() {
    let bytes: Vec<u8> = (0u8..64).collect();
    let src = AudioRef::packed(SampleFmt::S16, 2, &bytes).unwrap();
    let mut out = vec![0u8; 64];
    let mut dst = AudioMut::packed(SampleFmt::S16, 2, &mut out).unwrap();
    convert(src, &mut dst).unwrap();
    drop(dst);
    assert_eq!(out, bytes);
}

#[test]
fn strided_and_contiguous_walks_agree() {
    // The `walk!` macro has a stride-1 specialisation because a runtime
    // `step_by` blocks vectorisation. The two paths must not disagree.
    let src: Vec<u8> = (0..64u16).flat_map(|v| v.to_le_bytes()).collect();
    let mut a = vec![0u8; 64 * 4];
    let mut b = vec![0u8; 64 * 4 * 3];
    convert_elems(Elem::S16, &src, 1, Elem::F32, &mut a, 1, 64);
    convert_elems(Elem::S16, &src, 1, Elem::F32, &mut b, 3, 64);
    for i in 0..64 {
        assert_eq!(
            a[i * 4..i * 4 + 4],
            b[i * 12..i * 12 + 4],
            "sample {i} disagrees between the stride-1 and strided walks"
        );
    }
}

#[test]
fn empty_and_degenerate_buffers_do_not_panic() {
    assert!(AudioRef::packed(SampleFmt::S16, 0, &[]).is_err());
    assert!(AudioRef::packed(SampleFmt::S16P, 2, &[]).is_err());
    assert!(AudioRef::planar(SampleFmt::S16, &[]).is_err());
    let empty: [&[u8]; 0] = [];
    assert!(AudioRef::planar(SampleFmt::S16P, &empty).is_err());
    // A ragged planar buffer is rejected rather than silently truncated.
    let a = [0u8; 4];
    let b = [0u8; 6];
    let planes: [&[u8]; 2] = [&a, &b];
    assert!(AudioRef::planar(SampleFmt::S16P, &planes).is_err());
    // An odd byte count is not a whole number of frames.
    assert!(AudioRef::packed(SampleFmt::S16, 2, &[0u8; 3]).is_err());

    let src = AudioRef::packed(SampleFmt::S16, 1, &[]).unwrap();
    let mut out = vec![0u8; 8];
    let mut dst = AudioMut::packed(SampleFmt::F32, 1, &mut out).unwrap();
    assert_eq!(convert(src, &mut dst).unwrap(), 0);
}
