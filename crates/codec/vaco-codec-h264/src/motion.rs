//! Clause 8.4.1's motion vector prediction, built as a set of pure
//! functions over already-decoded neighbour state rather than folded into
//! `mb.rs`'s own CABAC decode loop -- the same "raster order already
//! guarantees every input exists" reasoning `crate::reconstruct` and
//! `crate::deblock` both already lean on: clause 8.4.1.3's own A/B/C/D
//! neighbours are always left/above/above-right/above-left, so by the
//! time a partition's own prediction runs, every neighbour it could ever
//! need is either a strictly-earlier partition of the *same* macroblock
//! (already written to the live `mv` grid by `mb.rs`'s own per-partition
//! loop, per that grid's own "write immediately" comment) or a
//! strictly-earlier macroblock in raster order.
//!
//! **Scope, explicit**: the plain median predictor (clause 8.4.1.3.1) and
//! its 16x16/16x8/8x16 directional special cases, plus `P_Skip`'s own
//! rule (clause 8.4.1.1). Spatial and temporal *direct* mode (B slices)
//! are out of scope for now -- every fixture this crate decodes is I/P
//! only (see `mb.rs`'s own module doc for why B was narrowed out of an
//! earlier dispatch), so nothing here has ever needed to exercise them.

/// One neighbour's contribution to a median MV prediction.
///
/// `available` is clause 6.4's **macroblock** availability (is there a
/// decoded macroblock at that position, in this slice, before this one?),
/// *not* "does that macroblock have motion data for this list". Those are
/// two different questions and the specification asks both, separately:
///
/// - Clause 8.4.1.3.2 gives an available-but-`Intra` neighbour (or one
///   whose `predFlagLX` is 0) `mvLXN = (0, 0)` and `refIdxLXN = -1`. It
///   stays *available*.
/// - Clause 8.4.1.3.1's "if `B` and `C` are both not available and `A` is
///   available, use `mvLXA`" shortcut, and clause 8.4.1.1's `P_Skip`
///   "`mbAddrA`/`mbAddrB` not available" zero-motion test, both read that
///   availability -- so an intra neighbour must *not* trigger either.
///
/// Collapsing the two (treating an intra neighbour as unavailable) was a
/// real bug: it made every `P_Skip` macroblock whose left or above
/// neighbour happened to be intra predict `mv = (0, 0)` where the correct
/// answer was the median predictor's, and the error then propagated
/// through every later picture that predicted from it. `mb.rs`'s
/// `MvInfo::mb_available` is the source of this field.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Neighbour {
    pub(crate) available: bool,
    pub(crate) ref_idx: i8,
    pub(crate) mv: (i16, i16),
}

impl Neighbour {
    /// Clause 6.4's own "not available": no macroblock there at all
    /// (outside the picture, or outside this slice). Distinct from an
    /// available neighbour carrying `ref_idx == -1` -- see this type's
    /// own doc.
    #[cfg(test)]
    pub(crate) const UNAVAILABLE: Self = Self {
        available: false,
        ref_idx: -1,
        mv: (0, 0),
    };

    /// An available macroblock that carries no motion for this list
    /// (`Intra`, `I_PCM`, or `predFlagLX == 0`): clause 8.4.1.3.2's own
    /// `mvLXN = (0, 0)`, `refIdxLXN = -1` substitution.
    #[cfg(test)]
    pub(crate) const INTRA: Self = Self {
        available: true,
        ref_idx: -1,
        mv: (0, 0),
    };

    const fn mv_or_zero(self) -> (i16, i16) {
        if self.available { self.mv } else { (0, 0) }
    }
}

const fn median3(a: i32, b: i32, c: i32) -> i32 {
    // min + max + sum - min - max, the standard branch-free median-of-3 --
    // equivalent to Max(Min(a,b), Min(Max(a,b),c)) clause 8.4.1.3.1 itself
    // uses, just without needing four comparisons written out by hand.
    a + b + c - i32_min3(a, b, c) - i32_max3(a, b, c)
}

const fn i32_min3(a: i32, b: i32, c: i32) -> i32 {
    let ab = if a < b { a } else { b };
    if ab < c { ab } else { c }
}

const fn i32_max3(a: i32, b: i32, c: i32) -> i32 {
    let ab = if a > b { a } else { b };
    if ab > c { ab } else { c }
}

/// The plain median predictor (clause 8.4.1.3.1), used directly by
/// `16x16`/`8x8`(sub-mb) partitions and as the fallback the `16x8`/`8x16`
/// directional cases above it reduce to when their own single-neighbour
/// shortcut does not apply.
fn median_predictor(a: Neighbour, b: Neighbour, c: Neighbour, ref_idx: i8) -> (i16, i16) {
    // "B and C both unavailable, A available" -> use A directly, skipping
    // the median (clause 8.4.1.3.1's own first special case; with B/C's
    // own substituted (0,0)/-1 values, a literal median would otherwise
    // silently answer (0,0) whenever A alone was ever set, which is wrong
    // whenever A's own mv is not zero).
    if !b.available && !c.available && a.available {
        return a.mv_or_zero();
    }
    let matches: Vec<Neighbour> = [a, b, c]
        .into_iter()
        .filter(|n| n.available && n.ref_idx == ref_idx)
        .collect();
    if matches.len() == 1 {
        // Exactly one neighbour shares this partition's own ref_idx ->
        // use that one directly (clause 8.4.1.3.1's second special case).
        #[allow(clippy::indexing_slicing, reason = "matches.len() == 1 just checked")]
        return matches[0].mv_or_zero();
    }
    let (ax, ay) = a.mv_or_zero();
    let (bx, by) = b.mv_or_zero();
    let (cx, cy) = c.mv_or_zero();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "median of three i16-range values fits back in i16"
    )]
    let mvx = median3(i32::from(ax), i32::from(bx), i32::from(cx)) as i16;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "median of three i16-range values fits back in i16"
    )]
    let mvy = median3(i32::from(ay), i32::from(by), i32::from(cy)) as i16;
    (mvx, mvy)
}

/// Which whole-macroblock partition shape this prediction is for -- the
/// directional `16x8`/`8x16` shortcuts only apply to those two shapes;
/// every other shape (`16x16`, and every `8x8`-or-smaller sub-mb
/// partition) uses the plain median unconditionally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartitionShape {
    Whole,
    Top16x8,
    Bottom16x8,
    Left8x16,
    Right8x16,
}

/// Clause 8.4.1.3: derive one partition's predicted motion vector from its
/// own `A` (left), `B` (above) and `C` (above-right, already resolved by
/// the caller to `D`/above-left if `C` itself is unavailable -- clause
/// 8.4.1.3.2's own substitution) neighbours.
pub(crate) fn predict_mv(
    shape: PartitionShape,
    a: Neighbour,
    b: Neighbour,
    c: Neighbour,
    ref_idx: i8,
) -> (i16, i16) {
    match shape {
        PartitionShape::Top16x8 if b.available && b.ref_idx == ref_idx => b.mv_or_zero(),
        // Clause 8.4.1.3.1: the 16x8-bottom and 8x16-left partitions share
        // the same `A`-only shortcut, not a coincidental duplicate.
        PartitionShape::Bottom16x8 | PartitionShape::Left8x16
            if a.available && a.ref_idx == ref_idx =>
        {
            a.mv_or_zero()
        }
        PartitionShape::Right8x16 if c.available && c.ref_idx == ref_idx => c.mv_or_zero(),
        _ => median_predictor(a, b, c, ref_idx),
    }
}

/// `P_Skip`'s own motion vector (clause 8.4.1.1): `(0, 0)` if `A` or `B`
/// is unavailable, or if either is available with `ref_idx == 0` and
/// `mv == (0, 0)` -- otherwise the plain median predictor with
/// `ref_idx == 0`.
///
/// "Unavailable" here is clause 6.4's macroblock availability, exactly as
/// [`Neighbour`]'s own doc describes. An `Intra` neighbour is available
/// and carries `ref_idx == -1`, so it fails the `ref_idx == 0` test and
/// this function falls through to the median predictor -- it does **not**
/// short-circuit to `(0, 0)`.
pub(crate) fn p_skip_mv(a: Neighbour, b: Neighbour, c: Neighbour) -> (i16, i16) {
    if !a.available
        || !b.available
        || (a.ref_idx == 0 && a.mv == (0, 0))
        || (b.ref_idx == 0 && b.mv == (0, 0))
    {
        return (0, 0);
    }
    median_predictor(a, b, c, 0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn avail(ref_idx: i8, mv: (i16, i16)) -> Neighbour {
        Neighbour {
            available: true,
            ref_idx,
            mv,
        }
    }

    #[test]
    fn median_of_three_matches_hand_computed_cases() {
        assert_eq!(median3(1, 2, 3), 2);
        assert_eq!(median3(3, 2, 1), 2);
        assert_eq!(median3(-5, 0, 5), 0);
        assert_eq!(median3(5, 5, 5), 5);
        assert_eq!(median3(-1, -1, 2), -1);
    }

    #[test]
    fn only_a_available_uses_a_directly_not_a_median_with_zeroes() {
        let mv = median_predictor(
            avail(0, (12, -8)),
            Neighbour::UNAVAILABLE,
            Neighbour::UNAVAILABLE,
            0,
        );
        assert_eq!(mv, (12, -8));
    }

    #[test]
    fn exactly_one_matching_ref_idx_is_used_directly() {
        let a = avail(1, (4, 4));
        let b = avail(0, (10, -10));
        let c = avail(1, (4, 4));
        assert_eq!(median_predictor(a, b, c, 0), (10, -10));
    }

    #[test]
    fn ordinary_median_when_all_three_available_and_no_single_match() {
        let a = avail(0, (0, 0));
        let b = avail(0, (4, 0));
        let c = avail(0, (8, 0));
        assert_eq!(median_predictor(a, b, c, 0), (4, 0));
    }

    #[test]
    fn p_skip_is_zero_when_a_neighbour_is_unavailable() {
        assert_eq!(
            p_skip_mv(
                Neighbour::UNAVAILABLE,
                avail(0, (5, 5)),
                Neighbour::UNAVAILABLE
            ),
            (0, 0)
        );
    }

    #[test]
    fn p_skip_is_zero_when_a_or_b_is_zero_motion_ref0() {
        assert_eq!(
            p_skip_mv(avail(0, (0, 0)), avail(0, (7, 7)), Neighbour::UNAVAILABLE),
            (0, 0)
        );
    }

    /// The bug this distinction exists for, pinned: an `Intra` left
    /// neighbour is *available* with `ref_idx == -1`, so clause 8.4.1.1's
    /// zero-motion test does not fire and `P_Skip` must use the median
    /// predictor. Treating intra as unavailable returned `(0, 0)` here and
    /// silently corrupted every picture predicted from the result.
    #[test]
    fn p_skip_does_not_zero_when_a_neighbour_is_intra_rather_than_absent() {
        let a = Neighbour::INTRA;
        let b = avail(0, (8, 0));
        assert_eq!(p_skip_mv(a, b, Neighbour::UNAVAILABLE), (8, 0));
        // ... and the genuinely-absent case still does zero, so this is a
        // real distinction and not a blanket loosening.
        assert_eq!(
            p_skip_mv(Neighbour::UNAVAILABLE, b, Neighbour::UNAVAILABLE),
            (0, 0)
        );
    }

    /// Clause 8.4.1.3.1's "`B` and `C` both not available" shortcut is
    /// also macroblock availability: an intra `B` keeps the median path
    /// (median of `A`, `(0, 0)`, `(0, 0)`), it does not hand `A` through
    /// untouched.
    #[test]
    fn intra_b_does_not_trigger_the_a_only_shortcut() {
        // `A` deliberately carries a *different* `ref_idx` from the one
        // being predicted, so the "exactly one neighbour matches" rule
        // cannot fire and the two paths give visibly different answers.
        let a = avail(1, (12, -8));
        assert_eq!(
            median_predictor(a, Neighbour::INTRA, Neighbour::UNAVAILABLE, 0),
            (0, 0),
            "an available intra B keeps the median path: median(12, 0, 0) == 0"
        );
        assert_eq!(
            median_predictor(a, Neighbour::UNAVAILABLE, Neighbour::UNAVAILABLE, 0),
            (12, -8),
            "with B genuinely absent the A-only shortcut does apply"
        );
    }

    #[test]
    fn p_skip_falls_back_to_median_otherwise() {
        let a = avail(0, (4, 0));
        let b = avail(0, (8, 0));
        assert_eq!(
            p_skip_mv(a, b, Neighbour::UNAVAILABLE),
            median_predictor(a, b, Neighbour::UNAVAILABLE, 0)
        );
    }

    #[test]
    fn top_16x8_uses_b_directly_when_ref_idx_matches() {
        let b = avail(0, (9, -3));
        let mv = predict_mv(
            PartitionShape::Top16x8,
            Neighbour::UNAVAILABLE,
            b,
            Neighbour::UNAVAILABLE,
            0,
        );
        assert_eq!(mv, (9, -3));
    }

    #[test]
    fn bottom_16x8_falls_back_to_median_when_a_does_not_match() {
        let a = avail(1, (9, -3));
        let b = avail(0, (2, 2));
        let c = avail(0, (2, 2));
        let mv = predict_mv(PartitionShape::Bottom16x8, a, b, c, 0);
        assert_eq!(mv, median_predictor(a, b, c, 0));
    }
}
