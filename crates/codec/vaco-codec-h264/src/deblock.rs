//! Whole-picture wiring for clause 8.7's deblocking filter, built on
//! [`vaco_codec_dsp_deblock`]'s pure per-edge primitives.
//!
//! This module owns exactly what that crate's own doc says stays in the
//! caller: walking the picture in macroblock raster order, deriving
//! boundary strength from macroblock coding mode (clause 8.7.2.1),
//! honouring `disable_deblocking_filter_idc`, and the vertical-then-
//! horizontal, left-to-right-then-top-to-bottom filtering order clause
//! 8.7 itself specifies -- each edge's filtering uses samples already
//! modified by every earlier edge in that same order, which is why this
//! is a second, ordered pass over already-reconstructed pixels rather
//! than something foldable into [`crate::reconstruct::reconstruct_picture_luma`]'s
//! own single top-to-bottom, left-to-right walk: a macroblock's own
//! *right* and *bottom* edges are not yet known when reconstruction visits
//! it, but deblocking only ever needs a macroblock's *left*/*top* neighbour,
//! which reconstruction's own raster order already guarantees is complete
//! (finished decoding, *and* finished deblocking, since deblocking follows
//! the same raster order) by the time this pass reaches it.
//!
//! # Boundary strength, general case (clause 8.7.2.1, Table 8-18)
//!
//! [`boundary_strength`] implements the full non-MBAFF table: either side
//! `Intra`/`I_PCM` gives `bS = 4` at a macroblock edge and `3` at an
//! internal one; otherwise `bS = 2` when either side's own 4x4 luma block
//! has a nonzero transform coefficient; otherwise `bS = 1` when the two
//! sides use a different set of reference pictures or a matching pair of
//! motion vectors differs by at least 4 quarter-luma-samples in either
//! component; otherwise `bS = 0`.
//!
//! # Two reference lists (B slices)
//!
//! A P slice's own `MvInfo` never populates list 1 (`ref_idx_l1() == -1`,
//! `mv_l1() == (0, 0)` by construction -- see that type's own doc), so a
//! single-list `ref_idx`/`mv` comparison was the whole rule until B-slice
//! support existed. It is not the general rule: clause 8.7.2.1's own text
//! compares reference *pictures*, not list-relative `ref_idx` values, and a
//! bi-predicted block's two motion vectors can legitimately need matching
//! against the *other* side's lists with list 0 and list 1 swapped, when
//! the two sides reference the same two pictures through different lists
//! -- x264 does this whenever `RefPicList1[0]` equals some `RefPicList0[k]`,
//! which is common for a single-reference-each-way B slice.
//!
//! [`boundary_strength`]'s two-list branch is transcribed from JM 19.1's
//! `get_strength_ver`/`get_strength_hor` (`loop_filter_normal.c`, Tier A per
//! `provenance/sources.toml`) rather than re-derived from the specification
//! prose a second time: reference-picture identity there is a `StorablePicture*`
//! pointer, which this crate has no equivalent of at deblocking time (motion
//! is stored per 4x4 block as a list-relative `ref_idx`, not a picture
//! pointer) -- POC stands in for it instead ([`boundary_strength`]'s own
//! `ref_list0_poc`/`ref_list1_poc` parameters, one entry per active
//! `RefPicList0`/`RefPicList1` position, built by the caller from the same
//! DPB lookup `crate::decoder` already does for reconstruction). Two
//! distinct pictures never share a POC within the one coded video sequence
//! this crate ever has open at once (no long-term references, no multiple
//! sequences in flight -- both explicitly out of scope, `decoder.rs`'s own
//! refusals), so POC equality is exactly picture-pointer equality for every
//! input this crate accepts. JM's `compare_mvs(a, b, mvlimit)` itself is the
//! same per-component `>= 4` test [`boundary_strength`]'s own single-list
//! branch already used -- [`mv_differs`] is that primitive, shared by both.
//!
//! **Scope, explicitly, not merely unimplemented**: MBAFF/field pictures
//! (this crate does not decode them at all -- see `decoder.rs`'s own
//! refusal).
//!
//! # Chroma (clause 8.7, `EdgeLoopChromaVer`/`Hor` in the same reference)
//!
//! 4:2:0 chroma reuses the *luma-derived* boundary strength at half
//! resolution rather than deriving its own -- clause 8.7.2.1 defines `bS`
//! once per luma edge position, and chroma is filtered "using the value of
//! `bS` ... derived for the luma edge". Concretely (matching JM's own
//! `Strength[pel >> 1]` indexing in `loop_filter_normal.c`): of luma's four
//! per-macroblock edge positions (local sample offset 0/4/8/12), only 0 and
//! 8 have a corresponding chroma sample column/row at all for 4:2:0 (chroma
//! is exactly half width/height), and each of luma's four per-4-row/column
//! `bS` groups along one edge covers exactly two chroma samples at that
//! edge. [`deblock_picture_chroma`] recomputes `bS` at luma granularity
//! internally (cheap -- it is a handful of integer comparisons, not a
//! table lookup) rather than threading luma's own per-edge `bS` array
//! through the call boundary, which would otherwise make the two functions'
//! argument lists mirror each other for no reader-visible benefit.
use vaco_codec_dsp_deblock::{EdgeThresholds, batch};
use vaco_simd::Caps;

use crate::mb::MbSummary;

/// Whether `mb`'s own coding mode makes it intra for clause 8.7.2.1's
/// purposes: `Intra_4x4`, `Intra_8x8`, `Intra_16x16` and `I_PCM` all take
/// the same `bS` equal 4 (macroblock edge) or 3 (internal edge) branch,
/// since `I_PCM` has no motion or transform coefficients to compare either.
///
/// `Intra_8x8` was missing from this list, which is not a cosmetic
/// omission: clause 8.7.2.1's intra branch is the *first* test, so an
/// `Intra_8x8` macroblock fell through to the coefficient and motion-vector
/// tests written for inter content and came out `bS = 2` (it has residual)
/// where the answer is `4` at a macroblock edge and `3` internally --
/// a weaker filter, on every edge of every `Intra_8x8` macroblock, i.e. on
/// most of every intra picture in High-profile content, which is x264's
/// own default.
const fn is_intra(mb: &MbSummary) -> bool {
    mb.is_intra4x4 || mb.is_intra8x8 || mb.is_intra16x16 || mb.is_ipcm
}

/// Converts a raster-ordered 4x4 luma block index (`row * 4 + col`, 0..16)
/// into `luma4x4BlkIdx`, the z-scan order of clause 6.4.3.
///
/// `MbSummary::mv_blocks` is raster-ordered (its own doc says so verbatim),
/// but `MbSummary::residual.luma_ac` is indexed by `luma4x4BlkIdx`, the
/// same order `residual_luma()` decodes coefficients in and
/// `crate::mb::blk_xy` maps back to pixel coordinates from. This module
/// addresses every 4x4 block by its raster position, matching how a
/// deblocking edge's own row/column naturally enumerates, so a residual
/// lookup needs converting first -- the inverse of `crate::mb::blk_xy`.
#[allow(
    clippy::integer_division,
    reason = "bx/by are each 0..4 (a 4x4 grid coordinate); dividing by 2 to find the 8x8 quadrant \
              is exact by construction, never a bitstream-derived value"
)]
const fn raster_to_luma4x4_blk_idx(raster: usize) -> usize {
    let bx = (raster % 4) as u32;
    let by = (raster / 4) as u32;
    let quadrant = (by / 2) * 2 + bx / 2;
    let within = (by % 2) * 2 + bx % 2;
    (quadrant * 4 + within) as usize
}

/// Whether the 4x4 luma block at raster position `raster` (0..16, within
/// its own macroblock) has at least one nonzero transform coefficient
/// level: clause 8.7.2.1's own "the block containing sample p0 [or q0]"
/// condition for `bS` equal 2. Only meaningful for a non-intra macroblock
/// -- an `Intra_16x16` block's shared DC coefficient never enters this,
/// since [`is_intra`] always wins first for any macroblock this could
/// apply to.
#[allow(
    clippy::integer_division,
    reason = "raster is 0..16 (a 4x4 grid position); /4 and /2 are that grid's own row and \
              8x8-quadrant coordinates, exact by construction, never a bitstream-derived value"
)]
fn has_luma_coeffs(mb: &MbSummary, raster: usize) -> bool {
    if mb.transform_8x8 {
        // With `transform_size_8x8_flag` set there are no 4x4 luma
        // transform blocks at all -- `MbResidual::luma_ac` is all-`None`
        // and the coefficients live in `luma8x8[luma8x8BlkIdx]` instead.
        // Clause 8.7.2.1's "the luma block containing sample p0" is then
        // the 8x8 block, so every 4x4 position inside a coded quadrant
        // answers `true`. Reading `luma_ac` here (as this function did)
        // answered `false` for every position of every 8x8-transform
        // macroblock, which silently turned every `bS = 2` edge in High
        // profile content into `bS = 1` or `0`. JM records the same thing
        // by setting all four of the 8x8 block's bits in its own
        // `s_cbp[0].blk` 4x4 bitmask.
        let quadrant = (raster / 4 / 2) * 2 + (raster % 4) / 2;
        return mb
            .residual
            .luma8x8
            .get(quadrant)
            .is_some_and(|slot| slot.as_ref().is_some_and(|r| !r.levels.is_empty()));
    }
    mb.residual
        .luma_ac
        .get(raster_to_luma4x4_blk_idx(raster))
        .is_some_and(|slot| slot.as_ref().is_some_and(|r| !r.levels.is_empty()))
}

/// Clause 8.7's `filterInternalEdgesFlag` companion for the 8x8 transform:
/// when `transform_size_8x8_flag` is 1 the macroblock has no 4x4 transform
/// block boundaries at luma offsets 4 and 12, so those two internal *luma*
/// edges are not filtered at all (JM's own
/// `filterNon8x8LumaEdgesFlag[1] = filterNon8x8LumaEdgesFlag[3] =
/// !luma_transform_size_8x8_flag`, `loopfilter.c`'s `DeblockMb`). Chroma
/// is unaffected: at 4:2:0 only luma offsets 0 and 8 have a corresponding
/// chroma edge in the first place.
const fn filters_luma_edge(mb: &MbSummary, local: u32) -> bool {
    !(mb.transform_8x8 && (local == 4 || local == 12))
}

/// JM 19.1's `compare_mvs(a, b, mvlimit)` (`loop_filter_normal.c`) for the
/// non-MBAFF `mvlimit == 4` case this crate's frame-only scope always uses:
/// `true` when the two motion vectors differ by at least 4 quarter-luma-samples
/// in either component. Shared by [`boundary_strength`]'s single- and
/// two-list branches -- see this module's own doc.
fn mv_differs(a: (i16, i16), b: (i16, i16)) -> bool {
    a.0.abs_diff(b.0) >= 4 || a.1.abs_diff(b.1) >= 4
}

/// One list's reference-picture identity for one 4x4 block, as a POC --
/// `None` when that list is not used at all (`ref_idx < 0`, [`crate::mb::MvInfo`]'s
/// own "this list not read" convention), matching JM's `NULL` `ref_pic`
/// pointer for the same case. `list_poc` is the current picture's own
/// `RefPicList0`/`RefPicList1` (whichever `ref_idx` indexes into), one POC
/// per active position -- see this module's own doc for why POC stands in
/// for JM's picture pointer here.
fn ref_poc(list_poc: &[i32], ref_idx: i8) -> Option<i32> {
    let idx = usize::try_from(ref_idx).ok()?;
    list_poc.get(idx).copied()
}

/// Clause 8.7.2.1's non-intra case (see this module's own doc for the
/// two-list generalisation and its JM provenance). `p`/`q` are the two 4x4
/// luma blocks on either side of the edge -- the same macroblock and a
/// different block index for an internal edge, two different macroblocks
/// for a macroblock edge. `ref_list0_poc`/`ref_list1_poc` are the *current*
/// picture's own reference lists (both empty for an I slice, `ref_list1_poc`
/// empty for a P slice -- every `MvInfo` in scope then has `ref_idx_l1() ==
/// -1` regardless, so [`ref_poc`] returns `None` for list 1 on both sides
/// and every comparison below degenerates to the single-list rule this
/// function used before B slices existed).
fn boundary_strength(
    mb_edge: bool,
    p_mb: &MbSummary,
    p_blk: usize,
    q_mb: &MbSummary,
    q_blk: usize,
    ref_list0_poc: &[i32],
    ref_list1_poc: &[i32],
) -> u8 {
    if is_intra(p_mb) || is_intra(q_mb) {
        return if mb_edge { 4 } else { 3 };
    }
    if has_luma_coeffs(p_mb, p_blk) || has_luma_coeffs(q_mb, q_blk) {
        return 2;
    }
    let p_mv = p_mb.mv_blocks.get(p_blk).copied().unwrap_or_default();
    let q_mv = q_mb.mv_blocks.get(q_blk).copied().unwrap_or_default();

    let p0 = ref_poc(ref_list0_poc, p_mv.ref_idx_l0());
    let p1 = ref_poc(ref_list1_poc, p_mv.ref_idx_l1());
    let q0 = ref_poc(ref_list0_poc, q_mv.ref_idx_l0());
    let q1 = ref_poc(ref_list1_poc, q_mv.ref_idx_l1());

    // JM's own `(ref_p0==ref_q0 && ref_p1==ref_q1) || (ref_p0==ref_q1 &&
    // ref_p1==ref_q0)`: the two sides use the same *set* of reference
    // pictures, matched either directly by list or swapped across lists.
    // `None == None` here plays the same role JM's `NULL == NULL` does for
    // an unused list on both sides -- both `Option<i32>` comparisons below
    // are total, no separate "list not used" case needed.
    let same_set_direct = p0 == q0 && p1 == q1;
    let same_set_swapped = p0 == q1 && p1 == q0;
    if !(same_set_direct || same_set_swapped) {
        return 1;
    }

    let differs = if p0 == p1 {
        // This block's own two lists reference the *same* picture twice
        // (and, by `same_set_direct`/`same_set_swapped` above, so does the
        // other side's) -- JM requires *both* the direct and the swapped
        // pairing to each have a small-enough difference before calling it
        // unchanged, not just the better of the two orderings.
        (mv_differs(p_mv.mv_l0(), q_mv.mv_l0()) || mv_differs(p_mv.mv_l1(), q_mv.mv_l1()))
            && (mv_differs(p_mv.mv_l0(), q_mv.mv_l1()) || mv_differs(p_mv.mv_l1(), q_mv.mv_l0()))
    } else {
        // The two lists reference two distinct pictures (the ordinary
        // case): match each side's motion to whichever of the other side's
        // lists points at the *same* picture, direct or swapped.
        if p0 == q0 {
            mv_differs(p_mv.mv_l0(), q_mv.mv_l0()) || mv_differs(p_mv.mv_l1(), q_mv.mv_l1())
        } else {
            mv_differs(p_mv.mv_l0(), q_mv.mv_l1()) || mv_differs(p_mv.mv_l1(), q_mv.mv_l0())
        }
    };
    u8::from(differs)
}

/// A macroblock grid addressable by `(mb_x, mb_y)`, `None` outside the
/// picture -- the shared lookup both [`deblock_picture_luma`] and
/// [`deblock_picture_chroma`] build once per picture rather than each
/// re-scanning `macroblocks` per edge.
struct MbGrid<'a> {
    mbs_wide: u32,
    mbs_high: u32,
    by_addr: Vec<Option<&'a MbSummary>>,
}

impl<'a> MbGrid<'a> {
    fn new(macroblocks: &'a [MbSummary], mbs_wide: u32, mbs_high: u32) -> Self {
        let n = usize::try_from(mbs_wide.saturating_mul(mbs_high)).unwrap_or(0);
        let mut by_addr = vec![None; n];
        for mb in macroblocks {
            let idx = (mb.mb_y.saturating_mul(mbs_wide) + mb.mb_x) as usize;
            if let Some(slot) = by_addr.get_mut(idx) {
                *slot = Some(mb);
            }
        }
        Self { mbs_wide, mbs_high, by_addr }
    }

    fn at(&self, mx: u32, my: u32) -> Option<&'a MbSummary> {
        if mx >= self.mbs_wide || my >= self.mbs_high {
            return None;
        }
        self.by_addr.get((my * self.mbs_wide + mx) as usize).copied().flatten()
    }

    fn qpy(&self, mx: u32, my: u32) -> u8 {
        let v = self.at(mx, my).map_or(0, |mb| mb.qpy);
        u8::try_from(v.clamp(0, 51)).unwrap_or(51)
    }
}

/// Runs clause 8.7's deblocking filter over an already-fully-reconstructed
/// luma plane, in place.
///
/// `disable_deblocking_filter_idc` is the slice header field verbatim (`0`
/// = filter everything this crate can see, including what would be a
/// slice boundary if this decoder supported multiple slices per picture
/// yet; `1` = do not filter this slice's own macroblocks at all; `2` =
/// filter internal edges but not the picture's own slice-boundary edges
/// -- indistinguishable from `0` here today, since every fixture this
/// crate decodes is one slice per whole picture, so there is no internal
/// slice boundary within a picture to treat differently). `slice_alpha_c0_offset_div2`/
/// `slice_beta_offset_div2` are the slice header fields verbatim; this
/// function applies clause 8.7.2.2's own `* 2` itself. `ref_list0_poc`/
/// `ref_list1_poc` are this picture's own `RefPicList0`/`RefPicList1`, as
/// POCs -- see [`boundary_strength`]'s own doc for why POC and not
/// `ref_idx`; both empty for an I slice, `ref_list1_poc` empty for a P
/// slice.
///
/// # Errors
///
/// This function no longer refuses non-intra macroblocks (clause 8.7.2.1's
/// general `bS` derivation is implemented -- see this module's own doc) --
/// kept fallible for interface stability with its own tests and the CABAC
/// engine's own `Result`-returning conventions elsewhere in this crate, but
/// no path in this function currently returns `Err`.
/// The whole-picture form: [`DeblockCtx::luma_mb_row`] over every macroblock
/// row, in order. The shipping decoder drives the rows itself (see
/// [`crate::reconstruct::reconstruct_picture_rows`]) so it can publish finished
/// rows early; this stays as the order-independent form the module's own tests
/// check that schedule against.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the row-driven schedule is what the decoder uses; this is its test oracle")
)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "kept Result for interface/call-site stability -- see this function's own doc"
)]
#[allow(
    clippy::integer_division,
    reason = "every division below is by the constant 4 (4x4 luma block granularity) over a loop \
              variable bounded 0..16 -- exact by construction, never a bitstream-derived value"
)]
pub(crate) fn deblock_picture_luma(
    luma: &mut [u8],
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    disable_deblocking_filter_idc: u32,
    slice_alpha_c0_offset_div2: i32,
    slice_beta_offset_div2: i32,
    ref_list0_poc: &[i32],
    ref_list1_poc: &[i32],
) -> vaco_core::Result<()> {
    if disable_deblocking_filter_idc == 1 {
        return Ok(());
    }

    let ctx = DeblockCtx::new(
        macroblocks,
        mbs_wide,
        mbs_high,
        slice_alpha_c0_offset_div2,
        slice_beta_offset_div2,
        ref_list0_poc,
        ref_list1_poc,
    );
    for my in 0..mbs_high {
        ctx.luma_mb_row(luma, my);
    }

    Ok(())
}


/// Runs clause 8.7's deblocking filter over one already-fully-reconstructed
/// 4:2:0 chroma plane (`Cb` or `Cr`), in place -- the chroma sibling of
/// [`deblock_picture_luma`], reusing luma-derived boundary strength at half
/// resolution (see this module's own doc for the exact mapping).
///
/// `chroma_qp_offset` is `chroma_qp_index_offset`/`second_chroma_qp_index_offset`
/// (PPS, verbatim) for whichever of `Cb`/`Cr` `chroma` is; `macroblocks` and
/// the remaining parameters mirror [`deblock_picture_luma`] exactly.
/// [`deblock_picture_luma`]'s chroma sibling, and kept for the same reason.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the row-driven schedule is what the decoder uses; this is its test oracle")
)]
#[allow(
    clippy::integer_division,
    reason = "every division below is by the constant 2 or 4 (chroma-to-luma 4x4 block granularity) \
              over a loop variable bounded 0..8 -- exact by construction, never a bitstream-derived value"
)]
pub(crate) fn deblock_picture_chroma(
    chroma: &mut [u8],
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    chroma_qp_offset: i32,
    disable_deblocking_filter_idc: u32,
    slice_alpha_c0_offset_div2: i32,
    slice_beta_offset_div2: i32,
    ref_list0_poc: &[i32],
    ref_list1_poc: &[i32],
) {
    if disable_deblocking_filter_idc == 1 {
        return;
    }

    let ctx = DeblockCtx::new(
        macroblocks,
        mbs_wide,
        mbs_high,
        slice_alpha_c0_offset_div2,
        slice_beta_offset_div2,
        ref_list0_poc,
        ref_list1_poc,
    );
    for my in 0..mbs_high {
        ctx.chroma_mb_row(chroma, chroma_qp_offset, my);
    }
}

/// Everything clause 8.7's filter needs that is constant across a picture,
/// held once so a single macroblock row can be filtered on its own.
///
/// The whole-picture entry points below are loops over
/// [`DeblockCtx::luma_mb_row`]/[`DeblockCtx::chroma_mb_row`] and nothing else;
/// the reason those rows are separately callable is that
/// [`crate::frame_task`] interleaves them with reconstruction so a row becomes
/// final -- and therefore publishable to a picture still being predicted from
/// -- before the whole picture is. See `docs/codec/frame-threading.md`.
pub(crate) struct DeblockCtx<'a> {
    caps: Caps,
    grid: MbGrid<'a>,
    mbs_wide: u32,
    filter_offset_a: i32,
    filter_offset_b: i32,
    ref_list0_poc: &'a [i32],
    ref_list1_poc: &'a [i32],
}

impl<'a> DeblockCtx<'a> {
    /// Build the per-picture state. `slice_alpha_c0_offset_div2`/
    /// `slice_beta_offset_div2` are the slice header fields verbatim; clause
    /// 8.7.2.2's own `* 2` is applied here, once, rather than per edge.
    pub(crate) fn new(
        macroblocks: &'a [MbSummary],
        mbs_wide: u32,
        mbs_high: u32,
        slice_alpha_c0_offset_div2: i32,
        slice_beta_offset_div2: i32,
        ref_list0_poc: &'a [i32],
        ref_list1_poc: &'a [i32],
    ) -> Self {
        Self {
            caps: Caps::detect(),
            grid: MbGrid::new(macroblocks, mbs_wide, mbs_high),
            mbs_wide,
            filter_offset_a: slice_alpha_c0_offset_div2.saturating_mul(2),
            filter_offset_b: slice_beta_offset_div2.saturating_mul(2),
            ref_list0_poc,
            ref_list1_poc,
        }
    }

    /// Filter macroblock row `my` of the luma plane, in the exact edge order
    /// clause 8.7 specifies -- left-to-right by macroblock, vertical edges
    /// before horizontal ones within each.
    ///
    /// Reads rows `my * 16 - 4 ..= my * 16 + 15` and writes
    /// `my * 16 - 3 ..= my * 16 + 14`: the three rows above the macroblock row
    /// belong to the row above and are its top macroblock edge's `p0`/`p1`/`p2`.
    /// That overhang is why a row is only final once the *next* row has been
    /// filtered.
    #[allow(
        clippy::integer_division,
        reason = "every division below is by the constant 4 (4x4 luma block granularity) over a loop \
                  variable bounded 0..16 -- exact by construction, never a bitstream-derived value"
    )]
    pub(crate) fn luma_mb_row(&self, luma: &mut [u8], my: u32) {
        let caps = self.caps;
        let grid = &self.grid;
        let filter_offset_a = self.filter_offset_a;
        let filter_offset_b = self.filter_offset_b;
        let ref_list0_poc = self.ref_list0_poc;
        let ref_list1_poc = self.ref_list1_poc;
        let mbs_wide = self.mbs_wide;
        let width = mbs_wide.saturating_mul(16);

        let get = |luma: &[u8], x: u32, y: u32| -> u8 { luma.get((y * width + x) as usize).copied().unwrap_or(0) };
        let set = |luma: &mut [u8], x: u32, y: u32, v: u8| {
            if let Some(slot) = luma.get_mut((y * width + x) as usize) {
                *slot = v;
            }
        };

        for mx in 0..mbs_wide {
            let Some(here) = grid.at(mx, my) else { continue };
            let qp_here = grid.qpy(mx, my);

            // Vertical edges first, left to right (clause 8.7's own
            // filtering order) -- edge `local == 0` is this macroblock's
            // shared boundary with its left neighbour; `4`/`8`/`12` are
            // internal to this macroblock alone.
            for local in [0u32, 4, 8, 12] {
                if local == 0 && mx == 0 {
                    continue;
                }
                if !filters_luma_edge(here, local) {
                    continue;
                }
                let mb_edge = local == 0;
                let (p_mb, qp_p) = if mb_edge {
                    #[allow(clippy::unwrap_used, reason = "mx > 0 here, checked above")]
                    (grid.at(mx - 1, my).unwrap(), grid.qpy(mx - 1, my))
                } else {
                    (here, qp_here)
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let x = mx * 16 + local;

                // Gather every line along this edge (16 rows, the batch
                // `vaco_codec_dsp_deblock::batch::filter_luma_edge` needs to
                // fill a vector register -- see that module's own doc for
                // why one line at a time cannot) before filtering once and
                // scattering back. `bs == 0` rows are included rather than
                // skipped: the batched primitive treats that as "leave this
                // line unmodified" itself, so writing every gathered value
                // straight back is always correct, not merely correct when
                // every row happens to have positive strength.
                let mut p0a = [0u8; 16];
                let mut p1a = [0u8; 16];
                let mut p2a = [0u8; 16];
                let mut p3a = [0u8; 16];
                let mut q0a = [0u8; 16];
                let mut q1a = [0u8; 16];
                let mut q2a = [0u8; 16];
                let mut q3a = [0u8; 16];
                let mut bsa = [0u8; 16];
                // `boundary_strength` is a pure function of `(mb_edge, p_blk,
                // q_blk)` here, and all four rows of one `blk_row` group
                // share the same `p_blk`/`q_blk` (both are `blk_row`-derived,
                // not `row`-derived) -- clause 8.7.2.1 defines one `bS` per
                // 4x4 luma block, not per pixel row. Calling it once per
                // group of 4 instead of once per row (4x fewer calls) does
                // not change a single `bS` value; it only stops recomputing
                // the one this loop already knows.
                let mut bs_by_blk_row = [0u8; 4];
                for (blk_row, slot) in bs_by_blk_row.iter_mut().enumerate() {
                    let q_blk = blk_row * 4 + (local / 4) as usize;
                    let p_blk = if mb_edge { blk_row * 4 + 3 } else { blk_row * 4 + (local / 4 - 1) as usize };
                    *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk, ref_list0_poc, ref_list1_poc);
                }
                for row in 0..16u32 {
                    let y = my * 16 + row;
                    let ri = row as usize;
                    if let Some(slot) = bsa.get_mut(ri) {
                        *slot = bs_by_blk_row.get(ri / 4).copied().unwrap_or(0);
                    }
                    if let Some(slot) = p0a.get_mut(ri) {
                        *slot = get(luma, x - 1, y);
                    }
                    if let Some(slot) = p1a.get_mut(ri) {
                        *slot = get(luma, x - 2, y);
                    }
                    if let Some(slot) = p2a.get_mut(ri) {
                        *slot = get(luma, x - 3, y);
                    }
                    if let Some(slot) = p3a.get_mut(ri) {
                        *slot = get(luma, x - 4, y);
                    }
                    if let Some(slot) = q0a.get_mut(ri) {
                        *slot = get(luma, x, y);
                    }
                    if let Some(slot) = q1a.get_mut(ri) {
                        *slot = get(luma, x + 1, y);
                    }
                    if let Some(slot) = q2a.get_mut(ri) {
                        *slot = get(luma, x + 2, y);
                    }
                    if let Some(slot) = q3a.get_mut(ri) {
                        *slot = get(luma, x + 3, y);
                    }
                }
                batch::filter_luma_edge(
                    caps, &mut p0a, &mut p1a, &mut p2a, &p3a, &mut q0a, &mut q1a, &mut q2a, &q3a, &bsa, edge,
                );
                for row in 0..16u32 {
                    let y = my * 16 + row;
                    let ri = row as usize;
                    set(luma, x - 1, y, p0a.get(ri).copied().unwrap_or(0));
                    set(luma, x - 2, y, p1a.get(ri).copied().unwrap_or(0));
                    set(luma, x - 3, y, p2a.get(ri).copied().unwrap_or(0));
                    set(luma, x, y, q0a.get(ri).copied().unwrap_or(0));
                    set(luma, x + 1, y, q1a.get(ri).copied().unwrap_or(0));
                    set(luma, x + 2, y, q2a.get(ri).copied().unwrap_or(0));
                }
            }

            // Then horizontal edges, top to bottom -- edge `local == 0` is
            // this macroblock's shared boundary with its above neighbour,
            // which by raster order has already had *both* its vertical
            // and horizontal edges filtered.
            for local in [0u32, 4, 8, 12] {
                if local == 0 && my == 0 {
                    continue;
                }
                if !filters_luma_edge(here, local) {
                    continue;
                }
                let mb_edge = local == 0;
                let (p_mb, qp_p) = if mb_edge {
                    #[allow(clippy::unwrap_used, reason = "my > 0 here, checked above")]
                    (grid.at(mx, my - 1).unwrap(), grid.qpy(mx, my - 1))
                } else {
                    (here, qp_here)
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let y = my * 16 + local;

                // Same batching as the vertical pass above, transposed:
                // gather all 16 columns along this edge, filter once,
                // scatter back.
                let mut p0a = [0u8; 16];
                let mut p1a = [0u8; 16];
                let mut p2a = [0u8; 16];
                let mut p3a = [0u8; 16];
                let mut q0a = [0u8; 16];
                let mut q1a = [0u8; 16];
                let mut q2a = [0u8; 16];
                let mut q3a = [0u8; 16];
                let mut bsa = [0u8; 16];
                // Same 4x reduction as the vertical pass above: one `bS`
                // per `blk_col` group of 4 columns, not per column.
                let mut bs_by_blk_col = [0u8; 4];
                for (blk_col, slot) in bs_by_blk_col.iter_mut().enumerate() {
                    let q_blk = (local / 4) as usize * 4 + blk_col;
                    let p_blk = if mb_edge { 12 + blk_col } else { (local / 4 - 1) as usize * 4 + blk_col };
                    *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk, ref_list0_poc, ref_list1_poc);
                }
                for col in 0..16u32 {
                    let x = mx * 16 + col;
                    let ci = col as usize;
                    if let Some(slot) = bsa.get_mut(ci) {
                        *slot = bs_by_blk_col.get(ci / 4).copied().unwrap_or(0);
                    }
                    if let Some(slot) = p0a.get_mut(ci) {
                        *slot = get(luma, x, y - 1);
                    }
                    if let Some(slot) = p1a.get_mut(ci) {
                        *slot = get(luma, x, y - 2);
                    }
                    if let Some(slot) = p2a.get_mut(ci) {
                        *slot = get(luma, x, y - 3);
                    }
                    if let Some(slot) = p3a.get_mut(ci) {
                        *slot = get(luma, x, y - 4);
                    }
                    if let Some(slot) = q0a.get_mut(ci) {
                        *slot = get(luma, x, y);
                    }
                    if let Some(slot) = q1a.get_mut(ci) {
                        *slot = get(luma, x, y + 1);
                    }
                    if let Some(slot) = q2a.get_mut(ci) {
                        *slot = get(luma, x, y + 2);
                    }
                    if let Some(slot) = q3a.get_mut(ci) {
                        *slot = get(luma, x, y + 3);
                    }
                }
                batch::filter_luma_edge(
                    caps, &mut p0a, &mut p1a, &mut p2a, &p3a, &mut q0a, &mut q1a, &mut q2a, &q3a, &bsa, edge,
                );
                for col in 0..16u32 {
                    let x = mx * 16 + col;
                    let ci = col as usize;
                    set(luma, x, y - 1, p0a.get(ci).copied().unwrap_or(0));
                    set(luma, x, y - 2, p1a.get(ci).copied().unwrap_or(0));
                    set(luma, x, y - 3, p2a.get(ci).copied().unwrap_or(0));
                    set(luma, x, y, q0a.get(ci).copied().unwrap_or(0));
                    set(luma, x, y + 1, q1a.get(ci).copied().unwrap_or(0));
                    set(luma, x, y + 2, q2a.get(ci).copied().unwrap_or(0));
                }
            }
        }
    }

    /// Filter macroblock row `my` of one 4:2:0 chroma plane -- the chroma
    /// sibling of [`DeblockCtx::luma_mb_row`].
    ///
    /// Reads rows `my * 8 - 2 ..= my * 8 + 7` and writes only `my * 8 - 1 ..=
    /// my * 8 + 7`: chroma's filter modifies `p0`/`q0` and nothing else, so the
    /// overhang into the row above is a single row rather than luma's three.
    #[allow(
        clippy::integer_division,
        reason = "every division below is by the constant 2 or 4 (chroma-to-luma 4x4 block granularity) \
                  over a loop variable bounded 0..8 -- exact by construction, never a bitstream-derived value"
    )]
    pub(crate) fn chroma_mb_row(&self, chroma: &mut [u8], chroma_qp_offset: i32, my: u32) {
        let caps = self.caps;
        let grid = &self.grid;
        let filter_offset_a = self.filter_offset_a;
        let filter_offset_b = self.filter_offset_b;
        let ref_list0_poc = self.ref_list0_poc;
        let ref_list1_poc = self.ref_list1_poc;
        let mbs_wide = self.mbs_wide;
        let width = mbs_wide.saturating_mul(8);
        let qpc = |mx: u32, my: u32| -> u8 {
            let v = crate::dequant::chroma_qp(i32::from(grid.qpy(mx, my)), chroma_qp_offset);
            u8::try_from(v.clamp(0, 51)).unwrap_or(51)
        };

        let get = |c: &[u8], x: u32, y: u32| -> u8 { c.get((y * width + x) as usize).copied().unwrap_or(0) };
        let set = |c: &mut [u8], x: u32, y: u32, v: u8| {
            if let Some(slot) = c.get_mut((y * width + x) as usize) {
                *slot = v;
            }
        };

        for mx in 0..mbs_wide {
            let Some(here) = grid.at(mx, my) else { continue };
            let qp_here = qpc(mx, my);

            // Vertical: chroma-local x == 0 (macroblock boundary, luma
            // local 0) and x == 4 (luma local 8, the only other luma edge
            // position with a real chroma column at 4:2:0).
            for (c_local, luma_local) in [(0u32, 0u32), (4, 8)] {
                if c_local == 0 && mx == 0 {
                    continue;
                }
                let mb_edge = c_local == 0;
                let (p_mb, qp_p) = if mb_edge {
                    #[allow(clippy::unwrap_used, reason = "mx > 0 here, checked above")]
                    (grid.at(mx - 1, my).unwrap(), qpc(mx - 1, my))
                } else {
                    (here, qp_here)
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let x = mx * 8 + c_local;

                // Batch all 8 rows along this edge -- chroma's own line
                // count, matching `vaco_codec_dsp_deblock::batch`'s
                // narrower `p0`/`p1`/`q0`/`q1` window.
                let mut p0a = [0u8; 8];
                let mut p1a = [0u8; 8];
                let mut q0a = [0u8; 8];
                let mut q1a = [0u8; 8];
                let mut bsa = [0u8; 8];
                // Same memoisation as luma: two chroma rows per `blk_row`
                // share one luma-derived `bS` (see the comment on
                // `blk_row` below), so compute it once per group of 2
                // instead of once per row.
                let mut bs_by_blk_row = [0u8; 4];
                for (blk_row, slot) in bs_by_blk_row.iter_mut().enumerate() {
                    // Luma row group this chroma row's bS borrows: chroma
                    // row `row` is luma row `2*row`, whose own 4-row group
                    // is `(2*row) / 4 == row / 2`.
                    let q_blk = blk_row * 4 + (luma_local / 4) as usize;
                    let p_blk =
                        if mb_edge { blk_row * 4 + 3 } else { blk_row * 4 + (luma_local / 4 - 1) as usize };
                    *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk, ref_list0_poc, ref_list1_poc);
                }
                for row in 0..8u32 {
                    let y = my * 8 + row;
                    let ri = row as usize;
                    if let Some(slot) = bsa.get_mut(ri) {
                        *slot = bs_by_blk_row.get(ri / 2).copied().unwrap_or(0);
                    }
                    if let Some(slot) = p0a.get_mut(ri) {
                        *slot = get(chroma, x - 1, y);
                    }
                    if let Some(slot) = p1a.get_mut(ri) {
                        *slot = get(chroma, x - 2, y);
                    }
                    if let Some(slot) = q0a.get_mut(ri) {
                        *slot = get(chroma, x, y);
                    }
                    if let Some(slot) = q1a.get_mut(ri) {
                        *slot = get(chroma, x + 1, y);
                    }
                }
                batch::filter_chroma_edge(caps, &mut p0a, &p1a, &mut q0a, &q1a, &bsa, edge);
                for row in 0..8u32 {
                    let y = my * 8 + row;
                    let ri = row as usize;
                    set(chroma, x - 1, y, p0a.get(ri).copied().unwrap_or(0));
                    set(chroma, x, y, q0a.get(ri).copied().unwrap_or(0));
                }
            }

            // Horizontal: same mapping, transposed.
            for (c_local, luma_local) in [(0u32, 0u32), (4, 8)] {
                if c_local == 0 && my == 0 {
                    continue;
                }
                let mb_edge = c_local == 0;
                let (p_mb, qp_p) = if mb_edge {
                    #[allow(clippy::unwrap_used, reason = "my > 0 here, checked above")]
                    (grid.at(mx, my - 1).unwrap(), qpc(mx, my - 1))
                } else {
                    (here, qp_here)
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let y = my * 8 + c_local;

                let mut p0a = [0u8; 8];
                let mut p1a = [0u8; 8];
                let mut q0a = [0u8; 8];
                let mut q1a = [0u8; 8];
                let mut bsa = [0u8; 8];
                // Same memoisation as the vertical chroma pass above: one
                // `bS` per `blk_col` group of 2 columns.
                let mut bs_by_blk_col = [0u8; 4];
                for (blk_col, slot) in bs_by_blk_col.iter_mut().enumerate() {
                    let q_blk = (luma_local / 4) as usize * 4 + blk_col;
                    let p_blk = if mb_edge { 12 + blk_col } else { (luma_local / 4 - 1) as usize * 4 + blk_col };
                    *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk, ref_list0_poc, ref_list1_poc);
                }
                for col in 0..8u32 {
                    let x = mx * 8 + col;
                    let ci = col as usize;
                    if let Some(slot) = bsa.get_mut(ci) {
                        *slot = bs_by_blk_col.get(ci / 2).copied().unwrap_or(0);
                    }
                    if let Some(slot) = p0a.get_mut(ci) {
                        *slot = get(chroma, x, y - 1);
                    }
                    if let Some(slot) = p1a.get_mut(ci) {
                        *slot = get(chroma, x, y - 2);
                    }
                    if let Some(slot) = q0a.get_mut(ci) {
                        *slot = get(chroma, x, y);
                    }
                    if let Some(slot) = q1a.get_mut(ci) {
                        *slot = get(chroma, x, y + 1);
                    }
                }
                batch::filter_chroma_edge(caps, &mut p0a, &p1a, &mut q0a, &q1a, &bsa, edge);
                for col in 0..8u32 {
                    let x = mx * 8 + col;
                    let ci = col as usize;
                    set(chroma, x, y - 1, p0a.get(ci).copied().unwrap_or(0));
                    set(chroma, x, y, q0a.get(ci).copied().unwrap_or(0));
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, clippy::integer_division, reason = "test code")]
mod tests {
    use super::*;
    use crate::mb::{MbResidual, MvInfo};

    /// An `Intra_16x16` macroblock, which gives every edge touching it
    /// `bS = 4` at a macroblock boundary and `3` internally -- the strongest
    /// filter, so nothing about this test depends on a motion or coefficient
    /// comparison happening to come out nonzero.
    fn intra_mb(mb_x: u32, mb_y: u32) -> MbSummary {
        MbSummary {
            mb_x,
            mb_y,
            skipped: false,
            is_ipcm: false,
            is_intra4x4: false,
            is_intra8x8: false,
            is_intra16x16: true,
            intra16x16_pred_mode: 0,
            transform_8x8: false,
            intra_chroma_pred_mode: 0,
            // High enough that clause 8.7.2.2's alpha/beta admit the gentle
            // variation the test plane carries; low enough to be a real QP.
            qpy: 40,
            residual: MbResidual::default(),
            mv_blocks: [MvInfo::default(); 16],
        }
    }

    fn grid(mbs_wide: u32, mbs_high: u32) -> Vec<MbSummary> {
        (0..mbs_high)
            .flat_map(|y| (0..mbs_wide).map(move |x| intra_mb(x, y)))
            .collect()
    }

    /// A plane that varies enough for filtering to move bytes, by less than
    /// the thresholds at `qpy = 40` so that it is not suppressed.
    fn plane(w: usize, h: usize) -> Vec<u8> {
        (0..w * h).map(|i| 128 + ((i * 7 + (i / w) * 5) % 7) as u8).collect()
    }

    fn changed_rows(before: &[u8], after: &[u8], w: usize, h: usize) -> Vec<usize> {
        (0..h).filter(|&y| before[y * w..(y + 1) * w] != after[y * w..(y + 1) * w]).collect()
    }

    /// Row granularity rests entirely on knowing how far *up* filtering one
    /// macroblock row reaches, because that overhang is what stops the row
    /// above from being final. Clause 8.7.2.3's luma filter modifies `p0`,
    /// `p1` and `p2`, so a top macroblock edge at `y = my * 16` rewrites rows
    /// `my * 16 - 1`, `- 2` and `- 3` and nothing higher.
    ///
    /// This asserts both halves: nothing outside `my * 16 - 3 ..= my * 16 + 14`
    /// moves, **and** row `my * 16 - 3` really does move -- an overhang that
    /// were only hypothetical would make the watermark needlessly conservative
    /// and this test vacuous.
    #[test]
    fn filtering_one_luma_macroblock_row_reaches_exactly_three_rows_above_it() {
        let (mbs_wide, mbs_high) = (3u32, 3u32);
        let (w, h) = ((mbs_wide * 16) as usize, (mbs_high * 16) as usize);
        let mbs = grid(mbs_wide, mbs_high);
        let ctx = DeblockCtx::new(&mbs, mbs_wide, mbs_high, 0, 0, &[], &[]);
        let before = plane(w, h);
        let mut after = before.clone();
        ctx.luma_mb_row(&mut after, 1);
        let changed = changed_rows(&before, &after, w, h);
        assert!(!changed.is_empty(), "the filter did not move a single byte, so this proves nothing");
        assert_eq!(*changed.iter().min().unwrap(), 13, "the top edge's `p2` is row my * 16 - 3");
        assert!(
            *changed.iter().max().unwrap() <= 30,
            "nothing below `my * 16 + 14` may move: {changed:?}"
        );
    }

    /// [`filtering_one_luma_macroblock_row_reaches_exactly_three_rows_above_it`]'s
    /// chroma half. Clause 8.7's chroma filter modifies `p0` and `q0` only, so
    /// the overhang is a single row, `my * 8 - 1` -- which is why
    /// [`crate::reconstruct::chroma_rows_final`] adds seven rather than luma's
    /// thirteen.
    #[test]
    fn filtering_one_chroma_macroblock_row_reaches_exactly_one_row_above_it() {
        let (mbs_wide, mbs_high) = (3u32, 3u32);
        let (w, h) = ((mbs_wide * 8) as usize, (mbs_high * 8) as usize);
        let mbs = grid(mbs_wide, mbs_high);
        let ctx = DeblockCtx::new(&mbs, mbs_wide, mbs_high, 0, 0, &[], &[]);
        let before = plane(w, h);
        let mut after = before.clone();
        ctx.chroma_mb_row(&mut after, 0, 1);
        let changed = changed_rows(&before, &after, w, h);
        assert!(!changed.is_empty(), "the filter did not move a single byte, so this proves nothing");
        assert_eq!(*changed.iter().min().unwrap(), 7, "the top edge's `p0` is row my * 8 - 1");
        assert!(
            *changed.iter().max().unwrap() <= 15,
            "nothing below `my * 8 + 7` may move: {changed:?}"
        );
    }

    /// The whole-picture form and the row-by-row form must be the same
    /// function. They are the same code, but the row form is what the decoder
    /// runs and the whole-picture form is what this module's older tests
    /// check, so the equivalence is worth pinning rather than assuming.
    #[test]
    fn filtering_row_by_row_matches_the_whole_picture_sweep() {
        let (mbs_wide, mbs_high) = (4u32, 4u32);
        let (w, h) = ((mbs_wide * 16) as usize, (mbs_high * 16) as usize);
        let mbs = grid(mbs_wide, mbs_high);
        let mut whole = plane(w, h);
        deblock_picture_luma(&mut whole, &mbs, mbs_wide, mbs_high, 0, 0, 0, &[], &[]).unwrap();
        let mut rows = plane(w, h);
        let ctx = DeblockCtx::new(&mbs, mbs_wide, mbs_high, 0, 0, &[], &[]);
        for my in 0..mbs_high {
            ctx.luma_mb_row(&mut rows, my);
        }
        assert_eq!(whole, rows);
    }
}
