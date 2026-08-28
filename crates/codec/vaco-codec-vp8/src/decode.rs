//! The per-macroblock decode loop: mode/MV records, residual tokens,
//! reconstruction and the loop filter, tied together into a
//! [`vaco_codec_core::Decoder`].
//!
//! # Known-unverified pieces
//!
//! Two details RFC 6386's own prose leaves to "the reference decoder" and
//! which this crate's spec extraction could not pin down from the primary
//! text alone (see `planning/TECH-DEBT.md`'s row for this crate):
//!
//! 1. The exact index a loop-filter *mode* delta (`mb_lf_adjustments()`'s
//!    four mode-delta slots) applies to for each macroblock mode. This
//!    crate uses the widely-documented convention (0 = `B_PRED`, 1 =
//!    `ZEROMV`, 2 = other inter modes, 3 = `SPLITMV`) but has not
//!    cross-checked it against RFC 6386 §9.4/§10's own text.
//! 2. Chroma motion-vector rounding: the four covering luma (eighth-pel)
//!    components are summed and divided by 8 with a symmetric round; the
//!    exact rounding RFC 6386 specifies was not captured in this crate's
//!    spec extraction pass.
//!
//! Both are implemented with a reasonable, documented choice rather than
//! left `todo!()`, and both are exactly the kind of thing the differential
//! pass against real encoder output (this crate's own tests, `tests/`) is
//! meant to catch.

use vaco_codec_core::machine::{Accept, Machine};
use vaco_codec_core::{Decoder, DecoderDesc};
use vaco_codec_msac::Vp8BoolDecoder as Bd;
use vaco_codec_core::CodecId;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;
use vaco_parse_vpx::vp8::parse_frame_tag;

use crate::framebuf::{Picture, Plane, RefFrames};
use crate::header::{self, EntropyContext, FrameHeader};
use crate::loopfilter;
use crate::mv::{self, Mv, NeighborMv};
use crate::predict;
use crate::tables;
use crate::tokens::{self, BlockCoeffs};
use crate::transform;

/// A macroblock/pixel coordinate as a signed offset. Frame dimensions are
/// `u16` (RFC 6386 §9.1), so `mb_cols * 16 + 20` never approaches
/// `i32::MAX`; the fallback exists only so this is a total function rather
/// than a place a future change could make it panic.
fn ix(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// The inverse of [`ix`], clamping negative values to 0 -- every call site
/// already established the value is non-negative before converting back to
/// a plane index.
fn ux(v: i32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

fn above_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if y < 0 { predict::OFF_FRAME_ABOVE } else { plane.get(x, y) }
}

fn left_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if x < 0 { predict::OFF_FRAME_LEFT } else { plane.get(x, y) }
}

/// `x`/`y` are the block's own top-left position (not already decremented,
/// unlike [`above_pixel`]/[`left_pixel`]'s callers) — the corner pixel is at
/// `(x-1, y-1)`, off-frame above whenever `y == 0` and off-frame left
/// whenever `x == 0` (the "above" fill wins at the frame's top-left corner,
/// matching [`predict::OFF_FRAME_ABOVE`]'s note in `predict`'s module doc).
fn corner_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if y == 0 {
        predict::OFF_FRAME_ABOVE
    } else if x == 0 {
        predict::OFF_FRAME_LEFT
    } else {
        plane.get(x - 1, y - 1)
    }
}

fn gather_above<const N: usize>(plane: &Plane, x: i32, y: i32) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = above_pixel(plane, x + ix(i), y - 1);
    }
    out
}

fn gather_left<const N: usize>(plane: &Plane, x: i32, y: i32) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = left_pixel(plane, x - 1, y + ix(i));
    }
    out
}

/// Everything about one already-decoded macroblock that a later macroblock
/// (mode context, motion-vector prediction, the loop filter) needs to know.
#[derive(Debug, Clone, Copy)]
struct MbInfo {
    skip_coeff: bool,
    ref_frame: u8, // 0 = intra
    mv: Mv,        // eighth-pel; representative MV (SPLITMV: subblock 15's)
    sub_mvs: [Mv; 16], // eighth-pel per-subblock MV (all equal to `mv` unless SPLITMV)
    is_splitmv: bool,
    has_y2: bool,
    filter_level: i32,
}

impl Default for MbInfo {
    fn default() -> Self {
        Self {
            skip_coeff: false,
            ref_frame: 0,
            mv: (0, 0),
            sub_mvs: [(0, 0); 16],
            is_splitmv: false,
            has_y2: true,
            filter_level: 0,
        }
    }
}

fn mode_delta_index(mode: i32) -> usize {
    if mode == tables::B_PRED {
        0
    } else if mode == tables::MV_ZEROMV {
        1
    } else if mode == tables::MV_SPLITMV {
        3
    } else if mode >= tables::MV_NEARESTMV {
        2
    } else {
        // Other whole-block intra modes get no mode delta contribution.
        usize::MAX
    }
}

#[allow(
    clippy::integer_division,
    reason = "chroma MV = luma sum / 8, symmetric rounding; see the crate doc's known-unverified note"
)]
fn round_div8(x: i32) -> i32 {
    if x >= 0 { (x + 4) / 8 } else { -((-x + 4) / 8) }
}

/// Everything the frame decode needs across macroblocks: persistent
/// entropy/segmentation/loop-filter state plus this frame's per-macroblock
/// records and coefficient-context bookkeeping.
struct FrameCtx<'a> {
    header: &'a FrameHeader,
    mb_cols: usize,
    mb_rows: usize,
    mbs: Vec<MbInfo>,
    segment_map: &'a mut Vec<u8>,
    y: Plane,
    u: Plane,
    v: Plane,
    // Coefficient "has non-zero" context: above row (one slot per MB
    // column) and left (reset every MB row).
    above_y: Vec<[bool; 4]>,
    above_u: Vec<[bool; 2]>,
    above_v: Vec<[bool; 2]>,
    above_y2: Vec<bool>,
    left_y: [bool; 4],
    left_u: [bool; 2],
    left_v: [bool; 2],
    left_y2: bool,
    // Key-frame-only B_PRED submode context.
    above_bmode: Vec<[i32; 4]>,
    left_bmode: [i32; 4],
}

fn derived_bmode(y_mode: i32) -> i32 {
    match y_mode {
        m if m == tables::V_PRED => tables::B_VE_PRED,
        m if m == tables::H_PRED => tables::B_HE_PRED,
        m if m == tables::TM_PRED => tables::B_TM_PRED,
        _ => tables::B_DC_PRED,
    }
}

impl FrameCtx<'_> {
    fn mb_index(&self, col: usize, row: usize) -> Option<usize> {
        if col >= self.mb_cols || row >= self.mb_rows {
            return None;
        }
        Some(row * self.mb_cols + col)
    }

    fn mb_at(&self, col: i32, row: i32) -> Option<MbInfo> {
        if col < 0 || row < 0 {
            return None;
        }
        let idx = self.mb_index(ux(col), ux(row))?;
        self.mbs.get(idx).copied()
    }
}

/// Read one macroblock's segment id, skip flag, and (mode-and-motion) mode
/// record; also runs residual token decode and reconstruction into the
/// frame's planes.
#[allow(clippy::too_many_arguments, reason = "one macroblock's worth of state")]
fn decode_macroblock(
    bd: &mut Bd<'_>,
    token_bd: &mut Bd<'_>,
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    refs: &RefFrames,
    sign_bias: [bool; 4],
    col: usize,
    row: usize,
) {
    let mb_idx = row * ctx.mb_cols + col;

    // -- segment id --
    let segment_id = if ctx.header.segmentation.enabled && ctx.header.segmentation.update_map {
        let id = u8::try_from(bd.read_tree(&tables::MB_SEGMENT_TREE, &ctx.header.segmentation.tree_probs)).unwrap_or(0);
        if let Some(slot) = ctx.segment_map.get_mut(mb_idx) {
            *slot = id;
        }
        id
    } else if ctx.header.segmentation.enabled {
        ctx.segment_map.get(mb_idx).copied().unwrap_or(0)
    } else {
        0
    };

    // -- skip coeff --
    let skip_coeff = if ctx.header.mb_no_skip_coeff {
        bd.read_bool(ctx.header.prob_skip_false)
    } else {
        false
    };

    let above = ctx.mb_at(ix(col), ix(row) - 1);
    let left = ctx.mb_at(ix(col) - 1, ix(row));
    let above_left = ctx.mb_at(ix(col) - 1, ix(row) - 1);

    let mode: i32;
    let mut is_splitmv = false;
    let whole_mv: Mv;
    let mut sub_modes = [tables::B_DC_PRED; 16];
    let mut sub_mvs = [(0i32, 0i32); 16];

    if ctx.header.key_frame {
        mode = bd.read_tree(&tables::KF_YMODE_TREE, &tables::KF_YMODE_PROB);
        if mode == tables::B_PRED {
            #[allow(
                clippy::integer_division,
                reason = "splitting a 0..16 subblock index into its 4x4 grid position"
            )]
            for i in 0..16 {
                let sub_col = i % 4;
                let sub_row = i / 4;
                let a = if sub_row == 0 {
                    ctx.above_bmode
                        .get(col)
                        .and_then(|r| r.get(sub_col))
                        .copied()
                        .unwrap_or(tables::B_DC_PRED)
                } else {
                    sub_modes.get(i - 4).copied().unwrap_or(tables::B_DC_PRED)
                };
                let l = if sub_col == 0 {
                    ctx.left_bmode.get(sub_row).copied().unwrap_or(tables::B_DC_PRED)
                } else {
                    sub_modes.get(i - 1).copied().unwrap_or(tables::B_DC_PRED)
                };
                let ai = usize::try_from(a).unwrap_or(0).min(9);
                let li = usize::try_from(l).unwrap_or(0).min(9);
                let probs = tables::KF_BMODE_PROB
                    .get(ai)
                    .and_then(|r| r.get(li))
                    .copied()
                    .unwrap_or([128; 9]);
                let m = bd.read_tree(&tables::BMODE_TREE, &probs);
                if let Some(slot) = sub_modes.get_mut(i) {
                    *slot = m;
                }
            }
        } else {
            sub_modes = [derived_bmode(mode); 16];
        }
        let uv_mode = bd.read_tree(&tables::UV_MODE_TREE, &tables::KF_UV_MODE_PROB);
        if let Some(r) = ctx.above_bmode.get_mut(col) {
            *r = [sub_modes[12], sub_modes[13], sub_modes[14], sub_modes[15]];
        }
        ctx.left_bmode = [sub_modes[3], sub_modes[7], sub_modes[11], sub_modes[15]];
        reconstruct_intra(ctx, entropy, token_bd, col, row, mode, uv_mode, &sub_modes, skip_coeff, segment_id);
        store_mb(ctx, col, row, segment_id, skip_coeff, 0, mode, (0, 0), [(0, 0); 16], false);
        return;
    }

    // -- interframe --
    let is_inter = bd.read_bool(ctx.header.prob_intra);
    if !is_inter {
        mode = bd.read_tree(&tables::YMODE_TREE, &entropy.ymode_prob);
        if mode == tables::B_PRED {
            for m in &mut sub_modes {
                *m = bd.read_tree(&tables::BMODE_TREE, &tables::BMODE_PROB);
            }
        } else {
            sub_modes = [derived_bmode(mode); 16];
        }
        let uv_mode = bd.read_tree(&tables::UV_MODE_TREE, &entropy.uv_mode_prob);
        reconstruct_intra(ctx, entropy, token_bd, col, row, mode, uv_mode, &sub_modes, skip_coeff, segment_id);
        store_mb(ctx, col, row, segment_id, skip_coeff, 0, mode, (0, 0), [(0, 0); 16], false);
        return;
    }

    let ref_frame: u8 = if !bd.read_bool(ctx.header.prob_last) {
        1
    } else if !bd.read_bool(ctx.header.prob_gf) {
        2
    } else {
        3
    };

    let neighbor = |m: Option<MbInfo>| -> Option<NeighborMv> {
        m.map(|m| NeighborMv {
            ref_frame: m.ref_frame,
            mv: m.mv,
            is_splitmv: m.is_splitmv,
        })
    };
    let current_sign_bias = sign_bias.get(usize::from(ref_frame)).copied().unwrap_or(false);
    let near = mv::find_near_mvs(neighbor(above), neighbor(left), neighbor(above_left), |rf| {
        sign_bias.get(usize::from(rf)).copied().unwrap_or(false) == current_sign_bias
    });

    let (to_left, to_right, to_top, to_bottom) = mv_bounds(col, row, ctx.mb_cols, ctx.mb_rows);
    let clamp = |mv: Mv| mv::clamp_mv(mv, to_left, to_right, to_top, to_bottom);
    let nearest = clamp(near.nearest);
    let near_mv = clamp(near.near);
    let best = clamp(near.best);

    let probs = mv::mv_ref_probs(near.cnt);
    let local = bd.read_tree(&tables::MV_REF_TREE, &probs);
    mode = match local {
        0 => tables::MV_NEARESTMV,
        1 => tables::MV_NEARMV,
        2 => tables::MV_ZEROMV,
        3 => tables::MV_NEWMV,
        _ => tables::MV_SPLITMV,
    };

    if mode == tables::MV_SPLITMV {
        is_splitmv = true;
        // A subblock's above/left neighbour, per RFC 6386 §16.4, when that
        // subblock sits on this macroblock's top/left edge: the
        // neighbouring macroblock's matching-edge subblock (row 3 for
        // "above", column 3 for "left"), or the zero vector if that
        // neighbour does not exist or is intra. Interior lookups are
        // handled by `decode_split` itself from partitions already decided
        // earlier in the same call.
        let above_boundary = |col: usize| -> Mv {
            above.filter(|m| m.ref_frame != 0).map_or((0, 0), |m| {
                m.sub_mvs.get(12 + col).copied().unwrap_or((0, 0))
            })
        };
        let left_boundary = |row: usize| -> Mv {
            left.filter(|m| m.ref_frame != 0).map_or((0, 0), |m| {
                m.sub_mvs.get(row * 4 + 3).copied().unwrap_or((0, 0))
            })
        };
        sub_mvs = mv::decode_split(bd, &entropy.mv_probs, best, above_boundary, left_boundary);
        whole_mv = sub_mvs.get(15).copied().unwrap_or((0, 0));
    } else {
        whole_mv = match mode {
            m if m == tables::MV_NEARESTMV => nearest,
            m if m == tables::MV_NEARMV => near_mv,
            m if m == tables::MV_ZEROMV => (0, 0),
            _ => {
                let (dr, dc) = mv::read_mv(bd, &entropy.mv_probs);
                clamp((best.0 + dr * 2, best.1 + dc * 2))
            }
        };
    }

    reconstruct_inter(
        ctx, entropy, token_bd, refs, col, row, ref_frame, whole_mv, &sub_mvs, is_splitmv,
        skip_coeff, segment_id,
    );
    let stored_sub_mvs = if is_splitmv { sub_mvs } else { [whole_mv; 16] };
    store_mb(ctx, col, row, segment_id, skip_coeff, ref_frame, mode, whole_mv, stored_sub_mvs, is_splitmv);
}

pub(crate) fn mv_bounds(col: usize, row: usize, mb_cols: usize, mb_rows: usize) -> (i32, i32, i32, i32) {
    let col = ix(col);
    let row = ix(row);
    let to_left = -((col + 1) << 7);
    let to_right = (ix(mb_cols) - col) << 7;
    let to_top = -((row + 1) << 7);
    let to_bottom = (ix(mb_rows) - row) << 7;
    (to_left, to_right, to_top, to_bottom)
}

#[allow(clippy::too_many_arguments)]
fn store_mb(
    ctx: &mut FrameCtx<'_>,
    col: usize,
    row: usize,
    segment_id: u8,
    skip_coeff: bool,
    ref_frame: u8,
    mode: i32,
    mv: Mv,
    sub_mvs: [Mv; 16],
    is_splitmv: bool,
) {
    let has_y2 = mode != tables::B_PRED && mode != tables::MV_SPLITMV;
    let level = macroblock_filter_level(ctx, segment_id, ref_frame, mode);
    let idx = row * ctx.mb_cols + col;
    if let Some(slot) = ctx.mbs.get_mut(idx) {
        *slot = MbInfo {
            skip_coeff,
            ref_frame,
            mv,
            sub_mvs,
            is_splitmv,
            has_y2,
            filter_level: level,
        };
    }
}

fn macroblock_filter_level(ctx: &FrameCtx<'_>, segment_id: u8, ref_frame: u8, mode: i32) -> i32 {
    let seg = &ctx.header.segmentation;
    let mut level = if seg.enabled {
        let idx = usize::from(segment_id).min(3);
        if seg.absolute {
            seg.lf_level.get(idx).copied().unwrap_or(0)
        } else {
            ctx.header.filter_level + seg.lf_level.get(idx).copied().unwrap_or(0)
        }
    } else {
        ctx.header.filter_level
    };
    level = level.clamp(0, 63);
    if ctx.header.lf_deltas.enabled {
        level += ctx
            .header
            .lf_deltas
            .ref_frame
            .get(usize::from(ref_frame))
            .copied()
            .unwrap_or(0);
        let mi = mode_delta_index(mode);
        if mi != usize::MAX {
            level += ctx.header.lf_deltas.mode.get(mi).copied().unwrap_or(0);
        }
        level = level.clamp(0, 63);
    }
    level
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_intra(
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    bd: &mut Bd<'_>,
    col: usize,
    row: usize,
    y_mode: i32,
    uv_mode: i32,
    sub_modes: &[i32; 16],
    skip_coeff: bool,
    segment_id: u8,
) {
    let has_y2 = y_mode != tables::B_PRED;
    let quant = dequant_for(ctx, segment_id);

    // -- residual decode --
    let (y_blocks, y2_block, u_blocks, v_blocks) = decode_residuals(bd, ctx, entropy, col, row, has_y2, skip_coeff, &quant);

    let base_x = ix(col * 16);
    let base_y = ix(row * 16);

    if y_mode == tables::B_PRED {
        #[allow(
            clippy::integer_division,
            reason = "splitting a 0..16 subblock index into its 4x4 grid position"
        )]
        for (i, (&sub_mode, block)) in sub_modes.iter().zip(y_blocks.iter()).enumerate() {
            let sub_col = i % 4;
            let sub_row = i / 4;
            let x = base_x + ix(sub_col * 4);
            let y = base_y + ix(sub_row * 4);
            let above8 = gather_above_right(&ctx.y, col, row, sub_col, sub_row, ctx.mb_cols);
            let left4: [u8; 4] = gather_left(&ctx.y, x, y);
            let corner = corner_pixel(&ctx.y, x, y);
            let pred = b_pred(sub_mode, &above8, &left4, corner);
            write_residual_block(&mut ctx.y, x, y, &pred, block);
        }
    } else {
        predict_and_write_16(&mut ctx.y, base_x, base_y, y_mode, &y_blocks, y2_block.as_ref());
    }

    predict_and_write_8(&mut ctx.u, ix(col * 8), ix(row * 8), uv_mode, &u_blocks);
    predict_and_write_8(&mut ctx.v, ix(col * 8), ix(row * 8), uv_mode, &v_blocks);
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "keeps the same by-reference calling convention as vaco_codec_vp8::predict's functions this dispatches to"
)]
fn b_pred(mode: i32, above8: &[u8; 8], left4: &[u8; 4], corner: u8) -> [[u8; 4]; 4] {
    let above4 = [above8[0], above8[1], above8[2], above8[3]];
    if mode == tables::B_DC_PRED {
        predict::b_dc(&above4, left4)
    } else if mode == tables::B_TM_PRED {
        predict::predict_tm(&above4, left4, corner)
    } else if mode == tables::B_VE_PRED {
        predict::b_ve(above8, corner)
    } else if mode == tables::B_HE_PRED {
        predict::b_he(left4, corner)
    } else if mode == tables::B_LD_PRED {
        predict::b_ld(above8)
    } else if mode == tables::B_RD_PRED {
        predict::b_rd(above8, left4, corner)
    } else if mode == tables::B_VR_PRED {
        predict::b_vr(above8, left4, corner)
    } else if mode == tables::B_VL_PRED {
        predict::b_vl(above8)
    } else if mode == tables::B_HD_PRED {
        predict::b_hd(above8, left4, corner)
    } else {
        predict::b_hu(left4)
    }
}

fn gather_above_right(plane: &Plane, col: usize, row: usize, sub_col: usize, sub_row: usize, mb_cols: usize) -> [u8; 8] {
    let base_x = ix(col * 16);
    let base_y = ix(row * 16);
    let x = base_x + ix(sub_col * 4);
    let y = base_y + ix(sub_row * 4);
    let mut out = [0u8; 8];
    for (k, slot) in out.iter_mut().enumerate().take(4) {
        *slot = above_pixel(plane, x + ix(k), y - 1);
    }
    if sub_col == 3 {
        let src_y = base_y - 1;
        if col + 1 >= mb_cols {
            let v = above_pixel(plane, base_x + 15, src_y);
            for slot in out.iter_mut().skip(4) {
                *slot = v;
            }
        } else {
            for (k, slot) in out.iter_mut().enumerate().skip(4) {
                *slot = above_pixel(plane, base_x + 16 + ix(k - 4), src_y);
            }
        }
    } else {
        for (k, slot) in out.iter_mut().enumerate().skip(4) {
            *slot = above_pixel(plane, x + 4 + ix(k - 4), y - 1);
        }
    }
    out
}

fn predict_and_write_16(plane: &mut Plane, x: i32, y: i32, mode: i32, blocks: &[BlockCoeffs; 16], y2: Option<&BlockCoeffs>) {
    let above: [u8; 16] = gather_above(plane, x, y);
    let left: [u8; 16] = gather_left(plane, x, y);
    let corner = corner_pixel(plane, x, y);
    let pred = match mode {
        m if m == tables::V_PRED => predict::predict_v(&above),
        m if m == tables::H_PRED => predict::predict_h(&left),
        m if m == tables::TM_PRED => predict::predict_tm(&above, &left, corner),
        _ => predict::predict_dc(
            if y > 0 { Some(&above) } else { None },
            if x > 0 { Some(&left) } else { None },
        ),
    };
    // Fold in the Y2 DC values (already inverse-WHT'd) before the per-block IDCT.
    // `has_coeffs` was computed from the AC scan only (the Y2 DC is decoded
    // and inverse-transformed separately), so a block with an all-zero AC
    // scan but a non-zero folded-in DC must still flip `has_coeffs` to true
    // here — otherwise `write_residual_block` skips the IDCT and the DC
    // contribution is silently dropped.
    let mut adjusted = *blocks;
    if let Some(y2) = y2 {
        let dc = transform::inverse_wht(&y2.coeffs);
        for (i, blk) in adjusted.iter_mut().enumerate() {
            if let Some(&d) = dc.get(i)
                && let Some(c0) = blk.coeffs.first_mut()
            {
                *c0 = d;
                if d != 0 {
                    blk.has_coeffs = true;
                }
            }
        }
    }
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..16 subblock index into its 4x4 grid position"
    )]
    for (i, block) in adjusted.iter().enumerate() {
        let sub_row = i / 4;
        let sub_col = i % 4;
        let sub_x = x + ix(sub_col * 4);
        let sub_y = y + ix(sub_row * 4);
        let mut block_pred = [[0u8; 4]; 4];
        for (r, row) in block_pred.iter_mut().enumerate() {
            for (c, px) in row.iter_mut().enumerate() {
                *px = get2d(&pred, sub_row * 4 + r, sub_col * 4 + c);
            }
        }
        write_residual_block(plane, sub_x, sub_y, &block_pred, block);
    }
}

/// A 2-D pixel-array lookup that returns 0 (rather than panicking) for an
/// out-of-range position — never actually reached here since every caller's
/// indices are derived from a fixed, in-range loop bound, but this is
/// cheaper to write than to prove to the linter.
fn get2d<const N: usize>(m: &[[u8; N]; N], r: usize, c: usize) -> u8 {
    m.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0)
}

fn predict_and_write_8(plane: &mut Plane, x: i32, y: i32, mode: i32, blocks: &[BlockCoeffs; 4]) {
    let above: [u8; 8] = gather_above(plane, x, y);
    let left: [u8; 8] = gather_left(plane, x, y);
    let corner = corner_pixel(plane, x, y);
    let pred = match mode {
        m if m == tables::V_PRED => predict::predict_v(&above),
        m if m == tables::H_PRED => predict::predict_h(&left),
        m if m == tables::TM_PRED => predict::predict_tm(&above, &left, corner),
        _ => predict::predict_dc(
            if y > 0 { Some(&above) } else { None },
            if x > 0 { Some(&left) } else { None },
        ),
    };
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..4 subblock index into its 2x2 grid position"
    )]
    for (i, block) in blocks.iter().enumerate() {
        let sub_row = i / 2;
        let sub_col = i % 2;
        let sub_x = x + ix(sub_col * 4);
        let sub_y = y + ix(sub_row * 4);
        let mut block_pred = [[0u8; 4]; 4];
        for (r, row) in block_pred.iter_mut().enumerate() {
            for (c, px) in row.iter_mut().enumerate() {
                *px = get2d(&pred, sub_row * 4 + r, sub_col * 4 + c);
            }
        }
        write_residual_block(plane, sub_x, sub_y, &block_pred, block);
    }
}

fn write_residual_block(plane: &mut Plane, x: i32, y: i32, pred: &[[u8; 4]; 4], block: &BlockCoeffs) {
    let residue = if block.has_coeffs {
        transform::inverse_dct(&block.coeffs)
    } else {
        [0i32; 16]
    };
    for r in 0..4 {
        for c in 0..4 {
            let p = pred.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0);
            let res = residue.get(r * 4 + c).copied().unwrap_or(0);
            let v = transform::add_residue(p, res);
            plane.set(ux(x + ix(c)), ux(y + ix(r)), v);
        }
    }
}

fn dequant_for(ctx: &FrameCtx<'_>, segment_id: u8) -> transform::DequantFactors {
    let seg = &ctx.header.segmentation;
    let base = if seg.enabled {
        let idx = usize::from(segment_id).min(3);
        if seg.absolute {
            seg.quant_idx.get(idx).copied().unwrap_or(0)
        } else {
            ctx.header.quant.y_ac_qi + seg.quant_idx.get(idx).copied().unwrap_or(0)
        }
    } else {
        ctx.header.quant.y_ac_qi
    };
    let q = &ctx.header.quant;
    transform::DequantFactors::new(
        base,
        q.y_dc_delta,
        q.y2_dc_delta,
        q.y2_ac_delta,
        q.uv_dc_delta,
        q.uv_ac_delta,
    )
}

fn dequantize(coeffs: &BlockCoeffs, dc: i32, ac: i32) -> BlockCoeffs {
    let mut out = *coeffs;
    for (i, c) in out.coeffs.iter_mut().enumerate() {
        let f = if i == 0 { dc } else { ac };
        *c *= f;
    }
    out
}

#[allow(clippy::type_complexity)]
fn decode_residuals(
    bd: &mut Bd<'_>,
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    col: usize,
    _row: usize,
    has_y2: bool,
    skip_coeff: bool,
    quant: &transform::DequantFactors,
) -> ([BlockCoeffs; 16], Option<BlockCoeffs>, [BlockCoeffs; 4], [BlockCoeffs; 4]) {
    let empty = BlockCoeffs {
        coeffs: [0; 16],
        has_coeffs: false,
        last_nonzero_scan: 0,
    };
    let mut y_blocks = [empty; 16];
    let mut u_blocks = [empty; 4];
    let mut v_blocks = [empty; 4];
    let mut y2_block: Option<BlockCoeffs> = None;

    if skip_coeff {
        for slot in &mut ctx.left_y {
            *slot = false;
        }
        if let Some(a) = ctx.above_y.get_mut(col) {
            *a = [false; 4];
        }
        for slot in &mut ctx.left_u {
            *slot = false;
        }
        if let Some(a) = ctx.above_u.get_mut(col) {
            *a = [false; 2];
        }
        for slot in &mut ctx.left_v {
            *slot = false;
        }
        if let Some(a) = ctx.above_v.get_mut(col) {
            *a = [false; 2];
        }
        if has_y2 {
            ctx.left_y2 = false;
            if let Some(a) = ctx.above_y2.get_mut(col) {
                *a = false;
            }
        }
        return (y_blocks, if has_y2 { Some(empty) } else { None }, u_blocks, v_blocks);
    }

    if has_y2 {
        let above_ctx = usize::from(ctx.above_y2.get(col).copied().unwrap_or(false));
        let left_ctx = usize::from(ctx.left_y2);
        let block = tokens::decode_block(bd, &entropy.coeff_probs[tables::PLANE_Y2], 0, above_ctx + left_ctx);
        ctx.left_y2 = block.has_coeffs;
        if let Some(a) = ctx.above_y2.get_mut(col) {
            *a = block.has_coeffs;
        }
        y2_block = Some(dequantize(&block, quant.y2_dc, quant.y2_ac));
    }

    let y_plane = if has_y2 { tables::PLANE_Y_AFTER_Y2 } else { tables::PLANE_Y_NO_Y2 };
    let first = usize::from(has_y2);
    let probs = entropy.coeff_probs.get(y_plane).unwrap_or(&entropy.coeff_probs[0]);
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..16 subblock index into its 4x4 grid position"
    )]
    for i in 0..16 {
        let sub_col = i % 4;
        let sub_row = i / 4;
        let above_ctx = if sub_row == 0 {
            usize::from(
                ctx.above_y
                    .get(col)
                    .and_then(|r| r.get(sub_col))
                    .copied()
                    .unwrap_or(false),
            )
        } else {
            usize::from(y_blocks.get(i - 4).is_some_and(|b| b.has_coeffs))
        };
        let left_ctx = if sub_col == 0 {
            usize::from(ctx.left_y.get(sub_row).copied().unwrap_or(false))
        } else {
            usize::from(y_blocks.get(i - 1).is_some_and(|b| b.has_coeffs))
        };
        let block = tokens::decode_block(bd, probs, first, above_ctx + left_ctx);
        if let Some(slot) = y_blocks.get_mut(i) {
            *slot = dequantize(&block, quant.y1_dc, quant.y1_ac);
        }
    }
    let y_has = |i: usize| y_blocks.get(i).is_some_and(|b| b.has_coeffs);
    if let Some(a) = ctx.above_y.get_mut(col) {
        *a = [y_has(12), y_has(13), y_has(14), y_has(15)];
    }
    ctx.left_y = [y_has(3), y_has(7), y_has(11), y_has(15)];

    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..4 subblock index into its 2x2 grid position"
    )]
    for (plane_blocks, above_state, left_state) in [
        (&mut u_blocks, &mut ctx.above_u, &mut ctx.left_u),
        (&mut v_blocks, &mut ctx.above_v, &mut ctx.left_v),
    ] {
        for i in 0..4 {
            let sub_col = i % 2;
            let sub_row = i / 2;
            let above_ctx = if sub_row == 0 {
                usize::from(
                    above_state
                        .get(col)
                        .and_then(|r| r.get(sub_col))
                        .copied()
                        .unwrap_or(false),
                )
            } else {
                usize::from(plane_blocks.get(i - 2).is_some_and(|b| b.has_coeffs))
            };
            let left_ctx = if sub_col == 0 {
                usize::from(left_state.get(sub_row).copied().unwrap_or(false))
            } else {
                usize::from(plane_blocks.get(i - 1).is_some_and(|b| b.has_coeffs))
            };
            let block = tokens::decode_block(bd, &entropy.coeff_probs[tables::PLANE_UV], 0, above_ctx + left_ctx);
            if let Some(slot) = plane_blocks.get_mut(i) {
                *slot = dequantize(&block, quant.uv_dc, quant.uv_ac);
            }
        }
        let has = |i: usize| plane_blocks.get(i).is_some_and(|b| b.has_coeffs);
        if let Some(a) = above_state.get_mut(col) {
            *a = [has(2), has(3)];
        }
        *left_state = [has(1), has(3)];
    }

    (y_blocks, y2_block, u_blocks, v_blocks)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_inter(
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    bd: &mut Bd<'_>,
    refs: &RefFrames,
    col: usize,
    row: usize,
    ref_frame: u8,
    whole_mv: Mv,
    sub_mvs: &[Mv; 16],
    is_splitmv: bool,
    skip_coeff: bool,
    segment_id: u8,
) {
    let quant = dequant_for(ctx, segment_id);
    let has_y2 = !is_splitmv;
    let (y_blocks, y2_block, u_blocks, v_blocks) =
        decode_residuals(bd, ctx, entropy, col, row, has_y2, skip_coeff, &quant);

    let Some(refp) = refs.get(ref_frame) else {
        return;
    };

    let base_x = ix(col * 16);
    let base_y = ix(row * 16);

    let mut y_dc = [0i32; 16];
    if let Some(y2) = &y2_block {
        y_dc = transform::inverse_wht(&y2.coeffs);
    }

    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..16 subblock index into its 4x4 grid position"
    )]
    for (i, y_block) in y_blocks.iter().enumerate() {
        let sub_col = i % 4;
        let sub_row = i / 4;
        let x = base_x + ix(sub_col * 4);
        let y = base_y + ix(sub_row * 4);
        let mv = if is_splitmv { sub_mvs.get(i).copied().unwrap_or(whole_mv) } else { whole_mv };
        let pred = mc_block::<4, 4>(&refp.y, x, y, mv, ctx.header.version);
        let mut block = *y_block;
        if has_y2
            && let Some(&d) = y_dc.get(i)
            && let Some(c0) = block.coeffs.first_mut()
        {
            *c0 = d;
            // Same fix as `predict_and_write_16`: the AC-only `has_coeffs`
            // flag must reflect a non-zero folded-in Y2 DC too, or
            // `write_residual_block` skips the IDCT and drops it.
            if d != 0 {
                block.has_coeffs = true;
            }
        }
        write_residual_block(&mut ctx.y, x, y, &pred, &block);
    }

    // Chroma: derive one MV per 4x4 chroma block from the 4 covering luma
    // subblocks (identical when not split), eighth-pel, divide-by-8 round.
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..4 subblock index into its 2x2 grid position"
    )]
    for (i, (u_block, v_block)) in u_blocks.iter().zip(v_blocks.iter()).enumerate() {
        let sub_col = i % 2;
        let sub_row = i / 2;
        let luma_idxs = [
            (sub_row * 2) * 4 + sub_col * 2,
            (sub_row * 2) * 4 + sub_col * 2 + 1,
            (sub_row * 2 + 1) * 4 + sub_col * 2,
            (sub_row * 2 + 1) * 4 + sub_col * 2 + 1,
        ];
        let sum_r: i32 = luma_idxs
            .iter()
            .map(|&k| if is_splitmv { sub_mvs.get(k).copied().unwrap_or(whole_mv).0 } else { whole_mv.0 })
            .sum();
        let sum_c: i32 = luma_idxs
            .iter()
            .map(|&k| if is_splitmv { sub_mvs.get(k).copied().unwrap_or(whole_mv).1 } else { whole_mv.1 })
            .sum();
        let chroma_mv = (round_div8(sum_r), round_div8(sum_c));
        let cx = ix(col * 8) + ix(sub_col * 4);
        let cy = ix(row * 8) + ix(sub_row * 4);

        let pred_u = mc_block::<4, 4>(&refp.u, cx, cy, chroma_mv, ctx.header.version);
        write_residual_block(&mut ctx.u, cx, cy, &pred_u, u_block);
        let pred_v = mc_block::<4, 4>(&refp.v, cx, cy, chroma_mv, ctx.header.version);
        write_residual_block(&mut ctx.v, cx, cy, &pred_v, v_block);
    }
}

/// Motion-compensated prediction for one `W x H` block. `mv` is eighth-pel.
/// RFC 6386 §9.1's version-number table: which reconstruction filter the
/// frame tag's `version` field selects (0 = bicubic/6-tap, 1-2 = bilinear,
/// 3 = "none", i.e. motion vectors truncated to full-pel). Loop filter
/// "type" is a separate, explicit per-frame header bit (§9.4) and is not
/// derived from `version` — see the RFC's own note that version's implied
/// loop-filter behaviour "has no effect whatsoever on the decoding
/// process".
fn reconstruction_filter(version: u8) -> (bool, bool) {
    match version {
        0 => (false, false),  // bicubic (6-tap), sub-pel
        1 | 2 => (true, false), // bilinear, sub-pel
        _ => (true, true),    // "none": truncate to full-pel
    }
}

pub(crate) fn mc_block<const W: usize, const H: usize>(refp: &Plane, x: i32, y: i32, mv: Mv, version: u8) -> [[u8; W]; H] {
    let (bilinear, full_pel) = reconstruction_filter(version);
    let mv = if full_pel { (mv.0 & !7, mv.1 & !7) } else { mv };
    let int_r = mv.0 >> 3;
    let int_c = mv.1 >> 3;
    let frac_r = ux(mv.0 - (int_r << 3));
    let frac_c = ux(mv.1 - (int_c << 3));
    let origin_x = x + int_c;
    let origin_y = y + int_r;
    crate::interpolate::predict_block::<W, H>(
        |dx, dy| refp.get_clamped(origin_x + dx, origin_y + dy),
        frac_c,
        frac_r,
        bilinear,
    )
}

/// Build [`loopfilter::MbFilterInfo`] for every macroblock and hand the
/// whole frame to [`loopfilter::apply_frame`] — the shared implementation
/// [`crate::encode`] also drives, so a decoded reference frame and an
/// encoded one that reconstructs the same macroblocks are filtered
/// identically.
fn apply_loop_filter(ctx: &mut FrameCtx<'_>) {
    if ctx.header.filter_level == 0 {
        return;
    }
    let mb_info: Vec<loopfilter::MbFilterInfo> = (0..ctx.mb_rows * ctx.mb_cols)
        .map(|idx| {
            #[allow(clippy::integer_division, reason = "splitting a flat macroblock index into its (col, row) grid position")]
            let (col, row) = (idx % ctx.mb_cols.max(1), idx / ctx.mb_cols.max(1));
            ctx.mb_at(ix(col), ix(row)).map_or(
                loopfilter::MbFilterInfo { filter_level: 0, skip_inner: false },
                |mb| loopfilter::MbFilterInfo {
                    filter_level: mb.filter_level,
                    skip_inner: mb.skip_coeff && mb.has_y2,
                },
            )
        })
        .collect();
    loopfilter::apply_frame(
        &mut ctx.y,
        &mut ctx.u,
        &mut ctx.v,
        ctx.mb_cols,
        ctx.mb_rows,
        ctx.header.sharpness_level,
        ctx.header.key_frame,
        ctx.header.filter_simple,
        &mb_info,
    );
}

/// Decoder state that persists across packets: the reference frame slots
/// and the cumulative entropy/segmentation/loop-filter state RFC 6386
/// carries frame to frame.
#[derive(Default)]
struct State {
    entropy: EntropyContext,
    segmentation: header::Segmentation,
    lf_deltas: header::LoopFilterDeltas,
    refs: RefFrames,
    segment_map: Vec<u8>,
    mb_cols: usize,
    mb_rows: usize,
    width: u16,
    height: u16,
}


/// RFC 6386 §9.5: split the token-partition byte range into `num_partitions`
/// slices. When there is one partition it is the whole range unmodified;
/// otherwise every partition but the last is preceded by its own 3-byte
/// little-endian size, and the last partition takes whatever remains.
///
/// Never panics or errors on a malformed size table: a size that runs past
/// the end of `residual`, or a table that does not fit at all, truncates the
/// affected partition(s) to empty rather than reading out of bounds --
/// `Vp8BoolDecoder` already treats an empty slice as an immediately-EOF
/// stream, which is the same graceful-degradation behaviour this crate
/// already relies on for a truncated single-partition frame.
fn split_token_partitions(residual: &[u8], num_partitions: usize) -> Vec<&[u8]> {
    let empty: &[u8] = residual.get(..0).unwrap_or(&[]);
    let num_partitions = num_partitions.max(1);
    if num_partitions == 1 {
        return vec![residual];
    }
    let table_len = 3 * (num_partitions - 1);
    let Some(size_table) = residual.get(..table_len) else {
        // Table itself does not fit: every partition is empty.
        return vec![empty; num_partitions];
    };
    let mut offset = table_len;
    let mut out: Vec<&[u8]> = Vec::new();
    for i in 0..num_partitions - 1 {
        let Some(&[b0, b1, b2]) = size_table.get(i * 3..i * 3 + 3) else {
            out.push(empty);
            continue;
        };
        let size = usize::from(b0) | (usize::from(b1) << 8) | (usize::from(b2) << 16);
        let end = offset.saturating_add(size).min(residual.len());
        out.push(residual.get(offset..end).unwrap_or(empty));
        offset = end;
    }
    // The last partition takes whatever remains, per RFC 6386 §9.5.
    out.push(residual.get(offset..).unwrap_or(empty));
    out
}

fn decode_frame(state: &mut State, budget: &mut Budget, data: &[u8]) -> Result<Option<Frame>> {
    let Some(tag) = parse_frame_tag(data) else {
        return Err(Error::InvalidData("vp8: bad frame tag"));
    };
    let header_offset = if tag.key_frame { 10 } else { 3 };
    let first_partition = data
        .get(header_offset..)
        .ok_or(Error::InvalidData("vp8: truncated frame"))?;

    if tag.key_frame
        && let Some((w, h)) = tag.size
    {
        state.width = w;
        state.height = h;
        state.mb_cols = usize::from(w).div_ceil(16);
        state.mb_rows = usize::from(h).div_ceil(16);
        state.segment_map = vec![0u8; state.mb_cols * state.mb_rows];
        state.entropy = EntropyContext::default();
        state.segmentation = header::Segmentation::default();
        state.lf_deltas = header::LoopFilterDeltas::default();
    }

    if state.mb_cols == 0 || state.mb_rows == 0 {
        return Err(Error::InvalidData("vp8: no key frame seen yet"));
    }

    let mut bd = Bd::new(first_partition);
    let saved_entropy = state.entropy.clone();
    let fh = header::parse(
        &mut bd,
        tag.key_frame,
        tag.version,
        tag.show_frame,
        tag.size,
        &mut state.entropy,
        &mut state.segmentation,
        &mut state.lf_deltas,
    );

    // The first data partition ends at `first_part_size`; the token
    // partition(s) start right after it. RFC 6386 §9.5: when there is more
    // than one token partition, each partition but the last is preceded by
    // a 3-byte little-endian size; the last partition takes whatever bytes
    // remain. `split_token_partitions` turns that layout into one slice per
    // partition, and macroblock row `r` reads its tokens from partition
    // `r % num_partitions` (RFC 6386 §9.5's stated reason for the split:
    // "the decoder can perform parallel decoding" one row group per
    // partition).
    let residual_start = header_offset + tag.first_part_size as usize;
    let residual = first_partition
        .get(tag.first_part_size as usize..)
        .filter(|_| residual_start <= data.len())
        .unwrap_or(&[]);
    let token_partitions = split_token_partitions(residual, fh.num_partitions);
    let mut token_bds: Vec<Bd<'_>> = token_partitions.iter().map(|p| Bd::new(p)).collect();

    let mut mbs = vec![MbInfo::default(); state.mb_cols * state.mb_rows];
    let mut picture = Picture::new(budget, state.mb_cols, state.mb_rows)?;

    let sign_bias = [false, false, fh.sign_bias_golden, fh.sign_bias_altref];

    {
        let mut ctx = FrameCtx {
            header: &fh,
            mb_cols: state.mb_cols,
            mb_rows: state.mb_rows,
            mbs: std::mem::take(&mut mbs),
            segment_map: &mut state.segment_map,
            y: std::mem::replace(&mut picture.y, Plane::new(budget, 0, 0)?),
            u: std::mem::replace(&mut picture.u, Plane::new(budget, 0, 0)?),
            v: std::mem::replace(&mut picture.v, Plane::new(budget, 0, 0)?),
            above_y: vec![[false; 4]; state.mb_cols],
            above_u: vec![[false; 2]; state.mb_cols],
            above_v: vec![[false; 2]; state.mb_cols],
            above_y2: vec![false; state.mb_cols],
            left_y: [false; 4],
            left_u: [false; 2],
            left_v: [false; 2],
            left_y2: false,
            above_bmode: vec![[tables::B_DC_PRED; 4]; state.mb_cols],
            left_bmode: [tables::B_DC_PRED; 4],
        };

        let num_token_partitions = token_bds.len().max(1);
        for row in 0..state.mb_rows {
            ctx.left_y = [false; 4];
            ctx.left_u = [false; 2];
            ctx.left_v = [false; 2];
            ctx.left_y2 = false;
            ctx.left_bmode = [tables::B_DC_PRED; 4];
            // RFC 6386 §9.5: macroblock row `row` reads its coefficient
            // tokens from partition `row % num_partitions`.
            let Some(token_bd) = token_bds.get_mut(row % num_token_partitions) else {
                continue;
            };
            for col in 0..state.mb_cols {
                decode_macroblock(&mut bd, token_bd, &mut ctx, &state.entropy, &state.refs, sign_bias, col, row);
            }
        }

        apply_loop_filter(&mut ctx);

        mbs = ctx.mbs;
        picture.y = ctx.y;
        picture.u = ctx.u;
        picture.v = ctx.v;
    }
    let _ = mbs;

    if !fh.refresh_entropy_probs {
        state.entropy = saved_entropy;
    }

    state.refs.update(
        picture.clone(),
        fh.refresh_last,
        fh.refresh_golden,
        fh.refresh_altref,
        fh.copy_to_golden,
        fh.copy_to_altref,
    );

    if !fh.show_frame {
        return Ok(None);
    }

    let fmt = PixFmt::from_name("yuv420p")
        .map_err(|_| Error::InvalidData("vp8: yuv420p pixel format is not registered"))?;
    let mut frame = Frame::alloc_video(budget, fmt, u32::from(state.width), u32::from(state.height))?;
    if fh.key_frame {
        frame.flags |= FrameFlags::KEY;
    }
    blit(&picture.y, &mut frame, 0, usize::from(state.width), usize::from(state.height));
    blit(&picture.u, &mut frame, 1, usize::from(state.width).div_ceil(2), usize::from(state.height).div_ceil(2));
    blit(&picture.v, &mut frame, 2, usize::from(state.width).div_ceil(2), usize::from(state.height).div_ceil(2));
    Ok(Some(frame))
}

fn blit(src: &Plane, frame: &mut Frame, plane_index: usize, width: usize, height: usize) {
    let Some(mut dst) = frame.plane_mut(plane_index) else {
        return;
    };
    for y in 0..height {
        let Some(row) = dst.row_mut(y) else { continue };
        let src_row = src.row(y);
        for (x, out) in row.iter_mut().enumerate().take(width) {
            *out = src_row.get(x).copied().unwrap_or(0);
        }
    }
}

/// VP8 decoder, RFC 6386.
pub struct Vp8Decoder {
    machine: Machine<Frame>,
    limits: Limits,
    budget: Budget,
    state: State,
}

impl Vp8Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            machine: Machine::new(vaco_codec_core::Caps::empty()),
            budget: Budget::new(limits.clone()),
            limits,
            state: State::default(),
        }
    }
}

impl std::fmt::Debug for Vp8Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vp8Decoder").finish_non_exhaustive()
    }
}

impl Decoder for Vp8Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                match decode_frame(&mut self.state, &mut self.budget, pkt.payload()) {
                    Ok(Some(mut frame)) => {
                        frame.pts = pkt.pts;
                        frame.duration = pkt.duration;
                        self.machine.emit(frame);
                        Ok(())
                    }
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.state = State::default();
        // Release every reference-frame byte charged to the budget along
        // with the state that held them.
        self.budget = Budget::new(self.limits.clone());
    }
}

/// `vaco-component.toml`'s decoder registration point.
pub static VP8_DECODER: DecoderDesc = DecoderDesc {
    name: "vp8",
    long_name: "On2 VP8 (RFC 6386)",
    id: CodecId::Vp8,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::empty(),
    supported_rates: &[],
    make: |limits| Box::new(Vp8Decoder::new(limits)),
};

#[cfg(test)]
mod token_partition_tests {
    use super::split_token_partitions;

    #[test]
    fn single_partition_is_the_whole_slice_unmodified() {
        let residual = [1u8, 2, 3, 4, 5];
        let parts = split_token_partitions(&residual, 1);
        assert_eq!(parts, vec![&residual[..]]);
    }

    #[test]
    fn four_partitions_split_on_the_rfc_6386_9_5_size_table() {
        // Size table (3 bytes each, little-endian) for 3 non-last
        // partitions of sizes 2, 3 and 1, followed by their data and then
        // whatever remains for the fourth (last) partition.
        let mut residual = Vec::new();
        residual.extend_from_slice(&[2, 0, 0]); // partition 0 size = 2
        residual.extend_from_slice(&[3, 0, 0]); // partition 1 size = 3
        residual.extend_from_slice(&[1, 0, 0]); // partition 2 size = 1
        residual.extend_from_slice(&[0xAA, 0xAB]); // partition 0 data (2 bytes)
        residual.extend_from_slice(&[0xBA, 0xBB, 0xBC]); // partition 1 data (3 bytes)
        residual.extend_from_slice(&[0xCA]); // partition 2 data (1 byte)
        residual.extend_from_slice(&[0xDA, 0xDB, 0xDC, 0xDD]); // partition 3 (last, remainder)

        let parts = split_token_partitions(&residual, 4);
        let want: [&[u8]; 4] = [&[0xAA, 0xAB], &[0xBA, 0xBB, 0xBC], &[0xCA], &[0xDA, 0xDB, 0xDC, 0xDD]];
        assert_eq!(parts, want);
    }

    #[test]
    fn a_size_table_that_does_not_fit_yields_empty_partitions_rather_than_panicking() {
        let residual = [0u8, 1]; // far too short for an 8-partition table (21 bytes)
        let parts = split_token_partitions(&residual, 8);
        assert_eq!(parts.len(), 8);
        assert!(parts.iter().all(|p| p.is_empty()));
    }

    #[test]
    fn a_size_that_overruns_the_buffer_is_clamped_not_out_of_bounds() {
        let mut residual = Vec::new();
        residual.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // partition 0 "size" = 16MB-ish, way past the buffer
        residual.extend_from_slice(&[1, 2, 3]); // only 3 bytes actually follow
        let parts = split_token_partitions(&residual, 2);
        // Clamped to whatever is actually left, and the last partition gets
        // nothing since the first one already consumed the rest.
        let want: [&[u8]; 2] = [&[1, 2, 3], &[]];
        assert_eq!(parts, want);
    }
}
