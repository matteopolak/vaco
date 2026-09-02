//! The VC-2 / Dirac wavelet filter family: seven reversible integer
//! lifting filters selected by `wavelet_index`, and the 2D multi-level
//! transform built from them.
//!
//! `Vaco-Spec-Ref`: the Dirac Specification, version 2.2.3, Tables 15.1
//! through 15.7 (the exact lifting steps, taps, offsets and shifts for
//! each `WAVELET_INDEX`) and §15.6.1 (the 2D interleave/synthesis
//! structure) -- transcribed from the primary specification text, not a
//! reference decoder's source (D6/D7). SMPTE ST 2042-1 (VC-2) is Dirac's
//! direct, near-identical successor and uses the same filter set and the
//! same `wavelet_index` numbering.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::lift::{LiftStep, StepKind, run_analysis, run_synthesis};

/// One of Dirac/VC-2's seven wavelet filters, in `wavelet_index` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveletKind {
    /// `wavelet_index == 0`: Deslauriers-Dubuc (9,7). VC-2's own default.
    DeslauriersDubuc9_7,
    /// `wavelet_index == 1`: `LeGall` (5,3) -- the same reversible filter
    /// JPEG 2000 uses for its own lossless path.
    LeGall5_3,
    /// `wavelet_index == 2`: Deslauriers-Dubuc (13,7).
    DeslauriersDubuc13_7,
    /// `wavelet_index == 3`: Haar, no shift.
    HaarNoShift,
    /// `wavelet_index == 4`: Haar, single shift per level.
    HaarSingleShift,
    /// `wavelet_index == 5`: the "Fidelity" filter (improved
    /// downconversion and anti-aliasing).
    Fidelity,
    /// `wavelet_index == 6`: an integer lifting approximation to
    /// Daubechies (9,7) -- distinct from JPEG 2000's own floating-point
    /// 9/7 filter ([`crate::cdf97`]), which this is a fixed-point stand-in
    /// for, not a transcription of.
    Daubechies9_7Integer,
}

const DD_9_7: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type2,
        taps: &[1, 1],
        offset: 0,
        shift: 2,
    },
    LiftStep {
        kind: StepKind::Type3,
        taps: &[-1, 9, 9, -1],
        offset: -1,
        shift: 4,
    },
];

const LEGALL_5_3: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type2,
        taps: &[1, 1],
        offset: 0,
        shift: 2,
    },
    LiftStep {
        kind: StepKind::Type3,
        taps: &[1, 1],
        offset: 0,
        shift: 1,
    },
];

const DD_13_7: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type2,
        taps: &[-1, 9, 9, -1],
        offset: -1,
        shift: 5,
    },
    LiftStep {
        kind: StepKind::Type3,
        taps: &[-1, 9, 9, -1],
        offset: -1,
        shift: 4,
    },
];

const HAAR: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type2,
        taps: &[1],
        offset: 1,
        shift: 1,
    },
    LiftStep {
        kind: StepKind::Type3,
        taps: &[1],
        offset: 0,
        shift: 0,
    },
];

const FIDELITY: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type3,
        taps: &[-2, 10, -25, 81, 81, -25, 10, -2],
        offset: -3,
        shift: 8,
    },
    LiftStep {
        kind: StepKind::Type2,
        taps: &[-8, 21, -46, 161, 161, -46, 21, -8],
        offset: -3,
        shift: 8,
    },
];

const DAUBECHIES_9_7_INT: &[LiftStep] = &[
    LiftStep {
        kind: StepKind::Type2,
        taps: &[1817, 1817],
        offset: 0,
        shift: 12,
    },
    LiftStep {
        kind: StepKind::Type4,
        taps: &[3616, 3616],
        offset: 0,
        shift: 12,
    },
    LiftStep {
        kind: StepKind::Type1,
        taps: &[217, 217],
        offset: 0,
        shift: 12,
    },
    LiftStep {
        kind: StepKind::Type3,
        taps: &[6497, 6497],
        offset: 0,
        shift: 12,
    },
];

impl WaveletKind {
    /// Table 15.1-15.7's own lifting-step sequence for this filter, in
    /// synthesis (decode-direction) order.
    #[must_use]
    pub const fn steps(self) -> &'static [LiftStep] {
        match self {
            Self::DeslauriersDubuc9_7 => DD_9_7,
            Self::LeGall5_3 => LEGALL_5_3,
            Self::DeslauriersDubuc13_7 => DD_13_7,
            Self::HaarNoShift | Self::HaarSingleShift => HAAR,
            Self::Fidelity => FIDELITY,
            Self::Daubechies9_7Integer => DAUBECHIES_9_7_INT,
        }
    }

    /// `filtershift()`: the per-level accuracy-bit shift Table 15.1-15.7
    /// each name alongside their own lifting steps.
    #[must_use]
    pub const fn filter_shift(self) -> u32 {
        match self {
            Self::DeslauriersDubuc9_7
            | Self::LeGall5_3
            | Self::DeslauriersDubuc13_7
            | Self::HaarSingleShift
            | Self::Daubechies9_7Integer => 1,
            Self::HaarNoShift | Self::Fidelity => 0,
        }
    }
}

#[cold]
fn err_dimensions() -> Error {
    Error::InvalidData(
        "vaco-codec-dsp-dwt: width/height must be non-zero and evenly divisible by 2^levels",
    )
}

#[cold]
fn err_buffer_len() -> Error {
    Error::InvalidData("vaco-codec-dsp-dwt: data buffer is smaller than width * height")
}

/// Check `width`/`height`/`levels` are consistent (both dimensions evenly
/// halve `levels` times) and `data` is large enough, once, before any
/// transform work runs.
fn check_shape(data_len: usize, width: usize, height: usize, levels: u32) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(err_dimensions());
    }
    let divisor = 1usize.checked_shl(levels).ok_or_else(err_dimensions)?;
    if !width.is_multiple_of(divisor) || !height.is_multiple_of(divisor) {
        return Err(err_dimensions());
    }
    if data_len < width.saturating_mul(height) {
        return Err(err_buffer_len());
    }
    Ok(())
}

/// Filter every row of the `w x h` region (top-left of `data`, row-major,
/// stride `stride`) with 1D analysis or synthesis.
#[allow(
    clippy::indexing_slicing,
    reason = "y bounded by h, checked once by check_shape before any level runs"
)]
fn filter_rows(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    steps: &[LiftStep],
    analysis: bool,
) -> Result<()> {
    for y in 0..h {
        let row = data
            .get_mut(y * stride..y * stride + w)
            .ok_or_else(err_buffer_len)?;
        if analysis {
            run_analysis(row, steps)?;
        } else {
            run_synthesis(row, steps)?;
        }
    }
    Ok(())
}

/// Filter every column of the `w x h` region with 1D analysis or
/// synthesis, gathering into contiguous `scratch` and scattering back
/// (a column is not contiguous in a row-major buffer).
#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "x/y bounded by w/h, checked once by check_shape before any level runs"
)]
fn filter_cols(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    steps: &[LiftStep],
    scratch: &mut [i32],
    analysis: bool,
) -> Result<()> {
    let col = scratch.get_mut(..h).ok_or_else(err_buffer_len)?;
    for x in 0..w {
        for y in 0..h {
            col[y] = *data.get(y * stride + x).ok_or_else(err_buffer_len)?;
        }
        if analysis {
            run_analysis(col, steps)?;
        } else {
            run_synthesis(col, steps)?;
        }
        for y in 0..h {
            *data.get_mut(y * stride + x).ok_or_else(err_buffer_len)? = col[y];
        }
    }
    Ok(())
}

/// One level's own separable 1D filtering, in the direction the 2D
/// transform requires: analysis (forward) filters rows then columns;
/// synthesis (inverse) undoes that in the opposite order -- columns then
/// rows -- exactly mirroring the Dirac Specification's own §15.6.1
/// `vh_synth` ("1D synthesis on every column, then on every row").
/// Filtering both axes in the *same* order for both directions is not a
/// harmless simplification: it silently breaks exact round-trip, since a
/// 2D separable transform's inverse must undo its two 1D passes in
/// reverse order, not the same order.
fn filter_rows_then_cols(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    steps: &[LiftStep],
    scratch: &mut [i32],
    analysis: bool,
) -> Result<()> {
    if analysis {
        filter_rows(data, stride, w, h, steps, true)?;
        filter_cols(data, stride, w, h, steps, scratch, true)?;
    } else {
        filter_cols(data, stride, w, h, steps, scratch, false)?;
        filter_rows(data, stride, w, h, steps, false)?;
    }
    Ok(())
}

/// Interleave four `hw x hh` quadrants (`LL`/`HL`/`LH`/`HH`, each already
/// in place at the corners of the `w x h` region) into the checkerboard
/// layout §15.6.1's own synthesis interleave step reads: `LL` at
/// `(2y, 2x)`, `HL` at `(2y, 2x+1)`, `LH` at `(2y+1, 2x)`, `HH` at
/// `(2y+1, 2x+1)`.
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "y/x bounded by hh/hw, both <= w/h; scratch sized to w*h by the caller; w/h are checked even at every level by check_shape so w/2, h/2 are exact"
)]
fn interleave(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    scratch: &mut [i32],
) -> Result<()> {
    let (hw, hh) = (w / 2, h / 2);
    let out = scratch.get_mut(..w * h).ok_or_else(err_buffer_len)?;
    for y in 0..hh {
        for x in 0..hw {
            let ll = *data.get(y * stride + x).ok_or_else(err_buffer_len)?;
            let hl = *data.get(y * stride + hw + x).ok_or_else(err_buffer_len)?;
            let lh = *data.get((hh + y) * stride + x).ok_or_else(err_buffer_len)?;
            let hh_ = *data
                .get((hh + y) * stride + hw + x)
                .ok_or_else(err_buffer_len)?;
            out[(2 * y) * w + 2 * x] = ll;
            out[(2 * y) * w + 2 * x + 1] = hl;
            out[(2 * y + 1) * w + 2 * x] = lh;
            out[(2 * y + 1) * w + 2 * x + 1] = hh_;
        }
    }
    for y in 0..h {
        let dst = data
            .get_mut(y * stride..y * stride + w)
            .ok_or_else(err_buffer_len)?;
        let src = out.get(y * w..y * w + w).ok_or_else(err_buffer_len)?;
        dst.copy_from_slice(src);
    }
    Ok(())
}

/// The exact inverse of [`interleave`]: split the checkerboard back into
/// four `hw x hh` quadrants at the corners of the `w x h` region.
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "see interleave's own identical reason"
)]
fn deinterleave(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    scratch: &mut [i32],
) -> Result<()> {
    let (hw, hh) = (w / 2, h / 2);
    let out = scratch.get_mut(..w * h).ok_or_else(err_buffer_len)?;
    for y in 0..hh {
        for x in 0..hw {
            let ll = *data
                .get((2 * y) * stride + 2 * x)
                .ok_or_else(err_buffer_len)?;
            let hl = *data
                .get((2 * y) * stride + 2 * x + 1)
                .ok_or_else(err_buffer_len)?;
            let lh = *data
                .get((2 * y + 1) * stride + 2 * x)
                .ok_or_else(err_buffer_len)?;
            let hh_ = *data
                .get((2 * y + 1) * stride + 2 * x + 1)
                .ok_or_else(err_buffer_len)?;
            out[y * w + x] = ll;
            out[y * w + hw + x] = hl;
            out[(hh + y) * w + x] = lh;
            out[(hh + y) * w + hw + x] = hh_;
        }
    }
    for y in 0..h {
        let dst = data
            .get_mut(y * stride..y * stride + w)
            .ok_or_else(err_buffer_len)?;
        let src = out.get(y * w..y * w + w).ok_or_else(err_buffer_len)?;
        dst.copy_from_slice(src);
    }
    Ok(())
}

#[allow(
    clippy::indexing_slicing,
    reason = "y/x bounded by h/w, checked by check_shape before this runs"
)]
fn shift_region(
    data: &mut [i32],
    stride: usize,
    w: usize,
    h: usize,
    shift: u32,
    left: bool,
) -> Result<()> {
    if shift == 0 {
        return Ok(());
    }
    for y in 0..h {
        let row = data
            .get_mut(y * stride..y * stride + w)
            .ok_or_else(err_buffer_len)?;
        for v in row.iter_mut() {
            *v = if left {
                v.wrapping_shl(shift)
            } else {
                (*v + (1i32 << (shift - 1))) >> shift
            };
        }
    }
    Ok(())
}

/// Inverse (synthesis) 2D multi-level wavelet transform, in place: `data`
/// (row-major, `width * height`, stride `width`) holds `levels` nested
/// nested decomposition levels, coarsest `LL` band innermost at the
/// top-left, in the packed single-buffer layout this crate uses (see the
/// module doc's own note on why this differs from Dirac's own per-
/// subband-array bitstream storage without changing the transform math).
///
/// # Errors
///
/// [`Error::InvalidData`] if `width`/`height` are not both evenly
/// divisible by `2^levels`, or `data` is shorter than `width * height`;
/// as [`Budget::alloc`] if the scratch allocation is refused.
pub fn idwt_2d(
    data: &mut [i32],
    width: usize,
    height: usize,
    kind: WaveletKind,
    levels: u32,
    budget: &mut Budget,
) -> Result<()> {
    check_shape(data.len(), width, height, levels)?;
    let mut scratch: Vec<i32> = budget.alloc(width.max(height).max(width * height))?;
    let steps = kind.steps();
    let shift = kind.filter_shift();
    // Coarsest level first: the smallest region grows outward.
    for level in (0..levels).rev() {
        let w = width >> level;
        let h = height >> level;
        interleave(data, width, w, h, &mut scratch)?;
        filter_rows_then_cols(data, width, w, h, steps, &mut scratch, false)?;
        shift_region(data, width, w, h, shift, false)?;
    }
    Ok(())
}

/// Forward (analysis) 2D multi-level wavelet transform, in place -- the
/// exact inverse of [`idwt_2d`] for the same `kind`/`levels` (checked
/// directly by this crate's own round-trip property tests, not merely
/// argued from the lifting-scheme theory in [`crate::lift`]'s own doc).
///
/// # Errors
///
/// As [`idwt_2d`].
pub fn dwt_2d(
    data: &mut [i32],
    width: usize,
    height: usize,
    kind: WaveletKind,
    levels: u32,
    budget: &mut Budget,
) -> Result<()> {
    check_shape(data.len(), width, height, levels)?;
    let mut scratch: Vec<i32> = budget.alloc(width.max(height).max(width * height))?;
    let steps = kind.steps();
    let shift = kind.filter_shift();
    // Finest level first: the whole picture shrinks inward.
    for level in 0..levels {
        let w = width >> level;
        let h = height >> level;
        shift_region(data, width, w, h, shift, true)?;
        filter_rows_then_cols(data, width, w, h, steps, &mut scratch, true)?;
        deinterleave(data, width, w, h, &mut scratch)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    const ALL_KINDS: [WaveletKind; 7] = [
        WaveletKind::DeslauriersDubuc9_7,
        WaveletKind::LeGall5_3,
        WaveletKind::DeslauriersDubuc13_7,
        WaveletKind::HaarNoShift,
        WaveletKind::HaarSingleShift,
        WaveletKind::Fidelity,
        WaveletKind::Daubechies9_7Integer,
    ];

    #[test]
    fn every_filter_round_trips_a_fixed_pattern_exactly() {
        let (w, h) = (16usize, 16usize);
        for kind in ALL_KINDS {
            let original: Vec<i32> = (0..w * h)
                .map(|i| i32::try_from((i * 37 + 11) % 251).unwrap_or(0) - 120)
                .collect();
            let mut a = original.clone();
            let mut budget = Budget::new(Limits::default());
            dwt_2d(&mut a, w, h, kind, 2, &mut budget).unwrap();
            assert_ne!(
                a, original,
                "{kind:?}: the transform must actually change the data"
            );
            idwt_2d(&mut a, w, h, kind, 2, &mut budget).unwrap();
            assert_eq!(
                a, original,
                "{kind:?}: forward-then-inverse must round-trip exactly"
            );
        }
    }

    #[test]
    fn wrong_shape_is_refused_not_panicked() {
        let mut budget = Budget::new(Limits::default());
        let mut a = vec![0i32; 15 * 16];
        assert!(idwt_2d(&mut a, 15, 16, WaveletKind::LeGall5_3, 1, &mut budget).is_err());
        let mut b = vec![0i32; 16 * 16];
        assert!(idwt_2d(&mut b, 16, 16, WaveletKind::LeGall5_3, 5, &mut budget).is_err());
    }

    fn kind_from_index(i: u8) -> WaveletKind {
        ALL_KINDS[usize::from(i) % ALL_KINDS.len()]
    }

    proptest::proptest! {
        /// The property the coordinator asked for explicitly: forward-
        /// then-inverse round-trips **exactly** (not just "close"), for
        /// every integer VC-2 filter, over random input and a random
        /// point in the size/levels domain -- not merely the one fixed
        /// pattern [`every_filter_round_trips_a_fixed_pattern_exactly`]
        /// checks.
        #[test]
        fn every_filter_round_trips_random_input_exactly(
            kind_idx in 0u8..7,
            level_pow in 0u32..3,
            w_mult in 1usize..6,
            h_mult in 1usize..6,
            seed in proptest::collection::vec(-30000i32..30000, 4..64),
        ) {
            let kind = kind_from_index(kind_idx);
            let levels = level_pow + 1; // 1..=3
            let divisor = 1usize << levels;
            let w = w_mult * divisor;
            let h = h_mult * divisor;
            let n = w * h;
            let original: Vec<i32> = (0..n).map(|i| seed[i % seed.len()]).collect();
            let mut a = original.clone();
            let mut budget = Budget::new(Limits::default());
            dwt_2d(&mut a, w, h, kind, levels, &mut budget).unwrap();
            idwt_2d(&mut a, w, h, kind, levels, &mut budget).unwrap();
            proptest::prop_assert_eq!(a, original, "{:?} levels={} w={} h={}", kind, levels, w, h);
        }
    }
}
