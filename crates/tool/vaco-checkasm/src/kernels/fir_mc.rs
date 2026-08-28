//! Wires the const-generic separable FIR motion-compensation engine (#23,
//! D-08a) through the harness.
//!
//! `vaco_codec_dsp_mc::fir::fir_row` is one dispatched, level-generic body
//! monomorphised per tap count via `const N: usize`; this checks the actual
//! tap set every consumer is expected to reach for first
//! (`taps::H264_LUMA_HALFPEL`, ITU-T H.264 §8.4.2.2.1) against
//! `fir_row_scalar`'s reference across every vector-width tail this crate's
//! `edge` module knows about.

use vaco_codec_dsp_mc::fir::{self, taps};
use vaco_simd::Caps;

use crate::Kernel;
use crate::edge;

/// One case: a source row long enough to produce `dst_len` output samples
/// through the six-tap filter (`dst_len + 5` source samples).
#[derive(Debug, Clone)]
pub struct FirCase {
    src: Vec<u8>,
    dst_len: usize,
}

/// [`Kernel`] adapter for [`fir::fir_row`] at H.264's six-tap luma half-pel
/// coefficients.
#[derive(Debug, Clone, Copy)]
pub struct FirMcKernel;

impl Kernel for FirMcKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::fir_row (h264 6-tap)";

    type Case = FirCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(1);
        edge::lengths_around(&widths)
            .into_iter()
            .map(|dst_len| FirCase {
                src: (0..dst_len + 5).map(|i| ((i * 53) & 0xFF) as u8).collect(),
                dst_len,
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        fir::fir_row_scalar(&case.src, &taps::H264_LUMA_HALFPEL, case.dst_len)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0u8; case.dst_len];
        fir::fir_row(Caps::detect(), &case.src, &taps::H264_LUMA_HALFPEL, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn dispatched_fir_agrees_with_the_scalar_reference() {
        let report = Differential::<FirMcKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }
}
