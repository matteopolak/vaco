//! The per-superblock decode orchestrator: §6.4's `decode_tiles`/
//! `decode_partition`/`decode_block`/`intra_frame_mode_info`/`residual`/
//! `tokens`, tied to §8.5.1's intra prediction and §8.6/§8.7's
//! dequantization and inverse transform, plus the [`Decoder`] trait impl.
//!
//! Scope: key frames only (`FrameIsIntra` is always true on the path this
//! module exercises for real pixel output) — see the crate doc.

use vaco_codec_core::{Decoder, DecoderDesc};
use vaco_codec_dsp_idct::vp9::TxType;
use vaco_codec_msac::Vp9BoolDecoder as Bd;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::framebuf::Picture;
use crate::header::{self, EntropyContext, FrameHeader, LoopFilterParams, Segmentation};
use crate::predict::predict_intra;
use crate::tables;
use crate::transform::{dequant_factors, reconstruct};
use crate::{superframe, tokens};

fn ix(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// One 8x8 "mode info" grid cell's persisted state, read back for neighbour
/// context derivation by later blocks in the same tile.
#[derive(Debug, Clone, Copy, Default)]
struct MiCell {
    skip: bool,
    tx_size: i32,
    sub_modes: [i32; 4],
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

    fn store_block(&mut self, r: usize, c: usize, subsize: i32, cell: MiCell) {
        let h = tables::NUM_8X8_BLOCKS_HIGH_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        let w = tables::NUM_8X8_BLOCKS_WIDE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(1);
        for y in 0..h {
            for x in 0..w {
                let (rr, cc) = (r + y, c + x);
                if rr < self.mi_rows
                    && cc < self.mi_cols
                    && let Some(slot) = self.grid.get_mut(rr * self.mi_cols + cc)
                {
                    *slot = cell;
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
    let probs = tables::KF_PARTITION_PROBS.get(pctx).copied().unwrap_or([128; 3]);
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

    let (segment_id, skip, tx_size, y_mode, uv_mode, sub_modes) =
        intra_frame_mode_info(bd, ctx, r, c, subsize, avail_u, avail_l);

    residual(bd, ctx, entropy, r, c, subsize, tx_size, skip, segment_id, y_mode, uv_mode, &sub_modes);

    ctx.store_block(
        r,
        c,
        subsize,
        MiCell { skip, tx_size, sub_modes },
    );
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
    let max_tx_size = tables::MAX_TXSIZE_LOOKUP.get(usize::try_from(subsize).unwrap_or(0)).copied().unwrap_or(0);
    let tx_mode = ctx.header.tx_mode;
    let tx_size = if tx_mode == tables::TX_MODE_SELECT && subsize >= tables::BLOCK_8X8 {
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
    };

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

fn get_uv_tx_size(mi_size: i32, tx_size: i32, subsampling_x: bool, subsampling_y: bool) -> i32 {
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

#[allow(clippy::too_many_arguments)]
fn residual(
    bd: &mut Bd<'_>,
    ctx: &mut FrameCtx,
    entropy: &EntropyContext,
    r: usize,
    c: usize,
    mi_size: i32,
    tx_size: i32,
    skip: bool,
    segment_id: u8,
    y_mode: i32,
    uv_mode: i32,
    sub_modes: &[i32; 4],
) {
    let bsize = if mi_size < tables::BLOCK_8X8 { tables::BLOCK_8X8 } else { mi_size };
    let (subx, suby) = (ctx.header.color.subsampling_x, ctx.header.color.subsampling_y);
    let bit_depth = u32::from(ctx.header.color.bit_depth);
    let lossless = ctx.header.quant.lossless;

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

                    if !skip {
                        let tx_type = get_scan_tx_type(plane, tx_sz, lossless, mi_size, y_mode, sub_modes, block_idx);
                        let scan = tables::get_scan(tx_sz, tx_type);
                        let (coefs, eob) =
                            tokens::decode_tokens(bd, entropy, ctx, plane, start_x, start_y, tx_sz, scan, tx_type, subx, suby, bit_depth);
                        nonzero = eob > 0;
                        if nonzero {
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

fn get_scan_tx_type(plane: usize, tx_sz: i32, lossless: bool, mi_size: i32, y_mode: i32, sub_modes: &[i32; 4], block_idx: usize) -> TxType {
    if plane > 0 || tx_sz == tables::TX_32X32 {
        TxType::DctDct
    } else if tx_sz == tables::TX_4X4 {
        if lossless {
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
        let mut c = 0usize;
        while c < ctx.mi_cols {
            decode_partition(bd, ctx, entropy, r, c, tables::BLOCK_64X64);
            c += 8;
        }
        r += 8;
    }
}

/// Decode one already-superframe-split VP9 frame's tile data.
fn decode_frame_tiles(header: &FrameHeader, entropy: &EntropyContext, tile_data: &[u8], pic: Picture) -> Picture {
    let mut ctx = FrameCtx {
        header: header.clone(),
        mi_cols: header.mi_cols,
        mi_rows: header.mi_rows,
        grid: vec![MiCell::default(); header.mi_rows.max(1) * header.mi_cols.max(1)],
        above_partition_context: vec![0u8; header.mi_cols.max(1)],
        left_partition_context: [0u8; 8],
        above_nz: [
            vec![false; header.mi_cols * 2 + 16],
            vec![false; header.mi_cols * 2 + 16],
            vec![false; header.mi_cols * 2 + 16],
        ],
        left_nz: [[false; 16]; 3],
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
    ctx.pic
}

fn tile_offset(tile_num: usize, mis: usize, tile_size_log2: u32) -> usize {
    let sbs = (mis + 7) >> 3;
    let offset = ((tile_num * sbs) >> tile_size_log2) << 3;
    offset.min(mis)
}

/// Everything the decoder keeps across packets: persisted loop-filter and
/// segmentation state (`parse_uncompressed_header` needs the previous
/// frame's, per §6.2/§7.2's persistence rules).
#[derive(Default)]
struct State {
    loop_filter: LoopFilterParams,
    segmentation: Segmentation,
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
        let Some((mut fh, header_bytes)) = header::parse_uncompressed_header(data, self.state.loop_filter, self.state.segmentation) else {
            return Ok(());
        };
        self.state.loop_filter = fh.loop_filter;
        self.state.segmentation = fh.segmentation;

        if fh.show_existing_frame || !fh.is_key_frame {
            // A previously-shown-frame pointer, or an inter frame (C-31,
            // out of this crate's scope — see the crate doc): nothing this
            // crate can correctly reconstruct. Skip rather than emit a
            // wrong picture.
            return Ok(());
        }

        let mut entropy = EntropyContext::default();
        let header_end = header_bytes + usize::from(fh.header_size_in_bytes);
        let Some(compressed) = data.get(header_bytes..header_end) else { return Ok(()) };
        let mut cbd = Bd::new(compressed);
        let tx_mode = header::parse_compressed_header(&mut cbd, fh.quant.lossless, &mut entropy);
        fh.tx_mode = tx_mode;
        fh.entropy = entropy.clone();

        let tile_data = data.get(header_end..).unwrap_or(&[]);

        let luma_w = fh.mi_cols * 8;
        let luma_h = fh.mi_rows * 8;
        let chroma_w = luma_w >> u32::from(fh.color.subsampling_x);
        let chroma_h = luma_h >> u32::from(fh.color.subsampling_y);
        let pic = Picture::new(&mut self.budget, luma_w, luma_h, chroma_w, chroma_h)?;

        let pic = decode_frame_tiles(&fh, &entropy, tile_data, pic);

        let mut frame = pic_to_frame(&mut self.budget, &fh, &pic)?;
        frame.flags |= vaco_frame::FrameFlags::KEY;
        frame.pts = pts;
        frame.duration = duration;
        self.machine.emit(frame);
        Ok(())
    }
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
    long_name: "VP9 (key-frame intra decode; VP9 Bitstream & Decoding Process Specification v0.6)",
    id: vaco_codec_core::CodecId::Vp9,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::SUBFRAMES,
    supported_rates: &[],
    make: |limits| Box::new(Vp9Decoder::new(limits)),
};
