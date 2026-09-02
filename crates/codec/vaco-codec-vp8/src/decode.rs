//! The per-macroblock decode loop: mode/MV records, residual tokens,
//! reconstruction and the loop filter, tied together into a
//! [`vaco_codec_core::Decoder`].
//!
//! # Two "known-unverified" pieces, now checked (issue #301)
//!
//! Two details RFC 6386's narrative prose leaves to "the reference decoder"
//! were previously implemented from a widely-documented convention rather
//! than a primary-text citation. Both are now checked line-by-line against
//! RFC 6386's own reference-decoder appendix (§20.6 `dixie_loopfilter.c` and
//! §20.13's `calculate_chroma_splitmv`/the non-split chroma shortcut in
//! §20.14's `predict_inter_emulated_edge` — Tier A, part of the RFC itself,
//! not a third-party implementation) and confirmed correct:
//!
//! 1. The loop-filter *mode* delta index (0 = `B_PRED`, 1 = `ZEROMV`, 2 =
//!    other inter modes, 3 = `SPLITMV`, no delta for other intra modes) —
//!    [`mode_delta_index`] — matches §20.6's `calculate_filter_parameters`
//!    exactly.
//! 2. Chroma motion-vector rounding — [`round_div8`] — summing four luma
//!    subblock components and dividing by 8 with this crate's symmetric
//!    round is algebraically identical to §20.14's whole-block shortcut
//!    (divide the single luma MV by 2, same rounding), and matches
//!    §20.13's `calculate_chroma_splitmv` exactly for the split case.
//!
//! A **real bug** turned up during the same check, in a third place the
//! loop filter reads: whether to skip the four *internal* subblock edges.
//! §20.6's own comment on the equivalent test warns "this conditional is
//! actually dependent on the number of coefficients decoded, not the skip
//! flag as coded in the bitstream" — this crate had used exactly that skip
//! flag ([`MbInfo`]'s now-removed `skip_coeff` reuse in [`apply_loop_filter`]),
//! which is wrong whenever `mb_skip_coeff` is clear but every decoded block
//! still happens to carry zero coefficients. Fixed by tracking
//! [`MbInfo::any_coeff`] from the actual decoded token results
//! ([`any_nonzero_coeff`]) instead.
//!
//! # Conformance (issue #301)
//!
//! `tests/conformance.rs` decodes real `webmproject/vp8-test-vectors` files
//! and diffs Y, U and V **separately** against `ffmpeg`'s own decode of the
//! same file. 58 of the 60 official vectors (a curated 10-vector subset is
//! committed under `tests/fixtures/vp8/`) are **byte-exact** on every plane
//! after the fix above. The remaining 2 use a non-zero RFC 6386 §9.1
//! display-rescale code this crate does not implement — a real, scoped-out
//! feature gap, not a reconstruction defect (see that test file's module
//! doc for how the two were told apart from real bugs, including an
//! `ffmpeg` default-vsync frame-duplication trap in the harness itself).

use vaco_codec_core::CodecId;
use vaco_codec_core::machine::{Accept, Machine};
use vaco_codec_core::picture::{PictureSpec, PlaneSpec, ProgressPicture};
use vaco_codec_core::{Decoder, DecoderDesc, FrameRunner, Threading};
use vaco_codec_msac::Vp8BoolDecoder as Bd;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_parse_vpx::vp8::parse_frame_tag;

use crate::frame_task::Vp8FrameTask;
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
pub(crate) fn ix(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// The inverse of [`ix`], clamping negative values to 0 -- every call site
/// already established the value is non-negative before converting back to
/// a plane index.
pub(crate) fn ux(v: i32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

pub(crate) fn above_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if y < 0 {
        predict::OFF_FRAME_ABOVE
    } else {
        plane.get(x, y)
    }
}

pub(crate) fn left_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if x < 0 {
        predict::OFF_FRAME_LEFT
    } else {
        plane.get(x, y)
    }
}

/// `x`/`y` are the block's own top-left position (not already decremented,
/// unlike [`above_pixel`]/[`left_pixel`]'s callers) — the corner pixel is at
/// `(x-1, y-1)`, off-frame above whenever `y == 0` and off-frame left
/// whenever `x == 0` (the "above" fill wins at the frame's top-left corner,
/// matching [`predict::OFF_FRAME_ABOVE`]'s note in `predict`'s module doc).
pub(crate) fn corner_pixel(plane: &Plane, x: i32, y: i32) -> u8 {
    if y == 0 {
        predict::OFF_FRAME_ABOVE
    } else if x == 0 {
        predict::OFF_FRAME_LEFT
    } else {
        plane.get(x - 1, y - 1)
    }
}

pub(crate) fn gather_above<const N: usize>(plane: &Plane, x: i32, y: i32) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = above_pixel(plane, x + ix(i), y - 1);
    }
    out
}

pub(crate) fn gather_left<const N: usize>(plane: &Plane, x: i32, y: i32) -> [u8; N] {
    let mut out = [0u8; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = left_pixel(plane, x - 1, y + ix(i));
    }
    out
}

/// Everything about one already-decoded macroblock that a later macroblock
/// (mode context, motion-vector prediction, the loop filter) needs to know.
///
/// `pub(crate)` (issue #301): [`crate::frame_task`] reads `filter_level`,
/// `has_y2` and `any_coeff` to drive the loop filter on its own copy of this
/// frame's macroblock grid, once reconstruction has moved off the serial
/// parse stage and onto a worker thread.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MbInfo {
    pub(crate) ref_frame: u8,     // 0 = intra
    pub(crate) mv: Mv,            // eighth-pel; representative MV (SPLITMV: subblock 15's)
    pub(crate) sub_mvs: [Mv; 16], // eighth-pel per-subblock MV (all equal to `mv` unless SPLITMV)
    pub(crate) is_splitmv: bool,
    pub(crate) has_y2: bool,
    /// Whether *any* Y1/Y2/U/V block actually decoded a non-zero
    /// coefficient. RFC 6386 §15.1's own reference decoder (`dixie`,
    /// §20.6) flags that its loop-filter skip test "is actually dependent
    /// on the number of coefficients decoded, not the skip flag as coded
    /// in the bitstream" — `eob_mask` there is set from each block's
    /// decoded end-of-block count, not from `mb_skip_coeff`. The two agree
    /// whenever `mb_skip_coeff` is set (no tokens are even read), but a
    /// macroblock can have `mb_skip_coeff` clear and still decode every
    /// block to all-zero, in which case the reference still skips the
    /// internal-edge filter and this field is what lets [`crate::decode`]
    /// match that.
    pub(crate) any_coeff: bool,
    pub(crate) filter_level: i32,
}

impl Default for MbInfo {
    fn default() -> Self {
        Self {
            ref_frame: 0,
            mv: (0, 0),
            sub_mvs: [(0, 0); 16],
            is_splitmv: false,
            has_y2: true,
            any_coeff: false,
            filter_level: 0,
        }
    }
}

/// One already-token-decoded macroblock, ready for prediction and the
/// inverse transform — the record that crosses the split/task boundary
/// (issue #301). Everything in here comes from the bitstream and the
/// bool-decoder-driven mode/motion-vector context of *this frame's own*
/// earlier macroblocks; nothing in it depends on any macroblock's
/// reconstructed pixels, which is what lets every macroblock's tokens be
/// decoded serially while the pixels they describe are produced later, on a
/// worker thread, overlapped with the next frame's own token decode.
#[derive(Debug, Clone)]
pub(crate) enum ParsedMb {
    Intra(ParsedIntra),
    Inter(ParsedInter),
}

impl Default for ParsedMb {
    fn default() -> Self {
        Self::Intra(ParsedIntra::default())
    }
}

/// An intra macroblock's decoded mode and residual, pre-dequantised.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParsedIntra {
    pub(crate) y_mode: i32,
    pub(crate) uv_mode: i32,
    pub(crate) sub_modes: [i32; 16],
    pub(crate) y_blocks: [BlockCoeffs; 16],
    pub(crate) y2_block: Option<BlockCoeffs>,
    pub(crate) u_blocks: [BlockCoeffs; 4],
    pub(crate) v_blocks: [BlockCoeffs; 4],
}

/// An inter macroblock's decoded reference/motion/residual, pre-dequantised.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParsedInter {
    pub(crate) ref_frame: u8,
    pub(crate) whole_mv: Mv,
    pub(crate) sub_mvs: [Mv; 16],
    pub(crate) is_splitmv: bool,
    pub(crate) y_blocks: [BlockCoeffs; 16],
    pub(crate) y2_block: Option<BlockCoeffs>,
    pub(crate) u_blocks: [BlockCoeffs; 4],
    pub(crate) v_blocks: [BlockCoeffs; 4],
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
pub(crate) fn round_div8(x: i32) -> i32 {
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
    /// Every macroblock's decoded mode/motion/residual (issue #301),
    /// populated in raster order alongside `mbs`. This struct carries no
    /// pixel planes at all: parsing needs none (see [`split_frame`]'s doc),
    /// and reconstruction reads this field instead, from
    /// [`crate::frame_task::Vp8FrameTask`].
    parsed: Vec<ParsedMb>,
    segment_map: &'a mut Vec<u8>,
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

/// Read one macroblock's segment id, skip flag, mode-and-motion record and
/// residual tokens, storing the result into `ctx.parsed` (issue #301).
///
/// This is the *parse* half only: it decodes everything the bitstream and
/// this frame's own already-parsed macroblocks can determine, and touches no
/// pixels — reconstruction (which needs the previous frame's pixels for
/// inter prediction) happens later, in
/// [`crate::frame_task::Vp8FrameTask::run`].
#[allow(clippy::too_many_arguments, reason = "one macroblock's worth of state")]
fn decode_macroblock(
    bd: &mut Bd<'_>,
    token_bd: &mut Bd<'_>,
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    sign_bias: [bool; 4],
    col: usize,
    row: usize,
) {
    let mb_idx = row * ctx.mb_cols + col;

    // -- segment id --
    let segment_id = if ctx.header.segmentation.enabled && ctx.header.segmentation.update_map {
        let id = u8::try_from(bd.read_tree(
            &tables::MB_SEGMENT_TREE,
            &ctx.header.segmentation.tree_probs,
        ))
        .unwrap_or(0);
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
                    ctx.left_bmode
                        .get(sub_row)
                        .copied()
                        .unwrap_or(tables::B_DC_PRED)
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
        let parsed = parse_intra(
            ctx, entropy, token_bd, col, row, mode, uv_mode, &sub_modes, skip_coeff, segment_id,
        );
        let any_coeff = any_nonzero_coeff(
            parsed.y2_block.as_ref(),
            &parsed.y_blocks,
            &parsed.u_blocks,
            &parsed.v_blocks,
        );
        store_parsed(ctx, col, row, ParsedMb::Intra(parsed));
        store_mb(
            ctx,
            col,
            row,
            segment_id,
            any_coeff,
            0,
            mode,
            (0, 0),
            [(0, 0); 16],
            false,
        );
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
        let parsed = parse_intra(
            ctx, entropy, token_bd, col, row, mode, uv_mode, &sub_modes, skip_coeff, segment_id,
        );
        let any_coeff = any_nonzero_coeff(
            parsed.y2_block.as_ref(),
            &parsed.y_blocks,
            &parsed.u_blocks,
            &parsed.v_blocks,
        );
        store_parsed(ctx, col, row, ParsedMb::Intra(parsed));
        store_mb(
            ctx,
            col,
            row,
            segment_id,
            any_coeff,
            0,
            mode,
            (0, 0),
            [(0, 0); 16],
            false,
        );
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
    let current_sign_bias = sign_bias
        .get(usize::from(ref_frame))
        .copied()
        .unwrap_or(false);
    let near = mv::find_near_mvs(
        neighbor(above),
        neighbor(left),
        neighbor(above_left),
        |rf| sign_bias.get(usize::from(rf)).copied().unwrap_or(false) == current_sign_bias,
    );

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

    let parsed = parse_inter(
        ctx, entropy, token_bd, col, row, ref_frame, whole_mv, &sub_mvs, is_splitmv, skip_coeff,
        segment_id,
    );
    let any_coeff = any_nonzero_coeff(
        parsed.y2_block.as_ref(),
        &parsed.y_blocks,
        &parsed.u_blocks,
        &parsed.v_blocks,
    );
    let stored_sub_mvs = if is_splitmv { sub_mvs } else { [whole_mv; 16] };
    store_parsed(ctx, col, row, ParsedMb::Inter(parsed));
    store_mb(
        ctx,
        col,
        row,
        segment_id,
        any_coeff,
        ref_frame,
        mode,
        whole_mv,
        stored_sub_mvs,
        is_splitmv,
    );
}

pub(crate) fn mv_bounds(
    col: usize,
    row: usize,
    mb_cols: usize,
    mb_rows: usize,
) -> (i32, i32, i32, i32) {
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
    any_coeff: bool,
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
            ref_frame,
            mv,
            sub_mvs,
            is_splitmv,
            has_y2,
            any_coeff,
            filter_level: level,
        };
    }
}

/// Store one macroblock's parse result at its raster position (issue #301).
fn store_parsed(ctx: &mut FrameCtx<'_>, col: usize, row: usize, parsed: ParsedMb) {
    let idx = row * ctx.mb_cols + col;
    if let Some(slot) = ctx.parsed.get_mut(idx) {
        *slot = parsed;
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
/// The parse half of an intra macroblock (issue #301): mode and residual
/// tokens only, no pixels. See [`apply_intra`] for the reconstruction half.
fn parse_intra(
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
) -> ParsedIntra {
    let has_y2 = y_mode != tables::B_PRED;
    let quant = dequant_for(ctx, segment_id);
    let (y_blocks, y2_block, u_blocks, v_blocks) =
        decode_residuals(bd, ctx, entropy, col, row, has_y2, skip_coeff, &quant);
    ParsedIntra {
        y_mode,
        uv_mode,
        sub_modes: *sub_modes,
        y_blocks,
        y2_block,
        u_blocks,
        v_blocks,
    }
}

/// The reconstruction half of an intra macroblock (issue #301): everything
/// [`parse_intra`] used to do after its own residual decode, driven by the
/// already-parsed record instead. Runs on
/// [`crate::frame_task::Vp8FrameTask`]'s own copy of this frame's planes.
pub(crate) fn apply_intra(
    y: &mut Plane,
    u: &mut Plane,
    v: &mut Plane,
    mb_cols: usize,
    col: usize,
    row: usize,
    p: &ParsedIntra,
) {
    let base_x = ix(col * 16);
    let base_y = ix(row * 16);

    if p.y_mode == tables::B_PRED {
        #[allow(
            clippy::integer_division,
            reason = "splitting a 0..16 subblock index into its 4x4 grid position"
        )]
        for (i, (&sub_mode, block)) in p.sub_modes.iter().zip(p.y_blocks.iter()).enumerate() {
            let sub_col = i % 4;
            let sub_row = i / 4;
            let x = base_x + ix(sub_col * 4);
            let y_pos = base_y + ix(sub_row * 4);
            let above8 = gather_above_right(y, col, row, sub_col, sub_row, mb_cols);
            let left4: [u8; 4] = gather_left(y, x, y_pos);
            let corner = corner_pixel(y, x, y_pos);
            let pred = b_pred(sub_mode, &above8, &left4, corner);
            write_residual_block(y, x, y_pos, &pred, block);
        }
    } else {
        predict_and_write_16(
            y,
            base_x,
            base_y,
            p.y_mode,
            &p.y_blocks,
            p.y2_block.as_ref(),
        );
    }

    predict_and_write_8(u, ix(col * 8), ix(row * 8), p.uv_mode, &p.u_blocks);
    predict_and_write_8(v, ix(col * 8), ix(row * 8), p.uv_mode, &p.v_blocks);
}

/// Whether any Y1/Y2/U/V block of a macroblock decoded a non-zero
/// coefficient — RFC 6386 §15.1's actual loop-filter skip test (see
/// [`MbInfo::any_coeff`]'s doc), computed from what token decode actually
/// produced rather than from the `mb_skip_coeff` bitstream flag.
fn any_nonzero_coeff(
    y2: Option<&BlockCoeffs>,
    y: &[BlockCoeffs; 16],
    u: &[BlockCoeffs; 4],
    v: &[BlockCoeffs; 4],
) -> bool {
    y2.is_some_and(|b| b.has_coeffs)
        || y.iter().any(|b| b.has_coeffs)
        || u.iter().any(|b| b.has_coeffs)
        || v.iter().any(|b| b.has_coeffs)
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

fn gather_above_right(
    plane: &Plane,
    col: usize,
    row: usize,
    sub_col: usize,
    sub_row: usize,
    mb_cols: usize,
) -> [u8; 8] {
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

pub(crate) fn predict_and_write_16(
    plane: &mut Plane,
    x: i32,
    y: i32,
    mode: i32,
    blocks: &[BlockCoeffs; 16],
    y2: Option<&BlockCoeffs>,
) {
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
pub(crate) fn get2d<const N: usize>(m: &[[u8; N]; N], r: usize, c: usize) -> u8 {
    m.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0)
}

pub(crate) fn predict_and_write_8(
    plane: &mut Plane,
    x: i32,
    y: i32,
    mode: i32,
    blocks: &[BlockCoeffs; 4],
) {
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

pub(crate) fn write_residual_block(
    plane: &mut Plane,
    x: i32,
    y: i32,
    pred: &[[u8; 4]; 4],
    block: &BlockCoeffs,
) {
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

pub(crate) fn dequantize(coeffs: &BlockCoeffs, dc: i32, ac: i32) -> BlockCoeffs {
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
) -> (
    [BlockCoeffs; 16],
    Option<BlockCoeffs>,
    [BlockCoeffs; 4],
    [BlockCoeffs; 4],
) {
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
        return (
            y_blocks,
            if has_y2 { Some(empty) } else { None },
            u_blocks,
            v_blocks,
        );
    }

    if has_y2 {
        let above_ctx = usize::from(ctx.above_y2.get(col).copied().unwrap_or(false));
        let left_ctx = usize::from(ctx.left_y2);
        let block = tokens::decode_block(
            bd,
            &entropy.coeff_probs[tables::PLANE_Y2],
            0,
            above_ctx + left_ctx,
        );
        ctx.left_y2 = block.has_coeffs;
        if let Some(a) = ctx.above_y2.get_mut(col) {
            *a = block.has_coeffs;
        }
        y2_block = Some(dequantize(&block, quant.y2_dc, quant.y2_ac));
    }

    let y_plane = if has_y2 {
        tables::PLANE_Y_AFTER_Y2
    } else {
        tables::PLANE_Y_NO_Y2
    };
    let first = usize::from(has_y2);
    let probs = entropy
        .coeff_probs
        .get(y_plane)
        .unwrap_or(&entropy.coeff_probs[0]);
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
            let block = tokens::decode_block(
                bd,
                &entropy.coeff_probs[tables::PLANE_UV],
                0,
                above_ctx + left_ctx,
            );
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

/// The parse half of an inter macroblock (issue #301): mode/motion and
/// residual tokens only, no reference-picture reads. See [`apply_inter`] for
/// the reconstruction half.
#[allow(clippy::too_many_arguments, reason = "one macroblock's worth of state")]
fn parse_inter(
    ctx: &mut FrameCtx<'_>,
    entropy: &EntropyContext,
    bd: &mut Bd<'_>,
    col: usize,
    row: usize,
    ref_frame: u8,
    whole_mv: Mv,
    sub_mvs: &[Mv; 16],
    is_splitmv: bool,
    skip_coeff: bool,
    segment_id: u8,
) -> ParsedInter {
    let quant = dequant_for(ctx, segment_id);
    let has_y2 = !is_splitmv;
    let (y_blocks, y2_block, u_blocks, v_blocks) =
        decode_residuals(bd, ctx, entropy, col, row, has_y2, skip_coeff, &quant);
    ParsedInter {
        ref_frame,
        whole_mv,
        sub_mvs: *sub_mvs,
        is_splitmv,
        y_blocks,
        y2_block,
        u_blocks,
        v_blocks,
    }
}

/// The reconstruction half of an inter macroblock (issue #301): motion
/// compensation and the inverse transform, driven by the already-parsed
/// record instead of decoding it here. `refp` is `None` exactly when the
/// original code's `refs.get(ref_frame)` would have been — a reference slot
/// that was never populated — in which case this leaves the macroblock's
/// pixels at their initial value, matching that behaviour exactly. Runs on
/// [`crate::frame_task::Vp8FrameTask`]'s own copy of this frame's planes,
/// against a reference already materialised by
/// [`crate::framebuf::materialize`].
pub(crate) fn apply_inter(
    y: &mut Plane,
    u: &mut Plane,
    v: &mut Plane,
    refp: Option<&Picture>,
    version: u8,
    col: usize,
    row: usize,
    p: &ParsedInter,
) {
    let Some(refp) = refp else {
        return;
    };
    let is_splitmv = p.is_splitmv;
    let whole_mv = p.whole_mv;
    let sub_mvs = &p.sub_mvs;
    let has_y2 = !is_splitmv;

    let base_x = ix(col * 16);
    let base_y = ix(row * 16);

    let mut y_dc = [0i32; 16];
    if let Some(y2) = &p.y2_block {
        y_dc = transform::inverse_wht(&y2.coeffs);
    }

    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..16 subblock index into its 4x4 grid position"
    )]
    for (i, y_block) in p.y_blocks.iter().enumerate() {
        let sub_col = i % 4;
        let sub_row = i / 4;
        let x = base_x + ix(sub_col * 4);
        let y_pos = base_y + ix(sub_row * 4);
        let mv = if is_splitmv {
            sub_mvs.get(i).copied().unwrap_or(whole_mv)
        } else {
            whole_mv
        };
        let pred = mc_block::<4, 4>(&refp.y, x, y_pos, mv, version);
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
        write_residual_block(y, x, y_pos, &pred, &block);
    }

    // Chroma: derive one MV per 4x4 chroma block from the 4 covering luma
    // subblocks (identical when not split), eighth-pel, divide-by-8 round.
    #[allow(
        clippy::integer_division,
        reason = "splitting a 0..4 subblock index into its 2x2 grid position"
    )]
    for (i, (u_block, v_block)) in p.u_blocks.iter().zip(p.v_blocks.iter()).enumerate() {
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
            .map(|&k| {
                if is_splitmv {
                    sub_mvs.get(k).copied().unwrap_or(whole_mv).0
                } else {
                    whole_mv.0
                }
            })
            .sum();
        let sum_c: i32 = luma_idxs
            .iter()
            .map(|&k| {
                if is_splitmv {
                    sub_mvs.get(k).copied().unwrap_or(whole_mv).1
                } else {
                    whole_mv.1
                }
            })
            .sum();
        let chroma_mv = (round_div8(sum_r), round_div8(sum_c));
        let cx = ix(col * 8) + ix(sub_col * 4);
        let cy = ix(row * 8) + ix(sub_row * 4);

        let pred_u = mc_block::<4, 4>(&refp.u, cx, cy, chroma_mv, version);
        write_residual_block(u, cx, cy, &pred_u, u_block);
        let pred_v = mc_block::<4, 4>(&refp.v, cx, cy, chroma_mv, version);
        write_residual_block(v, cx, cy, &pred_v, v_block);
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
        0 => (false, false),    // bicubic (6-tap), sub-pel
        1 | 2 => (true, false), // bilinear, sub-pel
        _ => (true, true),      // "none": truncate to full-pel
    }
}

pub(crate) fn mc_block<const W: usize, const H: usize>(
    refp: &Plane,
    x: i32,
    y: i32,
    mv: Mv,
    version: u8,
) -> [[u8; W]; H] {
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
/// The loop filter, driven by the `MbInfo` grid the serial parse stage
/// already built rather than by `FrameCtx` (issue #301) — called from
/// [`crate::frame_task::Vp8FrameTask::run`], on its own copy of this frame's
/// planes, after every macroblock has been reconstructed.
pub(crate) fn apply_loop_filter_task(
    y: &mut Plane,
    u: &mut Plane,
    v: &mut Plane,
    mb_cols: usize,
    mb_rows: usize,
    mbs: &[MbInfo],
    filter_level: i32,
    sharpness_level: i32,
    key_frame: bool,
    filter_simple: bool,
) {
    if filter_level == 0 {
        return;
    }
    let mb_info: Vec<loopfilter::MbFilterInfo> = mbs
        .iter()
        .map(|mb| loopfilter::MbFilterInfo {
            filter_level: mb.filter_level,
            skip_inner: mb.has_y2 && !mb.any_coeff,
        })
        .collect();
    loopfilter::apply_frame(
        y,
        u,
        v,
        mb_cols,
        mb_rows,
        sharpness_level,
        key_frame,
        filter_simple,
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

/// The serial half of frame decode (issue #301).
///
/// Parses the frame tag and header, decodes every macroblock's
/// mode/motion/residual tokens, and resolves every reference-frame and
/// entropy-persistence decision — everything this frame's own header and
/// this-frame-only macroblock context can answer without a single pixel
/// existing. Returns the parallel half as a [`Vp8FrameTask`], ready for a
/// [`FrameRunner`], plus whether the frame is shown: [`crate::frame_task`]'s
/// `run` always produces a `Frame` (an invisible altref update still needs
/// full reconstruction, since its pixels may become a future reference), and
/// [`Vp8Decoder`] is what decides whether to hand that frame to its caller.
fn split_frame(
    state: &mut State,
    limits: &Limits,
    budget: &mut Budget,
    data: &[u8],
    decode_index: u64,
) -> Result<(Vp8FrameTask, bool)> {
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

    let sign_bias = [false, false, fh.sign_bias_golden, fh.sign_bias_altref];

    let (mbs, parsed) = {
        let mut ctx = FrameCtx {
            header: &fh,
            mb_cols: state.mb_cols,
            mb_rows: state.mb_rows,
            mbs: vec![MbInfo::default(); state.mb_cols * state.mb_rows],
            parsed: vec![ParsedMb::default(); state.mb_cols * state.mb_rows],
            segment_map: &mut state.segment_map,
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
                decode_macroblock(
                    &mut bd,
                    token_bd,
                    &mut ctx,
                    &state.entropy,
                    sign_bias,
                    col,
                    row,
                );
            }
        }

        (ctx.mbs, ctx.parsed)
    };

    if !fh.refresh_entropy_probs {
        state.entropy = saved_entropy;
    }

    // The references *this* frame predicts from — captured before this
    // frame's own (not-yet-reconstructed) picture can become a candidate for
    // the *next* frame's last/golden/altref slots below.
    let refs_for_this_frame = state.refs.clone();

    let plane_spec = |w: usize, h: usize| {
        PlaneSpec::new(
            u32::try_from(w).unwrap_or(u32::MAX),
            u32::try_from(h).unwrap_or(u32::MAX),
        )
    };
    let spec = PictureSpec::new(vec![
        plane_spec(state.mb_cols * 16, state.mb_rows * 16),
        plane_spec(state.mb_cols * 8, state.mb_rows * 8),
        plane_spec(state.mb_cols * 8, state.mb_rows * 8),
    ])
    .single_band();
    let (writer, this_frame) = ProgressPicture::allocate(&spec, decode_index, budget)?;

    // RFC 6386 §9.7/§9.8's refresh/copy rules, resolved now: `this_frame` is
    // a handle, so this needs no pixels — see `crate::framebuf`'s module doc.
    state.refs.update(
        &this_frame,
        fh.refresh_last,
        fh.refresh_golden,
        fh.refresh_altref,
        fh.copy_to_golden,
        fh.copy_to_altref,
    );

    let task = Vp8FrameTask {
        mb_cols: state.mb_cols,
        mb_rows: state.mb_rows,
        parsed,
        mbs,
        refs: refs_for_this_frame,
        version: fh.version,
        filter_level: fh.filter_level,
        sharpness_level: fh.sharpness_level,
        filter_simple: fh.filter_simple,
        key_frame: fh.key_frame,
        width: state.width,
        height: state.height,
        writer,
        limits: limits.clone(),
    };

    Ok((task, fh.show_frame))
}

pub(crate) fn blit(
    src: &Plane,
    frame: &mut Frame,
    plane_index: usize,
    width: usize,
    height: usize,
) {
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
/// What one dispatched-but-not-yet-collected task needs stamped onto its
/// eventual `Frame`, and whether it should reach the caller at all — RFC
/// 6386's invisible altref frames (`show_frame == false`) still have to be
/// fully reconstructed (their pixels may become a future reference) but must
/// never be emitted, and [`FrameRunner`] deals in `Frame`s uniformly, so
/// this decoder tracks visibility itself, in the same dispatch order the
/// runner already guarantees collection follows.
struct InFlight {
    show_frame: bool,
    pts: vaco_core::Timestamp,
    duration: vaco_core::Duration,
}

/// VP8 decoder, RFC 6386.
///
/// # Threading (issue #301)
///
/// `-threads N` overlaps one frame's reconstruction and loop filter (RFC
/// 6386 §14/§15 — everything after this frame's own tokens are known) with
/// the *next* frame's token decode, over
/// [`vaco_codec_core::threading::FrameRunner`]. VP8 has no B-frame-style
/// reordering — decode order is display order — so unlike a codec with a
/// reorder buffer, this decoder's own output ordering is exactly
/// [`FrameRunner::collect`]'s dispatch order, with invisible altref updates
/// filtered out by `InFlight::show_frame`. See [`crate::frame_task`] for the
/// split itself and [`split_frame`] for where it happens.
pub struct Vp8Decoder {
    machine: Machine<Frame>,
    limits: Limits,
    budget: Budget,
    state: State,
    runner: FrameRunner<Vp8FrameTask>,
    threads: usize,
    /// One entry per dispatched-but-not-yet-collected task, oldest first —
    /// the same invariant `FrameRunner`'s own `slots` queue keeps, checked
    /// against it every time a task is dispatched or collected.
    in_flight: std::collections::VecDeque<InFlight>,
}

impl Vp8Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            // `Caps::DELAY` (issue #301): `-threads N > 1` can hold up to
            // `N` pictures in flight, so a picture accepted several packets
            // ago may only become available during the end-of-stream drain
            // — `Machine::emit`'s own debug assertion requires this flag be
            // declared whenever that can happen, mirroring
            // `vaco_codec_h264::H264Decoder`'s identical precedent.
            machine: Machine::new(vaco_codec_core::Caps::DELAY),
            budget: Budget::new(limits.clone()),
            limits,
            state: State::default(),
            runner: FrameRunner::new(1),
            threads: 1,
            in_flight: std::collections::VecDeque::new(),
        }
    }

    /// Pictures allowed outstanding at once before `send_packet` blocks on
    /// one. Unlike a reordering codec this needs no reorder-window slack:
    /// `threads` pictures actually reconstructing is already the most this
    /// decoder can usefully have in flight.
    const fn max_in_flight(&self) -> usize {
        self.threads
    }

    /// Take the oldest in-flight picture's frame and, unless it was an
    /// invisible altref update, hand it to the caller. `Ok(false)` when
    /// nothing was in flight.
    fn collect_one(&mut self, block: bool) -> Result<bool> {
        if self.in_flight.is_empty() {
            return Ok(false);
        }
        let Some(result) = (if block {
            self.runner.collect()
        } else {
            self.runner.try_collect()
        }) else {
            return Ok(false);
        };
        let Some(meta) = self.in_flight.pop_front() else {
            return Err(Error::InvalidData(
                "vaco-codec-vp8: a frame arrived with no in-flight record",
            ));
        };
        let mut frame = result?;
        if meta.show_frame {
            frame.pts = meta.pts;
            frame.duration = meta.duration;
            self.machine.emit(frame);
        }
        Ok(true)
    }

    fn drain_to_capacity(&mut self) -> Result<()> {
        while self.in_flight.len() >= self.max_in_flight() {
            if !self.collect_one(true)? {
                break;
            }
        }
        Ok(())
    }

    fn drain_all(&mut self) -> Result<()> {
        while self.collect_one(true)? {}
        Ok(())
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
                self.drain_all()?;
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                let decode_index = self.runner.next_decode_index();
                let (task, show_frame) = split_frame(
                    &mut self.state,
                    &self.limits,
                    &mut self.budget,
                    pkt.payload(),
                    decode_index,
                )?;
                self.runner.dispatch(task);
                self.in_flight.push_back(InFlight {
                    show_frame,
                    pts: pkt.pts,
                    duration: pkt.duration,
                });
                self.drain_to_capacity()
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        // Drains the pool before any state below is torn down, so no worker
        // is still holding a `PictureWriter` into a picture this method is
        // about to forget — mirrors `vaco_codec_h264::H264Decoder::flush`'s
        // own precedent for the identical hazard.
        self.runner.reset();
        self.in_flight.clear();
        self.machine.flush();
        self.state = State::default();
        // Release every reference-frame byte charged to the budget along
        // with the state that held them.
        self.budget = Budget::new(self.limits.clone());
    }

    /// `-threads N`. **Off by default**: `N <= 1` builds a runner that spawns
    /// nothing and runs every picture inline at dispatch, the exact call
    /// sequence this decoder had before frame threading existed.
    ///
    /// Legal to call only before the first packet; the runner is rebuilt
    /// here, which would discard anything in flight and desynchronise its
    /// decode-index counter from `state.refs`' own `PictureRef`s.
    fn set_thread_count(&mut self, threads: usize) -> Threading {
        if self.in_flight.is_empty() {
            self.threads = threads.max(1);
            self.runner = FrameRunner::new(self.threads);
            self.threads = self.runner.threads();
        }
        Threading::Frame {
            max_frames: self.max_in_flight(),
            // No extra *output* latency: `collect_one` still emits in
            // dispatch order, exactly as the serial path did.
            delay: 0,
        }
        .clamped_to(self.threads)
    }
}

/// `vaco-component.toml`'s decoder registration point.
pub static VP8_DECODER: DecoderDesc = DecoderDesc {
    name: "vp8",
    long_name: "On2 VP8 (RFC 6386)",
    id: CodecId::Vp8,
    media_type: MediaType::Video,
    caps: vaco_codec_core::Caps::FRAME_THREADS,
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
        let want: [&[u8]; 4] = [
            &[0xAA, 0xAB],
            &[0xBA, 0xBB, 0xBC],
            &[0xCA],
            &[0xDA, 0xDB, 0xDC, 0xDD],
        ];
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
