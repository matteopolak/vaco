//! Wires `vaco-codec-dsp-idct`'s dispatched
//! [`add_pixels_clamped_vector`](vaco_codec_dsp_idct::simd::add_pixels_clamped_vector)
//! variant through the harness (D-11, #123): every block-based codec's
//! `Clip1(pred + residual)` reconstruction step.
//!
//! This verifies `add_pixels_clamped_vector`, not the crate's public
//! `simd::add_pixels_clamped` — that entry is gated to the scalar
//! implementation (measured ~0.84–0.9x on aarch64/NEON, a pessimisation; see
//! `vaco-codec-dsp-idct`'s `src/simd.rs` module doc), so it no longer
//! reaches the dispatched body this kernel needs to keep exercising under
//! `Differential`.
//!
//! The corpus deliberately includes `i16` residual values near
//! `i16::MIN`/`i16::MAX` — a real bug lived exactly there (see that
//! function's own doc): an earlier version added the widened `u8` and the
//! `i16` residual directly in 16-bit lanes, which overflows for a residual
//! near the top of `i16`'s range even though the scalar reference (`i32`
//! arithmetic throughout) cannot. Found by this crate's own proptest before
//! it ever reached this harness; kept here too so a regression shows up in
//! the same differential sweep every other kernel gets.

use vaco_codec_dsp_idct::{blockdsp, simd};
use vaco_simd::Caps;

use crate::Kernel;
use crate::edge;

/// One case: a strided pixel plane, a residual block, and the geometry
/// (`stride`, `w`, `h`) relating them. `stride` can be narrower than `w`
/// (rows overlapping in the flat buffer) deliberately — both
/// implementations must agree on the resulting read-then-write order,
/// which this exercises directly rather than assuming.
#[derive(Debug, Clone)]
pub struct AddPixelsCase {
    dst_init: Vec<u8>,
    residual: Vec<i16>,
    stride: usize,
    w: usize,
    h: usize,
}

/// [`Kernel`] adapter for
/// [`vaco_codec_dsp_idct::simd::add_pixels_clamped_vector`].
#[derive(Debug, Clone, Copy)]
pub struct AddPixelsClampedKernel;

impl Kernel for AddPixelsClampedKernel {
    const NAME: &'static str = "vaco-codec-dsp-idct::blockdsp::add_pixels_clamped";

    type Case = AddPixelsCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(1); // u8 lanes are one byte each.
        let boundaries_i16 = edge::boundaries_i16();
        let mut cases = Vec::new();

        for w in edge::lengths_around(&widths) {
            for h in [1usize, 2, 3] {
                let len = w * h;
                let dst_init: Vec<u8> = (0..len).map(|i| ((i * 61) % 256) as u8).collect();
                let residual_ramp: Vec<i16> = (0..len)
                    .map(|i| i32::try_from((i * 37) % 512).unwrap_or(0) as i16 - 256)
                    .collect();
                // Every i16 boundary value, tiled across the block, so a
                // per-lane overflow (the bug this kernel's doc names) is
                // reachable at every vector-width tail, not just len == 1.
                let residual_boundary: Vec<i16> = (0..len)
                    .map(|i| {
                        *boundaries_i16
                            .get(i % boundaries_i16.len().max(1))
                            .unwrap_or(&0)
                    })
                    .collect();
                for residual in [residual_ramp, residual_boundary] {
                    // stride == w (contiguous rows) and stride < w
                    // (overlapping rows) both, when h > 1.
                    #[allow(
                        clippy::integer_division,
                        reason = "deliberately narrower stride, a third of w rounded down, to build an overlapping-row test case"
                    )]
                    let narrower_stride = w.saturating_sub(w / 3).max(1);
                    for stride in [w, narrower_stride] {
                        let buf_len = stride.saturating_mul(h.saturating_sub(1)) + w;
                        let mut dst = dst_init.clone();
                        dst.resize(buf_len.max(dst.len()), 0);
                        cases.push(AddPixelsCase {
                            dst_init: dst,
                            residual: residual.clone(),
                            stride,
                            w,
                            h,
                        });
                    }
                }
            }
        }
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        let mut dst = case.dst_init.clone();
        blockdsp::add_pixels_clamped(&case.residual, &mut dst, case.stride, case.w, case.h);
        dst
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut dst = case.dst_init.clone();
        simd::add_pixels_clamped_vector(
            Caps::detect(),
            &case.residual,
            &mut dst,
            case.stride,
            case.w,
            case.h,
        );
        dst
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn add_pixels_clamped_dispatched_agrees_with_scalar() {
        let report = Differential::<AddPixelsClampedKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }
}
