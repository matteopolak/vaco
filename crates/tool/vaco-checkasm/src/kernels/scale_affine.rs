//! Wires `vaco-scale`'s colour-matrix row kernel through the harness.
//!
//! Why this one, of every [`vaco_simd::KernelSet`] in the tree: it is on the
//! hot path of every colour-space conversion (`vaco-scale::fast::ScaleKernels
//! ::affine_row`), it already has a `fits_i32` correctness precondition that
//! is itself worth checking rather than assuming, and it is a genuine
//! production kernel — not a tutorial. `vaco_simd::example::ColorKernels`
//! exists specifically as an authoring template and stays out of `verify` for
//! that reason: it demonstrates the *pattern*, this demonstrates the harness
//! against something that ships.
//!
//! # The domain restriction, and why it is not a limitation dressed up
//!
//! [`vaco_scale::fast::fits_i32`] is the kernel's own documented precondition:
//! the vector path uses an `i32` accumulator, proved sufficient only for
//! component values in `0..=affine.max`. Feeding it values outside that
//! domain (`i32::MIN`, say) would not exercise a real bug — it would exercise
//! `i32` overflow the kernel's own contract already excludes, and a debug
//! build's overflow check would panic identically on both sides, which is
//! not a differential finding. So the corpus stays inside `0..=max` and
//! sweeps *every* boundary of that domain instead: `0`, `max`, both
//! neighbours of both, an alternating pattern, and a bounded ramp — the
//! shape [`crate::edge::boundaries_u8`] targets, widened to whatever depth
//! `max` implies.

use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
use vaco_pixfmt::PixFmt;
use vaco_scale::colour::{self, Affine, ColorStage};
use vaco_scale::fast::ScaleKernels;
use vaco_scale::spec::ImageSpec;
use vaco_simd::KernelSet;

use crate::Kernel;
use crate::edge;

/// A BT.709 limited-range `Y'CbCr` to full-range `R'G'B'` affine, 8-bit — the
/// same fixture `vaco-scale`'s own kernel test builds, so a divergence found
/// here is a divergence in something the crate already believes it verified.
fn fixture_affine() -> Affine {
    let src = ImageSpec {
        format: PixFmt::Yuv444p,
        width: 64,
        height: 1,
        color: ColorInfo {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt709,
            ..ColorInfo::default()
        },
    };
    let dst = ImageSpec {
        format: PixFmt::Rgb24,
        width: 64,
        height: 1,
        color: ColorInfo {
            range: ColorRange::Full,
            ..ColorInfo::default()
        },
    };
    match colour::build(&src, &dst, 8) {
        ColorStage::Affine(a) => a,
        ColorStage::None => {
            // `build` only returns `None` when the two specs need no colour
            // conversion at all; the fixture above deliberately differs in
            // both matrix and range, so this arm is unreachable for it. A
            // constant fallback keeps this module free of `panic`/`unwrap`.
            Affine {
                m: [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
                bias: [0, 0, 0],
                shift: 0,
                max: 255,
            }
        }
    }
}

/// One input: the shared affine plus three equal-length component rows.
#[derive(Debug, Clone)]
pub struct AffineCase {
    affine: Affine,
    r0: Vec<i32>,
    r1: Vec<i32>,
    r2: Vec<i32>,
}

/// [`Kernel`] adapter for `ScaleKernels::affine_row`.
///
/// A marker type — see [`crate::Kernel`]'s doc for why `Differential` is
/// generic over this rather than over the function pointer directly:
/// `KernelSet::reference()`/`select()` resolve the scalar and dispatched
/// tables respectively, and this type just remembers which field to call.
#[derive(Debug, Clone, Copy)]
pub struct AffineRowKernel;

impl Kernel for AffineRowKernel {
    const NAME: &'static str = "vaco-scale::affine_row";

    type Case = AffineCase;
    type Lane = i32;

    fn cases() -> Vec<Self::Case> {
        let affine = fixture_affine();
        let max = affine.max;
        let widths = edge::element_widths(4); // i32 lanes are 4 bytes wide
        let mut cases = Vec::new();
        for len in edge::lengths_around(&widths) {
            let zero = vec![0i32; len];
            let at_max = vec![max; len];
            let ramp = edge::ramp_bounded(len, max);
            let reverse_ramp: Vec<i32> = ramp.iter().rev().copied().collect();
            let alternating: Vec<i32> =
                (0..len).map(|i| if i % 2 == 0 { 0 } else { max }).collect();

            // Four permutations across the three rows, so the matrix's
            // off-diagonal terms (which mix channels) are actually exercised
            // rather than every row carrying the same value.
            cases.push(Self::Case {
                affine,
                r0: zero.clone(),
                r1: at_max.clone(),
                r2: ramp.clone(),
            });
            cases.push(Self::Case {
                affine,
                r0: at_max,
                r1: zero.clone(),
                r2: reverse_ramp.clone(),
            });
            cases.push(Self::Case {
                affine,
                r0: alternating.clone(),
                r1: ramp,
                r2: reverse_ramp,
            });
            cases.push(Self::Case {
                affine,
                r0: zero,
                r1: alternating,
                r2: vec![max; len],
            });
        }
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        run(ScaleKernels::reference(), case)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        run(ScaleKernels::select(), case)
    }
}

/// Run one [`ScaleKernels`] table's `affine_row` on a case, flattened as
/// `r0 ++ r1 ++ r2` — the same flattening [`AffineRowKernel::scalar`] and
/// [`AffineRowKernel::vector`] both use, so a reported lane index means the
/// same row/position under either.
fn run(kernels: ScaleKernels, case: &AffineCase) -> Vec<i32> {
    let mut r0 = case.r0.clone();
    let mut r1 = case.r1.clone();
    let mut r2 = case.r2.clone();
    (kernels.affine_row)(&case.affine, &mut r0, &mut r1, &mut r2);
    r0.into_iter().chain(r1).chain(r2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn the_wired_production_kernel_agrees_with_its_scalar_reference() {
        let report = Differential::<AffineRowKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }

    #[test]
    fn the_fixture_actually_exercises_the_vector_path() {
        // If this ever turned false, `verify` would still pass — every case
        // would just be routing through `fast::apply_affine`'s own fallback,
        // silently testing "scalar agrees with scalar." Pinning it here means
        // a future change to the fixture that broke that gets caught here,
        // not read as a green run that verified nothing.
        assert!(vaco_scale::fast::fits_i32(&fixture_affine()));
    }
}
