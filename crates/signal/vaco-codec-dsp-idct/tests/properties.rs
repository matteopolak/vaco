//! Property tests for invariants the golden vectors do not exercise:
//! no-panic over the full `i32` domain (plan 13 §2.2.1 — coefficients come
//! from an entropy decoder fed untrusted bytes) and linearity of the pure
//! matrix-multiply HEVC transforms (no rounding lives inside a single 1-D
//! pass, so superposition must hold exactly, not approximately).

use proptest::prelude::*;
use vaco_codec_dsp_idct::{h264, hevc};

proptest! {
    /// Every H.264 transform accepts arbitrary `i32` input — including
    /// `i32::MIN`, which is exactly the shape of input `vaco-scale`'s fuzzer
    /// found reachable and which panics under unary negation or unguarded
    /// addition/subtraction. No assertion beyond "returned without panicking"
    /// is meaningful here: the standard defines no output for
    /// out-of-conformance input.
    #[test]
    fn h264_4x4_never_panics(c in prop::array::uniform::<_, 16>(any::<i32>())) {
        let _ = h264::idct4x4(&c);
    }

    #[test]
    fn h264_8x8_never_panics(c in prop::array::uniform::<_, 64>(any::<i32>())) {
        let _ = h264::idct8x8(&c);
    }

    #[test]
    fn h264_luma_dc_never_panics(c in prop::array::uniform::<_, 16>(any::<i32>())) {
        let _ = h264::luma_dc_hadamard4x4(&c);
    }

    #[test]
    fn h264_chroma_dc_never_panics(c in prop::array::uniform::<_, 4>(any::<i32>()), c8 in prop::array::uniform::<_, 8>(any::<i32>())) {
        let _ = h264::chroma_dc_hadamard2x2(&c);
        let _ = h264::chroma_dc_hadamard2x4(&c8);
    }

    /// A DC-only 4x4 block is uniform for *every* DC value, not just the one
    /// example pinned in the unit tests — this is the property that caught
    /// the HEVC transpose bug (see `hevc.rs`), generalised.
    #[test]
    fn h264_4x4_dc_only_is_always_uniform(dc in -10_000_i32..10_000) {
        let mut c = [0i32; 16];
        if let Some(v) = c.first_mut() {
            *v = dc;
        }
        let r = h264::idct4x4(&c);
        let first = r.first().copied().unwrap_or(0);
        prop_assert!(r.iter().all(|&v| v == first));
    }

    #[test]
    fn hevc_dct4_never_panics(c in prop::array::uniform::<_, 4>(any::<i32>())) {
        let _ = hevc::dct1d(&c);
        let _ = hevc::dst1d(&c);
    }

    #[test]
    fn hevc_dct8_never_panics(c in prop::array::uniform::<_, 8>(any::<i32>())) {
        let _ = hevc::dct1d(&c);
    }

    #[test]
    fn hevc_2d_never_panics(c in prop::collection::vec(any::<i32>(), 16)) {
        let mut out = [0i32; 16];
        hevc::idct2d_dct::<4>(&c, &mut out, hevc::ClipRange::non_extended());
        hevc::idct2d_dst4(&c, &mut out, hevc::ClipRange::non_extended());
    }

    /// `dct1d` is a pure linear map (the only non-linearity in the whole
    /// 2-D transform is the caller's mid-pass rounding shift), so
    /// superposition holds exactly for inputs small enough that neither the
    /// per-term product nor the sum saturates `i32` — chosen well inside
    /// that margin (max term magnitude here is `1000 * 90 * 32 ≈ 2.9e6`).
    #[test]
    fn hevc_dct8_is_linear(a in prop::array::uniform::<_, 8>(-1000_i32..1000), b in prop::array::uniform::<_, 8>(-1000_i32..1000)) {
        let ya = hevc::dct1d(&a);
        let yb = hevc::dct1d(&b);
        let sum: [i32; 8] = core::array::from_fn(|i| a.get(i).unwrap_or(&0) + b.get(i).unwrap_or(&0));
        let ysum = hevc::dct1d(&sum);
        let combined: [i32; 8] = core::array::from_fn(|i| ya.get(i).unwrap_or(&0) + yb.get(i).unwrap_or(&0));
        prop_assert_eq!(ysum, combined);
    }

    /// The MPEG-2 wrapper never produces NaN/inf for finite, bounded input
    /// (the domain any real dequantised DCT coefficient lives in).
    #[test]
    fn mpeg2_8x8_stays_finite(c in prop::array::uniform::<_, 64>(-10_000.0_f32..10_000.0)) {
        let Ok(mut idct) = vaco_codec_dsp_idct::mpeg2::idct8x8_f32() else {
            return Ok(());
        };
        let mut out = [0f32; 64];
        idct.apply(&c, &mut out);
        prop_assert!(out.iter().all(|v| v.is_finite()));
    }
}
