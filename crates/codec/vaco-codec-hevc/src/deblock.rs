//! The in-loop deblocking filter, ITU-T H.265 §8.7.2, run once over the
//! whole picture after every CTU has been reconstructed.
//!
//! # Why this does not reuse `vaco-codec-dsp-deblock`
//!
//! That crate exists and `vaco-codec-h264` already calls it, so it was
//! checked directly (not assumed) before writing anything here — its own
//! module doc already says it hopes other codecs' equivalents can "land
//! here later the same way `vaco-codec-dsp-idct` grew from H.264-only to
//! four families." They cannot land *as-is*, though: its `EdgeThresholds`,
//! `filter_luma_line` and `filter_chroma_line` are clause 8.7's own H.264
//! algorithm, checked bin-for-bin against that crate's own tests, not a
//! generic "block deblocking" primitive. Cross-checked here against
//! `TComLoopFilter.cpp` (HM 18.0, BSD-3-Clause, Tier A — see this crate's
//! own `cabac_ctx` module doc for the clean-room posture), every stage
//! differs from the H.264 shape that crate implements:
//!
//! - **Different tables, indexed differently.** HEVC's `tC`/`beta` come from
//!   [`TC_TABLE`]/[`BETA_TABLE`] (HM's `sm_tcTable`/`sm_betaTable`), indexed
//!   by `qp + 2*(bS-1) + offset`; H.264's `ALPHA_TABLE`/`BETA_TABLE`/
//!   `TC0_TABLE` triple is indexed by `qp + offset` alone and has no `bS`
//!   term in the index at all.
//! - **A per-4-line-group decision, not a per-line one.** H.264 tests
//!   `filterSamplesFlag` on every single line. HEVC's `d < beta` gate
//!   ([`xUseStrongFiltering`]/`xCalcDP`/`xCalcDQ` in HM) is computed from
//!   only two lines of a four-line group (offsets 0 and 3) and then applied
//!   to all four — a genuinely coarser decision, not the same test run at a
//!   different cadence.
//! - **A different weak-filter delta and a wider strong filter.** HEVC's
//!   weak-filter delta is `(9*(q0-p0) - 3*(q1-p1) + 8) >> 4` with its own
//!   `thrCut` gate; H.264's is a different expression entirely. HEVC's
//!   strong filter can reach `p2`/`q2` with 5- and 6-tap sums no H.264
//!   equation matches.
//! - **Chroma has no activity gate at all.** H.264 chroma reruns the same
//!   `filterSamplesFlag` alpha/beta test luma uses. HEVC chroma filters
//!   unconditionally whenever `bS > 1` — there is no `alpha`/`beta` chroma
//!   test to share.
//!
//! A shared primitive would have to parameterise away every one of those,
//! which is not "the same filter, reused" — it is two different
//! clause/annex-shaped algorithms that happen to share the name
//! "deblocking". Kept in-crate instead, the same call `intra_pred.rs`
//! already made against `vaco-codec-dsp-intrapred` for the same reason (see
//! that module's own doc).
//!
//! # What this crate's own scope removes from HM's general derivation
//!
//! This decoder is I-slice-only, single-slice-segment, no tiles, and no
//! `cu_qp_delta` (see the crate doc) — `decoder::check_scope` refuses every
//! stream that would need more. That collapses two things HM's
//! `xGetBoundaryStrengthSingle`/`xEdgeFilterLuma` still carry for the
//! general case:
//!
//! - **Boundary strength is always 2** wherever an edge is marked at all.
//!   Table 8-12's `bS = 2` case is "either side is intra", and every CU this
//!   crate decodes is intra; the `bS = 1` (inter, non-zero luma CBF) and
//!   `bS = 0` cases can never be reached.
//! - **`qP_P`/`qP_Q` are looked up per edge from `CuGrid`'s own per-CU
//!   `QpY`** (`ctu::coding_unit`'s own post-transform-tree
//!   `CuGrid::fill_qp`), not one constant `slice_qp` — `cu_qp_delta` (see
//!   `ctu.rs`'s own module-level derivation) means two coding units either
//!   side of an edge can genuinely disagree, exactly HM's general
//!   `xEdgeFilterLuma`/`xEdgeFilterChroma` case (`iQP_P = pcCUP->getQP(...)`,
//!   `iQP_Q = pcCU->getQP(...)`, `iQP = (iQP_P + iQP_Q + 1) >> 1`) — this
//!   crate no longer collapses that average to a picture-wide constant. The
//!   chroma QP derivation (`transform::chroma_qp`) still needs only the one
//!   PPS-level `cb_qp_offset`/`cr_qp_offset` (never a slice-level one), and
//!   is still applied *after* averaging the two sides' plain luma `QpY` —
//!   §8.7.2.5.5's own chroma QP derivation names only the PPS offsets,
//!   confirmed directly against `xEdgeFilterChroma`'s own
//!   `iQP = ((iQP_P + iQP_Q + 1) >> 1) + chromaQPOffset`: the two sides'
//!   plain luma QPs are averaged *before* the chroma offset/scale, not
//!   mapped to chroma QP independently and averaged after.
//!
//! Boundary strength is still always 2 (a consequence of this crate's
//! intra-only scope, unrelated to `cu_qp_delta`).
//!
//! # The filtering grid
//!
//! HEVC never deblocks below an 8-luma-sample grid regardless of
//! `MinCbSizeY` — `DEBLOCK_SMALLEST_BLOCK` in HM is a fixed `8`, not derived
//! from `MinCbSizeY`, and any transform-tree split finer than
//! `MinCbSizeY` collapses into its enclosing `MinCbSizeY`-sized cell's own
//! edge rather than creating an independently-filtered one (confirmed by
//! reading `xSetEdgefilterTU`'s own `uiWidthInBaseUnits == 0` fallback,
//! which re-addresses the *whole* enclosing part rather than the leaf's own
//! finer rectangle). Since `MinCbSizeY >= 8` always (a structural
//! constraint HEVC itself imposes), using `1 << log2_min_cb_size` as the
//! luma grid reproduces this exactly for both the common case
//! (`MinCbSizeY == 8`, effectively every real encoder) and the rare
//! larger-`MinCbSizeY` case, with no separate branch needed —
//! [`framebuf::EdgeMarks`] already only ever marks edges on that grid.
//!
//! Chroma (4:2:0 — this crate's only scope) filters at a coarser grid still:
//! `DEBLOCK_SMALLEST_BLOCK` (8) *chroma* samples, i.e. 16 luma samples,
//! confirmed by `xEdgeFilterChroma`'s own skip test
//! (`uiEdgeNumInCtuVert % (DEBLOCK_SMALLEST_BLOCK/uiPelsInPartChromaH)`).
//! `chroma_grid = luma_grid.max(16)` reproduces that for both the common
//! (`luma_grid == 8`) and larger-`MinCbSizeY` cases.

use crate::ctu::Ctx;
use crate::framebuf::Plane;
use crate::transform::chroma_qp;

/// HM's `sm_tcTable`, `TComLoopFilter.cpp` — `MAX_QP + 1 +
/// DEFAULT_INTRA_TC_OFFSET` (`51 + 1 + 2`) entries, indexed by
/// `Clip3(0, 53, qp + 2*(bS-1) + tcOffsetDiv2*2)`.
const TC_TABLE: [i32; 54] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4,
    5, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24,
];

/// HM's `sm_betaTable`, `TComLoopFilter.cpp` — `MAX_QP + 1` (`52`) entries,
/// indexed by `Clip3(0, 51, qp + betaOffsetDiv2*2)`.
const BETA_TABLE: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 22, 24, 26,
    28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64,
];

/// `Clip3(-c, c, v)`, clause 8.7.2.3/8.7.2.4's own bound on a filter delta.
fn clip3_sym(c: i32, v: i32) -> i32 {
    v.clamp(-c, c)
}

/// `iIndexTC`/`sm_tcTable[iIndexTC]` for a given (already-derived, e.g. via
/// [`chroma_qp`] for chroma) QP — `bS == 2` always in this crate's
/// intra-only scope (see the module doc), so `2*(bS-1)` collapses to the
/// literal `2` HM calls `DEFAULT_INTRA_TC_OFFSET`.
fn tc_for_qp(qp: i32, tc_offset_div2: i32) -> i32 {
    let idx = (qp + 2 + tc_offset_div2 * 2).clamp(0, 53);
    TC_TABLE.get(usize::try_from(idx).unwrap_or(0)).copied().unwrap_or(0)
}

/// `iIndexB`/`sm_betaTable[iIndexB]`.
fn beta_for_qp(qp: i32, beta_offset_div2: i32) -> i32 {
    let idx = (qp + beta_offset_div2 * 2).clamp(0, 51);
    BETA_TABLE.get(usize::try_from(idx).unwrap_or(0)).copied().unwrap_or(0)
}

/// `ClipBD`, this crate's 8-bit-only scope (see the crate doc) collapsing
/// clause 8.7.2's general bit-depth clip to a fixed `0..=255`.
fn clip_bd(v: i32, bit_depth: u32) -> u16 {
    let max = (1i32 << bit_depth) - 1;
    u16::try_from(v.clamp(0, max)).unwrap_or(0)
}

/// One edge direction: which axis samples are gathered along, and which
/// axis steps as the edge is walked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    /// A vertical edge: p/q samples differ in `x`, the edge is walked in `y`.
    Vert,
    /// A horizontal edge: p/q samples differ in `y`, the edge is walked in `x`.
    Horiz,
}

/// The sample `k` steps from the edge (`k == 0` is q0, `k == -1` is p0,
/// matching HM's `piSrc[k * iOffset]` convention exactly), at position
/// `along` along the edge.
///
/// `across`/`along` name the two coordinates generically so every function
/// below this one is written once and works for both directions: for a
/// vertical edge, `across` is the x column that `k` steps across and
/// `along` is the row that stays fixed for one line; for a horizontal edge
/// the roles swap (`across` is the row, `along` is the column). Every
/// caller in this module already passes `(across, along)` in that order
/// regardless of `dir` — only this function and [`set_sample`] need to know
/// which axis `dir` maps each one to.
fn sample(plane: &Plane, dir: Dir, across: i32, along: i32, k: i32) -> i32 {
    let (x, y) = match dir {
        Dir::Vert => (across + k, along),
        Dir::Horiz => (along, across + k),
    };
    let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return 0 };
    i32::from(plane.get(x, y))
}

/// [`sample`]'s write counterpart.
fn set_sample(plane: &mut Plane, dir: Dir, across: i32, along: i32, k: i32, v: u16) {
    let (x, y) = match dir {
        Dir::Vert => (across + k, along),
        Dir::Horiz => (along, across + k),
    };
    if let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) {
        plane.set(x, y, v);
    }
}

/// `xUseStrongFiltering`: whether the strong (`bS == 4`-shaped, but reached
/// here purely from the `d`/`beta`/`tc` test — HEVC has no separate `bS == 4`
/// concept the way H.264 does) filter applies to one line.
#[allow(clippy::too_many_arguments, reason = "mirrors HM's own xUseStrongFiltering signature")]
fn use_strong_filtering(plane: &Plane, dir: Dir, bx: i32, by: i32, d2: i32, beta: i32, tc: i32) -> bool {
    let p3 = sample(plane, dir, bx, by, -4);
    let p0 = sample(plane, dir, bx, by, -1);
    let q0 = sample(plane, dir, bx, by, 0);
    let q3 = sample(plane, dir, bx, by, 3);
    let d_strong = (p3 - p0).abs() + (q3 - q0).abs();
    d_strong < (beta >> 3) && d2 < (beta >> 2) && (p0 - q0).abs() < ((tc * 5 + 1) >> 1)
}

/// `xPelFilterLuma`: filter one line (one fixed position along the edge)
/// against a precomputed `tc`/strong-or-weak decision.
#[allow(clippy::too_many_arguments, reason = "mirrors HM's own xPelFilterLuma signature")]
fn filter_luma_line(plane: &mut Plane, dir: Dir, bx: i32, by: i32, tc: i32, strong: bool, thr_cut: i32, filter_p: bool, filter_q: bool, bit_depth: u32) {
    let p3 = sample(plane, dir, bx, by, -4);
    let p2 = sample(plane, dir, bx, by, -3);
    let p1 = sample(plane, dir, bx, by, -2);
    let p0 = sample(plane, dir, bx, by, -1);
    let q0 = sample(plane, dir, bx, by, 0);
    let q1 = sample(plane, dir, bx, by, 1);
    let q2 = sample(plane, dir, bx, by, 2);
    let q3 = sample(plane, dir, bx, by, 3);

    if strong {
        // Clause 8.7.2.5.7's own filtered values, written as `Clip3(m -
        // 2*tc, m + 2*tc, formula)` in HM — algebraically identical to `m +
        // clip3_sym(2*tc, formula - m)`, which reads the same way this
        // module's other clip3-around-a-center calls already do.
        let two_tc = 2 * tc;
        let p0n = p0 + clip3_sym(two_tc, ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) - p0);
        let q0n = q0 + clip3_sym(two_tc, ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) - q0);
        let p1n = p1 + clip3_sym(two_tc, ((p2 + p1 + p0 + q0 + 2) >> 2) - p1);
        let q1n = q1 + clip3_sym(two_tc, ((p0 + q0 + q1 + q2 + 2) >> 2) - q1);
        let p2n = p2 + clip3_sym(two_tc, ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) - p2);
        let q2n = q2 + clip3_sym(two_tc, ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3) - q2);
        set_sample(plane, dir, bx, by, -1, clip_bd(p0n, bit_depth));
        set_sample(plane, dir, bx, by, 0, clip_bd(q0n, bit_depth));
        set_sample(plane, dir, bx, by, -2, clip_bd(p1n, bit_depth));
        set_sample(plane, dir, bx, by, 1, clip_bd(q1n, bit_depth));
        set_sample(plane, dir, bx, by, -3, clip_bd(p2n, bit_depth));
        set_sample(plane, dir, bx, by, 2, clip_bd(q2n, bit_depth));
        return;
    }

    let delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
    if delta.abs() >= thr_cut {
        return;
    }
    let delta = clip3_sym(tc, delta);
    set_sample(plane, dir, bx, by, -1, clip_bd(p0 + delta, bit_depth));
    set_sample(plane, dir, bx, by, 0, clip_bd(q0 - delta, bit_depth));
    let tc2 = tc >> 1;
    if filter_p {
        let delta1 = clip3_sym(tc2, (((p2 + p0 + 1) >> 1) - p1 + delta) >> 1);
        set_sample(plane, dir, bx, by, -2, clip_bd(p1 + delta1, bit_depth));
    }
    if filter_q {
        let delta2 = clip3_sym(tc2, (((q2 + q0 + 1) >> 1) - q1 - delta) >> 1);
        set_sample(plane, dir, bx, by, 1, clip_bd(q1 + delta2, bit_depth));
    }
}

/// `xPelFilterChroma`: unconditional (no activity gate — see the module
/// doc), single-formula filter of one chroma line.
fn filter_chroma_line(plane: &mut Plane, dir: Dir, bx: i32, by: i32, tc: i32, bit_depth: u32) {
    let p1 = sample(plane, dir, bx, by, -2);
    let p0 = sample(plane, dir, bx, by, -1);
    let q0 = sample(plane, dir, bx, by, 0);
    let q1 = sample(plane, dir, bx, by, 1);
    let delta = clip3_sym(tc, (((q0 - p0) << 2) + p1 - q1 + 4) >> 3);
    set_sample(plane, dir, bx, by, -1, clip_bd(p0 + delta, bit_depth));
    set_sample(plane, dir, bx, by, 0, clip_bd(q0 - delta, bit_depth));
}

/// `xCalcDP`/`xCalcDQ` combined with the per-4-line-group decision from
/// `xEdgeFilterLuma`'s own `iBlkIdx` loop: filter one 4-line group crossing
/// one luma edge, or do nothing if the group's own `d < beta` gate fails.
fn filter_luma_group(plane: &mut Plane, dir: Dir, bx: i32, by0: i32, tc: i32, beta: i32, bit_depth: u32) {
    let dp = |at: i32| (sample(plane, dir, bx, at, -3) - 2 * sample(plane, dir, bx, at, -2) + sample(plane, dir, bx, at, -1)).abs();
    let dq = |at: i32| (sample(plane, dir, bx, at, 0) - 2 * sample(plane, dir, bx, at, 1) + sample(plane, dir, bx, at, 2)).abs();

    let dp0 = dp(by0);
    let dq0 = dq(by0);
    let dp3 = dp(by0 + 3);
    let dq3 = dq(by0 + 3);
    let d = dp0 + dq0 + dp3 + dq3;
    if d >= beta {
        return;
    }
    let side_threshold = (beta + (beta >> 1)) >> 3;
    let filter_p = (dp0 + dp3) < side_threshold;
    let filter_q = (dq0 + dq3) < side_threshold;
    let strong = use_strong_filtering(plane, dir, bx, by0, 2 * (dp0 + dq0), beta, tc)
        && use_strong_filtering(plane, dir, bx, by0 + 3, 2 * (dp3 + dq3), beta, tc);
    let thr_cut = tc * 10;
    for i in 0..4 {
        filter_luma_line(plane, dir, bx, by0 + i, tc, strong, thr_cut, filter_p, filter_q, bit_depth);
    }
}

/// `qPav`/HM's `iQP`: the averaged luma `QpY` for an edge whose Q side (the
/// side named by the loop's own `(xq, yq)`) is looked up from
/// [`crate::framebuf::CuGrid::qp_at`], and whose P side is one CU-grid step
/// back along `dir`'s own perpendicular axis — `Dir::Vert` steps `x` (a
/// vertical edge's P/Q samples differ in `x`), `Dir::Horiz` steps `y`,
/// matching every other `dir`-generic helper in this module.
/// `s.slice_qp` is the fallback exactly where `qp_at` itself falls back to
/// unavailable: every in-bounds, fully-decoded position always has a real
/// value by the time this whole-picture pass runs, so the fallback only
/// matters, defensively, for a position outside the picture.
fn qp_avg(s: &Ctx<'_>, dir: Dir, xq: i32, yq: i32) -> i32 {
    let (xp, yp) = match dir {
        Dir::Vert => (xq - 1, yq),
        Dir::Horiz => (xq, yq - 1),
    };
    let qp_q = s.cu_grid.qp_at(xq, yq).map_or(s.slice_qp, i32::from);
    let qp_p = s.cu_grid.qp_at(xp, yp).map_or(s.slice_qp, i32::from);
    (qp_p + qp_q + 1) >> 1
}

/// Run the whole picture's deblocking pass: every vertical edge first (both
/// planes), then every horizontal edge (both planes) — matching
/// `TComLoopFilter::loopFilterPic`'s own two full, separate passes, since
/// horizontal filtering must see vertical filtering's own output.
pub(crate) fn filter_picture(s: &mut Ctx<'_>) {
    if s.deblocking_disabled {
        return;
    }
    let grid = 1i32 << s.log2_min_cb_size;
    let chroma_grid = grid.max(16);

    let (width, height) = s.pic.y.dims();
    let (width, height) = (i32::try_from(width).unwrap_or(0), i32::try_from(height).unwrap_or(0));

    // Vertical edges: luma at every `grid` column, chroma at every
    // `chroma_grid` column (see the module doc for why chroma is coarser).
    let mut x = grid;
    while x < width {
        let mut y = 0;
        while y < height {
            if s.edges.vert_at(x, y) {
                let qp = qp_avg(s, Dir::Vert, x, y);
                let tc = tc_for_qp(qp, s.tc_offset_div2);
                let beta = beta_for_qp(qp, s.beta_offset_div2);
                filter_luma_group(&mut s.pic.y, Dir::Vert, x, y, tc, beta, s.bit_depth_luma);
            }
            y += 4;
        }
        x += grid;
    }
    let mut x = chroma_grid;
    while x < width {
        let mut y = 0;
        while y < height {
            if s.edges.vert_at(x, y) {
                let qp = qp_avg(s, Dir::Vert, x, y);
                let cb_tc = tc_for_qp(chroma_qp(qp, s.cb_qp_offset), s.tc_offset_div2);
                let cr_tc = tc_for_qp(chroma_qp(qp, s.cr_qp_offset), s.tc_offset_div2);
                let cx = x >> 1;
                let cy0 = y >> 1;
                let rows = (grid >> 1).max(1);
                for i in 0..rows {
                    filter_chroma_line(&mut s.pic.cb, Dir::Vert, cx, cy0 + i, cb_tc, s.bit_depth_chroma);
                    filter_chroma_line(&mut s.pic.cr, Dir::Vert, cx, cy0 + i, cr_tc, s.bit_depth_chroma);
                }
            }
            y += grid;
        }
        x += chroma_grid;
    }

    // Horizontal edges, over the vertically-filtered picture.
    let mut y = grid;
    while y < height {
        let mut x = 0;
        while x < width {
            if s.edges.horiz_at(x, y) {
                let qp = qp_avg(s, Dir::Horiz, x, y);
                let tc = tc_for_qp(qp, s.tc_offset_div2);
                let beta = beta_for_qp(qp, s.beta_offset_div2);
                filter_luma_group(&mut s.pic.y, Dir::Horiz, y, x, tc, beta, s.bit_depth_luma);
            }
            x += 4;
        }
        y += grid;
    }
    let mut y = chroma_grid;
    while y < height {
        let mut x = 0;
        while x < width {
            if s.edges.horiz_at(x, y) {
                let qp = qp_avg(s, Dir::Horiz, x, y);
                let cb_tc = tc_for_qp(chroma_qp(qp, s.cb_qp_offset), s.tc_offset_div2);
                let cr_tc = tc_for_qp(chroma_qp(qp, s.cr_qp_offset), s.tc_offset_div2);
                let cy = y >> 1;
                let cx0 = x >> 1;
                let cols = (grid >> 1).max(1);
                for i in 0..cols {
                    filter_chroma_line(&mut s.pic.cb, Dir::Horiz, cy, cx0 + i, cb_tc, s.bit_depth_chroma);
                    filter_chroma_line(&mut s.pic.cr, Dir::Horiz, cy, cx0 + i, cr_tc, s.bit_depth_chroma);
                }
            }
            x += grid;
        }
        y += chroma_grid;
    }
}
