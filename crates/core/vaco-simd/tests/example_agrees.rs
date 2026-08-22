//! The worked example kernel, proved bit-identical to its scalar reference.
//!
//! This is the test a contributor copies alongside the kernel. It checks three
//! things a colour-conversion kernel gets wrong in practice:
//!
//! * the vector body against the reference over random pixel data;
//! * **every width from 0 to 200**, so the loop tail is exercised at every
//!   possible remainder, not just at a convenient multiple of 16;
//! * short and absent chroma rows, which is how a truncated frame reaches a
//!   scaler.

#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::panic,
    clippy::many_single_char_names,
    reason = "test code: a panic is the reporting mechanism"
)]

use proptest::prelude::*;
use vaco_simd::example::{
    ColorKernels, yuv420p_to_rgb24_row_dispatched, yuv420p_to_rgb24_row_scalar,
};
use vaco_simd::{KernelSet, Tier};

fn both(width: usize, y: &[u8], u: &[u8], v: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut a = vec![0u8; width * 3];
    let mut b = vec![0u8; width * 3];
    yuv420p_to_rgb24_row_scalar(y, u, v, &mut a);
    yuv420p_to_rgb24_row_dispatched(y, u, v, &mut b);
    (a, b)
}

#[test]
fn every_width_from_zero_to_two_hundred() {
    for width in 0..=200usize {
        let chroma = width.div_ceil(2);
        let y: Vec<u8> = (0..width).map(|i| ((i * 7) & 0xFF) as u8).collect();
        let u: Vec<u8> = (0..chroma).map(|i| ((i * 13) & 0xFF) as u8).collect();
        let v: Vec<u8> = (0..chroma)
            .map(|i| (255 - ((i * 3) & 0xFF)) as u8)
            .collect();
        let (scalar, simd) = both(width, &y, &u, &v);
        assert_eq!(simd, scalar, "width {width}");
    }
}

#[test]
fn saturating_extremes_clip_identically() {
    // Y=0 with V=0 drives R hard negative; Y=255 with V=255 drives it past 255.
    // Both ends must clip the same way in both implementations, which is the
    // whole reason `pack_u8_from_i16` exists.
    for (yv, uv, vv) in [(0u8, 0u8, 0u8), (255, 255, 255), (0, 255, 0), (255, 0, 255)] {
        let width = 64;
        let half = width >> 1;
        let (scalar, simd) = both(width, &vec![yv; width], &vec![uv; half], &vec![vv; half]);
        assert_eq!(simd, scalar, "y={yv} u={uv} v={vv}");
    }
}

#[test]
fn short_chroma_degrades_rather_than_panicking() {
    let width = 100;
    let y = vec![200u8; width];
    for chroma_len in [0usize, 1, 7, 8, 17, 49, 50] {
        let u = vec![90u8; chroma_len];
        let v = vec![170u8; chroma_len];
        let (scalar, simd) = both(width, &y, &u, &v);
        assert_eq!(simd, scalar, "chroma_len {chroma_len}");
    }
}

#[test]
fn kernel_set_tables_are_complete() {
    for tier in [
        Tier::Scalar,
        Tier::Sse2,
        Tier::Sse42,
        Tier::Avx2,
        Tier::Avx512,
        Tier::Neon,
    ] {
        let k = ColorKernels::for_tier(tier);
        let mut out = vec![0u8; 48];
        (k.yuv420p_to_rgb24_row)(&[128; 16], &[128; 8], &[128; 8], &mut out);
        assert_eq!(&out[..3], &[130, 130, 130], "tier {tier}");
    }
    assert_eq!(ColorKernels::kernel_names(), &["yuv420p_to_rgb24_row"]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn agrees_on_random_pixels(
        y in prop::collection::vec(any::<u8>(), 0..200),
        chroma in prop::collection::vec(any::<(u8, u8)>(), 0..100),
    ) {
        let width = y.len();
        let (u, v): (Vec<u8>, Vec<u8>) = chroma.into_iter().unzip();
        let (scalar, simd) = both(width, &y, &u, &v);
        prop_assert_eq!(simd, scalar);
    }
}
