//! The coding-tree-unit walk: `coding_quadtree()` (§7.3.8.4),
//! `coding_unit()` (§7.3.8.5, intra-only), `transform_tree()`/`transform_unit()`
//! (§7.3.8.8/§7.3.8.10), tying entropy decode ([`crate::residual`]),
//! prediction ([`crate::intra_pred`]) and reconstruction
//! ([`crate::transform`]) together CTU by CTU.
//!
//! Cross-checked against the HM reference decoder's `TDecCu::xDecodeCU` /
//! `TDecEntropy::xDecodeTransform` control flow (BSD-3-Clause, Tier A — see
//! `cabac_ctx`'s module doc), since a transform-tree recursion's exact
//! syntax-element *order* is exactly the kind of thing a fresh re-derivation
//! from clause text alone gets subtly wrong (`AGENT-CONSTRAINTS.md`'s
//! CABAC-context lesson). Decode and reconstruction are interleaved leaf by
//! leaf rather than done in two passes (HM's own `decodeCtu`/`decompressCtu`
//! split) because nothing here needs it: no CABAC context in this crate's
//! scope depends on a reconstructed pixel *value*, only on already-parsed
//! syntax (depth, `cbf`, mode) — see [`crate::framebuf`]'s module doc for the
//! same reasoning applied to neighbour availability.

use vaco_codec_cabac::{CabacDecoder, ContextModel};
use vaco_core::{Error, Result};
use vaco_parse_hevc::{Pps, Sps};

use vaco_limits::Budget;

use crate::cabac_ctx::ContextBank;
use crate::framebuf::{CuGrid, EdgeMarks, Picture};
use crate::intra_mode::{self, DC_IDX, DM_CHROMA_IDX};
use crate::intra_pred;
use crate::motion::{self, Mv, MotionInfo, PartMode, PuRect};
use crate::residual::{self, Coeffs};
use crate::sao::{self, CtuSao};
use crate::transform;

/// Everything one slice segment's CTU walk needs, held together so the
/// recursive functions below stay free functions taking `&mut Ctx` rather
/// than a method on a growing `impl` block.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SPS/PPS/slice-header flag this walk needs, not a state machine in disguise"
)]
pub(crate) struct Ctx<'p> {
    pub pic: &'p mut Picture,
    pub cu_grid: CuGrid,
    pub log2_ctb_size: u32,
    pub log2_min_cb_size: u32,
    pub log2_min_tb_size: u32,
    pub log2_max_tb_size: u32,
    pub max_transform_hierarchy_depth_intra: u32,
    pub pic_width: i32,
    pub pic_height: i32,
    pub slice_qp: i32,
    pub sign_data_hiding: bool,
    pub strong_intra_smoothing: bool,
    pub transform_skip_enabled: bool,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    /// `cu_qp_delta_enabled_flag`.
    cu_qp_delta_enabled: bool,
    /// `Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth`.
    log2_min_cu_qp_delta_size: u32,
    /// §8.6.1's `qPY_PREV`: the last coding unit's finalised `QpY` in
    /// decoding order, or `SliceQpY` at the very start of the slice and (via
    /// `decoder::decode_wpp_rows`'s own reset) at the start of each CTB row
    /// when WPP is active — `pub` because that reset happens in
    /// `decoder.rs`, a different module.
    pub qp_y_prev: i32,
    /// §8.6.1's `qPY_PRED` for the *current* quantisation group, cached at
    /// the QG's own reset ([`coding_quadtree`]) rather than recomputed per
    /// coding unit, since it is a pure function of the QG's own position and
    /// already-decoded neighbours (both fixed for the QG's whole lifetime).
    qg_qp_pred: i32,
    /// Whether `cu_qp_delta_abs`/`cu_qp_delta_sign_flag` has already been
    /// read for the current quantisation group (`!IsCuQpDeltaCoded`'s
    /// negation).
    is_cu_qp_delta_coded: bool,
    /// The current quantisation group's own `CuQpDeltaVal` — `0` until (and
    /// unless) [`maybe_parse_cu_qp_delta`] reads a real one.
    cu_qp_delta_val: i32,
    /// Per-4x4-block transform/CU boundary flags, populated as
    /// [`transform_unit`] reconstructs each luma leaf — the input
    /// [`crate::deblock`]'s post-picture filtering pass reads.
    pub edges: EdgeMarks,
    /// `slice_deblocking_filter_disabled_flag`, after the PPS/slice override
    /// rules `vaco_parse_hevc::SliceHeader` already resolves.
    pub deblocking_disabled: bool,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `slice_tc_offset_div2`.
    pub tc_offset_div2: i32,
    /// `slice_sao_luma_flag`.
    pub sao_luma: bool,
    /// `slice_sao_chroma_flag`.
    pub sao_chroma: bool,
    /// CTU columns per row, for [`sao::parse_ctu_sao`]'s left/above merge
    /// addressing.
    pub ctbs_x: u32,
    /// Every CTU's resolved SAO parameters so far, indexed by raster
    /// address — filled in by [`decode_ctu`] as each CTU's `sao()` is
    /// parsed, read back by a merge at a later address and by
    /// [`crate::sao::filter_picture`] once the whole picture is decoded.
    pub sao_params: Vec<CtuSao>,
    /// Whether this slice is a P slice (`decoder.rs::check_scope` refuses B
    /// slices, so this crate's own slice kinds are exactly `{I, P}`) — every
    /// inter-only field below is `Some` exactly when this is `true`.
    pub is_p_slice: bool,
    inter: Option<InterSliceParams<'p>>,
    /// `max_transform_hierarchy_depth_inter` — kept alongside its intra
    /// counterpart above rather than folded into [`InterSliceParams`],
    /// since [`quadtree_tu_log2_min_in_cu`]'s caller needs it regardless of
    /// which `CuPredMode` a given CU turns out to have (an inter slice's
    /// intra-refresh CU still calls `decode_intra_cu`, which reads the
    /// *intra* field — this one is read only by the inter path).
    pub max_transform_hierarchy_depth_inter: u32,
}

/// One entry of `RefPicList0`, as seen by the CTU walk: its own POC (for
/// merge/AMVP candidate resolution) and a borrow of its reconstructed
/// planes (for motion compensation). Borrowed straight out of
/// [`crate::dpb::Dpb`] by `decoder.rs`, independent of `Ctx::pic` — a
/// different, already-fully-decoded [`Picture`], never the one this walk is
/// currently writing.
pub(crate) struct RefPic<'p> {
    pub poc: i64,
    pub pic: &'p Picture,
}

/// Everything a P-slice CTU walk needs that an I-slice one does not —
/// bundled so [`Ctx::new`] does not grow an eleventh, twelfth, ... plain
/// argument for each one.
pub(crate) struct InterSliceParams<'p> {
    /// `MaxNumMergeCand = 5 - five_minus_max_num_merge_cand`.
    pub max_num_merge_cand: usize,
    /// `Log2ParallelMergeLevel = log2_parallel_merge_level_minus2 + 2`.
    pub log2_parallel_merge_level: u32,
    /// `amp_enabled_flag`.
    pub amp_enabled: bool,
    /// The current picture's own POC — every scaling comparison
    /// (§8.5.3.2.6's `xGetDistScaleFactor`) needs it alongside a neighbour's
    /// or the target reference's.
    pub cur_poc: i64,
    /// `RefPicList0`, resolved to real pictures — see [`RefPic`].
    pub ref_pics_l0: Vec<RefPic<'p>>,
    /// The collocated picture's own compressed motion field for TMVP
    /// (§8.5.3.2.8/.9), or `None` when `slice_temporal_mvp_enabled_flag` is
    /// clear, the collocated picture has none recorded (an I picture), or
    /// `RefPicList0[collocated_ref_idx]` does not resolve — see
    /// `crate::dpb`'s own module doc for how this is built.
    pub collocated: Option<crate::dpb::CollocatedMotionField>,
}

impl<'p> InterSliceParams<'p> {
    /// `RefPicList0`'s own POCs alone — [`crate::motion::derive_merge_candidates`]'s
    /// zero-candidate fill only needs the list, not the pictures.
    fn ref_pocs(&self) -> Vec<i64> {
        self.ref_pics_l0.iter().map(|r| r.poc).collect()
    }

    fn plane_for_poc(&self, poc: i64) -> Option<&'p Picture> {
        self.ref_pics_l0.iter().find(|r| r.poc == poc).map(|r| r.pic)
    }
}

impl<'p> Ctx<'p> {
    /// Re-derives the handful of walk-specific limits from an SPS/PPS pair
    /// the caller ([`crate::decoder`]) has already checked are within this
    /// crate's stated scope.
    #[allow(clippy::too_many_arguments, reason = "one call site (decoder.rs), grouping into a sub-struct would not aid clarity")]
    pub(crate) fn new(
        budget: &mut Budget,
        pic: &'p mut Picture,
        cu_grid: CuGrid,
        sps: &Sps,
        pps: &Pps,
        slice_qp: i32,
        deblocking_disabled: bool,
        beta_offset_div2: i32,
        tc_offset_div2: i32,
        sao_luma: bool,
        sao_chroma: bool,
        inter: Option<InterSliceParams<'p>>,
    ) -> Result<Self> {
        let is_p_slice = inter.is_some();
        let log2_ctb_size = u32::from(sps.log2_min_cb_size) + u32::from(sps.log2_diff_max_min_cb_size);
        let width = usize::try_from(sps.pic_width_in_luma_samples).unwrap_or(0);
        let height = usize::try_from(sps.pic_height_in_luma_samples).unwrap_or(0);
        let ctb_size = 1u32 << log2_ctb_size;
        let ctbs_x = u32::try_from(width).unwrap_or(0).div_ceil(ctb_size).max(1);
        let ctbs_y = u32::try_from(height).unwrap_or(0).div_ceil(ctb_size).max(1);
        let total_ctbs = usize::try_from(ctbs_x.saturating_mul(ctbs_y)).unwrap_or(0);
        let sao_params: Vec<CtuSao> = budget.alloc(total_ctbs)?;
        Ok(Self {
            pic_width: i32::try_from(sps.pic_width_in_luma_samples).unwrap_or(0),
            pic_height: i32::try_from(sps.pic_height_in_luma_samples).unwrap_or(0),
            log2_ctb_size,
            log2_min_cb_size: u32::from(sps.log2_min_cb_size),
            log2_min_tb_size: u32::from(sps.log2_min_tb_size),
            log2_max_tb_size: u32::from(sps.log2_min_tb_size) + u32::from(sps.log2_diff_max_min_tb_size),
            max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
            slice_qp,
            sign_data_hiding: pps.sign_data_hiding_enabled,
            strong_intra_smoothing: sps.strong_intra_smoothing_enabled,
            transform_skip_enabled: pps.transform_skip_enabled,
            bit_depth_luma: u32::from(sps.bit_depth_luma),
            bit_depth_chroma: u32::from(sps.bit_depth_chroma),
            cb_qp_offset: pps.cb_qp_offset,
            cr_qp_offset: pps.cr_qp_offset,
            cu_qp_delta_enabled: pps.cu_qp_delta_enabled,
            log2_min_cu_qp_delta_size: log2_ctb_size.saturating_sub(pps.diff_cu_qp_delta_depth),
            qp_y_prev: slice_qp,
            qg_qp_pred: slice_qp,
            is_cu_qp_delta_coded: false,
            cu_qp_delta_val: 0,
            edges: EdgeMarks::new(width, height),
            deblocking_disabled,
            beta_offset_div2,
            tc_offset_div2,
            sao_luma,
            sao_chroma,
            ctbs_x,
            sao_params,
            pic,
            cu_grid,
            is_p_slice,
            inter,
            max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        })
    }

    /// The P-slice-only parameters, or an error if called on an I-slice
    /// `Ctx` — every call site is itself only reachable when `is_p_slice`,
    /// so this should never actually return `Err`; a clean error rather
    /// than an `unwrap`/`expect` is this crate's own standing rule
    /// (`AGENT-CONSTRAINTS.md`'s code rules), not a case anyone expects to
    /// hit.
    fn inter(&self) -> Result<&InterSliceParams<'p>> {
        self.inter.as_ref().ok_or(Error::InvalidData("vaco-codec-hevc: inter CU decode reached with no P-slice context"))
    }
}

/// §8.6.1's `qPY_PRED`: the predicted luma QP for a quantisation group whose
/// own top-left is `(xqg, yqg)`. Each of `qPY_A`/`qPY_B` falls back to
/// [`Ctx::qp_y_prev`] whenever the corresponding neighbour would sit outside
/// this QG's own CTB — §8.6.1's own same-CTB restriction, confirmed against
/// HM's `getQpMinCuLeft`/`getQpMinCuAbove` (`TComDataCU.cpp`), which return
/// `NULL` exactly when the QG sits at column/row zero *of its own CTU*,
/// always addressing `m_pcPic->getCtu(getCtuRsAddr())` — i.e. never crossing
/// a CTB boundary regardless of whether the neighbour has already been
/// decoded. This subsumes the picture-edge case for free: a QG at `x == 0`
/// or `y == 0` is trivially CTB-aligned too.
fn qp_y_pred(s: &Ctx<'_>, xqg: i32, yqg: i32) -> i32 {
    let ctb = 1i32 << s.log2_ctb_size;
    let qp_a = if xqg % ctb == 0 {
        s.qp_y_prev
    } else {
        s.cu_grid.qp_at(xqg - 1, yqg).map_or(s.qp_y_prev, i32::from)
    };
    let qp_b = if yqg % ctb == 0 {
        s.qp_y_prev
    } else {
        s.cu_grid.qp_at(xqg, yqg - 1).map_or(s.qp_y_prev, i32::from)
    };
    (qp_a + qp_b + 1) >> 1
}

/// §8.6.1's final `QpY`: `qPY_PRED` plus the current quantisation group's own
/// `CuQpDeltaVal`, wrapped over the valid QP range. `QpBdOffsetY == 0`
/// throughout this crate's 8-bit-only scope (see `transform.rs`'s own
/// comment on [`crate::transform::chroma_qp`]), so the general `% (52 +
/// QpBdOffsetY)` collapses to `% 52`; `rem_euclid` rather than a literal
/// transcription of the spec's `+ 52 + 2*QpBdOffsetY` bias defends against a
/// malformed/out-of-declared-range `CuQpDeltaVal` rather than assuming a
/// conforming encoder.
fn derive_qp_y(qp_y_pred: i32, cu_qp_delta_val: i32) -> i32 {
    (qp_y_pred + cu_qp_delta_val).rem_euclid(52)
}

/// `cu_qp_delta_abs`'s truncated-unary prefix (§9.3.3.10's binarisation,
/// context assignment per its own table): the first bin uses `ctx0`, every
/// further bin (up to `max_symbol - 1` of them) shares `ctx1`. A literal port
/// of HM's `TDecSbac::xReadUnaryMaxSymbol` rather than a generic
/// truncated-unary reading, because its "the loop always consumes
/// `max_symbol - 1` further bins and only *afterward* decides whether the
/// last one means +1" shape does not fall out of the more obvious
/// early-return-on-cap reading (confirmed by tracing both against the same
/// bit sequence before trusting the port).
fn read_unary_max(cabac: &mut CabacDecoder<'_>, ctx0: &mut ContextModel, ctx1: &mut ContextModel, max_symbol: u32) -> u32 {
    if max_symbol == 0 {
        return 0;
    }
    let first = cabac.decode_decision(ctx0);
    if first == 0 || max_symbol == 1 {
        return 0;
    }
    let mut symbol = 0u32;
    let mut cont: u32;
    loop {
        cont = cabac.decode_decision(ctx1);
        symbol += 1;
        if !(cont != 0 && symbol < max_symbol - 1) {
            break;
        }
    }
    if cont != 0 && symbol == max_symbol - 1 {
        symbol += 1;
    }
    symbol
}

/// §7.3.8.11's `cu_qp_delta_abs`/`cu_qp_delta_sign_flag`: read at most once
/// per quantisation group, at the first transform-tree leaf (in decoding
/// order) whose luma-or-chroma `cbf` is set — exactly where HM's
/// `TDecEntropy::xDecodeTransform` calls `decodeQP` (`if (validCbf) { if
/// (bCodeDQP) { decodeQP(...); bCodeDQP = false; } }`), itself inside
/// `transform_unit()`'s own syntax table position, before that leaf's own
/// `residual_coding()` of luma. `cu_qp_delta_abs`'s binarisation is TR(cMax
/// = 5) followed, only on saturation, by an EGk(k = 0) suffix (HM's
/// `CU_DQP_TU_CMAX = 5`/`CU_DQP_EG_k = 0`, `xReadUnaryMaxSymbol` then
/// `xReadEpExGolomb`) — [`vaco_codec_cabac::CabacDecoder::decode_bypass_egk`]
/// already implements the latter bit-for-bit (its own doc derives the same
/// "run of 1s, terminating 0, then that many suffix bits" shape).
fn maybe_parse_cu_qp_delta(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, has_residual: bool) -> Result<()> {
    if !s.cu_qp_delta_enabled || s.is_cu_qp_delta_coded || !has_residual {
        return Ok(());
    }
    s.is_cu_qp_delta_coded = true;
    let (ctx0, rest) = ctx.cu_qp_delta.split_first_mut().ok_or(Error::InvalidData("cu_qp_delta ctx"))?;
    let ctx1 = rest.first_mut().ok_or(Error::InvalidData("cu_qp_delta ctx"))?;
    let mut abs_val = read_unary_max(cabac, ctx0, ctx1, 5);
    if abs_val >= 5 {
        abs_val += cabac.decode_bypass_egk(0);
    }
    s.cu_qp_delta_val = if abs_val == 0 {
        0
    } else {
        let sign = cabac.decode_bypass();
        let mag = i32::try_from(abs_val).unwrap_or(i32::MAX);
        if sign != 0 { -mag } else { mag }
    };
    Ok(())
}

/// Decode one CTU (`x0, y0` its luma top-left, in picture coordinates;
/// `addr` its raster address, needed only for `sao()`'s own merge
/// addressing). Parses `sao()` (§7.3.8.3) first, exactly where
/// `coding_tree_unit()`'s own syntax table puts it, when either
/// `slice_sao_luma_flag` or `slice_sao_chroma_flag` is set.
pub(crate) fn decode_ctu(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, addr: u32) -> Result<()> {
    if s.sao_luma || s.sao_chroma {
        let params = sao::parse_ctu_sao(cabac, ctx, addr, s.ctbs_x, s.sao_luma, s.sao_chroma, &s.sao_params)?;
        if let Some(slot) = usize::try_from(addr).ok().and_then(|i| s.sao_params.get_mut(i)) {
            *slot = params;
        }
    }
    coding_quadtree(cabac, ctx, s, x0, y0, s.log2_ctb_size, 0)
}

fn coding_quadtree(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    let in_bounds = x0 + size <= s.pic_width && y0 + size <= s.pic_height;
    let at_min = log2_size == s.log2_min_cb_size;

    let split = if !in_bounds {
        true
    } else if at_min {
        false
    } else {
        let left = s.cu_grid.depth_at(x0 - 1, y0).is_some_and(|d| u32::from(d) > depth);
        let above = s.cu_grid.depth_at(x0, y0 - 1).is_some_and(|d| u32::from(d) > depth);
        let inc = u32::from(left) + u32::from(above);
        let cm = ctx.split_cu_flag.get_mut(inc as usize).ok_or(Error::InvalidData("split_cu_flag ctx out of range"))?;
        cabac.decode_decision(cm) != 0
    };

    // §7.3.8.4: whenever this node is at least `Log2MinCuQpDeltaSize`, it
    // starts a *new* quantisation group — unconditionally, regardless of
    // `split` — since `Log2MinCuQpDeltaSize <= CtbLog2SizeY` always, this
    // fires at least once per CTU and, for a CU larger than the nominal QG
    // size, is the only reset its own (single, larger) QG ever gets.
    if s.cu_qp_delta_enabled && log2_size >= s.log2_min_cu_qp_delta_size {
        s.cu_qp_delta_val = 0;
        s.is_cu_qp_delta_coded = false;
        s.qg_qp_pred = qp_y_pred(s, x0, y0);
    }

    if split {
        let half = size >> 1;
        for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
            let (cx, cy) = (x0 + dx, y0 + dy);
            if cx < s.pic_width && cy < s.pic_height {
                coding_quadtree(cabac, ctx, s, cx, cy, log2_size - 1, depth + 1)?;
            }
        }
        Ok(())
    } else {
        coding_unit(cabac, ctx, s, x0, y0, log2_size, depth)
    }
}

/// One PU's geometry within its CU: top-left and side length.
#[derive(Clone, Copy)]
struct Pu {
    x: i32,
    y: i32,
    size: i32,
}

/// `coding_unit()`'s own dispatch (§7.3.8.5): an I-slice CU is always intra;
/// a P-slice CU reads `cu_skip_flag`/`pred_mode_flag` first and may be
/// either (§7.3.8.5's own field order — `decode_intra_cu` is the
/// I-slice-and-`pred_mode_flag==1` body either way, since an inter slice can
/// still code an intra-refresh CU).
fn coding_unit(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, depth: u32) -> Result<()> {
    if s.is_p_slice {
        coding_unit_p(cabac, ctx, s, x0, y0, log2_size, depth)
    } else {
        decode_intra_cu(cabac, ctx, s, x0, y0, log2_size, depth)
    }
}

/// §8.6.1's own per-coding-unit finalisation tail (HM's `xFinishDecodeCU`),
/// shared by every CU shape (intra, skip, inter): every coding unit gets its
/// own finalised `QpY` written to [`CuGrid`], whether or not it coded its
/// own `cu_qp_delta`.
fn finalize_cu_qp(s: &mut Ctx<'_>, x0: i32, y0: i32, size: i32) {
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid.fill_qp(bx0, by0, blocks, blocks, i8::try_from(qp_y).unwrap_or(0));
    s.qp_y_prev = qp_y;
}

fn decode_intra_cu(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    // §7.3.8.5: `part_mode` (a single ctx-coded bin for an intra CU: `1`
    // means `PART_2Nx2N`, `0` means `PART_NxN`) is present exactly when
    // this CU sits at the minimum coding block size.
    let is_nxn = if log2_size == s.log2_min_cb_size {
        let cm = ctx.part_size.first_mut().ok_or(Error::InvalidData("part_size ctx"))?;
        let bin = cabac.decode_decision(cm);
        bin == 0
    } else {
        false
    };

    let pus: Vec<Pu> = if is_nxn {
        let half = size >> 1;
        vec![
            Pu { x: x0, y: y0, size: half },
            Pu { x: x0 + half, y: y0, size: half },
            Pu { x: x0, y: y0 + half, size: half },
            Pu { x: x0 + half, y: y0 + half, size: half },
        ]
    } else {
        vec![Pu { x: x0, y: y0, size }]
    };

    // §7.3.8.5: every `prev_intra_luma_pred_flag` bin is read before any
    // `mpm_idx`/`rem_intra_luma_pred_mode`.
    let mut prev_flags = [false; 4];
    for slot in prev_flags.iter_mut().take(pus.len()) {
        let cm = ctx.prev_intra_luma_pred.first_mut().ok_or(Error::InvalidData("prev_intra ctx"))?;
        *slot = cabac.decode_decision(cm) != 0;
    }

    let mut luma_modes = [DC_IDX; 4];
    // §8.4.2's `candIntraPredModeB`: forced unavailable (→ `INTRA_DC`) not
    // just at the picture's own top edge but at *every* CTB row boundary —
    // HM's `getPUAbove(..., planarAtCtuBoundary = true)` returns `NULL`
    // whenever the queried position is the top row of *its own* CTB,
    // regardless of whether the CTB above has already been decoded. This
    // is a real spec rule (deliberately not "already decoded" the way
    // `split_cu_flag`'s own above-neighbour context is), not an
    // availability approximation this crate's single-slice/no-tiles scope
    // makes exact elsewhere (see `crate::framebuf`'s module doc) — missing
    // it only desyncs CABAC once a second CTB row exists, which no CTU-0
    // fixture can exercise.
    let ctb_size = 1i32 << s.log2_ctb_size;
    for (i, pu) in pus.iter().enumerate() {
        let left = s.cu_grid.mode_at(pu.x - 1, pu.y);
        let above = if pu.y % ctb_size == 0 { DC_IDX } else { s.cu_grid.mode_at(pu.x, pu.y - 1) };
        let mpm = intra_mode::mpm_list(left, above);
        let prev_flag = prev_flags.get(i).copied().unwrap_or(false);
        let mode = if prev_flag {
            let first = cabac.decode_bypass() != 0;
            let idx = if first { 1 + usize::from(cabac.decode_bypass() != 0) } else { 0 };
            mpm.get(idx).copied().unwrap_or(DC_IDX)
        } else {
            let rem = u8::try_from(cabac.decode_bypass_bits(5)).unwrap_or(0);
            intra_mode::resolve_rem_mode(rem, mpm)
        };
        if let Some(slot) = luma_modes.get_mut(i) {
            *slot = mode;
        }
        let blocks = usize::try_from((pu.size >> 2).max(1)).unwrap_or(1);
        let bx0 = usize::try_from(pu.x >> 2).unwrap_or(0);
        let by0 = usize::try_from(pu.y >> 2).unwrap_or(0);
        s.cu_grid.fill(bx0, by0, blocks, blocks, u8::try_from(depth).unwrap_or(u8::MAX), mode);
    }

    // intra_chroma_pred_mode: once per CU, referencing PU0's luma mode —
    // chroma always predicts as a single 2Nx2N block regardless of `PartMode`.
    let chroma_syntax = {
        let cm = ctx.intra_chroma_pred_mode.first_mut().ok_or(Error::InvalidData("chroma ctx"))?;
        if cabac.decode_decision(cm) == 0 {
            DM_CHROMA_IDX
        } else {
            u8::try_from(cabac.decode_bypass_bits(2)).unwrap_or(0)
        }
    };
    let chroma_mode = intra_mode::chroma_mode(chroma_syntax, luma_modes[0]);

    // §7.3.8.5: `rqt_root_cbf` does not exist for intra CUs (it is inferred
    // 1) — transform_tree() always runs.
    let intra_split_depth_extra = u32::from(is_nxn);
    let quadtree_tu_log2_min = quadtree_tu_log2_min_in_cu(s, log2_size, s.max_transform_hierarchy_depth_intra, intra_split_depth_extra);

    transform_tree(
        cabac,
        ctx,
        s,
        x0,
        y0,
        log2_size,
        0,
        0,
        is_nxn,
        &pus,
        luma_modes,
        chroma_mode,
        quadtree_tu_log2_min,
        true,
        true,
    )?;

    // §8.6.1's own per-coding-unit finalisation (HM's `xFinishDecodeCU`):
    // every coding unit gets a `QpY`, whether or not it coded its own
    // `cu_qp_delta` — `derive_qp_y` with the QG's still-zero
    // `cu_qp_delta_val` reproduces "not yet coded, use qPY_PRED" for free,
    // and reusing an *already*-coded QG's value for a later, delta-less
    // sibling CU is exactly `s.cu_qp_delta_val` not having changed since.
    finalize_cu_qp(s, x0, y0, size);
    Ok(())
}

// ---------------------------------------------------------------------
// P-slice coding units: `cu_skip_flag`, `pred_mode_flag`, the inter
// `part_mode` binarisation, `prediction_unit()` (merge or AMVP+`mvd`), and
// motion compensation. §7.3.8.5/.6/.9.

/// `getCtxSkipFlag`: `(left is skip) + (above is skip)`, both `0` when
/// unavailable — the CU's own top-left corner, not each PU's.
fn ctx_skip_flag(s: &Ctx<'_>, x0: i32, y0: i32) -> usize {
    usize::from(s.cu_grid.is_skip_at(x0 - 1, y0)) + usize::from(s.cu_grid.is_skip_at(x0, y0 - 1))
}

/// `parseMergeIndex`/§9.3.3.2: a truncated-unary code, `cMax =
/// MaxNumMergeCand - 1`, bin 0 context-coded, every further bin bypass.
fn parse_merge_index(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, max_num_merge_cand: usize) -> Result<usize> {
    if max_num_merge_cand <= 1 {
        return Ok(0);
    }
    let cm = ctx.merge_idx.first_mut().ok_or(Error::InvalidData("merge_idx ctx"))?;
    let first_bin = cabac.decode_decision(cm);
    if first_bin == 0 {
        return Ok(0);
    }
    let mut idx = 1usize;
    while idx < max_num_merge_cand - 1 {
        if cabac.decode_bypass() == 0 {
            break;
        }
        idx += 1;
    }
    Ok(idx)
}

/// `parseMVPIdx`: `AMVP_MAX_NUM_CANDS == 2`, so this is a single context-coded
/// bin (`xReadUnaryMaxSymbol`'s own `uiMaxSymbol == 1` case returns after the
/// first bin — see `ctu.rs`'s own `Vaco-Spec-Ref`'d HM reading).
fn parse_mvp_idx(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank) -> Result<usize> {
    let cm = ctx.mvp_idx.first_mut().ok_or(Error::InvalidData("mvp_idx ctx"))?;
    let val = cabac.decode_decision(cm);
    Ok(usize::from(val != 0))
}

/// `parseRefFrmIdx`/§9.3.3.2: bin 0 (`ctx[0]`) says "is it more than 0";
/// every further bin up to `NumRefIdxL0 - 2` of them is context-coded via
/// `ctx[1]` for the first and bypass afterward, truncated-unary shaped.
fn parse_ref_idx(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, num_ref_idx_l0: usize) -> Result<usize> {
    if num_ref_idx_l0 <= 1 {
        return Ok(0);
    }
    let (ctx0, rest) = ctx.ref_pic.split_first_mut().ok_or(Error::InvalidData("ref_idx ctx"))?;
    let bin0 = cabac.decode_decision(ctx0);
    if bin0 == 0 {
        return Ok(0);
    }
    if num_ref_idx_l0 == 2 {
        return Ok(1);
    }
    let ctx1 = rest.first_mut().ok_or(Error::InvalidData("ref_idx ctx"))?;
    let mut idx = 1usize;
    let bin1 = cabac.decode_decision(ctx1);
    if bin1 != 0 {
        idx = 2;
        while idx < num_ref_idx_l0 - 1 {
            if cabac.decode_bypass() == 0 {
                break;
            }
            idx += 1;
        }
    }
    Ok(idx)
}

/// `parseMvd`/§9.3.3.2: `abs_mvd_greater0_flag` (hor then ver, sharing one
/// context), `abs_mvd_greater1_flag` (hor then ver, sharing the second
/// context, each conditional on its own `greater0`), then each component's
/// own EGk(1) suffix (only when its `abs == 2` after the greater1 bin) and
/// sign — exact bin order confirmed against HM's `TDecSbac::parseMvd`; the
/// two axes are genuinely interleaved (`greater0` for both axes before
/// either axis's `greater1`), not decoded axis-by-axis.
fn parse_mvd(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank) -> Result<Mv> {
    let (ctx0, rest) = ctx.mvd.split_first_mut().ok_or(Error::InvalidData("mvd ctx"))?;
    let mut hor_abs = cabac.decode_decision(ctx0);
    let ver_abs_gr0_bin = cabac.decode_decision(ctx0);
    let mut ver_abs = ver_abs_gr0_bin;
    let hor_gr0 = hor_abs != 0;
    let ver_gr0 = ver_abs != 0;
    let ctx1 = rest.first_mut().ok_or(Error::InvalidData("mvd ctx"))?;
    if hor_gr0 {
        let bin = cabac.decode_decision(ctx1);
        hor_abs += bin;
    }
    if ver_gr0 {
        let bin = cabac.decode_decision(ctx1);
        ver_abs += bin;
    }
    let mut hor_sign = 0u32;
    let mut ver_sign = 0u32;
    if hor_gr0 {
        if hor_abs == 2 {
            hor_abs += cabac.decode_bypass_egk(1);
        }
        hor_sign = cabac.decode_bypass();
    }
    if ver_gr0 {
        if ver_abs == 2 {
            ver_abs += cabac.decode_bypass_egk(1);
        }
        ver_sign = cabac.decode_bypass();
    }
    let hor = i32::try_from(hor_abs).unwrap_or(i32::MAX);
    let ver = i32::try_from(ver_abs).unwrap_or(i32::MAX);
    Ok(Mv { x: if hor_sign != 0 { -hor } else { hor }, y: if ver_sign != 0 { -ver } else { ver } })
}

/// §7.3.8.5's inter `part_mode`: the `uiMaxNumBits`-bin prefix
/// (`2Nx2N`/`2NxN`/`Nx2N`[/`NxN`, only at the minimum CB size and only when
/// the CU is not 8x8]), then, when `amp_enabled` and this CU is *not* at the
/// minimum CB size, the AMP shape-refinement bin for `2NxN`/`Nx2N`. A direct
/// port of HM's own `parsePartSize` inter branch, since its bin-count
/// derivation (`uiMaxNumBits`) does not fall out of the binarisation table
/// alone without also knowing the min-CB/AMP gating it is entangled with.
fn parse_part_mode_inter(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, at_min_cb_size: bool, size: i32, amp_enabled: bool) -> Result<PartMode> {
    let max_num_bits: usize = if at_min_cb_size && size != 8 { 3 } else { 2 };
    let mut mode_idx = 0usize;
    for i in 0..max_num_bits {
        let cm = ctx.part_size.get_mut(i).ok_or(Error::InvalidData("part_size ctx"))?;
        let bin = cabac.decode_decision(cm);
        if bin != 0 {
            break;
        }
        mode_idx += 1;
    }
    let mut mode = match mode_idx {
        0 => PartMode::TwoNx2N,
        1 => PartMode::TwoNxN,
        2 => PartMode::Nx2N,
        _ => PartMode::NxN,
    };
    if amp_enabled && !at_min_cb_size {
        if mode == PartMode::TwoNxN {
            let cm = ctx.part_size.get_mut(3).ok_or(Error::InvalidData("part_size ctx"))?;
            if cabac.decode_decision(cm) == 0 {
                mode = if cabac.decode_bypass() == 0 { PartMode::TwoNxNu } else { PartMode::TwoNxNd };
            }
        } else if mode == PartMode::Nx2N {
            let cm = ctx.part_size.get_mut(3).ok_or(Error::InvalidData("part_size ctx"))?;
            if cabac.decode_decision(cm) == 0 {
                mode = if cabac.decode_bypass() == 0 { PartMode::NLx2N } else { PartMode::NRx2N };
            }
        }
    }
    Ok(mode)
}

/// §8.5.3.2.9's temporal candidate, scaled against `(curr_poc,
/// target_ref_poc)` — `None` whenever no collocated field is recorded, the
/// collocated block itself is intra, or (for the bottom-right position
/// only) it would cross the current CTB's own row.
fn temporal_candidate(s: &Ctx<'_>, pu_x: i32, pu_y: i32, pu_w: i32, pu_h: i32, curr_poc: i64, target_ref_poc: i64) -> Result<motion::TemporalCandidate> {
    let inter = s.inter()?;
    let Some(collocated) = &inter.collocated else { return Ok(None) };

    let x_br = pu_x + pu_w;
    let y_br = pu_y + pu_h;
    let ctb_size = 1i32 << s.log2_ctb_size;
    let same_ctb_row = (pu_y >> s.log2_ctb_size) == (y_br >> s.log2_ctb_size);
    let br_in_bounds = x_br < s.pic_width && y_br < s.pic_height && same_ctb_row;
    let _ = ctb_size; // documents the row check's own granularity; the shift comparison above is what matters.

    let raw = if br_in_bounds {
        collocated.get(x_br, y_br)
    } else {
        // §8.5.3.2.9's centre fallback: `(nPbW/4/2)*4` in each axis, matching
        // HM's own z-scan-index arithmetic (see `crate::motion`'s own doc on
        // why positions here are pixel coordinates) rather than a plain
        // `/2`, which the two only agree with when `nPbW`/`nPbH` are
        // multiples of 8 — not guaranteed for an AMP partition's shorter
        // side (a 12-wide/tall PU has a genuinely different centre).
        #[allow(clippy::integer_division, reason = "deliberate truncating division, matching HM's own integer z-scan-index arithmetic exactly")]
        let (cx, cy) = (pu_x + (pu_w / 4 / 2) * 4, pu_y + (pu_h / 4 / 2) * 4);
        collocated.get(cx, cy)
    };
    let Some(info) = raw else { return Ok(None) };
    let scale = motion::dist_scale_factor(curr_poc, target_ref_poc, collocated.poc, info.ref_poc);
    Ok(Some(if scale == 4096 { info.mv } else { motion::scale_mv(info.mv, scale) }))
}

/// A P-slice coding unit: `cu_skip_flag`, then (if not skipped)
/// `pred_mode_flag` — an inter slice can still code an intra-refresh CU,
/// which reuses [`decode_intra_cu`] unchanged (its own `part_mode`/MPM/
/// transform-tree logic has no dependency on the enclosing slice's type).
fn coding_unit_p(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, depth: u32) -> Result<()> {
    let skip_ctx = ctx_skip_flag(s, x0, y0);
    let cm = ctx.skip_flag.get_mut(skip_ctx).ok_or(Error::InvalidData("skip_flag ctx out of range"))?;
    let is_skip = cabac.decode_decision(cm) != 0;
    if is_skip {
        return decode_skip_cu(cabac, ctx, s, x0, y0, log2_size, depth);
    }

    let cm = ctx.pred_mode.first_mut().ok_or(Error::InvalidData("pred_mode ctx"))?;
    let is_intra = cabac.decode_decision(cm) != 0;
    if is_intra {
        decode_intra_cu(cabac, ctx, s, x0, y0, log2_size, depth)
    } else {
        decode_inter_cu(cabac, ctx, s, x0, y0, log2_size, depth)
    }
}

/// A skip CU: §7.3.8.5's `if (cu_skip_flag) { ... return }` shape — a single
/// `PART_2Nx2N` PU, merge-only, no residual (no `rqt_root_cbf`, no transform
/// tree at all).
fn decode_skip_cu(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, depth: u32) -> Result<()> {
    let size = 1i32 << log2_size;
    let max_num_merge_cand = s.inter()?.max_num_merge_cand;
    let merge_idx = parse_merge_index(cabac, ctx, max_num_merge_cand)?;
    let pu = PuRect { x: x0, y: y0, w: size, h: size };
    let chosen = resolve_merge_candidate(s, x0, y0, size, pu, 0, PartMode::TwoNx2N, merge_idx, max_num_merge_cand)?;
    write_inter_cu_no_residual(s, x0, y0, size, &[(pu, chosen)])?;
    let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid.fill(bx0, by0, blocks, blocks, u8::try_from(depth).unwrap_or(u8::MAX), DC_IDX);
    s.cu_grid.fill_motion(bx0, by0, blocks, blocks, chosen.mv, chosen.ref_poc, true);
    finalize_cu_qp(s, x0, y0, size);
    Ok(())
}

/// Runs §8.5.3.2.2's merge derivation for one PU and resolves `merge_idx`
/// into an actual [`MotionInfo`] — shared by the skip path (always PU 0,
/// `TwoNx2N`) and the non-skip merge path (any PU/`PartMode`). `cu_x0`/
/// `cu_y0`/`cu_size` are the CU's own geometry (distinct from `pu`'s own,
/// used only by the merge-parallelism override below).
#[allow(clippy::too_many_arguments, reason = "every argument is a distinct merge-derivation input; a sub-struct would not aid clarity at one internal call site")]
fn resolve_merge_candidate(s: &Ctx<'_>, cu_x0: i32, cu_y0: i32, cu_size: i32, pu: PuRect, pu_idx: usize, part_mode: PartMode, merge_idx: usize, max_num_merge_cand: usize) -> Result<MotionInfo> {
    let inter = s.inter()?;
    // §8.5.3.2.2's own merge-parallelism special case: an 8x8 CU split into
    // more than one PU, with `Log2ParallelMergeLevel > 2`, derives every one
    // of its PUs' merge candidates as if the whole CU were a single
    // `PART_2Nx2N` PU — HM's own `decodePUWise` applies this by temporarily
    // overriding `PartSize` before calling `getInterMergeCandidates`, not by
    // branching inside the derivation itself.
    let merge_override = inter.log2_parallel_merge_level > 2 && part_mode != PartMode::TwoNx2N && cu_size == 8;
    let (eff_pu, eff_idx, eff_mode) = if merge_override {
        (PuRect { x: cu_x0, y: cu_y0, w: cu_size, h: cu_size }, 0usize, PartMode::TwoNx2N)
    } else {
        (pu, pu_idx, part_mode)
    };

    let temporal = if inter.collocated.is_some() {
        let ref_poc0 = inter.ref_pics_l0.first().map_or(inter.cur_poc, |r| r.poc);
        temporal_candidate(s, eff_pu.x, eff_pu.y, eff_pu.w, eff_pu.h, inter.cur_poc, ref_poc0)?
    } else {
        None
    };
    let ref_pocs = inter.ref_pocs();
    let cands = motion::derive_merge_candidates(&s.cu_grid, eff_pu, eff_idx, eff_mode, inter.log2_parallel_merge_level, max_num_merge_cand, &ref_pocs, temporal);
    cands.get(merge_idx).copied().ok_or(Error::InvalidData("vaco-codec-hevc: merge_idx out of range"))
}

/// Motion-compensate one PU (luma + both chroma planes) directly into a
/// CU-relative `i32` buffer set — shared by the no-residual write path and
/// the transform-tree residual-add path, which differ only in what happens
/// to this buffer afterward.
struct CuPrediction {
    size: i32,
    y: Vec<i32>,
    cb: Vec<i32>,
    cr: Vec<i32>,
}

fn build_cu_prediction(s: &Ctx<'_>, x0: i32, y0: i32, size: i32, pus: &[(PuRect, MotionInfo)]) -> Result<CuPrediction> {
    let inter = s.inter()?;
    let ctb_size = 1i32 << s.log2_ctb_size;
    let csize = (size >> 1).max(1);
    let mut pred = CuPrediction { size, y: vec![0i32; (size * size) as usize], cb: vec![0i32; (csize * csize) as usize], cr: vec![0i32; (csize * csize) as usize] };

    for (pu, info) in pus {
        let ref_pic = inter.plane_for_poc(info.ref_poc).ok_or(Error::InvalidData("vaco-codec-hevc: merge/AMVP candidate names an unknown reference POC"))?;
        let clipped = motion::clip_mv(info.mv, x0, y0, s.pic_width, s.pic_height, ctb_size);

        let int_x = pu.x + (clipped.x >> 2);
        let int_y = pu.y + (clipped.y >> 2);
        let frac_x = clipped.x & 3;
        let frac_y = clipped.y & 3;
        let (w, h) = (usize::try_from(pu.w).unwrap_or(0), usize::try_from(pu.h).unwrap_or(0));
        let mut buf = vec![0i32; w * h];
        crate::mc::predict_block(&ref_pic.y, int_x, int_y, frac_x, frac_y, w, h, s.bit_depth_luma, true, &mut buf);
        blit(&mut pred.y, usize::try_from(size).unwrap_or(1), usize::try_from(pu.x - x0).unwrap_or(0), usize::try_from(pu.y - y0).unwrap_or(0), w, h, &buf);

        // Chroma (4:2:0): half-resolution PU rectangle, the same raw `mv`
        // interpreted at eighth-sample precision (shift 3, mask 7) — see
        // `mc.rs`'s own doc for why both components share one raw `mv`.
        let (cx0, cy0, cw, ch) = (pu.x >> 1, pu.y >> 1, (pu.w >> 1).max(1), (pu.h >> 1).max(1));
        let cint_x = cx0 + (clipped.x >> 3);
        let cint_y = cy0 + (clipped.y >> 3);
        let cfrac_x = clipped.x & 7;
        let cfrac_y = clipped.y & 7;
        let (cw_u, ch_u) = (usize::try_from(cw).unwrap_or(0), usize::try_from(ch).unwrap_or(0));
        let mut cb_buf = vec![0i32; cw_u * ch_u];
        crate::mc::predict_block(&ref_pic.cb, cint_x, cint_y, cfrac_x, cfrac_y, cw_u, ch_u, s.bit_depth_chroma, false, &mut cb_buf);
        blit(&mut pred.cb, usize::try_from(csize).unwrap_or(1), usize::try_from(cx0 - (x0 >> 1)).unwrap_or(0), usize::try_from(cy0 - (y0 >> 1)).unwrap_or(0), cw_u, ch_u, &cb_buf);
        let mut cr_buf = vec![0i32; cw_u * ch_u];
        crate::mc::predict_block(&ref_pic.cr, cint_x, cint_y, cfrac_x, cfrac_y, cw_u, ch_u, s.bit_depth_chroma, false, &mut cr_buf);
        blit(&mut pred.cr, usize::try_from(csize).unwrap_or(1), usize::try_from(cx0 - (x0 >> 1)).unwrap_or(0), usize::try_from(cy0 - (y0 >> 1)).unwrap_or(0), cw_u, ch_u, &cr_buf);
    }
    Ok(pred)
}

fn blit(dst: &mut [i32], dst_stride: usize, x0: usize, y0: usize, w: usize, h: usize, src: &[i32]) {
    for row in 0..h {
        for col in 0..w {
            if let Some(slot) = dst.get_mut((y0 + row) * dst_stride + x0 + col) {
                *slot = src.get(row * w + col).copied().unwrap_or(0);
            }
        }
    }
}

/// A non-skip merged `PART_2Nx2N` CU with `rqt_root_cbf == 0` (inferred, per
/// §7.3.8.5's own presence condition — never actually parsed) writes its MC
/// prediction straight to the picture, unmodified.
fn write_inter_cu_no_residual(s: &mut Ctx<'_>, x0: i32, y0: i32, size: i32, pus: &[(PuRect, MotionInfo)]) -> Result<()> {
    let pred = build_cu_prediction(s, x0, y0, size, pus)?;
    write_pred_block(&mut s.pic.y, x0, y0, pred.size, pred.size, &pred.y);
    let csize = (size >> 1).max(1);
    write_pred_block(&mut s.pic.cb, x0 >> 1, y0 >> 1, csize, csize, &pred.cb);
    write_pred_block(&mut s.pic.cr, x0 >> 1, y0 >> 1, csize, csize, &pred.cr);
    Ok(())
}

fn write_pred_block(plane: &mut crate::framebuf::Plane, x0: i32, y0: i32, w: i32, h: i32, src: &[i32]) {
    let (wu, hu) = (usize::try_from(w).unwrap_or(0), usize::try_from(h).unwrap_or(0));
    for row in 0..hu {
        for col in 0..wu {
            let v = src.get(row * wu + col).copied().unwrap_or(0).clamp(0, 255);
            let (Ok(px), Ok(py)) = (usize::try_from(x0 + i32::try_from(col).unwrap_or(0)), usize::try_from(y0 + i32::try_from(row).unwrap_or(0))) else { continue };
            plane.set(px, py, u16::try_from(v).unwrap_or(0));
        }
    }
}

/// A non-skip, non-intra coding unit: `part_mode`, then `prediction_unit()`
/// per PU (merge, or `ref_idx_l0`/`mvd_coding`/`mvp_l0_flag`), then
/// `rqt_root_cbf` and either a residual-free write or the transform tree.
fn decode_inter_cu(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, depth: u32) -> Result<()> {
    let size = 1i32 << log2_size;
    let at_min_cb = log2_size == s.log2_min_cb_size;
    let amp_enabled = s.inter()?.amp_enabled;
    let part_mode = parse_part_mode_inter(cabac, ctx, at_min_cb, size, amp_enabled)?;
    let num_pus = part_mode.num_pus();

    let mut pu_motion: Vec<(PuRect, MotionInfo)> = Vec::new();
    let mut all_merged = true;
    let depth_u8 = u8::try_from(depth).unwrap_or(u8::MAX);

    for pu_idx in 0..num_pus {
        let pu = part_mode.pu_rect(x0, y0, size, pu_idx);
        let cm = ctx.merge_flag.first_mut().ok_or(Error::InvalidData("merge_flag ctx"))?;
        let merge_flag = cabac.decode_decision(cm) != 0;

        let info = if merge_flag {
            let max_num_merge_cand = s.inter()?.max_num_merge_cand;
            let merge_idx = parse_merge_index(cabac, ctx, max_num_merge_cand)?;
            resolve_merge_candidate(s, x0, y0, size, pu, pu_idx, part_mode, merge_idx, max_num_merge_cand)?
        } else {
            all_merged = false;
            let num_ref_idx_l0 = s.inter()?.ref_pics_l0.len();
            let ref_idx = parse_ref_idx(cabac, ctx, num_ref_idx_l0)?;
            let mvd = parse_mvd(cabac, ctx)?;
            let mvp_idx = parse_mvp_idx(cabac, ctx)?;
            let (cur_poc, target_ref_poc, log2_pml, has_collocated) = {
                let inter = s.inter()?;
                let target_ref_poc = inter.ref_pics_l0.get(ref_idx).map(|r| r.poc).ok_or(Error::InvalidData("vaco-codec-hevc: ref_idx_l0 out of range"))?;
                (inter.cur_poc, target_ref_poc, inter.log2_parallel_merge_level, inter.collocated.is_some())
            };
            let temporal = if has_collocated { temporal_candidate(s, pu.x, pu.y, pu.w, pu.h, cur_poc, target_ref_poc)? } else { None };
            let cands = motion::derive_amvp_candidates(&s.cu_grid, pu, log2_pml, cur_poc, target_ref_poc, temporal);
            let predictor = cands.get(mvp_idx).copied().unwrap_or(Mv::ZERO);
            MotionInfo { mv: Mv { x: predictor.x + mvd.x, y: predictor.y + mvd.y }, ref_poc: target_ref_poc }
        };

        let blocks_w = usize::try_from((pu.w >> 2).max(1)).unwrap_or(1);
        let blocks_h = usize::try_from((pu.h >> 2).max(1)).unwrap_or(1);
        let bx0 = usize::try_from(pu.x >> 2).unwrap_or(0);
        let by0 = usize::try_from(pu.y >> 2).unwrap_or(0);
        // Both calls happen *inside* the PU loop, before the next PU (of
        // this same CU) is decoded: a later PU's own merge/AMVP spatial
        // search can name an earlier PU of the same CU as a neighbour
        // (§8.5.3.2.3's A1/B1 for a 2-PU split), and `CuGrid::inter_at`
        // gates on `written`, which only `fill` (not `fill_motion` alone)
        // sets.
        s.cu_grid.fill(bx0, by0, blocks_w, blocks_h, depth_u8, DC_IDX);
        s.cu_grid.fill_motion(bx0, by0, blocks_w, blocks_h, info.mv, info.ref_poc, false);
        pu_motion.push((pu, info));
    }

    // §7.3.8.5's own presence condition: `rqt_root_cbf` is inferred `1`
    // (transform tree always runs) exactly when the CU is a single merged
    // `PART_2Nx2N` PU; otherwise it is genuinely parsed.
    let rqt_root_cbf = if part_mode == PartMode::TwoNx2N && all_merged {
        true
    } else {
        let cm = ctx.qt_root_cbf.first_mut().ok_or(Error::InvalidData("qt_root_cbf ctx"))?;
        cabac.decode_decision(cm) != 0
    };

    if rqt_root_cbf {
        let pred = build_cu_prediction(s, x0, y0, size, &pu_motion)?;
        let max_depth = s.max_transform_hierarchy_depth_inter;
        let inter_split_flag = u32::from(max_depth == 1 && part_mode != PartMode::TwoNx2N);
        let quadtree_tu_log2_min = quadtree_tu_log2_min_in_cu(s, log2_size, max_depth, inter_split_flag);
        transform_tree_inter(cabac, ctx, s, x0, y0, log2_size, 0, inter_split_flag != 0, &pred, quadtree_tu_log2_min, true, true)?;
    } else {
        write_inter_cu_no_residual(s, x0, y0, size, &pu_motion)?;
    }

    finalize_cu_qp(s, x0, y0, size);
    Ok(())
}

// ---------------------------------------------------------------------
// The inter transform tree: structurally identical to the intra one
// (`transform_tree`/`transform_unit`) — same split/cbf syntax, same
// shared-4x4-chroma-leaf rule — but its leaves add residual to a
// pre-computed motion-compensated [`CuPrediction`] instead of calling
// [`intra_pred`], and it carries no PU/mode bookkeeping at all. Kept as a
// separate function rather than a generic parameter on the intra one: the
// two "leaf" bodies differ enough (no MPM, no chroma-derived-mode, a
// different prediction source entirely) that threading a callback through
// eight levels of recursion would obscure the control flow more than the
// duplication does.

#[allow(clippy::too_many_arguments)]
fn transform_tree_inter(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    trafo_depth: u32,
    force_split_at_root: bool,
    pred: &CuPrediction,
    quadtree_tu_log2_min: u32,
    parent_cbf_cb: bool,
    parent_cbf_cr: bool,
) -> Result<()> {
    let split = if (force_split_at_root && trafo_depth == 0) || log2_size > s.log2_max_tb_size {
        true
    } else if log2_size == s.log2_min_tb_size || log2_size == quadtree_tu_log2_min {
        false
    } else {
        let ctx_idx = usize::try_from(5u32.saturating_sub(log2_size)).unwrap_or(0);
        let cm = ctx.trans_subdiv_flag.get_mut(ctx_idx).ok_or(Error::InvalidData("trans_subdiv ctx"))?;
        cabac.decode_decision(cm) != 0
    };

    let chroma_splittable = log2_size > 2;
    let cbf_cb = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cb {
            let cm = ctx.qt_cbf.get_mut(5 + trafo_depth.min(4) as usize).ok_or(Error::InvalidData("cbf_cb ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cb
    };
    let cbf_cr = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cr {
            let cm = ctx.qt_cbf.get_mut(5 + trafo_depth.min(4) as usize).ok_or(Error::InvalidData("cbf_cr ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cr
    };

    if split {
        let half = 1i32 << (log2_size - 1);
        for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
            transform_tree_inter(cabac, ctx, s, x0 + dx, y0 + dy, log2_size - 1, trafo_depth + 1, force_split_at_root, pred, quadtree_tu_log2_min, cbf_cb, cbf_cr)?;
        }
        return Ok(());
    }

    let luma_ctx_idx = usize::from(trafo_depth == 0);
    let cm = ctx.qt_cbf.get_mut(luma_ctx_idx).ok_or(Error::InvalidData("cbf_luma ctx"))?;
    let cbf_luma = cabac.decode_decision(cm) != 0;

    transform_unit_inter(cabac, ctx, s, x0, y0, log2_size, cbf_luma, cbf_cb, cbf_cr, pred)
}

#[allow(clippy::too_many_arguments)]
fn transform_unit_inter(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, cbf_luma: bool, cbf_cb: bool, cbf_cr: bool, pred: &CuPrediction) -> Result<()> {
    let grid = 1i32 << s.log2_min_cb_size;
    let size = 1i32 << log2_size;
    s.edges.mark_vert(x0, y0, size, grid);
    s.edges.mark_horiz(x0, y0, size, grid);

    maybe_parse_cu_qp_delta(cabac, ctx, s, cbf_luma || cbf_cb || cbf_cr)?;

    reconstruct_luma_inter(cabac, ctx, s, x0, y0, log2_size, cbf_luma, pred)?;

    // The chroma-leaf rule (§7.3.8.8) does not depend on `CuPredMode` — an
    // inter CU's 4x4-luma leaves share their chroma the same way an intra
    // CU's do (see `transform_unit`'s own doc). `blk_idx` there exists to
    // recover which of the 4 luma siblings this leaf is; this crate's own
    // TU coordinates already carry that (the low 2 bits of `x0`/`y0` in
    // 4-sample units), so it is recovered directly rather than threaded
    // through as its own parameter.
    let luma_size_at_min = log2_size == 2;
    let blk_idx_is_3 = luma_size_at_min && (x0 & 4) != 0 && (y0 & 4) != 0;
    let chroma_leaf = log2_size > 2 || blk_idx_is_3;
    if chroma_leaf {
        let (cx0, cy0, clog2) = if log2_size > 2 { (x0 >> 1, y0 >> 1, log2_size - 1) } else { ((x0 - 4).max(0) >> 1, (y0 - 4).max(0) >> 1, 2u32) };
        if cbf_cb {
            reconstruct_chroma_inter(cabac, ctx, s, cx0, cy0, clog2, true, pred)?;
        } else {
            write_pred_chroma_only(s, cx0, cy0, clog2, true, pred);
        }
        if cbf_cr {
            reconstruct_chroma_inter(cabac, ctx, s, cx0, cy0, clog2, false, pred)?;
        } else {
            write_pred_chroma_only(s, cx0, cy0, clog2, false, pred);
        }
    }
    Ok(())
}

/// A slice of [`CuPrediction`]'s luma buffer at `(x0, y0)` (CU-relative
/// already recovered from picture coordinates via `cu_x0`/`cu_y0`), copied
/// into a plain `size x size` array the way `intra_pred::predict` hands its
/// own output back — kept as a real copy rather than a sub-slice view since
/// `transform::add_residual_clip` already expects an owned, densely packed
/// `&mut [u16]`-shaped buffer (mirrored here as `u16` post-clip, matching
/// `reconstruct_luma`'s own `pred: Vec<u16>`).
fn pred_slice(buf: &[i32], buf_size: i32, x0: i32, y0: i32, size: usize) -> Vec<u16> {
    let stride = usize::try_from(buf_size).unwrap_or(1);
    let mut out = vec![0u16; size * size];
    for row in 0..size {
        for col in 0..size {
            let (Ok(bx), Ok(by)) = (usize::try_from(x0 + i32::try_from(col).unwrap_or(0)), usize::try_from(y0 + i32::try_from(row).unwrap_or(0))) else { continue };
            let v = buf.get(by * stride + bx).copied().unwrap_or(0).clamp(0, 255);
            if let Some(slot) = out.get_mut(row * size + col) {
                *slot = u16::try_from(v).unwrap_or(0);
            }
        }
    }
    out
}

fn reconstruct_luma_inter(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, x0: i32, y0: i32, log2_size: u32, cbf: bool, pred_cu: &CuPrediction) -> Result<()> {
    let size = 1usize << log2_size;
    // `pred_cu`'s own top-left is the CU's own `(x0, y0)`, which this leaf's
    // `(x0, y0)` sit somewhere inside of — recovered by the caller passing
    // picture coordinates for both, so the CU's own origin has to be
    // subtracted out. `build_cu_prediction`'s buffer is indexed from `0`,
    // i.e. relative to the CU, so every leaf call here must know the CU's
    // own origin — threaded through via `s`'s own per-CU state would need a
    // new field; instead the leaf's absolute `(x0, y0)` combined with
    // `pred_cu.size` and the fact that every leaf lies within one CU is
    // enough: `crate::ctu::cu_relative` recovers the offset from the
    // picture-wide edge-mark grid's own 4-sample alignment, matching how
    // `EdgeMarks` addresses positions already.
    let (cu_x0, cu_y0) = cu_origin_of(x0, y0, pred_cu.size);
    let mut pred = pred_slice(&pred_cu.y, pred_cu.size, x0 - cu_x0, y0 - cu_y0, size);

    if cbf {
        if s.transform_skip_enabled && log2_size == 2 {
            let cm = ctx.transform_skip.first_mut().ok_or(Error::InvalidData("transform_skip ctx"))?;
            if cabac.decode_decision(cm) != 0 {
                return Err(Error::Unsupported("vaco-codec-hevc: transform_skip_flag set (transform-skip residual not implemented)"));
            }
        }
        // §7.4.9.11's mode-dependent scan order is an intra-only rule — an
        // inter TU's `scanIdx` is always `0` (diagonal), HM's own
        // `getCoefScanIdx` returning `SCAN_DIAG` whenever `CuPredMode !=
        // MODE_INTRA`.
        let coeffs = residual::residual_coding(cabac, ctx, log2_size, crate::scan::ScanOrder::Diag, false, s.sign_data_hiding);
        let use_dst = false; // §8.6.4.1: DST-VII only for 4x4 *intra* luma.
        let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
        let dequantised = transform::dequant(&coeffs.values, size, qp_y, s.bit_depth_luma);
        let residual = transform::inverse_transform(&dequantised, size, use_dst, s.bit_depth_luma);
        transform::add_residual_clip(&mut pred, &residual, size, s.bit_depth_luma);
    }
    write_block(&mut s.pic.y, x0, y0, size, &pred);
    Ok(())
}

fn reconstruct_chroma_inter(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank, s: &mut Ctx<'_>, cx0: i32, cy0: i32, log2_size: u32, is_cb: bool, pred_cu: &CuPrediction) -> Result<()> {
    let size = 1usize << log2_size;
    let (cu_x0, cu_y0) = cu_origin_of(cx0 << 1, cy0 << 1, pred_cu.size);
    let (ccu_x0, ccu_y0) = (cu_x0 >> 1, cu_y0 >> 1);
    let src = if is_cb { &pred_cu.cb } else { &pred_cu.cr };
    let csize = (pred_cu.size >> 1).max(1);
    let mut pred = pred_slice(src, csize, cx0 - ccu_x0, cy0 - ccu_y0, size);

    if s.transform_skip_enabled && log2_size == 2 {
        let cm = ctx.transform_skip.get_mut(1).ok_or(Error::InvalidData("transform_skip ctx"))?;
        if cabac.decode_decision(cm) != 0 {
            return Err(Error::Unsupported("vaco-codec-hevc: transform_skip_flag set (transform-skip residual not implemented)"));
        }
    }
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let qp = transform::chroma_qp(qp_y, if is_cb { s.cb_qp_offset } else { s.cr_qp_offset });
    let coeffs = residual::residual_coding(cabac, ctx, log2_size, crate::scan::ScanOrder::Diag, true, s.sign_data_hiding);
    let dequantised = transform::dequant(&coeffs.values, size, qp, s.bit_depth_chroma);
    let residual = transform::inverse_transform(&dequantised, size, false, s.bit_depth_chroma);
    transform::add_residual_clip(&mut pred, &residual, size, s.bit_depth_chroma);

    let plane = if is_cb { &mut s.pic.cb } else { &mut s.pic.cr };
    write_block(plane, cx0, cy0, size, &pred);
    Ok(())
}

fn write_pred_chroma_only(s: &mut Ctx<'_>, cx0: i32, cy0: i32, log2_size: u32, is_cb: bool, pred_cu: &CuPrediction) {
    let size = 1usize << log2_size;
    let (cu_x0, cu_y0) = cu_origin_of(cx0 << 1, cy0 << 1, pred_cu.size);
    let (ccu_x0, ccu_y0) = (cu_x0 >> 1, cu_y0 >> 1);
    let src = if is_cb { &pred_cu.cb } else { &pred_cu.cr };
    let csize = (pred_cu.size >> 1).max(1);
    let pred = pred_slice(src, csize, cx0 - ccu_x0, cy0 - ccu_y0, size);
    let plane = if is_cb { &mut s.pic.cb } else { &mut s.pic.cr };
    write_block(plane, cx0, cy0, size, &pred);
}

/// Recovers a transform leaf's own enclosing CU's top-left, given the
/// leaf's own absolute `(x, y)` and the CU's known `size` — every coding
/// unit's own top-left is aligned to its own size, so this is a plain
/// alignment mask, not a search.
fn cu_origin_of(x: i32, y: i32, cu_size: i32) -> (i32, i32) {
    let mask = !(cu_size - 1);
    (x & mask, y & mask)
}

/// `getQuadtreeTULog2MinSizeInCU`: `max_depth` is `QuadtreeTUMaxDepthIntra`
/// or `QuadtreeTUMaxDepthInter` depending on the CU's own `CuPredMode`
/// (chosen by the caller); `extra_split_flag` is `intraSplitFlag` (intra,
/// `PartMode == PART_NxN`) or `interSplitFlag` (inter,
/// `QuadtreeTUMaxDepthInter == 1 && PartMode != PART_2Nx2N` — confirmed
/// against HM's `TComDataCU::getQuadtreeTULog2MinSizeInCU`, whose own
/// `interSplitFlag` gate is on the *max-depth* value being exactly `1`, not
/// on `max_transform_hierarchy_depth_inter == 0`, a distinction this
/// function's caller must get right since the two differ by one).
fn quadtree_tu_log2_min_in_cu(s: &Ctx<'_>, log2_cb_size: u32, max_depth: u32, extra_split_flag: u32) -> u32 {
    let denom = max_depth.saturating_sub(1) + extra_split_flag;
    if log2_cb_size < s.log2_min_tb_size + denom {
        s.log2_min_tb_size
    } else {
        (log2_cb_size.saturating_sub(denom)).min(s.log2_max_tb_size)
    }
}

#[allow(clippy::too_many_arguments)]
fn transform_tree(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    trafo_depth: u32,
    blk_idx: u32,
    is_nxn: bool,
    pus: &[Pu],
    luma_modes: [u8; 4],
    chroma_mode: u8,
    quadtree_tu_log2_min: u32,
    parent_cbf_cb: bool,
    parent_cbf_cr: bool,
) -> Result<()> {
    let intra_split_and_root = is_nxn && trafo_depth == 0;
    let split = if intra_split_and_root || log2_size > s.log2_max_tb_size {
        true
    } else if log2_size == s.log2_min_tb_size || log2_size == quadtree_tu_log2_min {
        false
    } else {
        let ctx_idx = usize::try_from(5u32.saturating_sub(log2_size)).unwrap_or(0);
        let cm = ctx.trans_subdiv_flag.get_mut(ctx_idx).ok_or(Error::InvalidData("trans_subdiv ctx"))?;
        cabac.decode_decision(cm) != 0
    };

    let chroma_splittable = log2_size > 2;
    let cbf_cb = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cb {
            let cm = ctx.qt_cbf.get_mut(5 + trafo_depth.min(4) as usize).ok_or(Error::InvalidData("cbf_cb ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cb
    };
    let cbf_cr = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cr {
            let cm = ctx.qt_cbf.get_mut(5 + trafo_depth.min(4) as usize).ok_or(Error::InvalidData("cbf_cr ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cr
    };

    if split {
        let half = 1i32 << (log2_size - 1);
        for (i, (dx, dy)) in [(0, 0), (half, 0), (0, half), (half, half)].into_iter().enumerate() {
            transform_tree(
                cabac,
                ctx,
                s,
                x0 + dx,
                y0 + dy,
                log2_size - 1,
                trafo_depth + 1,
                u32::try_from(i).unwrap_or(0),
                is_nxn,
                pus,
                luma_modes,
                chroma_mode,
                quadtree_tu_log2_min,
                cbf_cb,
                cbf_cr,
            )?;
        }
        return Ok(());
    }

    // `getCtxQtCbf` for luma: `ctxInc = (trafoDepth == 0) ? 1 : 0`.
    let luma_ctx_idx = usize::from(trafo_depth == 0);
    let cm = ctx.qt_cbf.get_mut(luma_ctx_idx).ok_or(Error::InvalidData("cbf_luma ctx"))?;
    let cbf_luma = cabac.decode_decision(cm) != 0;

    transform_unit(cabac, ctx, s, x0, y0, log2_size, blk_idx, cbf_luma, cbf_cb, cbf_cr, pus, luma_modes, chroma_mode)
}

#[allow(clippy::too_many_arguments)]
fn transform_unit(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    blk_idx: u32,
    cbf_luma: bool,
    cbf_cb: bool,
    cbf_cr: bool,
    pus: &[Pu],
    luma_modes: [u8; 4],
    chroma_mode: u8,
) -> Result<()> {
    // §8.7.2's deblocking grid never filters below `MinCbSizeY` (see
    // `framebuf::EdgeMarks`'s own module doc) — marking every transform-unit
    // leaf's own top-left corner here, before prediction/reconstruction, is
    // sufficient because `EdgeMarks::mark_vert`/`mark_horiz` themselves
    // reject anything off that grid.
    let grid = 1i32 << s.log2_min_cb_size;
    let size = 1i32 << log2_size;
    s.edges.mark_vert(x0, y0, size, grid);
    s.edges.mark_horiz(x0, y0, size, grid);

    // §7.3.8.11: `cu_qp_delta_abs`/`sign`, if this quantisation group has not
    // already coded one, sit here — before this leaf's own luma
    // `residual_coding()` — gated on *this leaf's* cbf, luma or chroma.
    maybe_parse_cu_qp_delta(cabac, ctx, s, cbf_luma || cbf_cb || cbf_cr)?;

    let luma_mode = pu_mode_at(pus, luma_modes, x0, y0);
    reconstruct_luma(cabac, ctx, s, x0, y0, log2_size, luma_mode, cbf_luma)?;

    let chroma_leaf = log2_size > 2 || blk_idx == 3;
    if chroma_leaf {
        let (cx0, cy0, clog2) = if log2_size > 2 {
            (x0 >> 1, y0 >> 1, log2_size - 1)
        } else {
            // The shared 4x4-luma-leaf case: the chroma block covers the
            // parent 8x8 luma area, whose top-left is this leaf's own
            // parent — recovered here from `blk_idx == 3`'s sibling
            // geometry (`x0, y0` minus one luma-4x4 step in each axis).
            ((x0 - 4).max(0) >> 1, (y0 - 4).max(0) >> 1, 2u32)
        };
        if cbf_cb {
            reconstruct_chroma(cabac, ctx, s, cx0, cy0, clog2, chroma_mode, true)?;
        }
        if cbf_cr {
            reconstruct_chroma(cabac, ctx, s, cx0, cy0, clog2, chroma_mode, false)?;
        }
        if !cbf_cb {
            predict_chroma_only(s, cx0, cy0, clog2, chroma_mode, true);
        }
        if !cbf_cr {
            predict_chroma_only(s, cx0, cy0, clog2, chroma_mode, false);
        }
    }
    Ok(())
}

fn pu_mode_at(pus: &[Pu], luma_modes: [u8; 4], x: i32, y: i32) -> u8 {
    for (pu, &mode) in pus.iter().zip(luma_modes.iter()) {
        if x >= pu.x && x < pu.x + pu.size && y >= pu.y && y < pu.y + pu.size {
            return mode;
        }
    }
    luma_modes.first().copied().unwrap_or(DC_IDX)
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_luma(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    mode: u8,
    cbf: bool,
) -> Result<()> {
    let size = 1usize << log2_size;
    let line = intra_pred::build_reference_line(&s.pic.y, x0, y0, size, s.bit_depth_luma);
    let filtered;
    let ref_line = if intra_pred::should_filter(mode, size, true) {
        filtered = intra_pred::filter_reference_line(&line, size, s.bit_depth_luma, s.strong_intra_smoothing);
        &filtered
    } else {
        &line
    };
    let mut pred = vec![0u16; size * size];
    intra_pred::predict(mode, ref_line, size, s.bit_depth_luma, true, &mut pred);

    if cbf {
        if s.transform_skip_enabled && log2_size == 2 {
            let cm = ctx.transform_skip.first_mut().ok_or(Error::InvalidData("transform_skip ctx"))?;
            if cabac.decode_decision(cm) != 0 {
                return Err(Error::Unsupported("vaco-codec-hevc: transform_skip_flag set (transform-skip residual not implemented)"));
            }
        }
        let order = intra_mode::scan_order_for_mode(mode, log2_size, false);
        let coeffs: Coeffs = residual::residual_coding(cabac, ctx, log2_size, order, false, s.sign_data_hiding);
        let use_dst = log2_size == 2;
        let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
        let dequantised = transform::dequant(&coeffs.values, size, qp_y, s.bit_depth_luma);
        let residual = transform::inverse_transform(&dequantised, size, use_dst, s.bit_depth_luma);
        transform::add_residual_clip(&mut pred, &residual, size, s.bit_depth_luma);
    }

    write_block(&mut s.pic.y, x0, y0, size, &pred);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_>,
    cx0: i32,
    cy0: i32,
    log2_size: u32,
    mode: u8,
    is_cb: bool,
) -> Result<()> {
    let size = 1usize << log2_size;
    let plane = if is_cb { &s.pic.cb } else { &s.pic.cr };
    let line = intra_pred::build_reference_line(plane, cx0, cy0, size, s.bit_depth_chroma);
    let mut pred = vec![0u16; size * size];
    // Chroma never smooths its reference samples at 4:2:0 (see the crate
    // doc), so no `should_filter`/`filter_reference_line` call here.
    intra_pred::predict(mode, &line, size, s.bit_depth_chroma, false, &mut pred);

    if s.transform_skip_enabled && log2_size == 2 {
        let cm = ctx.transform_skip.get_mut(1).ok_or(Error::InvalidData("transform_skip ctx"))?;
        if cabac.decode_decision(cm) != 0 {
            return Err(Error::Unsupported("vaco-codec-hevc: transform_skip_flag set (transform-skip residual not implemented)"));
        }
    }
    let order = intra_mode::scan_order_for_mode(mode, log2_size, true);
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let qp = transform::chroma_qp(qp_y, if is_cb { s.cb_qp_offset } else { s.cr_qp_offset });
    let coeffs = residual::residual_coding(cabac, ctx, log2_size, order, true, s.sign_data_hiding);
    let dequantised = transform::dequant(&coeffs.values, size, qp, s.bit_depth_chroma);
    let residual = transform::inverse_transform(&dequantised, size, false, s.bit_depth_chroma);
    transform::add_residual_clip(&mut pred, &residual, size, s.bit_depth_chroma);

    let plane_mut = if is_cb { &mut s.pic.cb } else { &mut s.pic.cr };
    write_block(plane_mut, cx0, cy0, size, &pred);
    Ok(())
}

fn predict_chroma_only(s: &mut Ctx<'_>, cx0: i32, cy0: i32, log2_size: u32, mode: u8, is_cb: bool) {
    let size = 1usize << log2_size;
    let plane = if is_cb { &s.pic.cb } else { &s.pic.cr };
    let line = intra_pred::build_reference_line(plane, cx0, cy0, size, s.bit_depth_chroma);
    let mut pred = vec![0u16; size * size];
    intra_pred::predict(mode, &line, size, s.bit_depth_chroma, false, &mut pred);
    let plane_mut = if is_cb { &mut s.pic.cb } else { &mut s.pic.cr };
    write_block(plane_mut, cx0, cy0, size, &pred);
}

fn write_block(plane: &mut crate::framebuf::Plane, x0: i32, y0: i32, size: usize, block: &[u16]) {
    for y in 0..size {
        for x in 0..size {
            let v = block.get(y * size + x).copied().unwrap_or(0);
            let x_i = i32::try_from(x).unwrap_or(i32::MAX);
            let y_i = i32::try_from(y).unwrap_or(i32::MAX);
            let (Ok(px), Ok(py)) = (usize::try_from(x0 + x_i), usize::try_from(y0 + y_i)) else { continue };
            plane.set(px, py, v);
        }
    }
}
