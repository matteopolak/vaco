//! Wires `vaco-codec-dsp-intrapred`'s dispatched
//! [`dc_predict`](vaco_codec_dsp_intrapred::simd::dc_predict) variant
//! through the harness (D-09, #126) — the reference-sample summation,
//! picked over `planar_predict`/`angular_project` because it is the one
//! part of this crate shaped like a single-reduction kernel (see that
//! module's own doc for why).

use vaco_codec_dsp_intrapred::simd;
use vaco_simd::Caps;

use crate::Kernel;
use crate::edge;

/// One case: top/left reference arrays (possibly absent, modelled as
/// empty — matching [`vaco_codec_dsp_intrapred::dc_predict`]'s own
/// "shorter than `size` means unavailable" contract), a block size and a
/// bit depth.
#[derive(Debug, Clone)]
pub struct DcPredictCase {
    top: Vec<u16>,
    left: Vec<u16>,
    size: usize,
    bit_depth: u32,
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_intrapred::simd::dc_predict`].
#[derive(Debug, Clone, Copy)]
pub struct DcPredictKernel;

impl Kernel for DcPredictKernel {
    const NAME: &'static str = "vaco-codec-dsp-intrapred::dc_predict";

    type Case = DcPredictCase;
    type Lane = u16;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(2); // u16 lanes are two bytes each.
        let mut cases = Vec::new();
        for size in edge::lengths_around(&widths) {
            let top: Vec<u16> = (0..size).map(|i| u16::try_from((i * 37) % 4096).unwrap_or(0)).collect();
            let left: Vec<u16> = (0..size).map(|i| u16::try_from((i * 91) % 4096).unwrap_or(0)).collect();
            for bit_depth in [8u32, 10, 12] {
                // Both available, top-only, left-only, neither.
                cases.push(DcPredictCase { top: top.clone(), left: left.clone(), size, bit_depth });
                cases.push(DcPredictCase { top: top.clone(), left: Vec::new(), size, bit_depth });
                cases.push(DcPredictCase { top: Vec::new(), left: left.clone(), size, bit_depth });
                cases.push(DcPredictCase { top: Vec::new(), left: Vec::new(), size, bit_depth });
            }
        }
        // u16 saturation boundaries, on their own.
        for &v in &[0u16, 1, u16::MAX - 1, u16::MAX] {
            cases.push(DcPredictCase {
                top: vec![v; 16],
                left: vec![v; 16],
                size: 16,
                bit_depth: 8,
            });
        }
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![vaco_codec_dsp_intrapred::dc_predict(
            &case.top,
            &case.left,
            case.size,
            case.bit_depth,
        )]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![simd::dc_predict(
            Caps::detect(),
            &case.top,
            &case.left,
            case.size,
            case.bit_depth,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn dc_predict_dispatched_agrees_with_scalar() {
        let report = Differential::<DcPredictKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }
}
