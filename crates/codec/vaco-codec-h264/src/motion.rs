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

/// One neighbour's contribution to a median MV prediction: `None` means
/// clause 8.4.1.3's own "not available, or intra, or a different
/// reference list" substitution (`mv = (0, 0)`, `ref_idx = -1`, and it
/// still counts as a real input to the median, not skipped).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Neighbour {
    pub(crate) available: bool,
    pub(crate) ref_idx: i8,
    pub(crate) mv: (i16, i16),
}

impl Neighbour {
    pub(crate) const UNAVAILABLE: Self = Self {
        available: false,
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
