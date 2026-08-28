//! MPEG-1/2-style motion vector prediction (D-22c): each direction's motion
//! vector is delta-coded against the *immediately preceding coded
//! macroblock's own reconstructed vector*, not a spatial median (see
//! [`crate::median_mv`] for the H.263-family scheme, which is a genuinely
//! different mechanism, not a variant of this one).
//!
//! `Vaco-Spec-Ref: itu-t-h262-199502 §7.6.3.1` — the predictor `PMV[r][s][t]`
//! carried forward macroblock to macroblock within a slice, reset to zero at
//! the start of every slice and whenever an intervening macroblock breaks
//! the prediction chain (an intra macroblock without concealment vectors, or
//! a skipped/no-motion macroblock in a P-picture — a family's macroblock
//! loop calls [`SequentialMvPredictor::reset`] at exactly those points, the
//! same rule `vaco-codec-mpeg12::macroblock` already applies).

/// Two motion vectors (`r = 0, 1`; `r = 1` is field-based prediction's
/// second vector, unused outside it), each with a horizontal and vertical
/// half-pel component, tracked independently per direction (forward /
/// backward) — a decoder keeps one instance of this per direction, since the
/// two prediction chains reset independently of each other.
#[derive(Debug, Clone, Copy, Default)]
pub struct SequentialMvPredictor {
    pmv: [[i32; 2]; 2],
}

impl SequentialMvPredictor {
    /// A fresh predictor, `PMV == 0` in both components — the state at the
    /// start of a slice.
    #[must_use]
    pub const fn new() -> Self {
        Self { pmv: [[0, 0], [0, 0]] }
    }

    /// Reset to zero: a slice boundary, or any macroblock whose own coding
    /// mode breaks the prediction chain (see the module docs).
    pub const fn reset(&mut self) {
        self.pmv = [[0, 0], [0, 0]];
    }

    /// The predictor currently held for vector slot `r` (`0` or `1`),
    /// `(horizontal, vertical)`. Out-of-range `r` reads as the all-zero
    /// predictor rather than panicking.
    #[must_use]
    pub fn predictor(&self, r: usize) -> [i32; 2] {
        self.pmv.get(r).copied().unwrap_or([0, 0])
    }

    /// Record a freshly reconstructed vector as the new predictor for slot
    /// `r`, for the next macroblock in the chain to predict against.
    pub fn update(&mut self, r: usize, vector: [i32; 2]) {
        if let Some(slot) = self.pmv.get_mut(r) {
            *slot = vector;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SequentialMvPredictor;

    #[test]
    fn starts_at_zero() {
        let p = SequentialMvPredictor::new();
        assert_eq!(p.predictor(0), [0, 0]);
        assert_eq!(p.predictor(1), [0, 0]);
    }

    #[test]
    fn update_then_predictor_round_trips() {
        let mut p = SequentialMvPredictor::new();
        p.update(0, [5, -3]);
        assert_eq!(p.predictor(0), [5, -3]);
        // Slot 1 is independent of slot 0.
        assert_eq!(p.predictor(1), [0, 0]);
    }

    #[test]
    fn reset_zeroes_both_slots() {
        let mut p = SequentialMvPredictor::new();
        p.update(0, [5, -3]);
        p.update(1, [1, 1]);
        p.reset();
        assert_eq!(p.predictor(0), [0, 0]);
        assert_eq!(p.predictor(1), [0, 0]);
    }

    #[test]
    fn out_of_range_slot_reads_as_zero_rather_than_panicking() {
        let p = SequentialMvPredictor::new();
        assert_eq!(p.predictor(7), [0, 0]);
    }
}
