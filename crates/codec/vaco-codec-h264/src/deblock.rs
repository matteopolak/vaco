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
//! sides use a different `ref_idx` or their motion vectors differ by at
//! least 4 quarter-luma-samples in either component; otherwise `bS = 0`.
//! This is the JM reference decoder's own `get_strength_ver`/`_hor`
//! (`loop_filter_normal.c`) collapsed to what a single-reference-list,
//! frame-only (no MBAFF, no fields, no B slices) decoder ever needs:
//! `compare_mvs`' `List 1` half is dead code here because
//! [`crate::mb::MvInfo`] never populates it for a P slice.
//!
//! **Scope, explicitly, not merely unimplemented**: MBAFF/field pictures
//! (this crate does not decode them at all -- see `decoder.rs`'s own
//! refusal) and B slices (ditto, `mb.rs`'s own module doc).
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

/// Clause 8.7.2.1's non-intra case, collapsed to a single reference list
/// (see this module's own doc). `p`/`q` are the two 4x4 luma blocks on
/// either side of the edge -- the same macroblock and a different block
/// index for an internal edge, two different macroblocks for a macroblock
/// edge.
fn boundary_strength(mb_edge: bool, p_mb: &MbSummary, p_blk: usize, q_mb: &MbSummary, q_blk: usize) -> u8 {
    if is_intra(p_mb) || is_intra(q_mb) {
        return if mb_edge { 4 } else { 3 };
    }
    if has_luma_coeffs(p_mb, p_blk) || has_luma_coeffs(q_mb, q_blk) {
        return 2;
    }
    let p_mv = p_mb.mv_blocks.get(p_blk).copied().unwrap_or_default();
    let q_mv = q_mb.mv_blocks.get(q_blk).copied().unwrap_or_default();
    if p_mv.ref_idx_l0() != q_mv.ref_idx_l0() {
        return 1;
    }
    let (px, py) = p_mv.mv_l0();
    let (qx, qy) = q_mv.mv_l0();
    if px.abs_diff(qx) >= 4 || py.abs_diff(qy) >= 4 {
        return 1;
    }
    0
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
/// function applies clause 8.7.2.2's own `* 2` itself.
///
/// # Errors
///
/// This function no longer refuses non-intra macroblocks (clause 8.7.2.1's
/// general `bS` derivation is implemented -- see this module's own doc) --
/// kept fallible for interface stability with its own tests and the CABAC
/// engine's own `Result`-returning conventions elsewhere in this crate, but
/// no path in this function currently returns `Err`.
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
) -> vaco_core::Result<()> {
    if disable_deblocking_filter_idc == 1 {
        return Ok(());
    }

    let caps = Caps::detect();
    let grid = MbGrid::new(macroblocks, mbs_wide, mbs_high);
    let filter_offset_a = slice_alpha_c0_offset_div2.saturating_mul(2);
    let filter_offset_b = slice_beta_offset_div2.saturating_mul(2);
    let width = mbs_wide.saturating_mul(16);

    let get = |luma: &[u8], x: u32, y: u32| -> u8 { luma.get((y * width + x) as usize).copied().unwrap_or(0) };
    let set = |luma: &mut [u8], x: u32, y: u32, v: u8| {
        if let Some(slot) = luma.get_mut((y * width + x) as usize) {
            *slot = v;
        }
    };

    for my in 0..mbs_high {
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
                for row in 0..16u32 {
                    let y = my * 16 + row;
                    let blk_row = (row / 4) as usize;
                    let q_blk = blk_row * 4 + (local / 4) as usize;
                    let p_blk = if mb_edge { blk_row * 4 + 3 } else { blk_row * 4 + (local / 4 - 1) as usize };
                    let ri = row as usize;
                    if let Some(slot) = bsa.get_mut(ri) {
                        *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk);
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
                for col in 0..16u32 {
                    let x = mx * 16 + col;
                    let blk_col = (col / 4) as usize;
                    let q_blk = (local / 4) as usize * 4 + blk_col;
                    let p_blk = if mb_edge { 12 + blk_col } else { (local / 4 - 1) as usize * 4 + blk_col };
                    let ci = col as usize;
                    if let Some(slot) = bsa.get_mut(ci) {
                        *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk);
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
) {
    if disable_deblocking_filter_idc == 1 {
        return;
    }

    let caps = Caps::detect();
    let grid = MbGrid::new(macroblocks, mbs_wide, mbs_high);
    let filter_offset_a = slice_alpha_c0_offset_div2.saturating_mul(2);
    let filter_offset_b = slice_beta_offset_div2.saturating_mul(2);
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

    for my in 0..mbs_high {
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
                for row in 0..8u32 {
                    let y = my * 8 + row;
                    // Luma row group this chroma row's bS borrows: chroma
                    // row `row` is luma row `2*row`, whose own 4-row group
                    // is `(2*row) / 4 == row / 2`.
                    let blk_row = (row / 2) as usize;
                    let q_blk = blk_row * 4 + (luma_local / 4) as usize;
                    let p_blk =
                        if mb_edge { blk_row * 4 + 3 } else { blk_row * 4 + (luma_local / 4 - 1) as usize };
                    let ri = row as usize;
                    if let Some(slot) = bsa.get_mut(ri) {
                        *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk);
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
                for col in 0..8u32 {
                    let x = mx * 8 + col;
                    let blk_col = (col / 2) as usize;
                    let q_blk = (luma_local / 4) as usize * 4 + blk_col;
                    let p_blk = if mb_edge { 12 + blk_col } else { (luma_local / 4 - 1) as usize * 4 + blk_col };
                    let ci = col as usize;
                    if let Some(slot) = bsa.get_mut(ci) {
                        *slot = boundary_strength(mb_edge, p_mb, p_blk, here, q_blk);
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
