//! The four reversible integer lifting operations SMPTE ST 2042-1 (Dirac /
//! VC-2) §15.6.2 defines, and their exact inverses.
//!
//! `Vaco-Spec-Ref`: the Dirac Specification, version 2.2.3 (BBC, 2008;
//! SMPTE ST 2042-1's direct ancestor, itself the openly published royalty-
//! free specification these constants come from -- not `FFmpeg`, not any
//! other reference decoder's source, per D6/D7), §15.6.2 "One-dimensional
//! synthesis" and §15.6.2.1 "Mathematical formulation of lifting processes".
//!
//! # The four types
//!
//! A one-dimensional lifting filter modifies one parity class of a sequence
//! (even or odd indices) using a weighted sum of the *other*, unmodified
//! parity class. Four combinations of "which parity is modified" and
//! "add or subtract" cover every filter in this crate:
//!
//! | type | modifies | reads | operation |
//! |---|---|---|---|
//! | 1 | `A[2n]`   | odd  | `+=` |
//! | 2 | `A[2n]`   | odd  | `-=` |
//! | 3 | `A[2n+1]` | even | `+=` |
//! | 4 | `A[2n+1]` | even | `-=` |
//!
//! Because a step never reads the parity class it writes, undoing it is
//! exact and trivial: Type 1's own inverse is Type 2 with identical taps
//! (and vice versa), Type 3's is Type 4 (and vice versa). A **synthesis**
//! transform is a fixed sequence of these steps in a fixed order; the
//! matching **analysis** transform is the inverse of each step, applied in
//! the *reverse* order -- standard lifting-scheme inversion, not specific
//! to this crate, and what makes the round trip in this module's own
//! property tests exact rather than merely close.

use vaco_core::{Error, Result};

/// One lifting step: which of the four combinations above, its taps, the
/// tap-index offset `D` (can be negative -- a filter reads samples on both
/// sides of the position it modifies), and the post-sum right-shift `S`.
#[derive(Debug, Clone, Copy)]
pub struct LiftStep {
    pub kind: StepKind,
    pub taps: &'static [i32],
    pub offset: i32,
    pub shift: u32,
}

/// Which of the four lifting operations a [`LiftStep`] performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// `A[2n] += round(Σ taps[i] * A[2(n+i)+1]) >> S`
    Type1,
    /// `A[2n] -= round(Σ taps[i] * A[2(n+i)+1]) >> S`
    Type2,
    /// `A[2n+1] += round(Σ taps[i] * A[2(n+i)]) >> S`
    Type3,
    /// `A[2n+1] -= round(Σ taps[i] * A[2(n+i)]) >> S`
    Type4,
}

impl StepKind {
    /// The operation that exactly undoes this one, given the same taps,
    /// offset and shift -- see this module's own doc for why swapping
    /// `Type1`/`Type2` and `Type3`/`Type4` is exact, not approximate.
    #[must_use]
    const fn inverse(self) -> Self {
        match self {
            Self::Type1 => Self::Type2,
            Self::Type2 => Self::Type1,
            Self::Type3 => Self::Type4,
            Self::Type4 => Self::Type3,
        }
    }

    const fn writes_even(self) -> bool {
        matches!(self, Self::Type1 | Self::Type2)
    }

    const fn adds(self) -> bool {
        matches!(self, Self::Type1 | Self::Type3)
    }
}

impl LiftStep {
    /// This step with `kind` replaced by its own exact inverse.
    #[must_use]
    pub const fn inverted(self) -> Self {
        Self { kind: self.kind.inverse(), ..self }
    }
}

/// A one-dimensional array's length must be even and at least `2` for any
/// lifting step to have a well-defined half-length -- clause 15.6.2's own
/// scope ("a 1-dimensional array of coefficients of even length").
fn check_even_length(len: usize) -> Result<()> {
    if len < 2 || !len.is_multiple_of(2) {
        return Err(err_odd_length());
    }
    Ok(())
}

/// D21: error construction kept off the hot path -- a `#[cold]` function
/// the caller only ever branches to, never executes, on well-formed input.
#[cold]
fn err_odd_length() -> Error {
    Error::InvalidData("vaco-codec-dsp-dwt: a 1D lifting array must have even length >= 2")
}

#[cold]
fn err_out_of_range() -> Error {
    Error::InvalidData("vaco-codec-dsp-dwt: lifting read/write position out of range")
}

/// Apply one [`LiftStep`] to `a` in place, clause 15.6.2's own edge
/// extension (clamping the read position rather than a separate padding
/// pass -- exact per the specification, not an approximation of it).
///
/// # Errors
///
/// [`Error::InvalidData`] if `a`'s length is not even and at least 2.
#[allow(
    clippy::integer_division,
    reason = "len is checked even above, so len / 2 is exact -- the half-length of a lifting array, not a lossy approximation"
)]
pub fn apply_step(a: &mut [i32], step: &LiftStep) -> Result<()> {
    check_even_length(a.len())?;
    let len = a.len();
    let half = len / 2;
    let bias: i64 = if step.shift > 0 { 1i64 << (step.shift - 1) } else { 0 };
    for n in 0..half {
        let n = i32::try_from(n).unwrap_or(i32::MAX);
        let mut sum: i64 = 0;
        for (k, &tap) in step.taps.iter().enumerate() {
            let i = step.offset + i32::try_from(k).unwrap_or(0);
            let raw = if step.kind.writes_even() {
                // Type 1/2 read the odd parity: position 2*(n+i) - 1,
                // clamped to [1, len - 1] (the odd samples nearest each
                // edge -- clause 15.6.2.1's own note on why this differs
                // from Type 3/4's clamp).
                2 * (n + i) - 1
            } else {
                // Type 3/4 read the even parity: position 2*(n+i),
                // clamped to [0, len - 2].
                2 * (n + i)
            };
            let pos = clamp_read_pos(raw, len, step.kind.writes_even());
            let sample = *a.get(pos).ok_or_else(err_out_of_range)?;
            sum += i64::from(tap) * i64::from(sample);
        }
        sum += bias;
        let delta = i32::try_from(sum >> step.shift).unwrap_or(if sum < 0 { i32::MIN } else { i32::MAX });
        let n_usize = usize::try_from(n).unwrap_or(usize::MAX);
        let idx = if step.kind.writes_even() { 2 * n_usize } else { 2 * n_usize + 1 };
        let slot = a.get_mut(idx).ok_or_else(err_out_of_range)?;
        *slot = if step.kind.adds() { slot.wrapping_add(delta) } else { slot.wrapping_sub(delta) };
    }
    Ok(())
}

/// Clamp a computed read position to the nearest in-range sample of the
/// correct parity -- clause 15.6.2.1: "even values and odd values must be
/// extended separately to maintain the correct phase (and hence
/// invertibility) of the filter".
fn clamp_read_pos(raw: i32, len: usize, odd_parity: bool) -> usize {
    let last = i32::try_from(len).unwrap_or(i32::MAX) - 1;
    if odd_parity {
        raw.clamp(1, last.max(1)) as usize
    } else {
        raw.clamp(0, (last - 1).max(0)) as usize
    }
}

/// Run a **synthesis** (inverse-transform) sequence of steps in the order
/// given -- clause 15.6.2's own `1d_synthesis`.
///
/// # Errors
///
/// As [`apply_step`].
pub fn run_synthesis(a: &mut [i32], steps: &[LiftStep]) -> Result<()> {
    for step in steps {
        apply_step(a, step)?;
    }
    Ok(())
}

/// Run the matching **analysis** (forward-transform) sequence for a
/// synthesis defined by `steps`: each step's exact inverse, in reverse
/// order -- see this module's own doc for why that is exact rather than
/// approximate for a lifting scheme.
///
/// # Errors
///
/// As [`apply_step`].
pub fn run_analysis(a: &mut [i32], steps: &[LiftStep]) -> Result<()> {
    for step in steps.iter().rev() {
        apply_step(a, &step.inverted())?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_single_step_is_undone_by_its_own_inverse() {
        let step = LiftStep { kind: StepKind::Type2, taps: &[1, 1], offset: 0, shift: 2 };
        let original: Vec<i32> = (0..16i32).map(|i| i * 3 - 7).collect();
        let mut a = original.clone();
        apply_step(&mut a, &step).unwrap();
        assert_ne!(a, original, "the step must actually have changed something");
        apply_step(&mut a, &step.inverted()).unwrap();
        assert_eq!(a, original);
    }

    #[test]
    fn odd_length_is_refused() {
        let step = LiftStep { kind: StepKind::Type1, taps: &[1], offset: 0, shift: 0 };
        let mut a = vec![1, 2, 3];
        assert!(apply_step(&mut a, &step).is_err());
    }
}


