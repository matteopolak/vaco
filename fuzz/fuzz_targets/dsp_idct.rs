//! Fuzzing every inverse transform in `vaco-codec-dsp-idct` for panics on
//! arbitrary — including wildly out-of-spec-range — coefficients.
//!
//! Correctness is pinned separately by golden vectors transliterated from
//! the standards and by property tests (DC-uniformity, linearity); this
//! target exists purely to explore the input space a human would not think
//! to write by hand. The one bug class it is specifically aimed at:
//! `vaco-scale`'s fuzzer found `i32::MIN` reachable in a coefficient slot
//! where `.abs()` overflows in debug — this crate's H.264 butterflies use
//! the same shape of intermediate negation, so the same input class is
//! exercised here directly rather than hoped for.
//!
//! `hevc_2d` is a `Vec<i32>` fed, unmodified, to all four HEVC block sizes:
//! `idct2d_dct`/`idct2d_dst4` are documented to zero-pad or truncate a
//! mismatched length rather than panic (see `crate::util`), so a `Vec` of
//! arbitrary — including zero — length is exactly the adversarial shape that
//! documented behaviour needs to hold up against.
//! fuzz-crate: vaco-codec-dsp-idct
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_idct::{h264, hevc, mpeg2};

#[derive(Arbitrary, Debug)]
struct Input {
    h264_4x4: [i32; 16],
    h264_8x8: [i32; 64],
    h264_luma_dc: [i32; 16],
    h264_chroma_dc2x2: [i32; 4],
    h264_chroma_dc2x4: [i32; 8],
    hevc_4: [i32; 4],
    hevc_8: [i32; 8],
    hevc_16: [i32; 16],
    hevc_32: [i32; 32],
    hevc_2d: Vec<i32>,
    mpeg2_input: [f32; 64],
}

fuzz_target!(|input: Input| {
    let _ = h264::idct4x4(&input.h264_4x4);
    let _ = h264::idct8x8(&input.h264_8x8);
    let _ = h264::luma_dc_hadamard4x4(&input.h264_luma_dc);
    let _ = h264::chroma_dc_hadamard2x2(&input.h264_chroma_dc2x2);
    let _ = h264::chroma_dc_hadamard2x4(&input.h264_chroma_dc2x4);

    let _ = hevc::dct1d(&input.hevc_4);
    let _ = hevc::dst1d(&input.hevc_4);
    let _ = hevc::dct1d(&input.hevc_8);
    let _ = hevc::dct1d(&input.hevc_16);
    let _ = hevc::dct1d(&input.hevc_32);

    let clip = hevc::ClipRange::non_extended();
    let mut out4 = [0i32; 16];
    hevc::idct2d_dct::<4>(&input.hevc_2d, &mut out4, clip);
    hevc::idct2d_dst4(&input.hevc_2d, &mut out4, clip);

    let mut out8 = [0i32; 64];
    hevc::idct2d_dct::<8>(&input.hevc_2d, &mut out8, clip);

    let mut out16 = [0i32; 256];
    hevc::idct2d_dct::<16>(&input.hevc_2d, &mut out16, clip);

    let mut out32 = [0i32; 1024];
    hevc::idct2d_dct::<32>(&input.hevc_2d, &mut out32, clip);

    // NaN/inf coefficients are not something a dequantiser should ever
    // produce, but this module's contract (see `mpeg2.rs`) is stated over
    // finite input; sanitise rather than let a NaN propagate and trivially
    // "fail" every downstream finiteness check for a reason unrelated to the
    // transform itself.
    let sanitized: [f32; 64] = input.mpeg2_input.map(|v| if v.is_finite() { v } else { 0.0 });
    if let Ok(mut idct) = mpeg2::idct8x8_f32() {
        let mut mout = [0f32; 64];
        idct.apply(&sanitized, &mut mout);
    }
});
