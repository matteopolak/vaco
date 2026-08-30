//! Motion vector prediction: merge candidate derivation (§8.5.3.2.2 spatial,
//! §8.5.3.2.8/.9 temporal, then zero-fill) and AMVP candidate derivation
//! (§8.5.3.2.6/.7), plus the shared scaling/clipping arithmetic both use.
//!
//! # Scope: P-slices only
//!
//! Every function here assumes uni-prediction from `RefPicList0` alone —
//! `PredFlagL1` is always 0, `inter_pred_idc` is never parsed (§7.3.8.6's own
//! semantics infer `PRED_L0` whenever `slice_type == P`), and the B-slice-only
//! combined bi-predictive merge candidates (§8.5.3.2.4) are not derived at
//! all. `decoder.rs::check_scope` refuses B slices, so nothing here needs a
//! second list.
//!
//! # Why positions are pixel coordinates, not z-scan partition indices
//!
//! HM's own derivation (`TComDataCU::getInterMergeCandidates`/`fillMvpCand`)
//! is written in terms of z-scan partition addresses within a CTU, because
//! that is the addressing scheme its whole `TComDataCU` tree uses. This
//! crate's coding-tree walk never built that addressing at all — every
//! neighbour lookup elsewhere in the crate (`CuGrid::mode_at`,
//! `CuGrid::qp_at`) is already in plain luma-sample coordinates, and
//! §8.5.3.2's own clause text is written in exactly those terms (`xNbA1`,
//! `yNbA1`, ...). Reading HM's z-scan arithmetic back out to the pixel
//! positions it computes (confirmed by hand for every position used below)
//! and working in pixel coordinates throughout is the same simplification
//! `crate::framebuf`'s own module doc already makes for intra availability,
//! applied to the same problem one clause family later.
//!
//! # Specification
//!
//! ITU-T H.265 (08/2021) §8.5.3.2.2–§8.5.3.2.9. Cross-checked against HM
//! 18.0's `TComDataCU::getInterMergeCandidates`/`fillMvpCand`/
//! `xAddMVPCandUnscaled`/`xAddMVPCandWithScaling`/`xGetColMVP`/
//! `xGetDistScaleFactor` (Tier A, BSD-3-Clause).

use crate::framebuf::CuGrid;

/// A motion vector in quarter-luma-sample units — `TComMv`'s own
/// representation, which HM stores as two `Short` (`i16`); every value this
/// crate ever produces (a decoded `mvd`, a scaled predictor) fits in `i16`
/// for the same reason HM's does, so `i32` here is headroom for the
/// intermediate arithmetic in [`scale_mv`]/[`dist_scale_factor`], not a wider
/// representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Mv {
    pub x: i32,
    pub y: i32,
}

impl Mv {
    pub(crate) const ZERO: Self = Self { x: 0, y: 0 };
}

/// One resolved neighbour: its (already POC-scaled-if-needed, in the caller's
/// terms — this type itself carries no scaling) motion vector and the POC of
/// the picture it refers to. Storing the referenced *POC* rather than a
/// `ref_idx` is a deliberate departure from HM's own representation: within
/// one slice, `RefPicList0` is shared by every CU, so POC and `ref_idx` carry
/// the same information, and POC is what every comparison in this clause
/// family (`currRefPOC == neibRefPOC`, the distance-scale factor) actually
/// wants — carrying `ref_idx` instead would just re-derive POC from it at
/// every use site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MotionInfo {
    pub mv: Mv,
    pub ref_poc: i64,
}

/// A PU's own geometry within its CU, in picture-pixel coordinates — general
/// enough for every `PartMode` including the asymmetric (AMP) shapes, unlike
/// `ctu::Pu` (intra-only, always square).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PuRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// `part_mode` (§7.4.9.5's semantics table), the inter-CU superset of the
/// intra one (`NxN` is shared with intra; the rest are inter-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartMode {
    TwoNx2N,
    TwoNxN,
    Nx2N,
    NxN,
    TwoNxNu,
    TwoNxNd,
    NLx2N,
    NRx2N,
}

impl PartMode {
    /// Number of PUs this partition mode splits its CU into.
    #[must_use]
    pub(crate) const fn num_pus(self) -> usize {
        match self {
            Self::TwoNx2N => 1,
            Self::NxN => 4,
            _ => 2,
        }
    }

    /// `getPartPosition`: `pu_idx`'s own top-left and size, in pixels, given
    /// the CU's own top-left `(x0, y0)` and `size` (its square side length —
    /// every coding unit is square regardless of how its PUs split it).
    #[must_use]
    pub(crate) fn pu_rect(self, x0: i32, y0: i32, size: i32, pu_idx: usize) -> PuRect {
        let half = size >> 1;
        let quarter = size >> 2;
        match self {
            Self::TwoNx2N => PuRect { x: x0, y: y0, w: size, h: size },
            Self::TwoNxN => PuRect { x: x0, y: y0 + i32::try_from(pu_idx).unwrap_or(0) * half, w: size, h: half },
            Self::Nx2N => PuRect { x: x0 + i32::try_from(pu_idx).unwrap_or(0) * half, y: y0, w: half, h: size },
            Self::NxN => {
                let (dx, dy) = (i32::try_from(pu_idx & 1).unwrap_or(0), i32::try_from(pu_idx >> 1).unwrap_or(0));
                PuRect { x: x0 + dx * half, y: y0 + dy * half, w: half, h: half }
            }
            Self::TwoNxNu => {
                if pu_idx == 0 {
                    PuRect { x: x0, y: y0, w: size, h: quarter }
                } else {
                    PuRect { x: x0, y: y0 + quarter, w: size, h: size - quarter }
                }
            }
            Self::TwoNxNd => {
                if pu_idx == 0 {
                    PuRect { x: x0, y: y0, w: size, h: size - quarter }
                } else {
                    PuRect { x: x0, y: y0 + size - quarter, w: size, h: quarter }
                }
            }
            Self::NLx2N => {
                if pu_idx == 0 {
                    PuRect { x: x0, y: y0, w: quarter, h: size }
                } else {
                    PuRect { x: x0 + quarter, y: y0, w: size - quarter, h: size }
                }
            }
            Self::NRx2N => {
                if pu_idx == 0 {
                    PuRect { x: x0, y: y0, w: size - quarter, h: size }
                } else {
                    PuRect { x: x0 + size - quarter, y: y0, w: quarter, h: size }
                }
            }
        }
    }
}

/// `isDiffMER`, §8.5.3.2.3: whether two positions fall in different merge
/// estimation regions, gated by the PPS's `Log2ParallelMergeLevel`.
fn is_diff_mer(x_n: i32, y_n: i32, x_p: i32, y_p: i32, log2_parallel_merge_level: u32) -> bool {
    (x_n >> log2_parallel_merge_level) != (x_p >> log2_parallel_merge_level) || (y_n >> log2_parallel_merge_level) != (y_p >> log2_parallel_merge_level)
}

/// §8.5.3.2.9's distance scale factor (HM's `xGetDistScaleFactor`, a `static`
/// member with no `TComDataCU` state — a pure function of four POCs).
/// Returns `4096` (i.e. "no scaling needed") whenever the two POC deltas
/// already agree, checked *before* either is clamped to `[-128, 127]`, since
/// HM's own equality test runs on the unclamped values.
#[must_use]
pub(crate) fn dist_scale_factor(curr_poc: i64, curr_ref_poc: i64, other_poc: i64, other_ref_poc: i64) -> i32 {
    let diff_b = curr_poc - curr_ref_poc;
    let diff_d = other_poc - other_ref_poc;
    if diff_d == diff_b {
        return 4096;
    }
    let clip = |v: i64| -> i32 { i32::try_from(v.clamp(-128, 127)).unwrap_or(0) };
    let tdb = clip(diff_b);
    let tdd = clip(diff_d);
    if tdd == 0 {
        // Not reachable from a conforming stream (it would mean `diff_d ==
        // 0`, which only equals `diff_b` when `diff_b` is also `0` — already
        // handled above) but division by a clamped `0` is refused rather
        // than left to panic on a malformed one.
        return 4096;
    }
    // `xGetDistScaleFactor`'s own formula is truncating C++ integer
    // division (`iTDD/2`, `.../iTDD`) — not equivalent to a bit-shift for a
    // value that can be negative, so this stays real division rather than
    // `>>`, unlike the `size`-halving divisions elsewhere in this crate's
    // inter-prediction code.
    #[allow(clippy::integer_division, reason = "deliberate truncating division, matching HM's own xGetDistScaleFactor exactly")]
    let x = (0x4000 + (tdd / 2).abs()) / tdd;
    let scaled = (i64::from(tdb) * i64::from(x) + 32) >> 6;
    i32::try_from(scaled.clamp(-4096, 4095)).unwrap_or(0)
}

/// `TComMv::scaleMv`: scale a motion vector by a `dist_scale_factor` result,
/// rounding half-away-from-zero (the `+ 127 + (product < 0)` term) and
/// clamping to what a `Short` (`i16`) can hold — the same bound the result
/// is stored back into.
#[must_use]
pub(crate) fn scale_mv(mv: Mv, scale: i32) -> Mv {
    let round = |component: i32| -> i32 {
        let product = i64::from(scale) * i64::from(component);
        let biased = product + 127 + i64::from(product < 0);
        i32::try_from((biased >> 8).clamp(-32768, 32767)).unwrap_or(0)
    };
    Mv { x: round(mv.x), y: round(mv.y) }
}

/// `TComDataCU::clipMv`: clamp a motion vector so the reference-sample fetch
/// it implies cannot stray arbitrarily far outside the picture — `cu_x0`/
/// `cu_y0` are the *coding unit's* own top-left (not the PU's), and
/// `ctb_size` is the SPS's fixed CTB width/height, both matching HM's own
/// `m_uiCUPelX`/`sps.getMaxCUWidth()` exactly.
#[must_use]
pub(crate) fn clip_mv(mv: Mv, cu_x0: i32, cu_y0: i32, pic_width: i32, pic_height: i32, ctb_size: i32) -> Mv {
    const SHIFT: i32 = 2;
    const OFFSET: i32 = 8;
    let hor_max = (pic_width + OFFSET - cu_x0 - 1) << SHIFT;
    let hor_min = (-ctb_size - OFFSET - cu_x0 + 1) << SHIFT;
    let ver_max = (pic_height + OFFSET - cu_y0 - 1) << SHIFT;
    let ver_min = (-ctb_size - OFFSET - cu_y0 + 1) << SHIFT;
    Mv { x: mv.x.max(hor_min).min(hor_max), y: mv.y.max(ver_min).min(ver_max) }
}

/// A pre-fetched temporal candidate, already resolved (and scaled, if the
/// caller chose to scale it) against a specific target `(curr_poc,
/// curr_ref_poc)` pair — see `crate::dpb`'s collocated-motion-field doc for
/// how it is produced from a reference picture's own stored motion.
pub(crate) type TemporalCandidate = Option<Mv>;

/// The five spatial neighbour positions (§8.5.3.2.3's `xNbA1`/`yNbA1`, ...),
/// resolved from a PU's own rectangle.
struct SpatialPositions {
    a1: (i32, i32),
    b1: (i32, i32),
    b0: (i32, i32),
    a0: (i32, i32),
    b2: (i32, i32),
}

fn spatial_positions(pu: PuRect) -> SpatialPositions {
    SpatialPositions {
        a1: (pu.x - 1, pu.y + pu.h - 1),
        b1: (pu.x + pu.w - 1, pu.y - 1),
        b0: (pu.x + pu.w, pu.y - 1),
        a0: (pu.x - 1, pu.y + pu.h),
        b2: (pu.x - 1, pu.y - 1),
    }
}

/// One resolved spatial candidate slot: `None` when that position was never
/// available (outside the picture, not yet decoded, intra, in the same merge
/// estimation region, or excluded by the second-PU redundancy rule) — kept
/// distinct from "available but a duplicate of an earlier slot" (which is
/// also `None` in the final merge list but for a different reason, tracked
/// only implicitly by simply not pushing a duplicate).
fn lookup(grid: &CuGrid, pos: (i32, i32)) -> Option<MotionInfo> {
    grid.inter_at(pos.0, pos.1)
}

/// §8.5.3.2.2/.3: derive up to `max_num_merge_cand` merge candidates for one
/// PU. `pu_idx`/`part_mode` are the PU's own index and its CU's partition
/// mode *as merge candidate derivation should see them* — already
/// substituted to `(0, PartMode::TwoNx2N)` by the caller
/// (`ctu::maybe_override_for_merge_parallelism`) when the PPS's merge
/// parallelism level forces the whole CU to be treated as one PU for this
/// purpose (§8.5.3.2.2's own "when `Log2ParallelMergeLevel` is greater than 2
/// and nCbS is equal to 8" special case).
///
/// `ref_poc_l0` is `RefPicList0`, as POCs — both the zero-candidate fill's
/// own cycling bound (§8.5.3.2.5, `NumRefIdxL0` is `ref_poc_l0.len()`) *and*
/// the source of the real POC every zero/temporal candidate must carry, so
/// the caller (`ctu.rs`) can resolve a chosen candidate back to an actual
/// reference picture for motion compensation.
#[allow(clippy::too_many_arguments, reason = "one call site (ctu.rs); every argument is a distinct clause-8.5.3.2.2 input")]
pub(crate) fn derive_merge_candidates(
    grid: &CuGrid,
    pu: PuRect,
    pu_idx: usize,
    part_mode: PartMode,
    log2_parallel_merge_level: u32,
    max_num_merge_cand: usize,
    ref_poc_l0: &[i64],
    temporal: TemporalCandidate,
) -> Vec<MotionInfo> {
    let mut cands: Vec<MotionInfo> = Vec::new();
    if max_num_merge_cand == 0 {
        return cands;
    }
    let pos = spatial_positions(pu);
    let (x_p, y_p) = (pu.x, pu.y);

    let a1_excluded = pu_idx == 1 && matches!(part_mode, PartMode::Nx2N | PartMode::NLx2N | PartMode::NRx2N);
    let b1_excluded = pu_idx == 1 && matches!(part_mode, PartMode::TwoNxN | PartMode::TwoNxNu | PartMode::TwoNxNd);

    let a1 = (!a1_excluded && is_diff_mer(pos.a1.0, pos.a1.1, x_p, y_p, log2_parallel_merge_level))
        .then(|| lookup(grid, pos.a1))
        .flatten();
    if let Some(m) = a1 {
        cands.push(m);
    }

    if cands.len() < max_num_merge_cand {
        let b1 = (!b1_excluded && is_diff_mer(pos.b1.0, pos.b1.1, x_p, y_p, log2_parallel_merge_level))
            .then(|| lookup(grid, pos.b1))
            .flatten();
        if let Some(m) = b1
            && a1 != Some(m)
        {
            cands.push(m);
        }
    }

    if cands.len() < max_num_merge_cand {
        let b1 = (!b1_excluded && is_diff_mer(pos.b1.0, pos.b1.1, x_p, y_p, log2_parallel_merge_level)).then(|| lookup(grid, pos.b1)).flatten();
        let b0 = (is_diff_mer(pos.b0.0, pos.b0.1, x_p, y_p, log2_parallel_merge_level)).then(|| lookup(grid, pos.b0)).flatten();
        if let Some(m) = b0
            && b1 != Some(m)
        {
            cands.push(m);
        }
    }

    if cands.len() < max_num_merge_cand {
        let a0 = (is_diff_mer(pos.a0.0, pos.a0.1, x_p, y_p, log2_parallel_merge_level)).then(|| lookup(grid, pos.a0)).flatten();
        if let Some(m) = a0
            && a1 != Some(m)
        {
            cands.push(m);
        }
    }

    if cands.len() < max_num_merge_cand && cands.len() < 4 {
        let b1 = (!b1_excluded && is_diff_mer(pos.b1.0, pos.b1.1, x_p, y_p, log2_parallel_merge_level)).then(|| lookup(grid, pos.b1)).flatten();
        let b2 = (is_diff_mer(pos.b2.0, pos.b2.1, x_p, y_p, log2_parallel_merge_level)).then(|| lookup(grid, pos.b2)).flatten();
        if let Some(m) = b2
            && a1 != Some(m)
            && b1 != Some(m)
        {
            cands.push(m);
        }
    }

    if cands.len() < max_num_merge_cand
        && let Some(mv) = temporal
    {
        // §8.5.3.2.8: the merge candidate's own reference index is always 0
        // — `xGetColMVP`'s caller in `getInterMergeCandidates` fixes
        // `iRefIdx = 0` before scaling, unconditionally, regardless of which
        // picture `RefPicList0[0]` actually names for *this* slice — so this
        // candidate always predicts against `RefPicList0[0]`, whose POC is
        // recorded here for the caller's own reference-picture lookup.
        let ref_poc = ref_poc_l0.first().copied().unwrap_or(0);
        cands.push(MotionInfo { mv, ref_poc });
    }

    // §8.5.3.2.5: zero-motion candidates, cycling `ref_idx` from 0 up to
    // `NumRefIdxL0 - 1` and back, until the list reaches its target length —
    // each one's `ref_poc` is `RefPicList0[ref_idx]`'s real POC, needed the
    // same way the temporal candidate's is.
    let ref_cycle = ref_poc_l0.len().max(1);
    let mut r: usize = 0;
    while cands.len() < max_num_merge_cand {
        let ref_poc = ref_poc_l0.get(r % ref_cycle).copied().unwrap_or(0);
        cands.push(MotionInfo { mv: Mv::ZERO, ref_poc });
        r += 1;
    }

    cands
}

/// §8.5.3.2.6/.7: derive the (up to two) AMVP candidates for one reference
/// index on one list. `curr_poc` is the current picture's own POC;
/// `target_ref_poc` is `RefPicList0[refIdx]`'s POC — the picture the new
/// PU's own motion vector will actually predict against. `temporal` is the
/// already-fetched-and-scaled §8.5.3.2.8 candidate for *this* `refIdx`
/// (unlike merge, AMVP's temporal candidate is scaled against the real
/// target `refIdx`, not a fixed `0`).
#[allow(clippy::too_many_arguments, reason = "one call site (ctu.rs); every argument is a distinct clause-8.5.3.2.6/.7 input")]
pub(crate) fn derive_amvp_candidates(
    grid: &CuGrid,
    pu: PuRect,
    log2_parallel_merge_level: u32,
    curr_poc: i64,
    target_ref_poc: i64,
    temporal: TemporalCandidate,
) -> [Mv; 2] {
    let _ = log2_parallel_merge_level; // AMVP's own neighbour search has no MER gate (§8.5.3.2.7 has none); kept as a parameter for call-site symmetry with merge, unused here.
    let pos = spatial_positions(pu);

    // Left group: below-left then left, unscaled first, then the same order
    // scaled. `is_scaled_flag` (HM's own name) gates whether the *above*
    // group's scaled search runs at all.
    let below_left = grid.inter_at(pos.a0.0, pos.a0.1);
    let left = grid.inter_at(pos.a1.0, pos.a1.1);
    let is_scaled_flag = below_left.is_some() || left.is_some();

    let mut cands: Vec<Mv> = Vec::new();
    let push_unique = |mv: Mv, cands: &mut Vec<Mv>| {
        if cands.len() < 2 {
            cands.push(mv);
        }
    };

    if let Some(m) = amvp_unscaled(below_left, target_ref_poc).or_else(|| amvp_unscaled(left, target_ref_poc)) {
        push_unique(m, &mut cands);
    } else if let Some(m) = amvp_scaled(below_left, curr_poc, target_ref_poc).or_else(|| amvp_scaled(left, curr_poc, target_ref_poc)) {
        push_unique(m, &mut cands);
    }

    let above_right = grid.inter_at(pos.b0.0, pos.b0.1);
    let above = grid.inter_at(pos.b1.0, pos.b1.1);
    let above_left = grid.inter_at(pos.b2.0, pos.b2.1);

    if let Some(m) = amvp_unscaled(above_right, target_ref_poc)
        .or_else(|| amvp_unscaled(above, target_ref_poc))
        .or_else(|| amvp_unscaled(above_left, target_ref_poc))
    {
        push_unique(m, &mut cands);
    }

    if !is_scaled_flag
        && let Some(m) = amvp_scaled(above_right, curr_poc, target_ref_poc)
            .or_else(|| amvp_scaled(above, curr_poc, target_ref_poc))
            .or_else(|| amvp_scaled(above_left, curr_poc, target_ref_poc))
    {
        push_unique(m, &mut cands);
    }

    if cands.len() == 2 && cands.first() == cands.get(1) {
        cands.truncate(1);
    }

    if cands.len() < 2
        && let Some(mv) = temporal
    {
        cands.push(mv);
    }

    while cands.len() < 2 {
        cands.push(Mv::ZERO);
    }
    [cands.first().copied().unwrap_or(Mv::ZERO), cands.get(1).copied().unwrap_or(Mv::ZERO)]
}

/// `xAddMVPCandUnscaled`: a neighbour contributes its raw motion vector only
/// when it names *exactly* the target reference picture (by POC) — no
/// scaling, no substitution.
fn amvp_unscaled(neighbour: Option<MotionInfo>, target_ref_poc: i64) -> Option<Mv> {
    let m = neighbour?;
    (m.ref_poc == target_ref_poc).then_some(m.mv)
}

/// `xAddMVPCandWithScaling`: a neighbour contributes its motion vector
/// scaled by POC distance — every reference in this crate's scope is
/// short-term (long-term references are refused, `crate::dpb`'s own doc), so
/// HM's "both long-term" gate never applies and scaling always runs. HM's
/// own `neibPOC` argument is always `currPOC` here, since the neighbour is
/// in the same picture as the PU being predicted.
fn amvp_scaled(neighbour: Option<MotionInfo>, curr_poc: i64, target_ref_poc: i64) -> Option<Mv> {
    let m = neighbour?;
    let scale = dist_scale_factor(curr_poc, target_ref_poc, curr_poc, m.ref_poc);
    Some(if scale == 4096 { m.mv } else { scale_mv(m.mv, scale) })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code over fixed scenarios")]
mod tests {
    use super::*;

    #[test]
    fn dist_scale_factor_is_identity_when_deltas_agree() {
        assert_eq!(dist_scale_factor(10, 8, 20, 18), 4096);
    }

    #[test]
    fn dist_scale_factor_halves_for_double_distance() {
        // curr: 10 -> 8 (delta 2); other: 10 -> 6 (delta 4, twice as far).
        // Scaling a vector built for distance 4 down to distance 2 halves it.
        let scale = dist_scale_factor(10, 8, 10, 6);
        let mv = scale_mv(Mv { x: 100, y: -100 }, scale);
        assert_eq!(mv, Mv { x: 50, y: -50 });
    }

    #[test]
    fn scale_mv_rounds_half_away_from_zero() {
        // scale = 4096 * 1.5 style rounding check via a small explicit scale.
        let mv = scale_mv(Mv { x: 1, y: -1 }, 200);
        // (200*1 + 127) >> 8 = 327 >> 8 = 1 ; (200*-1 + 127 + 1) >> 8 = (-72)>>8 = -1 (arithmetic shift)
        assert_eq!(mv.x, 1);
        assert_eq!(mv.y, -1);
    }

    #[test]
    fn clip_mv_clamps_to_the_picture_plus_margin() {
        let mv = clip_mv(Mv { x: 100_000, y: -100_000 }, 0, 0, 64, 64, 64);
        assert!(mv.x < 100_000);
        assert!(mv.y > -100_000);
    }

    #[test]
    fn part_mode_pu_rects_partition_the_cu_exactly() {
        let cases: [(PartMode, usize); 8] = [
            (PartMode::TwoNx2N, 1),
            (PartMode::TwoNxN, 2),
            (PartMode::Nx2N, 2),
            (PartMode::NxN, 4),
            (PartMode::TwoNxNu, 2),
            (PartMode::TwoNxNd, 2),
            (PartMode::NLx2N, 2),
            (PartMode::NRx2N, 2),
        ];
        for (mode, n) in cases {
            assert_eq!(mode.num_pus(), n);
            let mut area = 0i64;
            for i in 0..n {
                let r = mode.pu_rect(0, 0, 32, i);
                assert!(r.x >= 0 && r.y >= 0 && r.x + r.w <= 32 && r.y + r.h <= 32);
                area += i64::from(r.w) * i64::from(r.h);
            }
            assert_eq!(area, 32 * 32, "{mode:?} does not tile its CU exactly");
        }
    }

    #[test]
    fn amvp_falls_back_to_zero_when_nothing_is_available() {
        let grid = CuGrid::new(&mut vaco_limits::Budget::new(vaco_limits::Limits::strict()), 64, 64).unwrap();
        let pu = PuRect { x: 0, y: 0, w: 16, h: 16 };
        let cands = derive_amvp_candidates(&grid, pu, 2, 10, 8, None);
        assert_eq!(cands, [Mv::ZERO, Mv::ZERO]);
    }

    #[test]
    fn merge_falls_back_to_zero_candidates_when_nothing_is_available() {
        let grid = CuGrid::new(&mut vaco_limits::Budget::new(vaco_limits::Limits::strict()), 64, 64).unwrap();
        let pu = PuRect { x: 0, y: 0, w: 16, h: 16 };
        let ref_poc_l0 = [100i64, 90i64];
        let cands = derive_merge_candidates(&grid, pu, 0, PartMode::TwoNx2N, 2, 5, &ref_poc_l0, None);
        assert_eq!(cands.len(), 5);
        for (i, c) in cands.iter().enumerate() {
            assert_eq!(c.mv, Mv::ZERO);
            assert_eq!(c.ref_poc, ref_poc_l0[i % 2]);
        }
    }
}
