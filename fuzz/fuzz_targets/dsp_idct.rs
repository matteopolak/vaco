//! Fuzzing every inverse transform in `vaco-codec-dsp-idct` for panics on
//! arbitrary — including wildly out-of-spec-range — coefficients.
//!
//! Most of this target explores the input space a human would not think to
//! write by hand and asserts only panic-freedom: the standard defines no
//! output for out-of-conformance coefficients, so there is nothing to check
//! a transform's result *against* over that domain. The one bug class it
//! is specifically aimed at there: `vaco-scale`'s fuzzer found `i32::MIN`
//! reachable in a coefficient slot where `.abs()` overflows in debug — this
//! crate's H.264 butterflies use the same shape of intermediate negation,
//! so the same input class is exercised here directly rather than hoped
//! for.
//!
//! Two exact properties from `tests/properties.rs` *are* wired in over a
//! constrained sub-domain, rather than left to that file's own hand-picked
//! proptest ranges: a DC-only H.264 4x4 block must decode to a uniform
//! block for every DC value, not just an example ("the property that
//! caught the HEVC transpose bug, generalised" per that file), and
//! `hevc::dct1d` is a pure linear map, so superposition must hold exactly
//! for inputs small enough that no term or sum saturates `i32`. Both
//! constrain a small derived value into the same range `tests/properties.rs`
//! itself uses, rather than testing the raw arbitrary coefficients directly
//! (which routinely saturate and would make the property no longer hold —
//! for a real, not a fabricated, reason).
//!
//! `mpeg2`'s wrapper is also checked for the property
//! `tests/properties.rs` calls `mpeg2_8x8_stays_finite`: finite, bounded
//! input never produces NaN/inf output.
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
use vaco_codec_dsp_idct::{blockdsp, h264, hevc, mpeg2, pixblockdsp};

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
    /// DC value for the H.264 4x4 uniformity check, independent of
    /// `h264_4x4` so mutating one does not disturb the other's coverage.
    /// Range matches `tests/properties.rs`'s own `-10_000..10_000`.
    h264_dc: i32,
    /// The two operands for the HEVC `dct1d` linearity check, independent
    /// of `hevc_8`. Range matches `tests/properties.rs`'s own
    /// `-1000..1000` per element, chosen there so neither a per-term
    /// product nor the sum saturates `i32`.
    hevc_lin_a: [i16; 8],
    hevc_lin_b: [i16; 8],

    /// D-11's block-plumbing addition: arbitrary strided-plane geometry and
    /// small pixel/coefficient buffers, so `get_pixels`/`diff_pixels`/
    /// `add_pixels_clamped`/`fill_block` see undersized buffers, zero
    /// dimensions and strides shorter than the block width in the same
    /// process as the transforms they feed.
    block_src: Vec<u8>,
    block_src2: Vec<u8>,
    block_dst_u8: Vec<u8>,
    block_i16: Vec<i16>,
    block_stride: u8,
    block_stride2: u8,
    block_w: u8,
    block_h: u8,
}

/// Clamp into `tests/properties.rs`'s own `-10_000..10_000` DC range,
/// rather than the full `i32` domain the fuzzer's `i32` field would
/// otherwise cover — outside it the transform can legitimately saturate,
/// which would make the uniformity property false for a reason that has
/// nothing to do with a bug.
fn clamp_dc(v: i32) -> i32 {
    v.clamp(-9_999, 9_999)
}

/// `i16` already fits `tests/properties.rs`'s `-1000..1000` linearity
/// range only up to +-32767, so narrow it the rest of the way explicitly.
fn clamp_lin(v: i16) -> i32 {
    i32::from(v).clamp(-999, 999)
}

fuzz_target!(|input: Input| {
    let _ = h264::idct4x4(&input.h264_4x4);
    let _ = h264::idct8x8(&input.h264_8x8);
    let _ = h264::luma_dc_hadamard4x4(&input.h264_luma_dc);
    let _ = h264::chroma_dc_hadamard2x2(&input.h264_chroma_dc2x2);
    let _ = h264::chroma_dc_hadamard2x4(&input.h264_chroma_dc2x4);

    // A DC-only 4x4 block must decode to a single uniform value, for every
    // DC in range — not just the one example a unit test would pin.
    {
        let dc = clamp_dc(input.h264_dc);
        let mut c = [0i32; 16];
        if let Some(v) = c.first_mut() {
            *v = dc;
        }
        let r = h264::idct4x4(&c);
        let first = r.first().copied().unwrap_or(0);
        assert!(
            r.iter().all(|&v| v == first),
            "DC-only 4x4 block (dc={dc}) decoded non-uniformly: {r:?}"
        );
    }

    let _ = hevc::dct1d(&input.hevc_4);
    let _ = hevc::dst1d(&input.hevc_4);
    let _ = hevc::dct1d(&input.hevc_8);
    let _ = hevc::dct1d(&input.hevc_16);
    let _ = hevc::dct1d(&input.hevc_32);

    // `dct1d` is a pure linear map (no rounding lives inside a single 1-D
    // pass), so superposition must hold exactly over a range small enough
    // that neither a per-term product nor the sum saturates `i32`.
    {
        let a: [i32; 8] = input.hevc_lin_a.map(clamp_lin);
        let b: [i32; 8] = input.hevc_lin_b.map(clamp_lin);
        let sum: [i32; 8] = core::array::from_fn(|i| a[i] + b[i]);
        let ya = hevc::dct1d(&a);
        let yb = hevc::dct1d(&b);
        let ysum = hevc::dct1d(&sum);
        let combined: [i32; 8] = core::array::from_fn(|i| ya[i] + yb[i]);
        assert_eq!(
            ysum, combined,
            "dct1d(a+b) != dct1d(a)+dct1d(b) for a={a:?} b={b:?}"
        );
    }

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
    // *finite, bounded* input (`tests/properties.rs`'s own
    // `mpeg2_8x8_stays_finite`, `-10_000.0..10_000.0`); sanitise rather than
    // let a NaN or an astronomical-but-technically-finite magnitude
    // propagate and trivially "fail" the finiteness check below for a
    // reason unrelated to the transform itself.
    let sanitized: [f32; 64] = input.mpeg2_input.map(|v| {
        if v.is_finite() {
            v.clamp(-10_000.0, 10_000.0)
        } else {
            0.0
        }
    });
    if let Ok(mut idct) = mpeg2::idct8x8_f32() {
        let mut mout = [0f32; 64];
        idct.apply(&sanitized, &mut mout);
        assert!(
            mout.iter().all(|v| v.is_finite()),
            "finite, bounded input produced a non-finite mpeg2 idct8x8 output: {mout:?}"
        );
    }

    // D-11: block extraction/reconstruction never panics on any
    // combination of buffer length, stride and block geometry, including
    // strides shorter than the block width and dimensions larger than
    // either buffer.
    {
        let w = usize::from(input.block_w);
        let h = usize::from(input.block_h);
        let stride = usize::from(input.block_stride);
        let stride2 = usize::from(input.block_stride2);

        let mut dst_i16 = vec![0i16; w.saturating_mul(h)];
        pixblockdsp::get_pixels(&mut dst_i16, &input.block_src, stride, w, h);
        pixblockdsp::diff_pixels(
            &mut dst_i16,
            &input.block_src,
            stride,
            &input.block_src2,
            stride2,
            w,
            h,
        );

        let mut coeff_block = input.block_i16.clone();
        blockdsp::clear_block(&mut coeff_block);

        let mut plane = input.block_dst_u8.clone();
        blockdsp::fill_block(&mut plane, stride, w, h, 0x55);
        blockdsp::put_pixels_clamped(&input.block_i16, &mut plane, stride, w, h);
        blockdsp::add_pixels_clamped(&input.block_i16, &mut plane, stride, w, h);
    }
});
