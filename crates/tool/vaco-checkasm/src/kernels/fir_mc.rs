//! Wires the const-generic separable FIR motion-compensation engine (#23,
//! D-08a) through the harness.
//!
//! `vaco_codec_dsp_mc::fir::fir_row` is one dispatched, level-generic body
//! monomorphised per tap count via `const N: usize`; this checks every tap set
//! shipped by the crate (`BILINEAR` and H.264's six-tap luma filter) against
//! `fir_row_scalar` across every available tier and vector-width tail.

use vaco_codec_dsp_mc::fir::{self, taps};
use vaco_simd::{Caps, Tier};

use crate::Kernel;
use crate::edge;
use crate::kernels::h264_mc::available_tiers;

#[derive(Debug, Clone, Copy)]
enum TapKind {
    Bilinear,
    H264Luma,
}

/// One case: a source row long enough to produce `dst_len` output samples
/// through the six-tap filter (`dst_len + 5` source samples).
#[derive(Debug, Clone)]
pub struct FirCase {
    src: Vec<u8>,
    dst_len: usize,
    tier: Tier,
    taps: TapKind,
}

/// [`Kernel`] adapter for [`fir::fir_row`] at H.264's six-tap luma half-pel
/// coefficients.
#[derive(Debug, Clone, Copy)]
pub struct FirMcKernel;

impl Kernel for FirMcKernel {
    const NAME: &'static str = "vaco-codec-dsp-mc::fir_row (all tap sets)";

    type Case = FirCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(1);
        available_tiers()
            .into_iter()
            .flat_map(|tier| {
                edge::lengths_around(&widths)
                    .into_iter()
                    .flat_map(move |dst_len| {
                        [TapKind::Bilinear, TapKind::H264Luma]
                            .into_iter()
                            .map(move |taps| {
                                let halo = match taps {
                                    TapKind::Bilinear => 1,
                                    TapKind::H264Luma => 5,
                                };
                                FirCase {
                                    src: (0..dst_len + halo)
                                        .map(|i| ((i * 53) & 0xFF) as u8)
                                        .collect(),
                                    dst_len,
                                    tier,
                                    taps,
                                }
                            })
                    })
            })
            .collect()
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        match case.taps {
            TapKind::Bilinear => fir::fir_row_scalar(&case.src, &taps::BILINEAR, case.dst_len),
            TapKind::H264Luma => {
                fir::fir_row_scalar(&case.src, &taps::H264_LUMA_HALFPEL, case.dst_len)
            }
        }
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        if case.tier.is_scalar() {
            return Self::scalar(case);
        }
        let Some(caps) = Caps::detect().capped_at(case.tier) else {
            return Self::scalar(case);
        };
        let mut out = vec![0u8; case.dst_len];
        match case.taps {
            TapKind::Bilinear => fir::fir_row(caps, &case.src, &taps::BILINEAR, &mut out),
            TapKind::H264Luma => {
                fir::fir_row(caps, &case.src, &taps::H264_LUMA_HALFPEL, &mut out);
            }
        }
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
