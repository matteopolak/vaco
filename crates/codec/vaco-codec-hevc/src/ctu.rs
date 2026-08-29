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

use vaco_codec_cabac::CabacDecoder;
use vaco_core::{Error, Result};
use vaco_parse_hevc::{Pps, Sps};

use vaco_limits::Budget;

use crate::cabac_ctx::ContextBank;
use crate::framebuf::{CuGrid, EdgeMarks, Picture};
use crate::intra_mode::{self, DC_IDX, DM_CHROMA_IDX};
use crate::intra_pred;
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
    ) -> Result<Self> {
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
        })
    }
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

fn coding_unit(
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
        cabac.decode_decision(cm) == 0
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
    let quadtree_tu_log2_min = quadtree_tu_log2_min_in_cu(s, log2_size, intra_split_depth_extra);

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
    )
}

/// `getQuadtreeTULog2MinSizeInCU`, simplified for intra with
/// `interSplitFlag == 0` always.
fn quadtree_tu_log2_min_in_cu(s: &Ctx<'_>, log2_cb_size: u32, intra_split_flag: u32) -> u32 {
    let max_depth = s.max_transform_hierarchy_depth_intra;
    let denom = max_depth.saturating_sub(1) + intra_split_flag;
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
        let dequantised = transform::dequant(&coeffs.values, size, s.slice_qp, s.bit_depth_luma);
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
    let qp = transform::chroma_qp(s.slice_qp, if is_cb { s.cb_qp_offset } else { s.cr_qp_offset });
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
