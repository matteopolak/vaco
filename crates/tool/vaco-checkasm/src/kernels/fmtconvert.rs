//! Wires `vaco-codec-dsp-fmtconvert`'s two exact widen-and-convert SIMD
//! variants (D-05, #122) through the harness.
//!
//! Both `int16_to_float` and `int32_to_float` are bit-exact reformulations
//! (a sign-extending widen and a single IEEE-754 round-to-nearest convert,
//! the same operation the scalar reference performs), so this exercises
//! every vector-width tail this crate's `edge` module knows about plus
//! `i16`/`i32` saturation-boundary values, and expects a byte-for-byte
//! match — there is no rounding daylight to allow, unlike a kernel that
//! legitimately needs a tolerance.

use vaco_codec_dsp_fmtconvert::simd;
use vaco_simd::Caps;

use crate::Kernel;
use crate::edge;

/// One case: an `i16` source row of some length worth covering.
#[derive(Debug, Clone)]
pub struct Int16Case {
    src: Vec<i16>,
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_fmtconvert::simd::int16_to_float`].
#[derive(Debug, Clone, Copy)]
pub struct Int16ToFloatKernel;

impl Kernel for Int16ToFloatKernel {
    const NAME: &'static str = "vaco-codec-dsp-fmtconvert::int16_to_float";

    type Case = Int16Case;
    type Lane = f32;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(2); // i16 lanes are two bytes each.
        let mut cases: Vec<Self::Case> = edge::lengths_around(&widths)
            .into_iter()
            .map(|len| Int16Case {
                src: (0..len)
                    .map(|i| {
                        let v = i32::try_from((i * 6551) % 65536).unwrap_or(0) - 32768;
                        i16::try_from(v).unwrap_or(0)
                    })
                    .collect(),
            })
            .collect();
        // Every i16 boundary value, on its own and packed together, so a
        // tail iteration that only ever sees "ordinary" ramp values cannot
        // hide a saturation-adjacent bug.
        for &b in &edge::boundaries_i16() {
            cases.push(Int16Case { src: vec![b] });
        }
        cases.push(Int16Case {
            src: edge::boundaries_i16(),
        });
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0f32; case.src.len()];
        vaco_codec_dsp_fmtconvert::int16_to_float(&mut out, &case.src);
        out
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0f32; case.src.len()];
        simd::int16_to_float(Caps::detect(), &case.src, &mut out);
        out
    }
}

/// One case: an `i32` source row of some length worth covering.
#[derive(Debug, Clone)]
pub struct Int32Case {
    src: Vec<i32>,
}

/// [`Kernel`] adapter for [`vaco_codec_dsp_fmtconvert::simd::int32_to_float`].
#[derive(Debug, Clone, Copy)]
pub struct Int32ToFloatKernel;

impl Kernel for Int32ToFloatKernel {
    const NAME: &'static str = "vaco-codec-dsp-fmtconvert::int32_to_float";

    type Case = Int32Case;
    type Lane = f32;

    fn cases() -> Vec<Self::Case> {
        let widths = edge::element_widths(4); // i32 lanes are four bytes each.
        let mut cases: Vec<Self::Case> = edge::lengths_around(&widths)
            .into_iter()
            .map(|len| Int32Case {
                src: (0..len)
                    .map(|i| {
                        let i = i64::try_from(i).unwrap_or(0);
                        i32::try_from((i * 104_729 - 5_000_000_000).clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
                    })
                    .collect(),
            })
            .collect();
        for &b in &edge::boundaries_i32() {
            cases.push(Int32Case { src: vec![b] });
        }
        cases.push(Int32Case {
            src: edge::boundaries_i32(),
        });
        cases
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0f32; case.src.len()];
        vaco_codec_dsp_fmtconvert::int32_to_float(&mut out, &case.src);
        out
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        let mut out = vec![0.0f32; case.src.len()];
        simd::int32_to_float(Caps::detect(), &case.src, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Differential;

    #[test]
    fn int16_to_float_dispatched_agrees_with_scalar() {
        let report = Differential::<Int16ToFloatKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }

    #[test]
    fn int32_to_float_dispatched_agrees_with_scalar() {
        let report = Differential::<Int32ToFloatKernel>::run();
        assert!(report.cases_run() > 0, "the corpus must not be empty");
        report.assert_clean();
    }
}
