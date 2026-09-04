//! Wires `vaco-scale`'s generic and fixed-width vertical filters through the
//! differential and benchmark harness.
//!
//! This is a specialization check rather than a SIMD-tier check: the harness's
//! legacy `scalar` label means the generic tap-major production callee here,
//! while `vector` means the output-major fixed-width production callee. The
//! default-off `vaco-scale/checkasm` feature exposes an opaque case adapter so
//! neither private filter function becomes part of the normal public API.

use vaco_scale::exec::checkasm::{FilterVCase, run_fixed, run_generic};

use crate::Kernel;
use crate::edge;

/// Differential and benchmark adapter for the vertical-filter specialization.
#[derive(Debug, Clone, Copy)]
pub struct ScaleFilterVKernel;

impl Kernel for ScaleFilterVKernel {
    const NAME: &'static str = "vaco-scale::filter_v_generic_vs_fixed";

    type Case = FilterVCase;
    type Lane = i32;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(4);
        [2, 4, 6, 8]
            .into_iter()
            .flat_map(|taps| {
                edge::lengths_around(&widths)
                    .into_iter()
                    .flat_map(move |width| {
                        [1, 2, 3]
                            .into_iter()
                            .filter_map(move |rows| FilterVCase::synthetic(taps, width, rows))
                    })
            })
            .collect()
    }

    fn benchmark_case() -> Option<Self::Case> {
        FilterVCase::synthetic(8, 1920, 1080)
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        run_generic(case)
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        run_fixed(case)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn every_fixed_tap_count_agrees_with_the_generic_oracle() {
        let cases = ScaleFilterVKernel::cases();
        assert!(!cases.is_empty(), "the corpus must not be empty");
        for taps in [2, 4, 6, 8] {
            assert!(
                cases.iter().any(|case| case.taps() == taps),
                "tap count {taps} must be exercised"
            );
        }
        Differential::<ScaleFilterVKernel>::run().assert_clean();
    }

    #[test]
    fn production_case_is_complete_and_exact() {
        let case = ScaleFilterVKernel::benchmark_case();
        assert!(case.is_some(), "the production benchmark case must build");
        let Some(case) = case else {
            return;
        };
        assert_eq!(case.output_len(), 1920 * 1080 + 1);
        let generic = ScaleFilterVKernel::scalar(&case);
        let fixed = ScaleFilterVKernel::vector(&case);
        assert_eq!(generic.len(), case.output_len());
        assert!(
            fixed == generic,
            "production generic/fixed outputs diverged"
        );
    }
}
