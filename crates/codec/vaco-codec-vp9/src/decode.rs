//! The per-superblock decode orchestrator: §6.4's `decode_tiles`/
//! `decode_partition`/`decode_block`/`intra_frame_mode_info`/
//! `inter_frame_mode_info`/`residual`/`tokens`, tied to §8.5's intra/inter
//! prediction, §6.5's motion-vector prediction and §8.6/§8.7's
//! dequantization and inverse transform, plus the [`Decoder`] trait impl.

use vaco_codec_core::{Decoder, DecoderDesc};
use vaco_codec_dsp_idct::vp9::TxType;
use vaco_codec_msac::Vp9BoolDecoder as Bd;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::framebuf::Picture;
use crate::header::{self, EntropyContext, FrameHeader, LoopFilterParams, PrevFrameInfo, RefFrameDims, Segmentation};
use crate::interpredict;
use crate::loopfilter;
use crate::mvpred::{self, MvCell};
use crate::predict::predict_intra;
use crate::refframe::{RefFrameStore, RefSlot};
use crate::tables;
use crate::transform::{dequant_factors, reconstruct};
use crate::{superframe, tokens};

fn ix(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// One 8x8 "mode info" grid cell's persisted state, read back for neighbour
/// context derivation by later blocks in the same tile (and, for
/// `ref_frame`/`mv`/`sub_mvs`, by a later frame's temporal motion-vector
/// candidate — see [`FrameCtx::prev_mv_grid`]).
#[derive(Debug, Clone, Copy, Default)]
struct MiCell {
    /// The `BLOCK_SIZES` value this mode-info block was decoded at —
    /// `MiSizes[MiRow][MiCol]` in §8.8's own notation, needed by the loop
    /// filter's `sbSize`/`isBlockEdge` derivation, which queries an
    /// arbitrary grid position rather than "the block currently being
    /// decoded".
    mi_size: i32,
    skip: bool,
    tx_size: i32,
    sub_modes: [i32; 4],
    /// The whole block's `y_mode` (an intra mode 0..9, or `NEARESTMV`..
    /// `NEWMV` for an inter block) — `sub_modes` already carries per-4x4
    /// detail for a sub-8x8 *intra* block, but `mode_2_counter` and the
    /// `intra_mode`/`default_intra_mode` neighbour lookups both want the
    /// single whole-block value.
    y_mode: i32,
    /// `RefFrames[MiRow][MiCol]`: `INTRA_FRAME`/`NONE` (both 0) by default.
    ref_frame: [i32; 2],
    /// `Mvs[MiRow][MiCol][refList]`, `[row, col]`.
    mv: [[i32; 2]; 2],
    /// `SubMvs[MiRow][MiCol][refList][subblock]`, `[row, col]`.
    sub_mvs: [[[i32; 2]; 4]; 2],
    interp_filter: i32,
}

impl MiCell {
    fn to_mv_cell(self) -> MvCell {
        MvCell { y_mode: self.y_mode, ref_frame: self.ref_frame, mv: self.mv, sub_mvs: self.sub_mvs }
    }
}

pub(crate) struct FrameCtx {
    header: FrameHeader,
    pub(crate) mi_cols: usize,
    pub(crate) mi_rows: usize,
    grid: Vec<MiCell>,
    above_partition_context: Vec<u8>,
    left_partition_context: [u8; 8],
    pub(crate) above_nz: [Vec<bool>; 3],
    pub(crate) left_nz: [[bool; 16]; 3],
    above_seg_pred_context: Vec<bool>,
    left_seg_pred_context: [bool; 8],
    /// §6.4.14's `PrevSegmentIds` — the previous frame's per-MI segment
    /// map. Empty (never indexed successfully) when there is no usable
    /// previous frame; `get_segment_id` treats a miss as segment 0, the
    /// spec's own implicit default for a freshly-cleared map.
    prev_segment_ids: Vec<u8>,
    /// This frame's own per-MI segment map, built up as blocks decode and
    /// handed back to the caller afterwards to become the *next* frame's
    /// `prev_segment_ids`.
    segment_ids: Vec<u8>,
    /// §6.5's `UsePrevFrameMvs` temporal candidate source — the previous
    /// frame's `RefFrames`/`Mvs` grid, at that frame's own `mi_cols`. Only
    /// ever read when `header.use_prev_frame_mvs` is true, which the
    /// header parse already gates on matching frame dimensions, so a
    /// same-sized grid is guaranteed whenever it is actually consulted.
    prev_mv_grid: Vec<MiCell>,
    ref_store: RefFrameStore,
    pic: Picture,
}

impl FrameCtx {
    fn mi_at(&self, r: i32, c: i32) -> Option<MiCell> {
        if r < 0 || c < 0 {
            return None;
        }
        let (r, c) = (usize::try_from(r).ok()?, usize::try_from(c).ok()?);
        if r >= self.mi_rows || c >= self.mi_cols {
            return None;
        }
        self.grid.get(r * self.mi_cols + c).copied()
    }

    fn prev_mv_cell_here(&self, r: usize, c: usize) -> MvCell {
        self.prev_mv_grid.get(r * self.mi_cols + c).copied().unwrap_or_default().to_mv_cell()
    }

    fn store_block(&mut self, r: usize, c: usize, subsize: i32, cell: MiCell, segment_id: u8) {
        let h = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let w = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        for y in 0..h {
            for x in 0..w {
                let (rr, cc) = (r + y, c + x);
                if rr < self.mi_rows && cc < self.mi_cols {
                    if let Some(slot) = self.grid.get_mut(rr * self.mi_cols + cc) {
                        *slot = cell;
                    }
                    if let Some(slot) = self.segment_ids.get_mut(rr * self.mi_cols + cc) {
                        *slot = segment_id;
                    }
                }
            }
        }
    }

    fn plane_mut(&mut self, plane: usize) -> &mut crate::framebuf::Plane {
        match plane {
            0 => &mut self.pic.y,
            1 => &mut self.pic.u,
            _ => &mut self.pic.v,
        }
    }

    fn plane(&self, plane: usize) -> &crate::framebuf::Plane {
        match plane {
            0 => &self.pic.y,
            1 => &self.pic.u,
            _ => &self.pic.v,
        }
    }
}

/// §9.3.2's `partition` context: `bsl*4 + left*2 + above`.
fn partition_ctx(ctx: &FrameCtx, r: usize, c: usize, bsize: i32, num8x8: usize) -> usize {
    let bsl = tables::MI_WIDTH_LOG2_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(0);
    let boffset = tables::MI_WIDTH_LOG2_LOOKUP.get(usize::try_from(tables::BLOCK_64X64).unwrap_or(0)).copied().unwrap_or(0) - bsl;
    let mut above = 0u8;
    let mut left = 0u8;
    for i in 0..num8x8 {
        above |= ctx.above_partition_context.get(c + i).copied().unwrap_or(0);
        left |= ctx.left_partition_context.get((r % 8) + i).copied().unwrap_or(0);
    }
    let above_bit = usize::from((above & (1 << boffset)) > 0);
    let left_bit = usize::from((left & (1 << boffset)) > 0);
    usize::try_from(bsl).unwrap_or(0) * 4 + left_bit * 2 + above_bit
}

#[allow(clippy::too_many_arguments)]
fn decode_partition(bd: &mut Bd<'_>, ctx: &mut FrameCtx, entropy: &EntropyContext, r: usize, c: usize, bsize: i32) {
    if r >= ctx.mi_rows || c >= ctx.mi_cols {
        return;
    }
    let num8x8 = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(bsize).unwrap_or(0)).copied().unwrap_or(1);
    let half = num8x8 >> 1;
    let has_rows = r + half < ctx.mi_rows;
    let has_cols = c + half < ctx.mi_cols;

    let pctx = partition_ctx(ctx, r, c, bsize, num8x8);
    // §9.3.2's `partition` probability selection: the spec text's own
    // "If FrameIsIntra is equal to 0, ... kf_partition_probs ... Otherwise,
    // ... partition_probs" is a well-known erratum with the condition
    // inverted — a key frame (`FrameIsIntra == 1`) reads the fixed
    // `kf_partition_probs`; every other frame reads the adaptive,
    // forward-updatable `partition_probs` (confirmed against libvpx's
    // `vp9_kfread_modes` vs `read_partition`, and by this exact bug: using
    // `kf_partition_probs` unconditionally decoded key frames correctly in
    // Phase B but desynced every inter frame's second-and-later superblock
    // in Phase C, since only a key frame's partition probabilities happen
    // to be the fixed table).
    let probs = if ctx.header.frame_is_intra {
        tables::KF_PARTITION_PROBS.get(pctx).copied().unwrap_or([128; 3])
    } else {
        entropy.partition_probs.get(pctx).copied().unwrap_or([128; 3])
    };
    let partition = if has_rows && has_cols {
        bd.read_tree(&tables::PARTITION_TREE, &probs)
    } else if has_cols {
        // node2 fixed at 1: the second (index-1) probability.
        let p = probs.get(1).copied().unwrap_or(128);
        if bd.read_bool(p) { tables::PARTITION_SPLIT } else { tables::PARTITION_HORZ }
    } else if has_rows {
        let p = probs.get(2).copied().unwrap_or(128);
        if bd.read_bool(p) { tables::PARTITION_SPLIT } else { tables::PARTITION_VERT }
    } else {
        tables::PARTITION_SPLIT
    };

    let subsize = tables::SUBSIZE_LOOKUP
        .get(usize::try_from(partition).unwrap_or(0))
        .and_then(|row| row.get(usize::try_from(bsize).unwrap_or(0)))
        .copied()
        .unwrap_or(tables::BLOCK_INVALID);

    if subsize < tables::BLOCK_8X8 || partition == tables::PARTITION_NONE {
        decode_block(bd, ctx, entropy, r, c, subsize);
    } else if partition == tables::PARTITION_HORZ {
        decode_block(bd, ctx, entropy, r, c, subsize);
        if has_rows {
            decode_block(bd, ctx, entropy, r + half, c, subsize);
        }
    } else if partition == tables::PARTITION_VERT {
        decode_block(bd, ctx, entropy, r, c, subsize);
        if has_cols {
            decode_block(bd, ctx, entropy, r, c + half, subsize);
        }
    } else {
        decode_partition(bd, ctx, entropy, r, c, subsize);
        decode_partition(bd, ctx, entropy, r, c + half, subsize);
        decode_partition(bd, ctx, entropy, r + half, c, subsize);
        decode_partition(bd, ctx, entropy, r + half, c + half, subsize);
    }

    if bsize == tables::BLOCK_8X8 || partition != tables::PARTITION_SPLIT {
        let bw = tables::B_WIDTH_LOG2_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(0);
        let bh = tables::B_HEIGHT_LOG2_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(0);
        for i in 0..num8x8 {
            if let Some(slot) = ctx.above_partition_context.get_mut(c + i) {
                *slot = 15u8 >> bw;
            }
            if let Some(slot) = ctx.left_partition_context.get_mut((r % 8) + i) {
                *slot = 15u8 >> bh;
            }
        }
    }
}

fn decode_block(bd: &mut Bd<'_>, ctx: &mut FrameCtx, entropy: &EntropyContext, r: usize, c: usize, subsize: i32) {
    let avail_u = r > 0;
    let avail_l = c > 0; // MiColStart is always 0: single tile column in this crate's scope.

    let mut block = if ctx.header.frame_is_intra {
        let (segment_id, skip, tx_size, y_mode, uv_mode, sub_modes) = intra_frame_mode_info(bd, ctx, r, c, subsize, avail_u, avail_l);
        DecodedBlock {
            segment_id,
            skip,
            is_inter: false,
            tx_size,
            y_mode,
            uv_mode,
            sub_modes,
            ref_frame: [tables::INTRA_FRAME, tables::NONE],
            block_mvs: [[[0, 0]; 4]; 2],
            interp_filter: tables::EIGHTTAP,
        }
    } else {
        inter_frame_mode_info(bd, ctx, entropy, r, c, subsize, avail_u, avail_l)
    };

    let eob_total_nonzero = residual(bd, ctx, entropy, r, c, subsize, &block);
    // §6.4.4's `decode_block`: a >=8x8 inter block with no coded residual
    // at all is stored as `skip = 1` regardless of what `read_skip` (or
    // `seg_feature_active(SEG_LVL_SKIP)`) actually produced — this is what
    // a *later* block's own `skip`/`tx_size` context formulas read back,
    // not merely a cosmetic renaming.
    if block.is_inter && subsize >= tables::BLOCK_8X8 && !eob_total_nonzero {
        block.skip = true;
    }

    ctx.store_block(
        r,
        c,
        subsize,
        MiCell {
            mi_size: subsize,
            skip: block.skip,
            tx_size: block.tx_size,
            sub_modes: block.sub_modes,
            y_mode: block.y_mode,
            ref_frame: block.ref_frame,
            mv: [block.block_mvs[0][3], block.block_mvs[1][3]],
            sub_mvs: block.block_mvs,
            interp_filter: block.interp_filter,
        },
        block.segment_id,
    );
}

/// Everything `residual`/`ctx.store_block` need out of whichever of
/// `intra_frame_mode_info`/`inter_frame_mode_info` ran — one shared shape
/// so `decode_block` does not need two divergent call sequences.
struct DecodedBlock {
    segment_id: u8,
    skip: bool,
    is_inter: bool,
    tx_size: i32,
    /// An intra mode (0..9) if `!is_inter`, else `NEARESTMV..=NEWMV`.
    y_mode: i32,
    uv_mode: i32,
    sub_modes: [i32; 4],
    ref_frame: [i32; 2],
    /// `BlockMvs[refList][subblock]`; all 4 subblocks identical when
    /// `MiSize >= BLOCK_8X8`. Meaningless (left `ZeroMv`) when `!is_inter`.
    block_mvs: [[[i32; 2]; 4]; 2],
    interp_filter: i32,
}

#[allow(clippy::too_many_arguments)]
fn intra_frame_mode_info(
    bd: &mut Bd<'_>,
    ctx: &mut FrameCtx,
    r: usize,
    c: usize,
    subsize: i32,
    avail_u: bool,
    avail_l: bool,
) -> (u8, bool, i32, i32, i32, [i32; 4]) {
    // -- intra_segment_id --
    let seg = &ctx.header.segmentation;
    let segment_id = if seg.enabled && seg.update_map {
        u8::try_from(bd.read_tree(&tables::SEGMENT_TREE, &seg.tree_probs)).unwrap_or(0)
    } else {
        0
    };

    // -- read_skip --
    let seg_feature_active = |feature: usize| -> bool {
        ctx.header.segmentation.enabled
            && ctx
                .header
                .segmentation
                .feature_enabled
                .get(usize::from(segment_id))
                .and_then(|row| row.get(feature))
                .copied()
                .unwrap_or(false)
    };
    let skip = if seg_feature_active(tables::SEG_LVL_SKIP) {
        true
    } else {
        let above = ctx.mi_at(ix(r) - 1, ix(c)).is_some_and(|m| m.skip);
        let left = ctx.mi_at(ix(r), ix(c) - 1).is_some_and(|m| m.skip);
        let sctx = usize::from(avail_u && above) + usize::from(avail_l && left);
        let p = entropy_skip_prob(ctx, sctx);
        bd.read_bool(p)
    };

    // -- read_tx_size(1) --
    let tx_size = read_tx_size(bd, ctx, r, c, subsize, avail_u, avail_l, true);

    // -- mode(s) --
    let mut sub_modes = [tables::DC_PRED; 4];
    let y_mode;
    if subsize >= tables::BLOCK_8X8 {
        let above_mode = if avail_u { ctx.mi_at(ix(r) - 1, ix(c)).map_or(tables::DC_PRED, |m| m.sub_modes.get(2).copied().unwrap_or(tables::DC_PRED)) } else { tables::DC_PRED };
        let left_mode = if avail_l { ctx.mi_at(ix(r), ix(c) - 1).map_or(tables::DC_PRED, |m| m.sub_modes.get(1).copied().unwrap_or(tables::DC_PRED)) } else { tables::DC_PRED };
        let m = read_kf_intra_mode(bd, above_mode, left_mode);
        y_mode = m;
        sub_modes = [m; 4];
    } else {
        let num4x4w = tables::NUM_4X4_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let num4x4h = tables::NUM_4X4_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let mut idy = 0usize;
        let mut last_mode = tables::DC_PRED;
        while idy < 2 {
            let mut idx = 0usize;
            while idx < 2 {
                let above_mode = if idy != 0 {
                    sub_modes.get(idx).copied().unwrap_or(tables::DC_PRED)
                } else if avail_u {
                    ctx.mi_at(ix(r) - 1, ix(c)).map_or(tables::DC_PRED, |m| m.sub_modes.get(2 + idx).copied().unwrap_or(tables::DC_PRED))
                } else {
                    tables::DC_PRED
                };
                let left_mode = if idx != 0 {
                    sub_modes.get(idy * 2).copied().unwrap_or(tables::DC_PRED)
                } else if avail_l {
                    ctx.mi_at(ix(r), ix(c) - 1).map_or(tables::DC_PRED, |m| m.sub_modes.get(1 + idy * 2).copied().unwrap_or(tables::DC_PRED))
                } else {
                    tables::DC_PRED
                };
                let m = read_kf_intra_mode(bd, above_mode, left_mode);
                for y2 in 0..num4x4h {
                    for x2 in 0..num4x4w {
                        let slot_idx = (idy + y2) * 2 + idx + x2;
                        if let Some(slot) = sub_modes.get_mut(slot_idx) {
                            *slot = m;
                        }
                    }
                }
                last_mode = m;
                idx += num4x4w;
            }
            idy += num4x4h;
        }
        y_mode = last_mode;
    }

    let uv_mode = {
        let ai = usize::try_from(y_mode).unwrap_or(0).min(9);
        let probs = tables::KF_UV_MODE_PROBS.get(ai).copied().unwrap_or([128; 9]);
        bd.read_tree(&tables::INTRA_MODE_TREE, &probs)
    };

    (segment_id, skip, tx_size, y_mode, uv_mode, sub_modes)
}

fn read_kf_intra_mode(bd: &mut Bd<'_>, above_mode: i32, left_mode: i32) -> i32 {
    let ai = usize::try_from(above_mode).unwrap_or(0).min(9);
    let li = usize::try_from(left_mode).unwrap_or(0).min(9);
    let probs = tables::KF_Y_MODE_PROBS.get(ai).and_then(|r| r.get(li)).copied().unwrap_or([128; 9]);
    bd.read_tree(&tables::INTRA_MODE_TREE, &probs)
}

fn entropy_skip_prob(ctx: &FrameCtx, sctx: usize) -> u8 {
    ctx.header.entropy.skip_prob.get(sctx).copied().unwrap_or(128)
}

/// §6.4.10's `read_tx_size(allowSelect)`, shared by `intra_frame_mode_info`
/// (which always passes `allowSelect = true`) and `inter_frame_mode_info`
/// (`!skip || !is_inter`).
#[allow(clippy::too_many_arguments)]
fn read_tx_size(bd: &mut Bd<'_>, ctx: &FrameCtx, r: usize, c: usize, subsize: i32, avail_u: bool, avail_l: bool, allow_select: bool) -> i32 {
    let max_tx_size = tables::MAX_TXSIZE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(0);
    let tx_mode = ctx.header.tx_mode;
    if allow_select && tx_mode == tables::TX_MODE_SELECT && subsize >= tables::BLOCK_8X8 {
        let above_avail_cell = ctx.mi_at(ix(r) - 1, ix(c));
        let left_avail_cell = ctx.mi_at(ix(r), ix(c) - 1);
        let mut above = max_tx_size;
        let mut left = max_tx_size;
        if avail_u
            && let Some(m) = above_avail_cell
            && !m.skip
        {
            above = m.tx_size;
        }
        if avail_l
            && let Some(m) = left_avail_cell
            && !m.skip
        {
            left = m.tx_size;
        }
        if !avail_l {
            left = above;
        }
        if !avail_u {
            above = left;
        }
        let tctx = usize::from((above + left) > max_tx_size);
        let row = usize::try_from(max_tx_size).unwrap_or(0);
        let probs = ctx.header.entropy.tx_probs.get(row).and_then(|r| r.get(tctx)).copied().unwrap_or([128; 3]);
        let tree: &[i8] = match max_tx_size {
            x if x == tables::TX_32X32 => &tables::TX_SIZE_32_TREE,
            x if x == tables::TX_16X16 => &tables::TX_SIZE_16_TREE,
            _ => &tables::TX_SIZE_8_TREE,
        };
        bd.read_tree(tree, &probs)
    } else {
        max_tx_size.min(tables::TX_MODE_TO_BIGGEST_TX_SIZE.get(usize::try_from(tx_mode).unwrap_or(0)).copied().unwrap_or(0))
    }
}

/// The neighbour reference-frame state §6.4.11 computes once at the top of
/// `inter_frame_mode_info` and every inter-only context formula
/// (`is_inter`/`comp_mode`/`comp_ref`/`single_ref_p1`/`single_ref_p2`)
/// reads from repeatedly.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools, reason = "each bool is an independent §6.4.11/9.3.2 context input (LeftIntra/AboveIntra/LeftSingle/AboveSingle), not related flags that belong in one enum")]
struct NeighborRefInfo {
    left_ref: [i32; 2],
    above_ref: [i32; 2],
    left_intra: bool,
    above_intra: bool,
    left_single: bool,
    above_single: bool,
}

fn neighbor_ref_info(ctx: &FrameCtx, r: usize, c: usize, avail_u: bool, avail_l: bool) -> NeighborRefInfo {
    let left = if avail_l { ctx.mi_at(ix(r), ix(c) - 1) } else { None };
    let above = if avail_u { ctx.mi_at(ix(r) - 1, ix(c)) } else { None };
    let left_ref = left.map_or([tables::INTRA_FRAME, tables::NONE], |m| m.ref_frame);
    let above_ref = above.map_or([tables::INTRA_FRAME, tables::NONE], |m| m.ref_frame);
    NeighborRefInfo {
        left_ref,
        above_ref,
        left_intra: left_ref[0] <= tables::INTRA_FRAME,
        above_intra: above_ref[0] <= tables::INTRA_FRAME,
        left_single: left_ref[1] <= tables::NONE,
        above_single: above_ref[1] <= tables::NONE,
    }
}

fn seg_feature_active_for(ctx: &FrameCtx, segment_id: u8, feature: usize) -> bool {
    ctx.header.segmentation.enabled
        && ctx.header.segmentation.feature_enabled.get(usize::from(segment_id)).and_then(|row| row.get(feature)).copied().unwrap_or(false)
}

/// §6.4.14's `get_segment_id`: the smallest `PrevSegmentIds` value covering
/// this block's on-screen 8x8 footprint.
fn get_segment_id(ctx: &FrameCtx, r: usize, c: usize, subsize: i32) -> u8 {
    let bw = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
    let bh = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
    let xmis = (ctx.mi_cols.saturating_sub(c)).min(bw);
    let ymis = (ctx.mi_rows.saturating_sub(r)).min(bh);
    let mut seg = 7u8;
    for y in 0..ymis {
        for x in 0..xmis {
            let v = ctx.prev_segment_ids.get((r + y) * ctx.mi_cols + (c + x)).copied().unwrap_or(0);
            seg = seg.min(v);
        }
    }
    seg
}

/// §6.4.12's `inter_segment_id`.
fn inter_segment_id(bd: &mut Bd<'_>, ctx: &mut FrameCtx, r: usize, c: usize, subsize: i32) -> u8 {
    let seg = ctx.header.segmentation;
    if !seg.enabled {
        return 0;
    }
    let predicted = get_segment_id(ctx, r, c, subsize);
    if !seg.update_map {
        return predicted;
    }
    if seg.temporal_update {
        let pctx = usize::from(ctx.left_seg_pred_context.get(r % 8).copied().unwrap_or(false)) + usize::from(ctx.above_seg_pred_context.get(c).copied().unwrap_or(false));
        let p = seg.pred_prob.get(pctx).copied().unwrap_or(255);
        let seg_id_predicted = bd.read_bool(p);
        let id = if seg_id_predicted { predicted } else { u8::try_from(bd.read_tree(&tables::SEGMENT_TREE, &seg.tree_probs)).unwrap_or(0) };
        let bw = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let bh = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        for i in 0..bw {
            if let Some(slot) = ctx.above_seg_pred_context.get_mut(c + i) {
                *slot = seg_id_predicted;
            }
        }
        for i in 0..bh {
            if let Some(slot) = ctx.left_seg_pred_context.get_mut((r + i) % 8) {
                *slot = seg_id_predicted;
            }
        }
        id
    } else {
        u8::try_from(bd.read_tree(&tables::SEGMENT_TREE, &seg.tree_probs)).unwrap_or(0)
    }
}

/// §6.4.13's `read_is_inter`.
fn read_is_inter(bd: &mut Bd<'_>, ctx: &FrameCtx, segment_id: u8, neighbors: NeighborRefInfo, avail_u: bool, avail_l: bool) -> bool {
    if seg_feature_active_for(ctx, segment_id, tables::SEG_LVL_REF_FRAME) {
        let data = ctx.header.segmentation.feature_data.get(usize::from(segment_id)).and_then(|r| r.get(tables::SEG_LVL_REF_FRAME)).copied().unwrap_or(tables::INTRA_FRAME);
        return data != tables::INTRA_FRAME;
    }
    let sctx = if avail_u && avail_l {
        if neighbors.left_intra && neighbors.above_intra { 3 } else { usize::from(neighbors.left_intra || neighbors.above_intra) }
    } else if avail_u || avail_l {
        2 * usize::from(if avail_u { neighbors.above_intra } else { neighbors.left_intra })
    } else {
        0
    };
    let p = ctx.header.entropy.is_inter_prob.get(sctx).copied().unwrap_or(128);
    bd.read_bool(p)
}

/// §6.4.15's `intra_block_mode_info` — an intra-coded block inside an
/// *inter* frame, which reads the adaptive `y_mode_probs`/`uv_mode_probs`
/// tables (forward-updated by this frame's own compressed header) rather
/// than the fixed `kf_y_mode_probs`/`kf_uv_mode_probs` a key frame's
/// `intra_frame_mode_info` uses.
fn intra_block_mode_info(bd: &mut Bd<'_>, entropy: &EntropyContext, subsize: i32) -> (i32, i32, [i32; 4]) {
    let mut sub_modes = [tables::DC_PRED; 4];
    let y_mode;
    if subsize >= tables::BLOCK_8X8 {
        let ctx_idx = tables::SIZE_GROUP_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(0);
        let probs = entropy.y_mode_probs.get(ctx_idx).copied().unwrap_or([128; 9]);
        let m = bd.read_tree(&tables::INTRA_MODE_TREE, &probs);
        y_mode = m;
        sub_modes = [m; 4];
    } else {
        let num4x4w = tables::NUM_4X4_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let num4x4h = tables::NUM_4X4_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let probs = entropy.y_mode_probs.first().copied().unwrap_or([128; 9]);
        let mut idy = 0usize;
        let mut last_mode = tables::DC_PRED;
        while idy < 2 {
            let mut idx = 0usize;
            while idx < 2 {
                let m = bd.read_tree(&tables::INTRA_MODE_TREE, &probs);
                for y2 in 0..num4x4h {
                    for x2 in 0..num4x4w {
                        let slot_idx = (idy + y2) * 2 + idx + x2;
                        if let Some(slot) = sub_modes.get_mut(slot_idx) {
                            *slot = m;
                        }
                    }
                }
                last_mode = m;
                idx += num4x4w;
            }
            idy += num4x4h;
        }
        y_mode = last_mode;
    }
    let uv_mode = {
        let ai = usize::try_from(y_mode).unwrap_or(0).min(9);
        let probs = entropy.uv_mode_probs.get(ai).copied().unwrap_or([128; 9]);
        bd.read_tree(&tables::INTRA_MODE_TREE, &probs)
    };
    (y_mode, uv_mode, sub_modes)
}

/// §6.4.17's `single_ref_p1` context (§9.3.2).
fn single_ref_p1_ctx(n: NeighborRefInfo, avail_u: bool, avail_l: bool) -> usize {
    let last = tables::LAST_FRAME;
    if avail_u && avail_l {
        if n.above_intra && n.left_intra {
            2
        } else if n.left_intra {
            if n.above_single { 4 * usize::from(n.above_ref[0] == last) } else { 1 + usize::from(n.above_ref[0] == last || n.above_ref[1] == last) }
        } else if n.above_intra {
            if n.left_single { 4 * usize::from(n.left_ref[0] == last) } else { 1 + usize::from(n.left_ref[0] == last || n.left_ref[1] == last) }
        } else if n.above_single && n.left_single {
            2 * usize::from(n.above_ref[0] == last) + 2 * usize::from(n.left_ref[0] == last)
        } else if !n.above_single && !n.left_single {
            1 + usize::from(n.above_ref[0] == last || n.above_ref[1] == last || n.left_ref[0] == last || n.left_ref[1] == last)
        } else {
            let (rfs, crf1, crf2) = if n.above_single { (n.above_ref[0], n.left_ref[0], n.left_ref[1]) } else { (n.left_ref[0], n.above_ref[0], n.above_ref[1]) };
            if rfs == last { 3 + usize::from(crf1 == last || crf2 == last) } else { usize::from(crf1 == last || crf2 == last) }
        }
    } else if avail_u {
        if n.above_intra {
            2
        } else if n.above_single {
            4 * usize::from(n.above_ref[0] == last)
        } else {
            1 + usize::from(n.above_ref[0] == last || n.above_ref[1] == last)
        }
    } else if avail_l {
        if n.left_intra {
            2
        } else if n.left_single {
            4 * usize::from(n.left_ref[0] == last)
        } else {
            1 + usize::from(n.left_ref[0] == last || n.left_ref[1] == last)
        }
    } else {
        2
    }
}

/// §6.4.17's `single_ref_p2` context (§9.3.2).
fn single_ref_p2_ctx(n: NeighborRefInfo, avail_u: bool, avail_l: bool) -> usize {
    let (last, golden, altref) = (tables::LAST_FRAME, tables::GOLDEN_FRAME, tables::ALTREF_FRAME);
    if avail_u && avail_l {
        if n.above_intra && n.left_intra {
            2
        } else if n.left_intra {
            if n.above_single {
                if n.above_ref[0] == last { 3 } else { 4 * usize::from(n.above_ref[0] == golden) }
            } else {
                1 + 2 * usize::from(n.above_ref[0] == golden || n.above_ref[1] == golden)
            }
        } else if n.above_intra {
            if n.left_single {
                if n.left_ref[0] == last { 3 } else { 4 * usize::from(n.left_ref[0] == golden) }
            } else {
                1 + 2 * usize::from(n.left_ref[0] == golden || n.left_ref[1] == golden)
            }
        } else if n.above_single && n.left_single {
            if n.above_ref[0] == last && n.left_ref[0] == last {
                3
            } else if n.above_ref[0] == last {
                4 * usize::from(n.left_ref[0] == golden)
            } else if n.left_ref[0] == last {
                4 * usize::from(n.above_ref[0] == golden)
            } else {
                2 * usize::from(n.above_ref[0] == golden) + 2 * usize::from(n.left_ref[0] == golden)
            }
        } else if !n.above_single && !n.left_single {
            if n.above_ref[0] == n.left_ref[0] && n.above_ref[1] == n.left_ref[1] {
                3 * usize::from(n.above_ref[0] == golden || n.above_ref[1] == golden)
            } else {
                2
            }
        } else {
            let (rfs, crf1, crf2) = if n.above_single { (n.above_ref[0], n.left_ref[0], n.left_ref[1]) } else { (n.left_ref[0], n.above_ref[0], n.above_ref[1]) };
            if rfs == golden {
                3 + usize::from(crf1 == golden || crf2 == golden)
            } else if rfs == altref {
                usize::from(crf1 == golden || crf2 == golden)
            } else {
                1 + 2 * usize::from(crf1 == golden || crf2 == golden)
            }
        }
    } else if avail_u {
        if n.above_intra || (n.above_ref[0] == last && n.above_single) {
            2
        } else if n.above_single {
            4 * usize::from(n.above_ref[0] == golden)
        } else {
            3 * usize::from(n.above_ref[0] == golden || n.above_ref[1] == golden)
        }
    } else if avail_l {
        if n.left_intra || (n.left_ref[0] == last && n.left_single) {
            2
        } else if n.left_single {
            4 * usize::from(n.left_ref[0] == golden)
        } else {
            3 * usize::from(n.left_ref[0] == golden || n.left_ref[1] == golden)
        }
    } else {
        2
    }
}

/// §6.4.17's `comp_mode` context (§9.3.2). `comp_fixed_ref` is
/// `header.comp_fixed_ref`.
fn comp_mode_ctx(n: NeighborRefInfo, avail_u: bool, avail_l: bool, comp_fixed_ref: i32) -> usize {
    if avail_u && avail_l {
        if n.above_single && n.left_single {
            usize::from((n.above_ref[0] == comp_fixed_ref) != (n.left_ref[0] == comp_fixed_ref))
        } else if n.above_single {
            2 + usize::from(n.above_ref[0] == comp_fixed_ref || n.above_intra)
        } else if n.left_single {
            2 + usize::from(n.left_ref[0] == comp_fixed_ref || n.left_intra)
        } else {
            4
        }
    } else if avail_u {
        if n.above_single { usize::from(n.above_ref[0] == comp_fixed_ref) } else { 3 }
    } else if avail_l {
        if n.left_single { usize::from(n.left_ref[0] == comp_fixed_ref) } else { 3 }
    } else {
        1
    }
}

/// §6.4.17's `comp_ref` context (§9.3.2). `comp_fixed_ref`/`comp_var_ref`
/// are `header.comp_fixed_ref`/`header.comp_var_ref`.
#[allow(clippy::too_many_lines, reason = "transcribed verbatim from the spec's own comp_ref context formula, which is this long in the spec text too")]
fn comp_ref_ctx(n: NeighborRefInfo, avail_u: bool, avail_l: bool, sign_bias: [bool; 4], comp_fixed_ref: i32, comp_var_ref: [i32; 2]) -> usize {
    let fix_ref_idx = usize::from(sign_bias.get(usize::try_from(comp_fixed_ref).unwrap_or(0)).copied().unwrap_or(false));
    let var_ref_idx = 1 - fix_ref_idx;
    let var1 = comp_var_ref[1];
    if avail_u && avail_l {
        if n.above_intra && n.left_intra {
            2
        } else if n.left_intra {
            let a = if n.above_single { n.above_ref[0] } else { n.above_ref.get(var_ref_idx).copied().unwrap_or(0) };
            1 + 2 * usize::from(a != var1)
        } else if n.above_intra {
            let a = if n.left_single { n.left_ref[0] } else { n.left_ref.get(var_ref_idx).copied().unwrap_or(0) };
            1 + 2 * usize::from(a != var1)
        } else {
            let vrfa = if n.above_single { n.above_ref[0] } else { n.above_ref.get(var_ref_idx).copied().unwrap_or(0) };
            let vrfl = if n.left_single { n.left_ref[0] } else { n.left_ref.get(var_ref_idx).copied().unwrap_or(0) };
            if vrfa == vrfl && var1 == vrfa {
                0
            } else if n.left_single && n.above_single {
                if (vrfa == comp_fixed_ref && vrfl == comp_var_ref[0]) || (vrfl == comp_fixed_ref && vrfa == comp_var_ref[0]) {
                    4
                } else if vrfa == vrfl {
                    3
                } else {
                    1
                }
            } else if n.left_single || n.above_single {
                let (vrfc, rfs) = if n.left_single { (vrfa, vrfl) } else { (vrfl, vrfa) };
                if vrfc == var1 && rfs != var1 {
                    1
                } else if rfs == var1 && vrfc != var1 {
                    2
                } else {
                    4
                }
            } else if vrfa == vrfl {
                4
            } else {
                2
            }
        }
    } else if avail_u {
        if n.above_intra {
            2
        } else if n.above_single {
            3 * usize::from(n.above_ref[0] != var1)
        } else {
            4 * usize::from(n.above_ref.get(var_ref_idx).copied().unwrap_or(0) != var1)
        }
    } else if avail_l {
        if n.left_intra {
            2
        } else if n.left_single {
            3 * usize::from(n.left_ref[0] != var1)
        } else {
            4 * usize::from(n.left_ref.get(var_ref_idx).copied().unwrap_or(0) != var1)
        }
    } else {
        2
    }
}

/// §6.4.17's `read_ref_frames`.
fn read_ref_frames(bd: &mut Bd<'_>, ctx: &FrameCtx, segment_id: u8, neighbors: NeighborRefInfo, avail_u: bool, avail_l: bool) -> [i32; 2] {
    if seg_feature_active_for(ctx, segment_id, tables::SEG_LVL_REF_FRAME) {
        let rf = ctx.header.segmentation.feature_data.get(usize::from(segment_id)).and_then(|r| r.get(tables::SEG_LVL_REF_FRAME)).copied().unwrap_or(tables::INTRA_FRAME);
        return [rf, tables::NONE];
    }
    let comp_mode = if ctx.header.reference_mode == tables::REFERENCE_MODE_SELECT {
        let cctx = comp_mode_ctx(neighbors, avail_u, avail_l, ctx.header.comp_fixed_ref);
        let p = ctx.header.entropy.comp_mode_prob.get(cctx).copied().unwrap_or(128);
        if bd.read_bool(p) { tables::COMPOUND_REFERENCE } else { tables::SINGLE_REFERENCE }
    } else {
        ctx.header.reference_mode
    };
    if comp_mode == tables::COMPOUND_REFERENCE {
        let idx = usize::from(ctx.header.ref_frame_sign_bias.get(usize::try_from(ctx.header.comp_fixed_ref).unwrap_or(0)).copied().unwrap_or(false));
        let cctx = comp_ref_ctx(neighbors, avail_u, avail_l, ctx.header.ref_frame_sign_bias, ctx.header.comp_fixed_ref, ctx.header.comp_var_ref);
        let p = ctx.header.entropy.comp_ref_prob.get(cctx).copied().unwrap_or(128);
        let comp_ref = usize::from(bd.read_bool(p));
        let var_ref = ctx.header.comp_var_ref.get(comp_ref).copied().unwrap_or(tables::LAST_FRAME);
        let mut ref_frame = [0i32; 2];
        if let Some(slot) = ref_frame.get_mut(idx) {
            *slot = ctx.header.comp_fixed_ref;
        }
        if let Some(slot) = ref_frame.get_mut(1 - idx) {
            *slot = var_ref;
        }
        ref_frame
    } else {
        let p1ctx = single_ref_p1_ctx(neighbors, avail_u, avail_l);
        let p1 = ctx.header.entropy.single_ref_prob.get(p1ctx).and_then(|r| r.first()).copied().unwrap_or(128);
        if bd.read_bool(p1) {
            let p2ctx = single_ref_p2_ctx(neighbors, avail_u, avail_l);
            let p2 = ctx.header.entropy.single_ref_prob.get(p2ctx).and_then(|r| r.get(1)).copied().unwrap_or(128);
            let rf = if bd.read_bool(p2) { tables::ALTREF_FRAME } else { tables::GOLDEN_FRAME };
            [rf, tables::NONE]
        } else {
            [tables::LAST_FRAME, tables::NONE]
        }
    }
}

/// §6.4.16's `interp_filter` context (§9.3.2).
fn interp_filter_ctx(ctx: &FrameCtx, r: usize, c: usize, avail_u: bool, avail_l: bool) -> usize {
    let left_interp = if avail_l && ctx.mi_at(ix(r), ix(c) - 1).is_some_and(|m| m.ref_frame[0] > tables::INTRA_FRAME) {
        usize::try_from(ctx.mi_at(ix(r), ix(c) - 1).map_or(3, |m| m.interp_filter)).unwrap_or(3)
    } else {
        3
    };
    let above_interp = if avail_u && ctx.mi_at(ix(r) - 1, ix(c)).is_some_and(|m| m.ref_frame[0] > tables::INTRA_FRAME) {
        usize::try_from(ctx.mi_at(ix(r) - 1, ix(c)).map_or(3, |m| m.interp_filter)).unwrap_or(3)
    } else {
        3
    };
    if left_interp == above_interp {
        left_interp
    } else if left_interp == 3 && above_interp != 3 {
        above_interp
    } else if left_interp != 3 && above_interp == 3 {
        left_interp
    } else {
        3
    }
}

/// §6.4.16's `inter_block_mode_info`. Returns `(y_mode, BlockMvs,
/// interp_filter)`.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn inter_block_mode_info(bd: &mut Bd<'_>, ctx: &FrameCtx, entropy: &EntropyContext, r: usize, c: usize, mi_size: i32, segment_id: u8, ref_frame: [i32; 2]) -> (i32, [[[i32; 2]; 4]; 2], i32) {
    let bw = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
    let bh = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
    let is_compound = ref_frame[1] > tables::INTRA_FRAME;

    let cell_at = |rr: i32, cc: i32| ctx.mi_at(rr, cc).map(MiCell::to_mv_cell);
    let prev_cell = || ctx.prev_mv_cell_here(r, c);
    let mv_ctx = mvpred::MvRefContext {
        mi_row: r,
        mi_col: c,
        mi_rows: ctx.mi_rows,
        mi_cols: ctx.mi_cols,
        mi_col_start: 0,
        mi_col_end: ctx.mi_cols,
        cell_at: &cell_at,
        use_prev_frame_mvs: ctx.header.use_prev_frame_mvs,
        prev_cell: &prev_cell,
        ref_frame_sign_bias: ctx.header.ref_frame_sign_bias,
    };

    let mut nearest_mv = [[0i32; 2]; 2];
    let mut near_mv = [[0i32; 2]; 2];
    let mut best_mv = [[0i32; 2]; 2];
    let mut mode_context = [tables::BOTH_PREDICTED; 4];
    for (j, &rf) in ref_frame.iter().enumerate() {
        if rf > tables::INTRA_FRAME {
            let (ref_list_mv, mc) = mvpred::find_mv_refs(&mv_ctx, mi_size, bw, bh, rf, -1);
            if let Some(slot) = mode_context.get_mut(usize::try_from(rf).unwrap_or(0)) {
                *slot = mc;
            }
            let (nmv, nrmv, bmv) = mvpred::find_best_ref_mvs(ref_list_mv, ctx.header.allow_high_precision_mv, r, c, ctx.mi_rows, ctx.mi_cols, bw, bh);
            if let Some(slot) = nearest_mv.get_mut(j) {
                *slot = nmv;
            }
            if let Some(slot) = near_mv.get_mut(j) {
                *slot = nrmv;
            }
            if let Some(slot) = best_mv.get_mut(j) {
                *slot = bmv;
            }
        }
    }

    let mut y_mode = tables::ZEROMV;
    if seg_feature_active_for(ctx, segment_id, tables::SEG_LVL_SKIP) {
        y_mode = tables::ZEROMV;
    } else if mi_size >= tables::BLOCK_8X8 {
        let mctx = usize::try_from(mode_context.get(usize::try_from(ref_frame[0]).unwrap_or(0)).copied().unwrap_or(0)).unwrap_or(0);
        let probs = entropy.inter_mode_probs.get(mctx).copied().unwrap_or([128; 3]);
        let inter_mode = bd.read_tree(&tables::INTER_MODE_TREE, &probs);
        y_mode = tables::NEARESTMV + inter_mode;
    }

    let interp_filter = if ctx.header.interpolation_filter == tables::SWITCHABLE {
        let ictx = interp_filter_ctx(ctx, r, c, r > 0, c > 0);
        let probs = entropy.interp_filter_probs.get(ictx).copied().unwrap_or([128; 2]);
        bd.read_tree(&tables::INTERP_FILTER_TREE, &probs)
    } else {
        ctx.header.interpolation_filter
    };

    let mut block_mvs = [[[0i32; 2]; 4]; 2];
    if mi_size < tables::BLOCK_8X8 {
        let num4x4w = tables::NUM_4X4_BLOCKS_WIDE_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
        let num4x4h = tables::NUM_4X4_BLOCKS_HIGH_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
        let mut idy = 0usize;
        while idy < 2 {
            let mut idx = 0usize;
            while idx < 2 {
                let mctx = usize::try_from(mode_context.get(usize::try_from(ref_frame[0]).unwrap_or(0)).copied().unwrap_or(0)).unwrap_or(0);
                let probs = entropy.inter_mode_probs.get(mctx).copied().unwrap_or([128; 3]);
                let inter_mode = bd.read_tree(&tables::INTER_MODE_TREE, &probs);
                y_mode = tables::NEARESTMV + inter_mode;
                let block = idy * 2 + idx;
                let mut mv = [[0i32; 2]; 2];
                if y_mode == tables::NEARESTMV || y_mode == tables::NEARMV {
                    for (j, &rf) in ref_frame.iter().enumerate() {
                        if j > usize::from(is_compound) {
                            continue;
                        }
                        let (ref_list_mv, _) = mvpred::find_mv_refs(&mv_ctx, mi_size, bw, bh, rf, i32::try_from(block).unwrap_or(0));
                        let block_mvs_ref = block_mvs.get(j).copied().unwrap_or([[0, 0]; 4]);
                        let (nmv, nrmv) = mvpred::append_sub8x8_mvs(ref_list_mv, block, &block_mvs_ref);
                        if let Some(slot) = nearest_mv.get_mut(j) {
                            *slot = nmv;
                        }
                        if let Some(slot) = near_mv.get_mut(j) {
                            *slot = nrmv;
                        }
                    }
                }
                for j in 0..=usize::from(is_compound) {
                    let m = if y_mode == tables::NEWMV {
                        mvpred::read_mv(bd, entropy, best_mv.get(j).copied().unwrap_or([0, 0]), ctx.header.allow_high_precision_mv)
                    } else if y_mode == tables::NEARESTMV {
                        nearest_mv.get(j).copied().unwrap_or([0, 0])
                    } else if y_mode == tables::NEARMV {
                        near_mv.get(j).copied().unwrap_or([0, 0])
                    } else {
                        [0, 0]
                    };
                    if let Some(slot) = mv.get_mut(j) {
                        *slot = m;
                    }
                }
                for y2 in 0..num4x4h {
                    for x2 in 0..num4x4w {
                        let b = (idy + y2) * 2 + idx + x2;
                        for ref_list in 0..=usize::from(is_compound) {
                            if let Some(row) = block_mvs.get_mut(ref_list)
                                && let Some(slot) = row.get_mut(b)
                            {
                                *slot = mv.get(ref_list).copied().unwrap_or([0, 0]);
                            }
                        }
                    }
                }
                idx += num4x4w;
            }
            idy += num4x4h;
        }
    } else {
        let mv = assign_mv(bd, entropy, y_mode, is_compound, nearest_mv, near_mv, best_mv, ctx.header.allow_high_precision_mv);
        for ref_list in 0..2 {
            if let Some(row) = block_mvs.get_mut(ref_list) {
                *row = [mv.get(ref_list).copied().unwrap_or([0, 0]); 4];
            }
        }
    }

    (y_mode, block_mvs, interp_filter)
}

/// §6.4.18's `assign_mv`.
fn assign_mv(bd: &mut Bd<'_>, entropy: &EntropyContext, y_mode: i32, is_compound: bool, nearest_mv: [[i32; 2]; 2], near_mv: [[i32; 2]; 2], best_mv: [[i32; 2]; 2], allow_high_precision_mv: bool) -> [[i32; 2]; 2] {
    let mut mv = [[0i32; 2]; 2];
    for (i, slot) in mv.iter_mut().enumerate().take(1 + usize::from(is_compound)) {
        *slot = if y_mode == tables::NEWMV {
            mvpred::read_mv(bd, entropy, best_mv.get(i).copied().unwrap_or([0, 0]), allow_high_precision_mv)
        } else if y_mode == tables::NEARESTMV {
            nearest_mv.get(i).copied().unwrap_or([0, 0])
        } else if y_mode == tables::NEARMV {
            near_mv.get(i).copied().unwrap_or([0, 0])
        } else {
            [0, 0]
        };
    }
    mv
}

/// §6.4.11's `inter_frame_mode_info`.
#[allow(clippy::too_many_arguments)]
fn inter_frame_mode_info(bd: &mut Bd<'_>, ctx: &mut FrameCtx, entropy: &EntropyContext, r: usize, c: usize, subsize: i32, avail_u: bool, avail_l: bool) -> DecodedBlock {
    let neighbors = neighbor_ref_info(ctx, r, c, avail_u, avail_l);

    let segment_id = inter_segment_id(bd, ctx, r, c, subsize);
    let skip = if seg_feature_active_for(ctx, segment_id, tables::SEG_LVL_SKIP) {
        true
    } else {
        let above = ctx.mi_at(ix(r) - 1, ix(c)).is_some_and(|m| m.skip);
        let left = ctx.mi_at(ix(r), ix(c) - 1).is_some_and(|m| m.skip);
        let sctx = usize::from(avail_u && above) + usize::from(avail_l && left);
        let p = entropy_skip_prob(ctx, sctx);
        bd.read_bool(p)
    };
    let is_inter = read_is_inter(bd, ctx, segment_id, neighbors, avail_u, avail_l);
    let tx_size = read_tx_size(bd, ctx, r, c, subsize, avail_u, avail_l, !skip || !is_inter);

    if is_inter {
        let ref_frame = read_ref_frames(bd, ctx, segment_id, neighbors, avail_u, avail_l);
        let (y_mode, block_mvs, interp_filter) = inter_block_mode_info(bd, ctx, entropy, r, c, subsize, segment_id, ref_frame);
        DecodedBlock {
            segment_id,
            skip,
            is_inter: true,
            tx_size,
            y_mode,
            uv_mode: 0,
            sub_modes: [tables::DC_PRED; 4],
            ref_frame,
            block_mvs,
            interp_filter,
        }
    } else {
        let (y_mode, uv_mode, sub_modes) = intra_block_mode_info(bd, entropy, subsize);
        DecodedBlock {
            segment_id,
            skip,
            is_inter: false,
            tx_size,
            y_mode,
            uv_mode,
            sub_modes,
            ref_frame: [tables::INTRA_FRAME, tables::NONE],
            block_mvs: [[[0, 0]; 4]; 2],
            interp_filter: tables::EIGHTTAP,
        }
    }
}

pub(crate) fn get_uv_tx_size(mi_size: i32, tx_size: i32, subsampling_x: bool, subsampling_y: bool) -> i32 {
    if mi_size < tables::BLOCK_8X8 {
        return tables::TX_4X4;
    }
    let plane_sz = get_plane_block_size(mi_size, 1, subsampling_x, subsampling_y);
    let max_uv = tables::MAX_TXSIZE_LOOKUP.get(usize::try_from(plane_sz).unwrap_or(0)).copied().unwrap_or(0);
    tx_size.min(max_uv)
}

fn get_plane_block_size(subsize: i32, plane: usize, subsampling_x: bool, subsampling_y: bool) -> i32 {
    let subx = if plane > 0 { usize::from(subsampling_x) } else { 0 };
    let suby = if plane > 0 { usize::from(subsampling_y) } else { 0 };
    tables::SS_SIZE_LOOKUP
        .get(usize::try_from(subsize).unwrap_or(0))
        .and_then(|row| row.get(subx))
        .and_then(|row2| row2.get(suby))
        .copied()
        .unwrap_or(tables::BLOCK_INVALID)
}

fn residual(bd: &mut Bd<'_>, ctx: &mut FrameCtx, entropy: &EntropyContext, r: usize, c: usize, mi_size: i32, block: &DecodedBlock) -> bool {
    let bsize = if mi_size < tables::BLOCK_8X8 { tables::BLOCK_8X8 } else { mi_size };
    let (subx, suby) = (ctx.header.color.subsampling_x, ctx.header.color.subsampling_y);
    let bit_depth = u32::from(ctx.header.color.bit_depth);
    let lossless = ctx.header.quant.lossless;
    let tx_size = block.tx_size;
    let skip = block.skip;
    let segment_id = block.segment_id;
    let y_mode = block.y_mode;
    let uv_mode = block.uv_mode;
    let sub_modes = &block.sub_modes;
    let mut eob_total = 0usize;

    for plane in 0..3usize {
        let tx_sz = if plane > 0 { get_uv_tx_size(mi_size, tx_size, subx, suby) } else { tx_size };
        let step = 1usize << tx_sz;
        let plane_sz = get_plane_block_size(bsize, plane, subx, suby);
        let num4x4w = tables::NUM_4X4_BLOCKS_WIDE_LOOKUP.get(usize::try_from(plane_sz).unwrap_or(0)).copied().unwrap_or(1);
        let num4x4h = tables::NUM_4X4_BLOCKS_HIGH_LOOKUP.get(usize::try_from(plane_sz).unwrap_or(0)).copied().unwrap_or(1);
        let (sub_x, sub_y) = if plane > 0 { (subx, suby) } else { (false, false) };
        let base_x = (c * 8) >> u32::from(sub_x);
        let base_y = (r * 8) >> u32::from(sub_y);
        let maxx = (ctx.mi_cols * 8) >> u32::from(sub_x);
        let maxy = (ctx.mi_rows * 8) >> u32::from(sub_y);

        // §6.4.21's inter branch: the whole plane region is motion-compensated
        // once (or once per 4x4 for a sub-8x8 block), *before* the
        // per-transform-block residual loop below — unlike intra prediction,
        // which has to interleave with residual decode because each transform
        // block's prediction depends on the previous one's already-added
        // residual.
        if block.is_inter {
            if mi_size < tables::BLOCK_8X8 {
                for y in 0..num4x4h {
                    for x in 0..num4x4w {
                        let block_idx = y * num4x4w + x;
                        predict_inter_region(ctx, plane, r, c, mi_size, block, base_x + 4 * x, base_y + 4 * y, 4, 4, block_idx, subx, suby, bit_depth);
                    }
                }
            } else {
                predict_inter_region(ctx, plane, r, c, mi_size, block, base_x, base_y, num4x4w * 4, num4x4h * 4, 0, subx, suby, bit_depth);
            }
        }

        let (dc_quant, ac_quant) = {
            let qindex = get_qindex(ctx, segment_id);
            let (delta_dc, delta_ac) =
                if plane == 0 { (ctx.header.quant.delta_q_y_dc, 0) } else { (ctx.header.quant.delta_q_uv_dc, ctx.header.quant.delta_q_uv_ac) };
            dequant_factors(ctx.header.color.bit_depth, qindex, delta_dc, delta_ac)
        };

        let mut block_idx = 0usize;
        let mut y = 0usize;
        while y < num4x4h {
            let mut x = 0usize;
            while x < num4x4w {
                let start_x = base_x + 4 * x;
                let start_y = base_y + 4 * y;
                let mut nonzero = false;
                if start_x < maxx && start_y < maxy {
                    if !block.is_inter {
                        let mode = if plane > 0 {
                            uv_mode
                        } else if mi_size >= tables::BLOCK_8X8 {
                            y_mode
                        } else {
                            sub_modes.get(block_idx).copied().unwrap_or(tables::DC_PRED)
                        };
                        let have_left = c > 0 || x > 0;
                        let have_above = r > 0 || y > 0;
                        let not_on_right = x + step < num4x4w;
                        predict_block(ctx, plane, start_x, start_y, mode, tx_sz, have_left, have_above, not_on_right, maxx, maxy, bit_depth);
                    }

                    if !skip {
                        let tx_type = get_scan_tx_type(plane, tx_sz, lossless, block.is_inter, mi_size, y_mode, sub_modes, block_idx);
                        let scan = tables::get_scan(tx_sz, tx_type);
                        let (coefs, eob) =
                            tokens::decode_tokens(bd, entropy, ctx, plane, start_x, start_y, tx_sz, scan, tx_type, block.is_inter, subx, suby, bit_depth);
                        nonzero = eob > 0;
                        if nonzero {
                            eob_total += 1;
                            let residue = reconstruct(&coefs, tx_sz, dc_quant, ac_quant, tx_type, lossless);
                            add_residue(ctx, plane, start_x, start_y, tx_sz, &residue, bit_depth);
                        }
                    }
                }
                for i in 0..step {
                    if let Some(row) = ctx.above_nz.get_mut(plane)
                        && let Some(slot) = row.get_mut((start_x >> 2) + i)
                    {
                        *slot = nonzero;
                    }
                    if let Some(row) = ctx.left_nz.get_mut(plane)
                        && let Some(slot) = row.get_mut(((start_y >> 2) % 16) + i)
                    {
                        *slot = nonzero;
                    }
                }
                block_idx += 1;
                x += step;
            }
            y += step;
        }
    }
    eob_total > 0
}

/// The reference-frame slot `ref_frame` (a `LAST_FRAME`/`GOLDEN_FRAME`/
/// `ALTREF_FRAME` value) maps to, via `ref_frame_idx`.
fn ref_slot_for(ctx: &FrameCtx, ref_frame: i32) -> Option<RefSlot> {
    let idx = usize::try_from(ref_frame - tables::LAST_FRAME).ok()?;
    let slot_idx = ctx.header.ref_frame_idx.get(idx).copied()?;
    ctx.ref_store.get(slot_idx).cloned()
}

/// §8.5.2's inter prediction process for one `w x h` region of one plane.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::many_single_char_names, reason = "x/y/w/h/a/b/v are pixel coordinates, dimensions and sample values, matching the spec's own single-letter notation")]
fn predict_inter_region(ctx: &mut FrameCtx, plane: usize, mi_row: usize, mi_col: usize, mi_size: i32, block: &DecodedBlock, x: usize, y: usize, w: usize, h: usize, block_idx: usize, subsampling_x: bool, subsampling_y: bool, bit_depth: u32) {
    let is_compound = block.ref_frame[1] > tables::NONE;
    let bw = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
    let bh = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or(1);
    let mi_size_ge_8x8 = mi_size >= tables::BLOCK_8X8;
    let frame_width = ctx.header.width;
    let frame_height = ctx.header.height;

    let mut preds: [Vec<i32>; 2] = [vec![0i32; w * h], vec![0i32; w * h]];
    for ref_list in 0..=usize::from(is_compound) {
        let rf = block.ref_frame.get(ref_list).copied().unwrap_or(tables::NONE);
        let Some(slot) = ref_slot_for(ctx, rf) else { continue };
        let block_mvs = block.block_mvs.get(ref_list).copied().unwrap_or([[0, 0]; 4]);
        let mv = interpredict::select_mv(&block_mvs, block_idx, plane, mi_size_ge_8x8, subsampling_x, subsampling_y);
        let clamped = interpredict::clamp_mv(mv, mi_row, mi_col, ctx.mi_rows, ctx.mi_cols, bw, bh, plane, subsampling_x, subsampling_y);
        let scaled = interpredict::scale_mv(clamped, x, y, plane, subsampling_x, subsampling_y, slot.width, slot.height, frame_width, frame_height);
        let interp_filter = usize::try_from(block.interp_filter).unwrap_or(0);
        if let Some(pred) = preds.get_mut(ref_list) {
            interpredict::block_inter_predict(pred, &slot, plane, &scaled, w, h, interp_filter, bit_depth);
        }
    }

    let plane_mut = ctx.plane_mut(plane);
    let clip_max = (1i32 << bit_depth) - 1;
    for i in 0..h {
        for j in 0..w {
            let v = if is_compound {
                let a = preds[0].get(i * w + j).copied().unwrap_or(0);
                let b = preds[1].get(i * w + j).copied().unwrap_or(0);
                (a + b + 1) >> 1
            } else {
                preds[0].get(i * w + j).copied().unwrap_or(0)
            };
            plane_mut.set(x + j, y + i, u16::try_from(v.clamp(0, clip_max)).unwrap_or(0));
        }
    }
}

fn get_qindex(ctx: &FrameCtx, segment_id: u8) -> i32 {
    let seg = &ctx.header.segmentation;
    if seg.enabled
        && seg.feature_enabled.get(usize::from(segment_id)).and_then(|r| r.get(tables::SEG_LVL_ALT_Q)).copied().unwrap_or(false)
    {
        let mut data = seg.feature_data.get(usize::from(segment_id)).and_then(|r| r.get(tables::SEG_LVL_ALT_Q)).copied().unwrap_or(0);
        if !seg.abs_or_delta_update {
            data += ctx.header.quant.base_q_idx;
        }
        data.clamp(0, 255)
    } else {
        ctx.header.quant.base_q_idx
    }
}

#[allow(clippy::too_many_arguments)]
fn get_scan_tx_type(plane: usize, tx_sz: i32, lossless: bool, is_inter: bool, mi_size: i32, y_mode: i32, sub_modes: &[i32; 4], block_idx: usize) -> TxType {
    if plane > 0 || tx_sz == tables::TX_32X32 {
        TxType::DctDct
    } else if tx_sz == tables::TX_4X4 {
        if lossless || is_inter {
            TxType::DctDct
        } else {
            let mode = if mi_size < tables::BLOCK_8X8 { sub_modes.get(block_idx).copied().unwrap_or(tables::DC_PRED) } else { y_mode };
            tables::MODE2TXFM_MAP.get(usize::try_from(mode).unwrap_or(0)).copied().unwrap_or(TxType::DctDct)
        }
    } else {
        tables::MODE2TXFM_MAP.get(usize::try_from(y_mode).unwrap_or(0)).copied().unwrap_or(TxType::DctDct)
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_block(
    ctx: &mut FrameCtx,
    plane: usize,
    x: usize,
    y: usize,
    mode: i32,
    tx_sz: i32,
    have_left: bool,
    have_above: bool,
    not_on_right: bool,
    maxx: usize,
    maxy: usize,
    bit_depth: u32,
) {
    let log2_size = u32::try_from(tx_sz + 2).unwrap_or(2);
    let size = 1usize << log2_size;
    let half = 1i32 << (bit_depth - 1);
    let p = ctx.plane(plane);
    let (xi, yi) = (ix(x), ix(y));
    let mut above_row = vec![0i32; 2 * size + 1];
    for i in 0..size {
        let v = if have_above { i32::from(p.get_clamped((xi + ix(i)).min(ix(maxx) - 1), yi - 1)) } else { half - 1 };
        if let Some(slot) = above_row.get_mut(1 + i) {
            *slot = v;
        }
    }
    for i in size..2 * size {
        let v = if have_above && not_on_right && tx_sz == tables::TX_4X4 {
            i32::from(p.get_clamped((xi + ix(i)).min(ix(maxx) - 1), yi - 1))
        } else {
            above_row.get(size).copied().unwrap_or(half - 1)
        };
        if let Some(slot) = above_row.get_mut(1 + i) {
            *slot = v;
        }
    }
    let corner = if have_above && have_left {
        i32::from(p.get_clamped((xi - 1).min(ix(maxx) - 1), yi - 1))
    } else if have_above {
        half + 1
    } else {
        half - 1
    };
    if let Some(slot) = above_row.first_mut() {
        *slot = corner;
    }
    let mut left_col = vec![0i32; size];
    for (i, slot) in left_col.iter_mut().enumerate() {
        *slot = if have_left {
            i32::from(p.get_clamped(xi - 1, (yi + ix(i)).min(ix(maxy) - 1)))
        } else {
            half + 1
        };
    }

    let mut pred = vec![0i32; size * size];
    predict_intra(&mut pred, mode, size, log2_size, &above_row, &left_col, have_left, have_above, bit_depth);

    let clip_max = (1i32 << bit_depth) - 1;
    let plane_mut = ctx.plane_mut(plane);
    for i in 0..size {
        for j in 0..size {
            let v = pred.get(i * size + j).copied().unwrap_or(0).clamp(0, clip_max);
            plane_mut.set(x + j, y + i, u16::try_from(v).unwrap_or(0));
        }
    }
}

fn add_residue(ctx: &mut FrameCtx, plane: usize, x: usize, y: usize, tx_sz: i32, residue: &[i64], bit_depth: u32) {
    let n0 = 1usize << (tx_sz + 2);
    let clip_max = (1i64 << bit_depth) - 1;
    let plane_mut = ctx.plane_mut(plane);
    for i in 0..n0 {
        for j in 0..n0 {
            let cur = i64::from(plane_mut.get_clamped(ix(x + j), ix(y + i)));
            let res = residue.get(i * n0 + j).copied().unwrap_or(0);
            let v = (cur + res).clamp(0, clip_max);
            plane_mut.set(x + j, y + i, u16::try_from(v).unwrap_or(0));
        }
    }
}

fn decode_tile(bd: &mut Bd<'_>, ctx: &mut FrameCtx, entropy: &EntropyContext, mi_row_start: usize, mi_row_end: usize) {
    let mut r = mi_row_start;
    while r < mi_row_end {
        for row in &mut ctx.left_partition_context {
            *row = 0;
        }
        for plane_nz in &mut ctx.left_nz {
            for slot in plane_nz.iter_mut() {
                *slot = false;
            }
        }
        for slot in &mut ctx.left_seg_pred_context {
            *slot = false;
        }
        let mut c = 0usize;
        while c < ctx.mi_cols {
            decode_partition(bd, ctx, entropy, r, c, tables::BLOCK_64X64);
            c += 8;
        }
        r += 8;
    }
}

/// Decode one already-superframe-split VP9 frame's tile data. Returns the
/// reconstructed picture plus this frame's own per-MI grid and segment map
/// (`prev_mv_grid`/`prev_segment_ids` for whichever frame reads them next).
#[allow(clippy::too_many_arguments)]
fn decode_frame_tiles(
    header: &FrameHeader,
    entropy: &EntropyContext,
    tile_data: &[u8],
    pic: Picture,
    ref_store: &RefFrameStore,
    prev_mv_grid: &[MiCell],
    prev_mi_cols: usize,
    prev_mi_rows: usize,
    prev_segment_ids: &[u8],
) -> (Picture, Vec<MiCell>, Vec<u8>) {
    let cell_count = header.mi_rows.max(1) * header.mi_cols.max(1);
    let same_dims = prev_mi_cols == header.mi_cols && prev_mi_rows == header.mi_rows;
    let mut ctx = FrameCtx {
        header: header.clone(),
        mi_cols: header.mi_cols,
        mi_rows: header.mi_rows,
        grid: vec![MiCell::default(); cell_count],
        above_partition_context: vec![0u8; header.mi_cols.max(1)],
        left_partition_context: [0u8; 8],
        above_nz: [
            vec![false; header.mi_cols * 2 + 16],
            vec![false; header.mi_cols * 2 + 16],
            vec![false; header.mi_cols * 2 + 16],
        ],
        left_nz: [[false; 16]; 3],
        above_seg_pred_context: vec![false; header.mi_cols.max(1)],
        left_seg_pred_context: [false; 8],
        prev_segment_ids: if same_dims { prev_segment_ids.to_vec() } else { vec![0u8; cell_count] },
        segment_ids: vec![0u8; cell_count],
        prev_mv_grid: if same_dims { prev_mv_grid.to_vec() } else { Vec::new() },
        ref_store: ref_store.clone(),
        pic,
    };

    let tile_cols = 1usize << header.tile.cols_log2;
    let tile_rows = 1usize << header.tile.rows_log2;
    let mut sz = tile_data;
    for tile_row in 0..tile_rows {
        for tile_col in 0..tile_cols {
            let last = tile_row == tile_rows - 1 && tile_col == tile_cols - 1;
            let tile_size = if last {
                sz.len()
            } else {
                let (size_bytes, rest) = sz.split_at_checked(4).unwrap_or((&[], sz));
                sz = rest;
                let mut v = 0usize;
                for &b in size_bytes {
                    v = (v << 8) | usize::from(b);
                }
                v
            };
            let Some(this_tile) = sz.get(..tile_size) else { break };
            sz = sz.get(tile_size..).unwrap_or(&[]);

            let mi_row_start = tile_offset(tile_row, header.mi_rows, header.tile.rows_log2);
            let mi_row_end = tile_offset(tile_row + 1, header.mi_rows, header.tile.rows_log2);
            // Single tile column supported (`MiColStart` is always 0 in
            // decode_block's `AvailL` check above) — a multi-tile-column
            // stream still decodes each column's own bits correctly here,
            // but `AvailL`'s column-0 assumption means context at a
            // non-first tile column's left edge is not spec-exact. See
            // `planning/TECH-DEBT.md`.
            let mut bd = Bd::new(this_tile);
            decode_tile(&mut bd, &mut ctx, entropy, mi_row_start, mi_row_end);
        }
    }
    let lf_grid: Vec<loopfilter::MiInfo> = ctx
        .grid
        .iter()
        .map(|cell| loopfilter::MiInfo { mi_size: cell.mi_size, tx_size: cell.tx_size, skip: cell.skip, ref_frame0: cell.ref_frame[0], y_mode: cell.y_mode })
        .collect();
    let grid = loopfilter::Grid { mi: &lf_grid, segment_ids: &ctx.segment_ids, mi_rows: ctx.mi_rows, mi_cols: ctx.mi_cols };
    loopfilter::filter_frame(&mut ctx.pic, &grid, &header.loop_filter, &header.segmentation, header.color.subsampling_x, header.color.subsampling_y, u32::from(header.color.bit_depth));

    (ctx.pic, ctx.grid, ctx.segment_ids)
}

fn tile_offset(tile_num: usize, mis: usize, tile_size_log2: u32) -> usize {
    let sbs = (mis + 7) >> 3;
    let offset = ((tile_num * sbs) >> tile_size_log2) << 3;
    offset.min(mis)
}

/// Everything the decoder keeps across packets: persisted loop-filter and
/// segmentation state (`parse_uncompressed_header` needs the previous
/// frame's, per §6.2/§7.2's persistence rules), the reference-frame store
/// (§7.2), the four saved probability contexts `save_probs`/`load_probs`
/// switch between (forward-update only — see the crate doc on backward
/// adaptation), and the previous frame's per-MI grid/segment map for the
/// next frame's `UsePrevFrameMvs`/`PrevSegmentIds`.
struct State {
    loop_filter: LoopFilterParams,
    segmentation: Segmentation,
    frame_contexts: [EntropyContext; 4],
    ref_store: RefFrameStore,
    prev_frame_info: Option<PrevFrameInfo>,
    prev_mv_grid: Vec<MiCell>,
    prev_mi_cols: usize,
    prev_mi_rows: usize,
    prev_segment_ids: Vec<u8>,
    /// The sequence's color config as last established by a key frame or a
    /// profile>0 intra-only frame — a regular inter frame's own header does
    /// not carry one (see `header::parse_uncompressed_header`'s doc). The
    /// spec-default initial value here is only ever observed if a malformed
    /// stream's first frame is not a key frame.
    color: header::ColorConfig,
}

impl Default for State {
    fn default() -> Self {
        Self {
            loop_filter: LoopFilterParams::default(),
            segmentation: Segmentation::default(),
            frame_contexts: std::array::from_fn(|_| EntropyContext::default()),
            ref_store: RefFrameStore::default(),
            prev_frame_info: None,
            prev_mv_grid: Vec::new(),
            prev_mi_cols: 0,
            prev_mi_rows: 0,
            prev_segment_ids: Vec::new(),
            color: header::ColorConfig { bit_depth: 8, color_space: 1, full_range: false, subsampling_x: true, subsampling_y: true },
        }
    }
}

pub struct Vp9Decoder {
    machine: vaco_codec_core::machine::Machine<Frame>,
    limits: Limits,
    budget: Budget,
    state: State,
}

impl std::fmt::Debug for Vp9Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp9Decoder").finish_non_exhaustive()
    }
}

impl Vp9Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: vaco_codec_core::machine::Machine::new(vaco_codec_core::Caps::SUBFRAMES),
            budget: Budget::new(limits.clone()),
            limits,
            state: State::default(),
        }
    }

    fn decode_one_frame(&mut self, data: &[u8], pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let mut ref_dims: [Option<RefFrameDims>; tables::NUM_REF_FRAMES] = [None; tables::NUM_REF_FRAMES];
        for (i, slot) in ref_dims.iter_mut().enumerate() {
            if let Some((w, h)) = self.state.ref_store.dims(u8::try_from(i).unwrap_or(0)) {
                *slot = Some(RefFrameDims { width: w, height: h });
            }
        }

        let Some((mut fh, header_bytes)) = header::parse_uncompressed_header(
            data,
            self.state.loop_filter,
            self.state.segmentation,
            &ref_dims,
            self.state.prev_frame_info,
            self.state.color,
        ) else {
            return Ok(());
        };
        self.state.loop_filter = fh.loop_filter;
        self.state.segmentation = fh.segmentation;
        self.state.color = fh.color;

        if fh.show_existing_frame {
            if let Some(slot) = self.state.ref_store.get(fh.frame_to_show_map_idx).cloned() {
                let mut frame = ref_slot_to_frame(&mut self.budget, &slot)?;
                frame.pts = pts;
                frame.duration = duration;
                self.machine.emit(frame);
            }
            return Ok(());
        }

        // §6.2's `if (FrameIsIntra || error_resilient_mode) { setup_past_independence(); ...save_probs...; frame_context_idx = 0 }`.
        if fh.frame_is_intra || fh.error_resilient_mode {
            if fh.is_key_frame || fh.error_resilient_mode || fh.reset_frame_context == 3 {
                self.state.frame_contexts = std::array::from_fn(|_| EntropyContext::default());
            } else if fh.reset_frame_context == 2
                && let Some(slot) = self.state.frame_contexts.get_mut(usize::from(fh.frame_context_idx))
            {
                *slot = EntropyContext::default();
            }
            fh.frame_context_idx = 0;
        }

        let mut entropy = self.state.frame_contexts.get(usize::from(fh.frame_context_idx)).cloned().unwrap_or_default();
        let header_end = header_bytes + usize::from(fh.header_size_in_bytes);
        let Some(compressed) = data.get(header_bytes..header_end) else { return Ok(()) };
        let mut cbd = Bd::new(compressed);
        let info =
            header::parse_compressed_header(&mut cbd, fh.quant.lossless, fh.frame_is_intra, fh.allow_high_precision_mv, fh.ref_frame_sign_bias, fh.interpolation_filter, &mut entropy);
        fh.tx_mode = info.tx_mode;
        fh.reference_mode = info.reference_mode;
        fh.comp_fixed_ref = info.comp_fixed_ref;
        fh.comp_var_ref = info.comp_var_ref;
        fh.entropy = entropy.clone();

        let tile_data = data.get(header_end..).unwrap_or(&[]);

        let luma_w = fh.mi_cols * 8;
        let luma_h = fh.mi_rows * 8;
        let chroma_w = luma_w >> u32::from(fh.color.subsampling_x);
        let chroma_h = luma_h >> u32::from(fh.color.subsampling_y);
        let pic = Picture::new(&mut self.budget, luma_w, luma_h, chroma_w, chroma_h)?;

        let (pic, mv_grid, segment_ids) = decode_frame_tiles(
            &fh,
            &entropy,
            tile_data,
            pic,
            &self.state.ref_store,
            &self.state.prev_mv_grid,
            self.state.prev_mi_cols,
            self.state.prev_mi_rows,
            &self.state.prev_segment_ids,
        );

        // §6.1.2's `refresh_probs()`: forward-updated probabilities are
        // saved back when `refresh_frame_context` is set. Backward
        // adaptation (§8.4) — folding this frame's own coefficient/mode/MV
        // symbol counts into the saved context before saving it — is not
        // implemented; see the crate doc and `planning/TECH-DEBT.md`.
        if fh.refresh_frame_context
            && let Some(slot) = self.state.frame_contexts.get_mut(usize::from(fh.frame_context_idx))
        {
            *slot = entropy;
        }
        self.state.prev_frame_info = Some(PrevFrameInfo { width: fh.width, height: fh.height, show_frame: fh.show_frame });
        self.state.prev_mv_grid = mv_grid;
        self.state.prev_mi_cols = fh.mi_cols;
        self.state.prev_mi_rows = fh.mi_rows;
        self.state.prev_segment_ids = segment_ids;

        let rc_pic = std::sync::Arc::new(pic);
        self.state.ref_store.refresh(
            fh.refresh_frame_flags,
            &RefSlot { pic: rc_pic.clone(), width: fh.width, height: fh.height, subsampling_x: fh.color.subsampling_x, subsampling_y: fh.color.subsampling_y, bit_depth: fh.color.bit_depth },
        );

        if fh.show_frame {
            let mut frame = pic_to_frame(&mut self.budget, &fh, &rc_pic)?;
            if fh.is_key_frame {
                frame.flags |= vaco_frame::FrameFlags::KEY;
            }
            frame.pts = pts;
            frame.duration = duration;
            self.machine.emit(frame);
        }
        Ok(())
    }
}

/// Emit an already-decoded reference-frame-store slot directly
/// (`show_existing_frame`'s whole point: no decode work at all, just point
/// the output at a picture decoded by an earlier packet).
fn ref_slot_to_frame(budget: &mut Budget, slot: &RefSlot) -> Result<Frame> {
    let fh = FrameHeader {
        profile: 0,
        show_existing_frame: false,
        frame_to_show_map_idx: 0,
        is_key_frame: false,
        show_frame: true,
        error_resilient_mode: false,
        intra_only: false,
        frame_is_intra: false,
        color: header::ColorConfig { bit_depth: slot.bit_depth, color_space: 0, full_range: false, subsampling_x: slot.subsampling_x, subsampling_y: slot.subsampling_y },
        width: slot.width,
        height: slot.height,
        mi_cols: 0,
        mi_rows: 0,
        sb64_cols: 0,
        sb64_rows: 0,
        refresh_frame_flags: 0,
        refresh_frame_context: false,
        frame_parallel_decoding_mode: true,
        reset_frame_context: 0,
        frame_context_idx: 0,
        ref_frame_idx: [0; 3],
        ref_frame_sign_bias: [false; 4],
        allow_high_precision_mv: false,
        interpolation_filter: tables::EIGHTTAP,
        use_prev_frame_mvs: false,
        loop_filter: LoopFilterParams::default(),
        quant: header::QuantParams::default(),
        segmentation: Segmentation::default(),
        tile: header::TileInfo::default(),
        header_size_in_bytes: 0,
        entropy: EntropyContext::default(),
        tx_mode: tables::ONLY_4X4,
        reference_mode: tables::SINGLE_REFERENCE,
        comp_fixed_ref: 0,
        comp_var_ref: [0; 2],
    };
    pic_to_frame(budget, &fh, &slot.pic)
}

fn pic_to_frame(budget: &mut Budget, fh: &FrameHeader, pic: &Picture) -> Result<Frame> {
    // Profile 1/3 read `subsampling_x`/`subsampling_y` independently
    // (§6.2.2), so `(x=0, y=1)` — 4:4:0-style vertical-only subsampling —
    // is syntactically legal even though no test fixture in this crate's
    // reach produces one; matched for totality, not verified.
    let name = match (fh.color.bit_depth, fh.color.subsampling_x, fh.color.subsampling_y) {
        (8, true, true) => "yuv420p".to_string(),
        (8, true, false) => "yuv422p".to_string(),
        (8, false, false) => "yuv444p".to_string(),
        (8, false, true) => "yuv440p".to_string(),
        (b, true, true) => format!("yuv420p{b}le"),
        (b, true, false) => format!("yuv422p{b}le"),
        (b, false, false) => format!("yuv444p{b}le"),
        (b, false, true) => format!("yuv440p{b}le"),
    };
    let pix_fmt = PixFmt::from_name(&name).map_err(|_| Error::InvalidData("vp9: unsupported pixel format"))?;

    let width = fh.width;
    let height = fh.height;
    let mut frame = Frame::alloc_video(budget, pix_fmt, width, height)?;
    let (w, h) = (usize::try_from(width).unwrap_or(0), usize::try_from(height).unwrap_or(0));
    let cw = w.div_ceil(1 + usize::from(fh.color.subsampling_x));
    let ch = h.div_ceil(1 + usize::from(fh.color.subsampling_y));
    blit(&pic.y, &mut frame, 0, w, h, fh.color.bit_depth);
    blit(&pic.u, &mut frame, 1, cw, ch, fh.color.bit_depth);
    blit(&pic.v, &mut frame, 2, cw, ch, fh.color.bit_depth);
    Ok(frame)
}

fn blit(src: &crate::framebuf::Plane, frame: &mut Frame, plane_index: usize, width: usize, height: usize, bit_depth: u8) {
    let Some(mut dst) = frame.plane_mut(plane_index) else { return };
    let two_bytes = bit_depth > 8;
    for y in 0..height {
        let Some(row) = dst.row_mut(y) else { continue };
        for x in 0..width {
            let v = src.get_clamped(ix(x), ix(y));
            if two_bytes {
                let bytes = v.to_le_bytes();
                if let Some(b) = row.get_mut(2 * x) {
                    *b = bytes[0];
                }
                if let Some(b) = row.get_mut(2 * x + 1) {
                    *b = bytes[1];
                }
            } else if let Some(b) = row.get_mut(x) {
                *b = u8::try_from(v).unwrap_or(0);
            }
        }
    }
}

impl Decoder for Vp9Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            vaco_codec_core::machine::Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            vaco_codec_core::machine::Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                for frame_data in superframe::split(pkt.payload()) {
                    self.decode_one_frame(frame_data, pkt.pts, pkt.duration)?;
                }
                Ok(())
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.state = State::default();
        self.budget = Budget::new(self.limits.clone());
    }
}

/// `vaco-component.toml`'s decoder registration point.
pub static VP9_DECODER: DecoderDesc = DecoderDesc {
    name: "vp9",
    long_name: "VP9 (no loop filter; VP9 Bitstream & Decoding Process Specification v0.6)",
    id: vaco_codec_core::CodecId::Vp9,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::SUBFRAMES,
    supported_rates: &[],
    make: |limits| Box::new(Vp9Decoder::new(limits)),
};
