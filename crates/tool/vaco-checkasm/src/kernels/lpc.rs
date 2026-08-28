//! Wires `vaco-codec-dsp-lpc`'s dispatched [`autocorrelate`
//! variant](vaco_codec_dsp_lpc::simd::autocorrelate) through the harness
//! (D-07, #257).
//!
//! Autocorrelation's vector lanes accumulate independently and are combined
//! at the end, which is a different summation order than the scalar
//! reference's strict left-to-right loop — floating-point addition is not
//! associative, so this is not expected to be bit-identical the way an
//! integer or a single-operation float kernel (see `kernels::fmtconvert`)
//! is. [`Kernel::lanes_match`] is overridden to a relative tolerance tight
//! enough to still catch a real divergence: `1e-9` relative error is far
//! below what reassociation alone produces on the corpus below (double
//! precision, sums over at most a few hundred terms) and far above what an
//! actually wrong lag, a dropped term or a swapped operand produces (which
//! is the whole answer being wrong, not the fifteenth digit).

use vaco_codec_dsp_lpc::simd;
use vaco_simd::Caps;

use crate::Kernel;
use crate::edge;

/// One case: a sample block and how many lags to compute.
#[derive(Debug, Clone)]
pub struct AutocorrelateCase {
    samples: Vec<f64>,
    max_lag: usize,
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_lpc::simd::autocorrelate`].
#[derive(Debug, Clone, Copy)]
pub struct AutocorrelateKernel;

impl Kernel for AutocorrelateKernel {
    const NAME: &'static str = "vaco-codec-dsp-lpc::autocorrelate";

    type Case = AutocorrelateCase;
    type Lane = f64;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(8); // f64 lanes are eight bytes each.
        let lags = [0usize, 1, 4, 8, 20, 32];
        let mut cases = Vec::new();
        for len in edge::lengths_around(&widths) {
            let sine: Vec<f64> = (0..len).map(|i| (i as f64 * 0.17).sin() * 1234.5).collect();
            let ramp: Vec<f64> = (0..len).map(|i| i as f64 * 3.0 - 7.0).collect();
            let silence = vec![0.0f64; len];
            for &max_lag in &lags {
                for samples in [&sine, &ramp, &silence] {
                    cases.push(AutocorrelateCase {
                        samples: samples.clone(),
                        max_lag,
                    });
                }
            }
        }
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0; case.max_lag + 1];
        vaco_codec_dsp_lpc::autocorrelate(&case.samples, &mut out);
        out
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0; case.max_lag + 1];
        simd::autocorrelate(Caps::detect(), &case.samples, &mut out);
        out
    }

    fn lanes_match(a: &Self::Lane, b: &Self::Lane) -> bool {
        let scale = a.abs().max(b.abs()).max(1.0);
        (a - b).abs() <= 1e-9 * scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn autocorrelate_dispatched_agrees_with_scalar() {
        let report = Differential::<AutocorrelateKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }
}
