//! Wires the masked-lane-select technique (#127's spike) through the harness.
//!
//! `vaco-codec-dsp-deblock`'s own per-edge filter decisions (and any future
//! separable-FIR edge handling) are exactly the shape masked-lane select
//! exists for: compute both outcomes unconditionally, then pick one per lane
//! from a comparison-derived mask. The spike this kernel backs measured that
//! `fearless_simd` already provides that operation natively — there is no
//! composition gap in `vaco_simd::ops` to fill, only a thin pass-through
//! ([`vaco_simd::ops::simd::select_u8`]) and a dispatched row entry point
//! ([`vaco_simd::ops::dispatched_select_u8_row`], used below) — so what needs
//! checking is not "does a composition match a scalar model", it is "does the
//! native op agree with the branchy scalar formula it replaces, including at
//! every vector-width tail this crate's `edge` module knows about."
//!
//! See `crates/core/vaco-simd/benches/adoption.rs`'s `group_select` for the
//! paired performance measurement: on this host (aarch64/NEON) the native
//! `mask8x16::select` and a hand-composed `(m&a)|(!m&b)` bitwise blend
//! measure identically, both ~10x the branchy scalar loop. Prefer the native
//! op regardless: it needs no bit-pattern engineering when the mask already
//! comes from a comparison (`simd_gt`/`simd_eq`, which produce the same mask
//! type), and it costs nothing here.

use vaco_simd::Caps;
use vaco_simd::ops;

use crate::Kernel;
use crate::edge;

/// One case: a mask (any byte pattern; `0` is false, anything else is true —
/// see the module doc for why non-canonical input is still safe here) and
/// two equal-length operand vectors distinguishable enough that selecting
/// the wrong one is visible at every lane.
#[derive(Debug, Clone)]
pub struct SelectCase {
    mask: Vec<u8>,
    a: Vec<u8>,
    b: Vec<u8>,
}

/// [`Kernel`] adapter for the masked-lane-select primitive.
#[derive(Debug, Clone, Copy)]
pub struct MaskedSelectKernel;

impl Kernel for MaskedSelectKernel {
    const NAME: &'static str = "vaco-simd::ops::select_u8";

    type Case = SelectCase;
    type Lane = u8;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(1); // u8 lanes are one byte each.
        let mut cases = Vec::new();
        for len in edge::lengths_around(&widths) {
            let a: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
            let b: Vec<u8> = a.iter().rev().copied().collect();
            for mask in vaco_simd::testing::edge_patterns(len) {
                cases.push(Self::Case {
                    mask,
                    a: a.clone(),
                    b: b.clone(),
                });
            }
        }
        cases
    }

    fn benchmark_case() -> Option<Self::Case> {
        let len = 1024 * 1024;
        let a: Vec<u8> = (0..len).map(|index| (index & 0xff) as u8).collect();
        let b: Vec<u8> = a.iter().rev().copied().collect();
        let mask = (0..len)
            .map(|index| if index % 2 == 0 { 0 } else { 255 })
            .collect();
        Some(Self::Case { mask, a, b })
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        case.mask
            .iter()
            .zip(&case.a)
            .zip(&case.b)
            .map(|((&m, &a), &b)| ops::select_u8(m, a, b))
            .collect()
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0u8; case.mask.len()];
        ops::dispatched_select_u8_row(Caps::detect(), &case.mask, &case.a, &case.b, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn native_select_agrees_with_the_branchy_scalar_reference() {
        let report = Differential::<MaskedSelectKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }

    #[test]
    fn benchmark_case_is_large_enough_to_measure_the_kernel_not_dispatch() {
        let Some(case) = MaskedSelectKernel::benchmark_case() else {
            unreachable!("masked select has a benchmark case");
        };
        assert!(case.mask.len() >= 1024 * 1024);
        assert_eq!(
            MaskedSelectKernel::scalar(&case),
            MaskedSelectKernel::vector(&case)
        );
    }
}
