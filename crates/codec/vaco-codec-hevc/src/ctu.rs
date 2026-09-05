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

use vaco_bitstream::BitReader;
use vaco_codec_cabac::{CabacDecoder, ContextModel};
use vaco_core::{Error, Result};
use vaco_parse_hevc::sps::PcmParameters;
use vaco_parse_hevc::{Pps, Sps};

use crate::cabac_ctx::ContextBank;
use crate::framebuf::{CuGrid, EdgeMarks, Picture, Plane, ReconPicture};
use crate::intra_mode::{self, DC_IDX, DM_CHROMA_IDX};
use crate::intra_pred;
use crate::motion::{self, MotionInfo, Mv, PartMode, PuRect, RefList, UniMotion};
use crate::residual::{self, Coeffs};
use crate::sao;
use crate::tile::TileLayout;
use crate::transform;
use crate::weight::RefWeights;

/// Stage 2b step 3b (`docs/codec/hevc-wavefront-threading.md`): everything
/// in `Ctx` that is constant for the whole slice and safe to share
/// read-only across every row worker once real dispatch exists — every
/// SPS/PPS/slice-header-derived scalar and flag, and `inter` (reference
/// lists and merge/AMVP/TMVP slice-level parameters, never written after
/// construction). Now actually `Arc`-shared (step 3b's own follow-up,
/// `docs/codec/hevc-wavefront-threading.md`'s "don't share the writer"
/// resolution): every field here is read-only after [`Ctx::new`]/
/// [`Ctx::retarget_pic_for_test`] construct it, so `Arc<CtxShared<'p>>`
/// needs no lock at all — cloning the `Arc` is the whole cost of handing a
/// row worker its own reference.
///
/// `pic` deliberately does **not** live here, even though it is equally
/// constant-for-the-slice in the sense of "never reassigned": deblocking
/// and SAO mutate the `Picture` it points at (`&mut s.pic.y`, throughout
/// `deblock.rs`/`sao.rs`), and `Arc<T>` can only ever hand back `&T` short
/// of `Arc::get_mut`'s own "only if nothing else holds a clone right now"
/// condition — a runtime invariant this crate would rather not depend on
/// when a static one (never put a `&mut` behind an `Arc` in the first
/// place) is available for free. `pic` stays a direct field of [`Ctx`]
/// itself instead, exactly where the still-serial deblock/SAO pass that is
/// its only real reader already finds it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SPS/PPS/slice-header flag this walk needs, not a state machine in disguise"
)]
pub(crate) struct CtxShared<'p> {
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
    /// §7.4.3.3.1/§7.4.5's effective matrices for this active SPS/PPS pair,
    /// including PPS-over-SPS precedence, copy references and defaults.
    pub scaling_matrices: crate::transform::ScalingMatrices,
    /// `transquant_bypass_enabled_flag`, which makes every coding unit carry
    /// `cu_transquant_bypass_flag` as its first CABAC syntax element.
    pub transquant_bypass_enabled: bool,
    /// `pcm_enabled_flag` and its SPS parameters. Protected I_PCM CUs share
    /// [`CuGrid`]'s filter-bypass mask with transquant-bypass CUs so deblocking
    /// and SAO preserve their samples without changing intra-neighbour semantics.
    pub pcm: Option<PcmParameters>,
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    /// `constrained_intra_pred_flag` — §8.4.4.2.2's reference-sample
    /// availability gate: when set, a neighbouring sample belonging to an
    /// inter-coded prediction block is treated as unavailable for intra
    /// prediction even though it is otherwise in-picture/in-slice, so
    /// [`reconstruct_luma`]/[`reconstruct_chroma`]/[`predict_chroma_only`]
    /// consult [`CuGrid::inter_at`] before trusting a neighbour, on top of
    /// [`crate::framebuf::Plane::is_ready`]'s ordinary check.
    constrained_intra_pred: bool,
    /// `cu_qp_delta_enabled_flag`.
    cu_qp_delta_enabled: bool,
    /// `Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth`.
    log2_min_cu_qp_delta_size: u32,
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
    /// Validated tile geometry retained for the post-reconstruction loop
    /// filter. It is distinct from [`Ctx::tile_ctb_rect`], whose value is
    /// deliberately the one currently decoded tile substream.
    pub tile_layout: Option<TileLayout>,
    /// Whether this slice has any inter path at all (P or B) — every
    /// inter-only field below is `Some` exactly when this is `true`. The
    /// name predates B-slice support; [`InterSliceParams::is_b`] is what
    /// actually distinguishes a P slice from a B slice once this is `true`.
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

/// Everything one slice segment's CTU walk needs, held together so the
/// recursive functions below stay free functions taking `&mut Ctx` rather
/// than a method on a growing `impl` block.
///
/// `shared` ([`CtxShared`]) holds the current segment's syntax state plus
/// picture-wide geometry and reference state;
/// what remains here directly is either genuinely per-row-exclusive
/// (`qp_y_prev`/`qg_qp_pred`/`is_cu_qp_delta_coded`/`cu_qp_delta_val`, reset
/// at row or quantisation-group granularity, never read across a row
/// boundary) or one of the four structures Stage 2b step 3a
/// (`recon`/`cu_grid`/`edges`/`sao_params`) already split internally into
/// its own `current`/shared-board halves — see `framebuf.rs`'s own "Stage
/// 1" section doc and each type's own doc for that split. Pulling *those*
/// two halves apart at the `Ctx` level too (so `Ctx` itself cleanly
/// separates into an `Arc`-able shared struct and a per-row-exclusive one)
/// is step 4's own work, once real dispatch needs it — not attempted here,
/// per this document's own repeated preference for deferring a design
/// decision until the thing that needs it exists, rather than guessing its
/// shape ahead of time.
pub(crate) struct Ctx<'p, 'c, 's, 'r> {
    pub shared: &'s mut CtxShared<'p>,
    /// The finished-picture buffer deblocking/SAO/emission (and every
    /// future picture's own reference reads) use — mutated in place by
    /// both, so it stays a direct field here rather than inside `shared`;
    /// see that type's own doc for why. Set once at construction, like
    /// every field of `Ctx`, but never behind the `Arc`.
    pub pic: &'c mut Picture,
    /// The CTU walk's own in-progress reconstruction buffer — see
    /// `crate::framebuf`'s "Stage 1" section doc for why this is a
    /// separate type from `pic` (which stays the finished-picture shape
    /// deblocking/SAO/emission already know).
    pub recon: &'r mut ReconPicture<'r>,
    pub cu_grid: CuGrid<'p>,
    /// §8.6.1's `qPY_PREV`: the last coding unit's finalised `QpY` in
    /// decoding order, or `SliceQpY` at the very start of the slice and (via
    /// `decoder::decode_wpp_rows`'s own reset) at the start of each CTB row
    /// when WPP is active — `pub` because that reset happens in
    /// `decoder.rs`, a different module.
    pub qp_y_prev: i32,
    /// Raster CTB range of the independent slice segment currently being
    /// decoded.  Syntax neighbours and intra reference samples outside this
    /// range are unavailable even when an earlier segment has reconstructed
    /// them already (H.265 §6.4.1).
    slice_start_ctb: u32,
    slice_end_ctb: u32,
    /// Optional tile-local CTB rectangle. When present, a syntax neighbour
    /// must be both in the slice segment and in this tile.
    tile_ctb_rect: Option<(u32, u32, u32, u32)>,
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
    /// The current coding unit's `cu_transquant_bypass_flag`. Set before the
    /// CU's prediction syntax and retained through all of its transform leaves.
    cu_transquant_bypass: bool,
    /// Per-4x4-block transform/CU boundary flags, populated as
    /// [`transform_unit`] reconstructs each luma leaf — the input
    /// [`crate::deblock`]'s post-picture filtering pass reads.
    pub edges: EdgeMarks<'p>,
    /// Every CTU's resolved SAO parameters so far, indexed by raster
    /// address — filled in by [`decode_ctu`] as each CTU's `sao()` is
    /// parsed, read back by a merge at a later address and by
    /// [`crate::sao::filter_picture`] once the whole picture is decoded.
    /// Row-banded (PERF-PROGRAMME.md item B4, Stage 1 step 3's third
    /// piece) — see [`crate::sao::SaoParamsGrid`]'s own doc.
    pub sao_params: crate::sao::SaoParamsGrid<'p>,
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

/// Everything a P- or B-slice CTU walk needs that an I-slice one does not —
/// bundled so [`Ctx::new`] does not grow an eleventh, twelfth, ... plain
/// argument for each one.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent slice-header/PPS flag this walk needs, not a state machine in disguise — same rationale as Ctx's own allow"
)]
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
    /// `RefPicList1`, resolved to real pictures — always empty for a P
    /// slice (`is_b == false`).
    pub ref_pics_l1: Vec<RefPic<'p>>,
    /// Whether this is a B slice (`is_p_slice`'s own name predates B-slice
    /// support and is kept for the intra/inter split alone — this field is
    /// the one that actually distinguishes P from B once `inter` is
    /// `Some`).
    pub is_b: bool,
    /// `collocated_from_l0_flag`; only meaningful when `is_b` — see
    /// [`col_mvp`]'s own doc for how it, together with
    /// [`InterSliceParams::is_low_delay`], picks which of the collocated
    /// PU's own two lists a temporal candidate reads.
    pub collocated_from_l0: bool,
    /// `NoBackwardPredFlag` (§8.5.3.2.9): whether *every* picture in both
    /// `RefPicList0` and `RefPicList1` has a POC no greater than the current
    /// picture's — resolved once per slice in `decoder.rs` rather than
    /// re-scanning both lists on every temporal-candidate query.
    pub is_low_delay: bool,
    /// `mvd_l1_zero_flag` — when set, a bi-predictive (`PRED_BI`) PU's own
    /// `mvd_coding(x0, y0, 1)` is skipped entirely and `MvdL1` is inferred
    /// `(0, 0)` (§7.3.8.6's own presence condition).
    pub mvd_l1_zero: bool,
    /// The collocated picture's own compressed motion field for TMVP
    /// (§8.5.3.2.8/.9), or `None` when `slice_temporal_mvp_enabled_flag` is
    /// clear, the collocated picture has none recorded (an I picture), or
    /// the named list/index does not resolve — see `crate::dpb`'s own
    /// module doc for how this is built.
    pub collocated: Option<crate::dpb::CollocatedMotionField>,
    /// `RefPicList0`'s own resolved weight/offset table (§8.5.3.3.4.3),
    /// indexed the same way `ref_pics_l0` is — `Some` exactly when
    /// `weightedPredFlag` (`weighted_pred_flag && P`, or `weighted_bipred_flag
    /// && B`, §8.5.3.3.4.1) is set, `None` (the default, unweighted
    /// §8.5.3.3.4.2 path) otherwise. See [`crate::weight`]'s own module doc.
    pub weights_l0: Option<Vec<crate::weight::RefWeights>>,
    /// `RefPicList1`'s own resolved weight/offset table, indexed like
    /// `ref_pics_l1` — `Some`/`None` together with `weights_l0` (one
    /// slice-wide `weightedPredFlag` gates both), always `None` for a P
    /// slice.
    pub weights_l1: Option<Vec<crate::weight::RefWeights>>,
}

impl<'p> InterSliceParams<'p> {
    /// `RefPicList0`'s own POCs alone — [`crate::motion::derive_merge_candidates`]'s
    /// zero-candidate fill only needs the list, not the pictures.
    fn ref_pocs_l0(&self) -> Vec<i64> {
        self.ref_pics_l0.iter().map(|r| r.poc).collect()
    }

    /// [`InterSliceParams::ref_pocs_l0`]'s `RefPicList1` counterpart —
    /// always empty for a P slice.
    fn ref_pocs_l1(&self) -> Vec<i64> {
        self.ref_pics_l1.iter().map(|r| r.poc).collect()
    }

    /// A reference picture's own reconstructed planes, by POC, searched in
    /// `target_list` first and the other list second — the same POC can
    /// legitimately appear in both lists (a B slice referencing the same
    /// picture from either direction), and either occurrence names the same
    /// pixels, so which list resolves it first has no effect on the answer.
    fn plane_for_poc(&self, target_list: motion::RefList, poc: i64) -> Option<&'p Picture> {
        let (own, other) = match target_list {
            motion::RefList::L0 => (&self.ref_pics_l0, &self.ref_pics_l1),
            motion::RefList::L1 => (&self.ref_pics_l1, &self.ref_pics_l0),
        };
        own.iter()
            .chain(other.iter())
            .find(|r| r.poc == poc)
            .map(|r| r.pic)
    }

    /// The `RefPicListX` index a `poc` resolves to on `list`, for
    /// [`InterSliceParams::weights_l0`]/[`InterSliceParams::weights_l1`] to
    /// be indexed by — the position `pred_weight_table()`'s own
    /// `LumaWeightLX[refIdxLX]`/`ChromaWeightLX[refIdxLX]` are actually
    /// addressed by.
    ///
    /// [`MotionInfo`] carries a resolved POC rather than a `ref_idx` (see
    /// that type's own doc for why: within one slice `RefPicListX` is
    /// shared and fixed, so the two normally carry the same information).
    /// That equivalence has exactly one gap: §8.3.4's `RefPicListTempX`
    /// cycling can place the *same* POC at more than one list position when
    /// fewer distinct reference pictures exist than
    /// `num_ref_idx_lX_active_minus1 + 1` requests. This resolves to the
    /// *first* matching position on `list` alone (never crossing to the
    /// other list, unlike [`InterSliceParams::plane_for_poc`] — a weight
    /// table's own `l0`/`l1` halves are genuinely distinct, not
    /// interchangeable the way two lists' picture pointers can be) — exact
    /// whenever a POC appears once in that list, and a known, narrow
    /// approximation (picking one of several equally valid list positions,
    /// all naming the same picture) in the cycling case, which a real
    /// weighted-prediction fixture has never exercised.
    fn weights_for(&self, list: motion::RefList, poc: i64) -> Option<crate::weight::RefWeights> {
        let (refs, weights) = match list {
            motion::RefList::L0 => (&self.ref_pics_l0, &self.weights_l0),
            motion::RefList::L1 => (&self.ref_pics_l1, &self.weights_l1),
        };
        let idx = refs.iter().position(|r| r.poc == poc)?;
        weights.as_ref()?.get(idx).copied()
    }
}

impl<'p> CtxShared<'p> {
    /// Re-derives the handful of walk-specific limits from an SPS/PPS pair
    /// the caller ([`crate::decoder`]) has already checked are within this
    /// crate's stated scope. Built by the caller as its own local, living
    /// in the same stack frame as the `EdgeMarksShared`/`CuGridShared`/
    /// `SaoParamsGridShared`/`ReconPictureShared` values [`Ctx::new`]'s own
    /// `edges`/`cu_grid`/`sao_params`/`recon` borrow from — see this
    /// module's own "don't share the writer" resolution for why every one
    /// of these `*Shared` types is now a plain borrow rather than an `Arc`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call site (decoder.rs), grouping into a sub-struct would not aid clarity"
    )]
    pub(crate) fn new(
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
        let log2_ctb_size =
            u32::from(sps.log2_min_cb_size) + u32::from(sps.log2_diff_max_min_cb_size);
        let width = usize::try_from(sps.pic_width_in_luma_samples).unwrap_or(0);
        let ctb_size = 1u32 << log2_ctb_size;
        let ctbs_x = u32::try_from(width).unwrap_or(0).div_ceil(ctb_size).max(1);
        let ctbs_y = sps.pic_height_in_luma_samples.div_ceil(ctb_size).max(1);
        let tile_layout = pps
            .tiles
            .as_ref()
            .map(|tiles| TileLayout::from_pps(tiles, ctbs_x, ctbs_y))
            .transpose()?;
        Ok(Self {
            pic_width: i32::try_from(sps.pic_width_in_luma_samples).unwrap_or(0),
            pic_height: i32::try_from(sps.pic_height_in_luma_samples).unwrap_or(0),
            log2_ctb_size,
            log2_min_cb_size: u32::from(sps.log2_min_cb_size),
            log2_min_tb_size: u32::from(sps.log2_min_tb_size),
            log2_max_tb_size: u32::from(sps.log2_min_tb_size)
                + u32::from(sps.log2_diff_max_min_tb_size),
            max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
            slice_qp,
            sign_data_hiding: pps.sign_data_hiding_enabled,
            strong_intra_smoothing: sps.strong_intra_smoothing_enabled,
            transform_skip_enabled: pps.transform_skip_enabled,
            scaling_matrices: crate::transform::ScalingMatrices::from_parameter_sets(sps, pps)?,
            transquant_bypass_enabled: pps.transquant_bypass_enabled,
            pcm: sps.pcm,
            bit_depth_luma: u32::from(sps.bit_depth_luma),
            bit_depth_chroma: u32::from(sps.bit_depth_chroma),
            cb_qp_offset: pps.cb_qp_offset,
            cr_qp_offset: pps.cr_qp_offset,
            constrained_intra_pred: pps.constrained_intra_pred,
            cu_qp_delta_enabled: pps.cu_qp_delta_enabled,
            log2_min_cu_qp_delta_size: log2_ctb_size.saturating_sub(pps.diff_cu_qp_delta_depth),
            deblocking_disabled,
            beta_offset_div2,
            tc_offset_div2,
            sao_luma,
            sao_chroma,
            ctbs_x,
            tile_layout,
            is_p_slice,
            inter,
            max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        })
    }

    /// Apply the syntax state that is allowed to differ at an independent
    /// segment boundary (§7.4.7.1). The picture boards stay shared, while
    /// prediction mode, QP, filtering and motion-list state are segment-local.
    pub(crate) fn set_slice_segment(
        &mut self,
        slice_qp: i32,
        cb_qp_offset: i32,
        cr_qp_offset: i32,
        deblocking_disabled: bool,
        beta_offset_div2: i32,
        tc_offset_div2: i32,
        sao_luma: bool,
        sao_chroma: bool,
        inter: Option<InterSliceParams<'p>>,
    ) {
        self.slice_qp = slice_qp;
        self.cb_qp_offset = cb_qp_offset;
        self.cr_qp_offset = cr_qp_offset;
        self.deblocking_disabled = deblocking_disabled;
        self.beta_offset_div2 = beta_offset_div2;
        self.tc_offset_div2 = tc_offset_div2;
        self.sao_luma = sao_luma;
        self.sao_chroma = sao_chroma;
        self.is_p_slice = inter.is_some();
        self.inter = inter;
    }
}

impl<'p, 'c, 's, 'r> Ctx<'p, 'c, 's, 'r> {
    /// Assembles the walk's own per-row-exclusive state (`pic`, `recon`,
    /// `cu_grid`, `edges`, `sao_params`, all already constructed by the
    /// caller against their own `*Shared` boards) together with `shared`,
    /// borrowed from the caller's own local. No allocation happens here any
    /// more -- every one of `recon`/`cu_grid`/`edges`/`sao_params` is
    /// already built by the time this runs.
    pub(crate) fn new(
        shared: &'s mut CtxShared<'p>,
        pic: &'c mut Picture,
        recon: &'r mut ReconPicture<'r>,
        cu_grid: CuGrid<'p>,
        edges: EdgeMarks<'p>,
        sao_params: crate::sao::SaoParamsGrid<'p>,
    ) -> Self {
        let slice_qp = shared.slice_qp;
        Self {
            shared,
            pic,
            qp_y_prev: slice_qp,
            slice_start_ctb: 0,
            slice_end_ctb: u32::MAX,
            tile_ctb_rect: None,
            qg_qp_pred: slice_qp,
            is_cu_qp_delta_coded: false,
            cu_qp_delta_val: 0,
            cu_transquant_bypass: false,
            edges,
            sao_params,
            recon,
            cu_grid,
        }
    }

    /// Test-only: a second `Ctx` sharing every field this one has except
    /// `pic` (retargeted at `pic`) and `inter` (dropped -- nothing the
    /// deblocking-lag experiment `decoder.rs`'s own test module runs
    /// exercises inter prediction, only `deblock::filter_picture`, which
    /// never reads `Ctx::inter`). Exists so that experiment can run
    /// `deblock::filter_picture` twice -- once against the real, pristine
    /// reconstruction and once against a copy with rows beyond a candidate
    /// lag corrupted -- over identical coding-unit/edge metadata, without
    /// duplicating `Ctx::new`'s own SPS/PPS/DPB-derived construction (which
    /// a test living outside this module could not do anyway: most of
    /// `Ctx`'s fields are private to `ctu`, by design).
    #[cfg(test)]
    pub(crate) fn retarget_pic_for_test<'q>(
        &'q mut self,
        pic: &'q mut Picture,
        recon: &'q mut ReconPicture<'q>,
    ) -> Ctx<'p, 'q, 'q, 'q>
    where
        'p: 'q,
        's: 'q,
    {
        // `shared` is reborrowed rather than rebuilt: every field of
        // `CtxShared` is either `Copy` or, for `inter`, unread by
        // `deblock::filter_picture` (the only thing this retargeted copy
        // ever runs) -- so there is no need to zero it out the way the
        // pre-borrowed-reference version did, and `'p: 'q` lets the
        // reference itself (covariant in its lifetime, since it holds no
        // `&'p mut` fields) simply narrow from `'p` to `'q`.
        Ctx {
            shared: &mut *self.shared,
            pic,
            // `deblock::filter_picture`, the only thing this retargeted
            // copy ever runs, never reads `Ctx::recon` -- the caller passes
            // a throwaway one purely to satisfy the field.
            recon,
            cu_grid: self.cu_grid.clone(),
            qp_y_prev: self.qp_y_prev,
            slice_start_ctb: self.slice_start_ctb,
            slice_end_ctb: self.slice_end_ctb,
            tile_ctb_rect: self.tile_ctb_rect,
            qg_qp_pred: self.qg_qp_pred,
            is_cu_qp_delta_coded: self.is_cu_qp_delta_coded,
            cu_qp_delta_val: self.cu_qp_delta_val,
            cu_transquant_bypass: self.cu_transquant_bypass,
            edges: self.edges.clone(),
            sao_params: self.sao_params.clone(),
        }
    }

    /// The P-slice-only parameters, or an error if called on an I-slice
    /// `Ctx` — every call site is itself only reachable when `is_p_slice`,
    /// so this should never actually return `Err`; a clean error rather
    /// than an `unwrap`/`expect` is this crate's own standing rule
    /// (`AGENT-CONSTRAINTS.md`'s code rules), not a case anyone expects to
    /// hit.
    fn inter(&self) -> Result<&InterSliceParams<'p>> {
        self.shared.inter.as_ref().ok_or(Error::InvalidData(
            "vaco-codec-hevc: inter CU decode reached with no P-slice context",
        ))
    }

    /// The total bytes [`Budget::alloc`] charged for this `Ctx`'s own two
    /// `Budget`-tracked working buffers — [`CuGrid::budget_bytes`] plus
    /// [`crate::sao::SaoParamsGrid::budget_bytes`] (exactly what
    /// [`Ctx::new`]'s own [`crate::sao::SaoParamsGrid::new`] plus every
    /// [`crate::sao::SaoParamsGrid::begin_row`] call charged for it, summed
    /// across every row band the same way [`CuGrid::budget_bytes`] sums
    /// its own). Neither outlives one slice's own `decode_ctu_slice` call
    /// — `decoder.rs`'s own call site releases this right before dropping
    /// the `Ctx` that owns them, the other half of the leak
    /// [`CuGrid::budget_bytes`]'s own doc describes: `sao_params` is
    /// smaller than `cu_grid` per slice, but is charged on every slice
    /// that has any SAO syntax to parse at all (`slice_sao_luma_flag ||
    /// slice_sao_chroma_flag`), not only ones that end up applying a
    /// non-`Off` mode anywhere, so it leaked on exactly the same
    /// stock-`libx265` fixtures `cu_grid`'s own charge did.
    #[must_use]
    pub(crate) fn working_budget_bytes(&self) -> u64 {
        self.cu_grid
            .budget_bytes()
            .saturating_add(self.sao_params.budget_bytes())
    }

    /// Start an independent slice segment.  CABAC context state is local to
    /// the caller, while the QP predictor and spatial-neighbour availability
    /// are the two pieces of this CTU walk that must restart at its boundary.
    pub(crate) fn begin_slice_segment(&mut self, start_ctb: u32, end_ctb: u32, slice_qp: i32) {
        self.slice_start_ctb = start_ctb;
        self.slice_end_ctb = end_ctb;
        self.tile_ctb_rect = None;
        self.qp_y_prev = slice_qp;
        self.qg_qp_pred = slice_qp;
        self.is_cu_qp_delta_coded = false;
        self.cu_qp_delta_val = 0;
        self.cu_transquant_bypass = false;
    }

    /// Start one tile-local substream within the current slice segment.
    ///
    /// The CABAC caller resets its arithmetic/context state separately. Tile
    /// boundaries do not reset the slice's QP predictor, so this only narrows
    /// spatial-neighbour availability.
    pub(crate) fn begin_tile_substream(
        &mut self,
        ctb_x_start: u32,
        ctb_x_end: u32,
        ctb_y_start: u32,
        ctb_y_end: u32,
    ) {
        self.tile_ctb_rect = Some((ctb_x_start, ctb_x_end, ctb_y_start, ctb_y_end));
    }

    /// Whether a picture-coordinate neighbour belongs to this segment.
    /// Coordinates outside the picture are unavailable by the same test.
    #[allow(
        clippy::integer_division,
        reason = "CTB coordinates deliberately truncate picture coordinates by the fixed CTB width"
    )]
    pub(crate) fn in_current_slice(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.shared.pic_width || y >= self.shared.pic_height {
            return false;
        }
        let ctb = 1i32 << self.shared.log2_ctb_size;
        let ctb_x = u32::try_from(x / ctb).unwrap_or(u32::MAX);
        let ctb_y = u32::try_from(y / ctb).unwrap_or(u32::MAX);
        let addr = ctb_y
            .saturating_mul(self.shared.ctbs_x)
            .saturating_add(ctb_x);
        if addr < self.slice_start_ctb || addr >= self.slice_end_ctb {
            return false;
        }
        self.tile_ctb_rect
            .is_none_or(|(x0, x1, y0, y1)| ctb_x >= x0 && ctb_x < x1 && ctb_y >= y0 && ctb_y < y1)
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
fn qp_y_pred(s: &Ctx<'_, '_, '_, '_>, xqg: i32, yqg: i32) -> i32 {
    let ctb = 1i32 << s.shared.log2_ctb_size;
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

/// §7.3.8.11's `transform_skip_flag[x0][y0][cIdx]`, present only when
/// `transform_skip_enabled_flag` is set and the transform block is 4x4.
///
/// `log2_max_transform_skip_block_size_minus2` — the PPS range extension that
/// would widen that size condition past 4x4 — is refused by
/// `decoder::check_scope`, so `log2_size == 2` is the whole condition here.
/// `ctx_offset` selects §9.3.4.2.1's per-component context: `0` for luma,
/// `1` shared by both chroma components.
fn read_transform_skip_flag(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &Ctx<'_, '_, '_, '_>,
    log2_size: u32,
    ctx_offset: usize,
) -> Result<bool> {
    if s.cu_transquant_bypass || !s.shared.transform_skip_enabled || log2_size != 2 {
        return Ok(false);
    }
    let cm = ctx
        .transform_skip
        .get_mut(ctx_offset)
        .ok_or(Error::InvalidData("transform_skip ctx"))?;
    Ok(cabac.decode_decision(cm) != 0)
}

/// §8.6.4.2's branch selection: `transform_skip_flag` wins over `trType`,
/// which the spec's own ordering makes explicit (the skip branch is tested
/// first and the DST-VII/DCT-II choice is only reached when it does not
/// apply).
const fn transform_kind(skip: bool, use_dst: bool) -> transform::TransformKind {
    match (skip, use_dst) {
        (true, _) => transform::TransformKind::Skip,
        (false, true) => transform::TransformKind::Dst4,
        (false, false) => transform::TransformKind::Dct,
    }
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
fn read_unary_max(
    cabac: &mut CabacDecoder<'_>,
    ctx0: &mut ContextModel,
    ctx1: &mut ContextModel,
    max_symbol: u32,
) -> u32 {
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
fn maybe_parse_cu_qp_delta(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    has_residual: bool,
) -> Result<()> {
    if !s.shared.cu_qp_delta_enabled || s.is_cu_qp_delta_coded || !has_residual {
        return Ok(());
    }
    s.is_cu_qp_delta_coded = true;
    let (ctx0, rest) = ctx
        .cu_qp_delta
        .split_first_mut()
        .ok_or(Error::InvalidData("cu_qp_delta ctx"))?;
    let ctx1 = rest
        .first_mut()
        .ok_or(Error::InvalidData("cu_qp_delta ctx"))?;
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
pub(crate) fn decode_ctu(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    addr: u32,
) -> Result<()> {
    if s.shared.sao_luma || s.shared.sao_chroma {
        let params = sao::parse_ctu_sao(
            cabac,
            ctx,
            addr,
            s.shared.ctbs_x,
            s.shared.sao_luma,
            s.shared.sao_chroma,
            &s.sao_params,
            s.in_current_slice(x0 - 1, y0),
            s.in_current_slice(x0, y0 - 1),
        )?;
        s.sao_params.set(addr, &params);
    }
    coding_quadtree(cabac, ctx, s, x0, y0, s.shared.log2_ctb_size, 0)
}

fn coding_quadtree(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    let in_bounds = x0 + size <= s.shared.pic_width && y0 + size <= s.shared.pic_height;
    let at_min = log2_size == s.shared.log2_min_cb_size;

    let split = if !in_bounds {
        true
    } else if at_min {
        false
    } else {
        let left = s.in_current_slice(x0 - 1, y0)
            && s.cu_grid
                .depth_at(x0 - 1, y0)
                .is_some_and(|d| u32::from(d) > depth);
        let above = s.in_current_slice(x0, y0 - 1)
            && s.cu_grid
                .depth_at(x0, y0 - 1)
                .is_some_and(|d| u32::from(d) > depth);
        let inc = u32::from(left) + u32::from(above);
        let cm = ctx
            .split_cu_flag
            .get_mut(inc as usize)
            .ok_or(Error::InvalidData("split_cu_flag ctx out of range"))?;
        cabac.decode_decision(cm) != 0
    };

    // §7.3.8.4: whenever this node is at least `Log2MinCuQpDeltaSize`, it
    // starts a *new* quantisation group — unconditionally, regardless of
    // `split` — since `Log2MinCuQpDeltaSize <= CtbLog2SizeY` always, this
    // fires at least once per CTU and, for a CU larger than the nominal QG
    // size, is the only reset its own (single, larger) QG ever gets.
    if s.shared.cu_qp_delta_enabled && log2_size >= s.shared.log2_min_cu_qp_delta_size {
        s.cu_qp_delta_val = 0;
        s.is_cu_qp_delta_coded = false;
        s.qg_qp_pred = qp_y_pred(s, x0, y0);
    }

    if split {
        let half = size >> 1;
        for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
            let (cx, cy) = (x0 + dx, y0 + dy);
            if cx < s.shared.pic_width && cy < s.shared.pic_height {
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
fn coding_unit(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    // §7.3.8.5: this is the first syntax element in every CU when the PPS
    // enables it, before either an inter slice's skip flag or intra syntax.
    s.cu_transquant_bypass = if s.shared.transquant_bypass_enabled {
        let cm = ctx
            .cu_transquant_bypass
            .first_mut()
            .ok_or(Error::InvalidData("cu_transquant_bypass ctx"))?;
        cabac.decode_decision(cm) != 0
    } else {
        false
    };
    if s.cu_transquant_bypass {
        let blocks = usize::try_from(((1i32 << log2_size) >> 2).max(1)).unwrap_or(1);
        let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
        let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
        s.cu_grid.fill_filter_bypass(bx0, by0, blocks, blocks);
    }

    if s.shared.is_p_slice {
        coding_unit_p(cabac, ctx, s, x0, y0, log2_size, depth)
    } else {
        decode_intra_cu(cabac, ctx, s, x0, y0, log2_size, depth)
    }
}

/// §8.6.1's own per-coding-unit finalisation tail (HM's `xFinishDecodeCU`),
/// shared by every CU shape (intra, skip, inter): every coding unit gets its
/// own finalised `QpY` written to [`CuGrid`], whether or not it coded its
/// own `cu_qp_delta`.
fn finalize_cu_qp(s: &mut Ctx<'_, '_, '_, '_>, x0: i32, y0: i32, size: i32) {
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid
        .fill_qp(bx0, by0, blocks, blocks, i8::try_from(qp_y).unwrap_or(0));
    s.qp_y_prev = qp_y;
}

/// §7.3.8.7's `pcm_sample()` and §8.4.1 equations 8-12/8-15/8-16 for one
/// component plane. Samples are in raster order and scaled to the picture's
/// bit depth before being written to the reconstruction buffer.
fn read_pcm_plane(
    reader: &mut BitReader<'_>,
    plane: &mut crate::framebuf::ReconPlane<'_>,
    x0: i32,
    y0: i32,
    size: usize,
    pcm_bit_depth: u32,
    output_bit_depth: u32,
) {
    let Ok(x0u) = usize::try_from(x0) else { return };
    let shift = output_bit_depth.saturating_sub(pcm_bit_depth);
    let mut row = [0u8; MAX_CTB];
    let size = size.min(MAX_CTB);
    for y in 0..size {
        for sample in row.iter_mut().take(size) {
            let value = reader.get(pcm_bit_depth) << shift;
            *sample = u8::try_from(value).unwrap_or(0);
        }
        let Ok(py) = usize::try_from(y0.saturating_add(i32::try_from(y).unwrap_or(0))) else {
            continue;
        };
        let Some(samples) = row.get(..size) else {
            continue;
        };
        plane.write_row(x0u, py, samples);
        plane.mark_row_ready(py, x0u, size);
    }
}

/// Decode and reconstruct the `pcm_flag == 1` branch of §7.3.8.5.
///
/// `pcm_flag` uses the termination process, so no arithmetic read-ahead lies
/// between it and the raw aligned samples. Context models survive unchanged;
/// only the arithmetic engine is initialized again after the samples (§9.3.1,
/// §9.3.2.6).
fn decode_pcm_cu(
    cabac: &mut CabacDecoder<'_>,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
    pcm: PcmParameters,
) -> Result<()> {
    if cabac.malformed() {
        return Err(Error::InvalidData(
            "vaco-codec-hevc: malformed CABAC before I_PCM samples",
        ));
    }
    let mut reader = core::mem::replace(cabac, CabacDecoder::new(&[])).into_reader();
    while reader.bit_pos() & 7 != 0 {
        if reader.get_bit() != 0 {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: non-zero pcm_alignment_zero_bit",
            ));
        }
    }

    let size = 1usize << log2_size;
    read_pcm_plane(
        &mut reader,
        &mut s.recon.y,
        x0,
        y0,
        size,
        u32::from(pcm.sample_bit_depth_luma),
        s.shared.bit_depth_luma,
    );
    let chroma_size = size >> 1;
    read_pcm_plane(
        &mut reader,
        &mut s.recon.cb,
        x0 >> 1,
        y0 >> 1,
        chroma_size,
        u32::from(pcm.sample_bit_depth_chroma),
        s.shared.bit_depth_chroma,
    );
    read_pcm_plane(
        &mut reader,
        &mut s.recon.cr,
        x0 >> 1,
        y0 >> 1,
        chroma_size,
        u32::from(pcm.sample_bit_depth_chroma),
        s.shared.bit_depth_chroma,
    );
    if reader.overrun() {
        return Err(Error::InvalidData(
            "vaco-codec-hevc: I_PCM samples truncated",
        ));
    }
    *cabac = CabacDecoder::from_reader(reader);

    let size_i32 = 1i32 << log2_size;
    let blocks = usize::try_from((size_i32 >> 2).max(1)).unwrap_or(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid.fill(
        bx0,
        by0,
        blocks,
        blocks,
        u8::try_from(depth).unwrap_or(u8::MAX),
        DC_IDX,
    );
    if pcm.loop_filter_disabled {
        s.cu_grid.fill_filter_bypass(bx0, by0, blocks, blocks);
    }
    let grid = crate::deblock::DEBLOCK_GRID;
    s.edges.mark_vert(x0, y0, size_i32, grid);
    s.edges.mark_horiz(x0, y0, size_i32, grid);
    finalize_cu_qp(s, x0, y0, size_i32);
    Ok(())
}

fn decode_intra_cu(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    // §7.3.8.5: `part_mode` (a single ctx-coded bin for an intra CU: `1`
    // means `PART_2Nx2N`, `0` means `PART_NxN`) is present exactly when
    // this CU sits at the minimum coding block size.
    let is_nxn = if log2_size == s.shared.log2_min_cb_size {
        let cm = ctx
            .part_size
            .first_mut()
            .ok_or(Error::InvalidData("part_size ctx"))?;
        let bin = cabac.decode_decision(cm);
        bin == 0
    } else {
        false
    };

    // §7.3.8.5: `pcm_flag` is present only for an intra PART_2Nx2N CU whose
    // size lies in the SPS-declared PCM range. Its bin uses the termination
    // process (§9.3.4.3.5), not a context model.
    if !is_nxn
        && let Some(pcm) = s.shared.pcm
        && log2_size >= u32::from(pcm.log2_min_cb_size)
        && log2_size
            <= u32::from(pcm.log2_min_cb_size)
                .saturating_add(u32::from(pcm.log2_diff_max_min_cb_size))
        && cabac.decode_terminate() != 0
    {
        return decode_pcm_cu(cabac, s, x0, y0, log2_size, depth, pcm);
    }

    let pus: Vec<Pu> = if is_nxn {
        let half = size >> 1;
        vec![
            Pu {
                x: x0,
                y: y0,
                size: half,
            },
            Pu {
                x: x0 + half,
                y: y0,
                size: half,
            },
            Pu {
                x: x0,
                y: y0 + half,
                size: half,
            },
            Pu {
                x: x0 + half,
                y: y0 + half,
                size: half,
            },
        ]
    } else {
        vec![Pu { x: x0, y: y0, size }]
    };

    // §7.3.8.5: every `prev_intra_luma_pred_flag` bin is read before any
    // `mpm_idx`/`rem_intra_luma_pred_mode`.
    let mut prev_flags = [false; 4];
    for slot in prev_flags.iter_mut().take(pus.len()) {
        let cm = ctx
            .prev_intra_luma_pred
            .first_mut()
            .ok_or(Error::InvalidData("prev_intra ctx"))?;
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
    let ctb_size = 1i32 << s.shared.log2_ctb_size;
    for (i, pu) in pus.iter().enumerate() {
        let left = if s.in_current_slice(pu.x - 1, pu.y) {
            s.cu_grid.mode_at(pu.x - 1, pu.y)
        } else {
            DC_IDX
        };
        let above = if pu.y % ctb_size == 0 || !s.in_current_slice(pu.x, pu.y - 1) {
            DC_IDX
        } else {
            s.cu_grid.mode_at(pu.x, pu.y - 1)
        };
        let mpm = intra_mode::mpm_list(left, above);
        let prev_flag = prev_flags.get(i).copied().unwrap_or(false);
        let mode = if prev_flag {
            let first = cabac.decode_bypass() != 0;
            let idx = if first {
                1 + usize::from(cabac.decode_bypass() != 0)
            } else {
                0
            };
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
        s.cu_grid.fill(
            bx0,
            by0,
            blocks,
            blocks,
            u8::try_from(depth).unwrap_or(u8::MAX),
            mode,
        );
    }

    // intra_chroma_pred_mode: once per CU, referencing PU0's luma mode —
    // chroma always predicts as a single 2Nx2N block regardless of `PartMode`.
    let chroma_syntax = {
        let cm = ctx
            .intra_chroma_pred_mode
            .first_mut()
            .ok_or(Error::InvalidData("chroma ctx"))?;
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
    let quadtree_tu_log2_min = quadtree_tu_log2_min_in_cu(
        s,
        log2_size,
        s.shared.max_transform_hierarchy_depth_intra,
        intra_split_depth_extra,
    );

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
fn ctx_skip_flag(s: &Ctx<'_, '_, '_, '_>, x0: i32, y0: i32) -> usize {
    usize::from(s.in_current_slice(x0 - 1, y0) && s.cu_grid.is_skip_at(x0 - 1, y0))
        + usize::from(s.in_current_slice(x0, y0 - 1) && s.cu_grid.is_skip_at(x0, y0 - 1))
}

/// `parseMergeIndex`/§9.3.3.2: a truncated-unary code, `cMax =
/// MaxNumMergeCand - 1`, bin 0 context-coded, every further bin bypass.
fn parse_merge_index(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    max_num_merge_cand: usize,
) -> Result<usize> {
    if max_num_merge_cand <= 1 {
        return Ok(0);
    }
    let cm = ctx
        .merge_idx
        .first_mut()
        .ok_or(Error::InvalidData("merge_idx ctx"))?;
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
    let cm = ctx
        .mvp_idx
        .first_mut()
        .ok_or(Error::InvalidData("mvp_idx ctx"))?;
    let val = cabac.decode_decision(cm);
    Ok(usize::from(val != 0))
}

/// `parseRefFrmIdx`/§9.3.3.2: bin 0 (`ctx[0]`) says "is it more than 0";
/// every further bin up to `NumRefIdxL0 - 2` of them is context-coded via
/// `ctx[1]` for the first and bypass afterward, truncated-unary shaped.
fn parse_ref_idx(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    num_ref_idx_l0: usize,
) -> Result<usize> {
    if num_ref_idx_l0 <= 1 {
        return Ok(0);
    }
    let (ctx0, rest) = ctx
        .ref_pic
        .split_first_mut()
        .ok_or(Error::InvalidData("ref_idx ctx"))?;
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
    let (ctx0, rest) = ctx
        .mvd
        .split_first_mut()
        .ok_or(Error::InvalidData("mvd ctx"))?;
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
    Ok(Mv {
        x: if hor_sign != 0 { -hor } else { hor },
        y: if ver_sign != 0 { -ver } else { ver },
    })
}

/// §7.3.8.5's inter `part_mode`: the `uiMaxNumBits`-bin prefix
/// (`2Nx2N`/`2NxN`/`Nx2N`[/`NxN`, only at the minimum CB size and only when
/// the CU is not 8x8]), then, when `amp_enabled` and this CU is *not* at the
/// minimum CB size, the AMP shape-refinement bin for `2NxN`/`Nx2N`. A direct
/// port of HM's own `parsePartSize` inter branch, since its bin-count
/// derivation (`uiMaxNumBits`) does not fall out of the binarisation table
/// alone without also knowing the min-CB/AMP gating it is entangled with.
fn parse_part_mode_inter(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    at_min_cb_size: bool,
    size: i32,
    amp_enabled: bool,
) -> Result<PartMode> {
    let max_num_bits: usize = if at_min_cb_size && size != 8 { 3 } else { 2 };
    let mut mode_idx = 0usize;
    for i in 0..max_num_bits {
        let cm = ctx
            .part_size
            .get_mut(i)
            .ok_or(Error::InvalidData("part_size ctx"))?;
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
            let cm = ctx
                .part_size
                .get_mut(3)
                .ok_or(Error::InvalidData("part_size ctx"))?;
            if cabac.decode_decision(cm) == 0 {
                mode = if cabac.decode_bypass() == 0 {
                    PartMode::TwoNxNu
                } else {
                    PartMode::TwoNxNd
                };
            }
        } else if mode == PartMode::Nx2N {
            let cm = ctx
                .part_size
                .get_mut(3)
                .ok_or(Error::InvalidData("part_size ctx"))?;
            if cabac.decode_decision(cm) == 0 {
                mode = if cabac.decode_bypass() == 0 {
                    PartMode::NLx2N
                } else {
                    PartMode::NRx2N
                };
            }
        }
    }
    Ok(mode)
}

/// `inter_pred_idc`'s three possible values (§7.4.9.6's own semantics table)
/// — `PRED_BI` only ever reachable on a B slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterPredIdc {
    L0,
    L1,
    Bi,
}

/// §7.3.8.6's `inter_pred_idc`/§9.3.4.2.1: B-slice-only syntax — a P slice
/// infers `PRED_L0` and never reaches this at all. Bi-prediction is only
/// readable at all when the CU is not a split 8x8 (`PartMode::TwoNx2N` or
/// `size != 8` — HM's own `getHeight(uiAbsPartIdx)` returns the *coding
/// unit's* height regardless of partition, not the PU's, so this checks the
/// CU's own `size`, confirmed directly against `TDecSbac::parseInterDir`);
/// otherwise only the second (`L0`-vs-`L1`) bin is read. The first bin's
/// `ctxInc` is the CU's own quadtree `depth` (HM's `getCtxInterDir`); the
/// second bin always uses context index 4.
fn parse_inter_pred_idc(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    part_mode: PartMode,
    size: i32,
    depth: u32,
) -> Result<InterPredIdc> {
    if part_mode == PartMode::TwoNx2N || size != 8 {
        let d = usize::try_from(depth).unwrap_or(0).min(3);
        let cm = ctx
            .inter_dir
            .get_mut(d)
            .ok_or(Error::InvalidData("inter_dir ctx"))?;
        if cabac.decode_decision(cm) != 0 {
            return Ok(InterPredIdc::Bi);
        }
    }
    let cm = ctx
        .inter_dir
        .get_mut(4)
        .ok_or(Error::InvalidData("inter_dir ctx"))?;
    Ok(if cabac.decode_decision(cm) != 0 {
        InterPredIdc::L1
    } else {
        InterPredIdc::L0
    })
}

/// §8.5.3.2.9's own per-position collocated-motion-vector derivation, for
/// one candidate position and one target list: `None` when the collocated PU
/// is intra, or (per this crate's short-term-only scope) never for any other
/// reason.
///
/// # B slices: which of the collocated PU's own two lists to read
///
/// §8.5.3.2.9's own text: if the collocated PU used only one list, that
/// list's motion is used regardless of `target_list`. Otherwise (a
/// bi-predicted collocated PU) the list read is `target_list` itself when
/// [`InterSliceParams::is_low_delay`] (`NoBackwardPredFlag`) is set, or
/// `L(collocated_from_l0_flag)` otherwise — confirmed directly against HM's
/// own `xGetColMVP` (`RefPicList eColRefPicList = getCheckLDC() ? eRefPicList
/// : RefPicList(getColFromL0Flag())`, with a fallback to `1 -
/// eColRefPicList` whenever the chosen list's own `refIdx` is negative,
/// i.e. whenever that list is actually unavailable at this specific
/// position — which subsumes the "only one list used" case above without a
/// separate branch: [`RefList::pick`] already returns `None` for an unused
/// list, and `.or_else(pick(other))` is exactly HM's fallback).
fn col_mvp(
    s: &Ctx<'_, '_, '_, '_>,
    pos: (i32, i32),
    target_list: motion::RefList,
    curr_poc: i64,
    target_ref_poc: i64,
) -> Option<Mv> {
    let inter = s.inter().ok()?;
    let collocated = inter.collocated.as_ref()?;
    let info = collocated.get(pos.0, pos.1)?;
    let col_list = if inter.is_low_delay {
        target_list
    } else if inter.collocated_from_l0 {
        motion::RefList::L1
    } else {
        motion::RefList::L0
    };
    let uni = col_list
        .pick(info)
        .or_else(|| col_list.other().pick(info))?;
    let scale = motion::dist_scale_factor(curr_poc, target_ref_poc, collocated.poc, uni.ref_poc);
    Some(if scale == 4096 {
        uni.mv
    } else {
        motion::scale_mv(uni.mv, scale)
    })
}

/// §8.5.3.2.8's temporal candidate: try the bottom-right position, falling
/// back to the centre position, and resolve/scale via [`col_mvp`] for
/// `target_list` — `None` whenever no collocated field is recorded, or
/// *neither* position yields usable motion on `target_list`.
///
/// # The bottom-right-vs-centre fallback is gated on the wrong condition —
/// found and fixed
///
/// HM's own `fillMvpCand`/`getInterMergeCandidates` (confirmed directly
/// against `TComDataCU.cpp`, Tier A) always attempts the bottom-right
/// position first when it is geometrically available (in the picture, same
/// CTB row as the PU) and falls back to the centre position whenever that
/// attempt *fails for any reason* — `xGetColMVP` returning `false` because
/// the position names an intra block (`!isInter`) is exactly as much a
/// reason to fall back as the position being outside the picture in the
/// first place; HM's own `if (ctuRsAddr >= 0 && xGetColMVP(...)) { BR } else
/// { centre }` does not distinguish the two. An earlier version of this
/// function conflated "geometrically available" with "yields motion":
/// it only ever tried the centre position when `br_in_bounds` was `false`,
/// so a geometrically-valid bottom-right position that happened to name an
/// intra (or otherwise motion-less) block returned `None` outright, dropping
/// the temporal candidate entirely instead of falling back — even though the
/// *bin-for-bin identical* HM decode of the same stream successfully
/// resolves a real, non-zero predictor from the centre position in exactly
/// this case. Found via a byte-for-byte HM 18.0 trace (`xGetColMVP`
/// instrumented to report its own success/failure and reason) on the
/// documented repro (CU (208, 24), frame 2 of the P-only fixture): both
/// HM and this crate compute the *identical* bottom-right pixel position
/// (confirming §8.5.3.2.8's naive `xPb + nPbW`/`yPb + nPbH` arithmetic
/// needs no z-scan-index correction — the "dropped, not miscomputed"
/// language in this module's own history is right about *which* candidate
/// is wrong, but the earlier "z-scan-index arithmetic" hypothesis for *why*
/// is refuted by this trace), and HM's own trace shows that position is
/// genuinely intra in the collocated picture (`xGetColMVP_fail
/// reason=notInter`) — exactly the case this fix now falls back for.
fn temporal_candidate(
    s: &Ctx<'_, '_, '_, '_>,
    pu_x: i32,
    pu_y: i32,
    pu_w: i32,
    pu_h: i32,
    curr_poc: i64,
    target_ref_poc: i64,
    target_list: motion::RefList,
) -> Result<motion::TemporalCandidate> {
    if s.inter()?.collocated.is_none() {
        return Ok(None);
    }

    let x_br = pu_x + pu_w;
    let y_br = pu_y + pu_h;
    let same_ctb_row = (pu_y >> s.shared.log2_ctb_size) == (y_br >> s.shared.log2_ctb_size);
    let br_in_bounds = x_br < s.shared.pic_width && y_br < s.shared.pic_height && same_ctb_row;

    let br = br_in_bounds
        .then(|| col_mvp(s, (x_br, y_br), target_list, curr_poc, target_ref_poc))
        .flatten();
    let result = br.or_else(|| {
        // §8.5.3.2.9's centre fallback: `(nPbW/4/2)*4` in each axis, matching
        // HM's own z-scan-index arithmetic (see `crate::motion`'s own doc on
        // why positions here are pixel coordinates) rather than a plain
        // `/2`, which the two only agree with when `nPbW`/`nPbH` are
        // multiples of 8 — not guaranteed for an AMP partition's shorter
        // side (a 12-wide/tall PU has a genuinely different centre). Tried
        // whenever the bottom-right attempt above did not produce a
        // candidate, for *any* reason — geometrically unavailable or
        // geometrically fine but motion-less — matching HM's own
        // undifferentiated `else` branch (see this function's own doc).
        #[allow(clippy::integer_division, reason = "deliberate truncating division, matching HM's own integer z-scan-index arithmetic exactly")]
        let (cx, cy) = (pu_x + (pu_w / 4 / 2) * 4, pu_y + (pu_h / 4 / 2) * 4);
        col_mvp(s, (cx, cy), target_list, curr_poc, target_ref_poc)
    });
    Ok(result)
}

/// A P-slice coding unit: `cu_skip_flag`, then (if not skipped)
/// `pred_mode_flag` — an inter slice can still code an intra-refresh CU,
/// which reuses [`decode_intra_cu`] unchanged (its own `part_mode`/MPM/
/// transform-tree logic has no dependency on the enclosing slice's type).
fn coding_unit_p(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let skip_ctx = ctx_skip_flag(s, x0, y0);
    let cm = ctx
        .skip_flag
        .get_mut(skip_ctx)
        .ok_or(Error::InvalidData("skip_flag ctx out of range"))?;
    let is_skip = cabac.decode_decision(cm) != 0;
    if is_skip {
        return decode_skip_cu(cabac, ctx, s, x0, y0, log2_size, depth);
    }

    let cm = ctx
        .pred_mode
        .first_mut()
        .ok_or(Error::InvalidData("pred_mode ctx"))?;
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
fn decode_skip_cu(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    // A skip CU never reaches `transform_tree_inter`/`transform_unit_inter`
    // — the only other call sites that mark this picture's deblocking edges
    // — so without this, a skip CU's own left/top boundary (shared with
    // whichever CU precedes it) is silently never marked at all. HM marks it
    // unconditionally regardless of skip status: `TComLoopFilter::xDeblockCU`
    // calls `xSetEdgefilterTU`/`xSetEdgefilterPU` for every coding unit,
    // skip included, treating a skip CU as one trivial (residual-free)
    // transform-block leaf spanning its own whole extent — reproduced here
    // as `mark_tu_vert`/`mark_tu_horiz` (not the plain, non-`tu_`
    // variant): the "also a transform-block edge" cbf condition in
    // `deblock::boundary_strength` still resolves correctly, since a skip
    // CU's own `cbf_luma_at` is never written and so already reads `false`
    // — the same "no residual" answer HM's own `getCbf` gives it.
    let grid = crate::deblock::DEBLOCK_GRID;
    s.edges.mark_tu_vert(x0, y0, size, grid);
    s.edges.mark_tu_horiz(x0, y0, size, grid);
    let max_num_merge_cand = s.inter()?.max_num_merge_cand;
    let merge_idx = parse_merge_index(cabac, ctx, max_num_merge_cand)?;
    let pu = PuRect {
        x: x0,
        y: y0,
        w: size,
        h: size,
    };
    let chosen = resolve_merge_candidate(
        s,
        x0,
        y0,
        size,
        pu,
        0,
        PartMode::TwoNx2N,
        merge_idx,
        max_num_merge_cand,
    )?;
    write_inter_cu_no_residual(s, x0, y0, size, &[(pu, chosen)])?;
    let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid.fill(
        bx0,
        by0,
        blocks,
        blocks,
        u8::try_from(depth).unwrap_or(u8::MAX),
        DC_IDX,
    );
    s.cu_grid
        .fill_motion(bx0, by0, blocks, blocks, chosen, true);
    finalize_cu_qp(s, x0, y0, size);
    Ok(())
}

/// Runs §8.5.3.2.2's merge derivation for one PU and resolves `merge_idx`
/// into an actual [`MotionInfo`] — shared by the skip path (always PU 0,
/// `TwoNx2N`) and the non-skip merge path (any PU/`PartMode`). `cu_x0`/
/// `cu_y0`/`cu_size` are the CU's own geometry (distinct from `pu`'s own,
/// used only by the merge-parallelism override below).
#[allow(
    clippy::too_many_arguments,
    reason = "every argument is a distinct merge-derivation input; a sub-struct would not aid clarity at one internal call site"
)]
fn resolve_merge_candidate(
    s: &Ctx<'_, '_, '_, '_>,
    cu_x0: i32,
    cu_y0: i32,
    cu_size: i32,
    pu: PuRect,
    pu_idx: usize,
    part_mode: PartMode,
    merge_idx: usize,
    max_num_merge_cand: usize,
) -> Result<MotionInfo> {
    let inter = s.inter()?;
    // §8.5.3.2.2's own merge-parallelism special case: an 8x8 CU split into
    // more than one PU, with `Log2ParallelMergeLevel > 2`, derives every one
    // of its PUs' merge candidates as if the whole CU were a single
    // `PART_2Nx2N` PU — HM's own `decodePUWise` applies this by temporarily
    // overriding `PartSize` before calling `getInterMergeCandidates`, not by
    // branching inside the derivation itself.
    let merge_override =
        inter.log2_parallel_merge_level > 2 && part_mode != PartMode::TwoNx2N && cu_size == 8;
    let (eff_pu, eff_idx, eff_mode) = if merge_override {
        (
            PuRect {
                x: cu_x0,
                y: cu_y0,
                w: cu_size,
                h: cu_size,
            },
            0usize,
            PartMode::TwoNx2N,
        )
    } else {
        (pu, pu_idx, part_mode)
    };

    let (temporal_l0, temporal_l1) = if inter.collocated.is_some() {
        let ref_poc0_l0 = inter.ref_pics_l0.first().map_or(inter.cur_poc, |r| r.poc);
        let t0 = temporal_candidate(
            s,
            eff_pu.x,
            eff_pu.y,
            eff_pu.w,
            eff_pu.h,
            inter.cur_poc,
            ref_poc0_l0,
            RefList::L0,
        )?;
        let t1 = if inter.is_b {
            let ref_poc0_l1 = inter.ref_pics_l1.first().map_or(inter.cur_poc, |r| r.poc);
            temporal_candidate(
                s,
                eff_pu.x,
                eff_pu.y,
                eff_pu.w,
                eff_pu.h,
                inter.cur_poc,
                ref_poc0_l1,
                RefList::L1,
            )?
        } else {
            None
        };
        (t0, t1)
    } else {
        (None, None)
    };
    let ref_pocs_l0 = inter.ref_pocs_l0();
    let ref_pocs_l1 = inter.ref_pocs_l1();
    let cands = motion::derive_merge_candidates(
        &s.cu_grid,
        eff_pu,
        eff_idx,
        eff_mode,
        inter.log2_parallel_merge_level,
        max_num_merge_cand,
        &ref_pocs_l0,
        &ref_pocs_l1,
        temporal_l0,
        temporal_l1,
        inter.is_b,
        |x, y| s.in_current_slice(x, y),
    );
    let mut chosen = cands.get(merge_idx).copied().ok_or(Error::InvalidData(
        "vaco-codec-hevc: merge_idx out of range",
    ))?;
    // §8.5.3.2.1's own last step: a *bi-predictive* merge candidate for an
    // 8x4 or 4x8 PU (`nOrigPbW + nOrigPbH == 12`) is forced to uni-prediction
    // from L0 -- `refIdxL1 = -1`, `predFlagL1 = 0`. The clause's condition is
    // `predFlagL0 == 1 && predFlagL1 == 1`, so an L1-only candidate is left
    // alone rather than emptied. `nOrigPb*` is the PU's own size *before* the
    // merge-parallelism override above replaces it with the whole CU, so this
    // reads `pu`, not `eff_pu`.
    if pu.w + pu.h == 12 && chosen.l0.is_some() && chosen.l1.is_some() {
        chosen.l1 = None;
    }
    Ok(chosen)
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

fn build_cu_prediction(
    s: &Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    size: i32,
    pus: &[(PuRect, MotionInfo)],
) -> Result<CuPrediction> {
    let inter = s.inter()?;
    let ctb_size = 1i32 << s.shared.log2_ctb_size;
    let csize = (size >> 1).max(1);
    let mut pred = CuPrediction {
        size,
        y: vec![0i32; (size * size) as usize],
        cb: vec![0i32; (csize * csize) as usize],
        cr: vec![0i32; (csize * csize) as usize],
    };

    for (pu, info) in pus {
        // Clipped once per list (§8.5.3.2's own `clipMv`, `crate::motion`'s
        // own doc), reused for luma (shift 2) and both chroma planes (shift
        // 3) below — not re-derived per plane.
        let clipped_l0 = info.l0.map(|u| {
            motion::clip_mv(
                u.mv,
                x0,
                y0,
                s.shared.pic_width,
                s.shared.pic_height,
                ctb_size,
            )
        });
        let clipped_l1 = info.l1.map(|u| {
            motion::clip_mv(
                u.mv,
                x0,
                y0,
                s.shared.pic_width,
                s.shared.pic_height,
                ctb_size,
            )
        });

        let (w, h) = (
            usize::try_from(pu.w).unwrap_or(0),
            usize::try_from(pu.h).unwrap_or(0),
        );
        let y_buf = predict_component(
            inter,
            *info,
            clipped_l0,
            clipped_l1,
            pu.x,
            pu.y,
            2,
            3,
            w,
            h,
            s.shared.bit_depth_luma,
            true,
            |pic| &pic.y,
            |rw| rw.luma,
        )?;
        blit(
            &mut pred.y,
            usize::try_from(size).unwrap_or(1),
            usize::try_from(pu.x - x0).unwrap_or(0),
            usize::try_from(pu.y - y0).unwrap_or(0),
            w,
            h,
            &y_buf,
        );

        // Chroma (4:2:0): half-resolution PU rectangle, the same raw `mv`
        // interpreted at eighth-sample precision (shift 3, mask 7) — see
        // `mc.rs`'s own doc for why both components share one raw `mv`.
        let (cx0, cy0, cw, ch) = (pu.x >> 1, pu.y >> 1, (pu.w >> 1).max(1), (pu.h >> 1).max(1));
        let (cw_u, ch_u) = (
            usize::try_from(cw).unwrap_or(0),
            usize::try_from(ch).unwrap_or(0),
        );
        let cb_buf = predict_component(
            inter,
            *info,
            clipped_l0,
            clipped_l1,
            cx0,
            cy0,
            3,
            7,
            cw_u,
            ch_u,
            s.shared.bit_depth_chroma,
            false,
            |pic| &pic.cb,
            |rw| rw.chroma[0],
        )?;
        let cr_buf = predict_component(
            inter,
            *info,
            clipped_l0,
            clipped_l1,
            cx0,
            cy0,
            3,
            7,
            cw_u,
            ch_u,
            s.shared.bit_depth_chroma,
            false,
            |pic| &pic.cr,
            |rw| rw.chroma[1],
        )?;
        blit(
            &mut pred.cb,
            usize::try_from(csize).unwrap_or(1),
            usize::try_from(cx0 - (x0 >> 1)).unwrap_or(0),
            usize::try_from(cy0 - (y0 >> 1)).unwrap_or(0),
            cw_u,
            ch_u,
            &cb_buf,
        );
        blit(
            &mut pred.cr,
            usize::try_from(csize).unwrap_or(1),
            usize::try_from(cx0 - (x0 >> 1)).unwrap_or(0),
            usize::try_from(cy0 - (y0 >> 1)).unwrap_or(0),
            cw_u,
            ch_u,
            &cr_buf,
        );
    }
    Ok(pred)
}

/// One plane's own motion-compensated samples for one PU, §8.5.3.3.1–.4:
/// uni-predictive when only one of `info.l0`/`info.l1` is set (the existing,
/// unchanged single-list path — `predict_block`'s folded final shift when
/// unweighted, [`crate::mc::predict_block_intermediate`] +
/// [`crate::mc::apply_weight`] when weighted), bi-predictive when both are
/// (§8.5.3.3.4.2's [`crate::mc::default_biprediction`] or §8.5.3.3.4.3's
/// [`crate::mc::apply_weight_bi`], combining two
/// [`crate::mc::predict_block_intermediate`] outputs — one per list, each
/// clipped/rounded only once, together, never separately).
///
/// `origin_x`/`origin_y` is the PU's own top-left in this plane's own sample
/// grid (luma or chroma-halved); `shift`/`mask` split a clipped motion
/// vector's quarter- (luma, `2`/`3`) or eighth- (chroma, `3`/`7`) sample
/// fraction from its integer part. `plane_of`/`weight_of` pick this call's
/// own component (luma, Cb or Cr) out of a [`Picture`]/[`RefWeights`]
/// without three near-identical call sites duplicating the whole
/// uni/bi-predictive branch above.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site per component (luma/Cb/Cr) inside build_cu_prediction; every argument is a distinct §8.5.3.3 input"
)]
fn predict_component(
    inter: &InterSliceParams<'_>,
    info: MotionInfo,
    clipped_l0: Option<Mv>,
    clipped_l1: Option<Mv>,
    origin_x: i32,
    origin_y: i32,
    shift: i32,
    mask: i32,
    w: usize,
    h: usize,
    bit_depth: u32,
    is_luma: bool,
    plane_of: impl Fn(&Picture) -> &Plane,
    weight_of: impl Fn(&RefWeights) -> crate::mc::Weight,
) -> Result<Vec<i32>> {
    let sample = |mv: Mv| {
        (
            origin_x + (mv.x >> shift),
            origin_y + (mv.y >> shift),
            mv.x & mask,
            mv.y & mask,
        )
    };
    let unknown_ref = || {
        Error::InvalidData("vaco-codec-hevc: merge/AMVP candidate names an unknown reference POC")
    };

    let uni = |list: RefList, u: UniMotion, clipped: Option<Mv>| -> Result<Vec<i32>> {
        let ref_pic = inter
            .plane_for_poc(list, u.ref_poc)
            .ok_or_else(unknown_ref)?;
        let (ix, iy, fx, fy) = sample(clipped.unwrap_or(Mv::ZERO));
        let weight = inter.weights_for(list, u.ref_poc).map(|rw| weight_of(&rw));
        let mut buf = vec![0i32; w * h];
        if let Some(wt) = weight {
            crate::mc::predict_block_intermediate(
                plane_of(ref_pic),
                ix,
                iy,
                fx,
                fy,
                w,
                h,
                is_luma,
                &mut buf,
            );
            for v in &mut buf {
                *v = crate::mc::apply_weight(*v, wt, bit_depth);
            }
        } else {
            crate::mc::predict_block(
                plane_of(ref_pic),
                ix,
                iy,
                fx,
                fy,
                w,
                h,
                bit_depth,
                is_luma,
                &mut buf,
            );
        }
        Ok(buf)
    };

    match (info.l0, info.l1) {
        (Some(l0), Some(l1)) => {
            let ref0 = inter
                .plane_for_poc(RefList::L0, l0.ref_poc)
                .ok_or_else(unknown_ref)?;
            let ref1 = inter
                .plane_for_poc(RefList::L1, l1.ref_poc)
                .ok_or_else(unknown_ref)?;
            let (ix0, iy0, fx0, fy0) = sample(clipped_l0.unwrap_or(Mv::ZERO));
            let (ix1, iy1, fx1, fy1) = sample(clipped_l1.unwrap_or(Mv::ZERO));
            let mut buf0 = vec![0i32; w * h];
            let mut buf1 = vec![0i32; w * h];
            crate::mc::predict_block_intermediate(
                plane_of(ref0),
                ix0,
                iy0,
                fx0,
                fy0,
                w,
                h,
                is_luma,
                &mut buf0,
            );
            crate::mc::predict_block_intermediate(
                plane_of(ref1),
                ix1,
                iy1,
                fx1,
                fy1,
                w,
                h,
                is_luma,
                &mut buf1,
            );
            let w0 = inter
                .weights_for(RefList::L0, l0.ref_poc)
                .map(|rw| weight_of(&rw));
            let w1 = inter
                .weights_for(RefList::L1, l1.ref_poc)
                .map(|rw| weight_of(&rw));
            let mut out = vec![0i32; w * h];
            for (i, o) in out.iter_mut().enumerate() {
                let p0 = buf0.get(i).copied().unwrap_or(0);
                let p1 = buf1.get(i).copied().unwrap_or(0);
                *o = match (w0, w1) {
                    (Some(a), Some(b)) => crate::mc::apply_weight_bi(p0, a, p1, b, bit_depth),
                    _ => crate::mc::default_biprediction(p0, p1, bit_depth),
                };
            }
            Ok(out)
        }
        (Some(l0), None) => uni(RefList::L0, l0, clipped_l0),
        (None, Some(l1)) => uni(RefList::L1, l1, clipped_l1),
        (None, None) => Err(Error::InvalidData(
            "vaco-codec-hevc: a coded PU predicts from neither reference list",
        )),
    }
}

/// Copy one PU's `w x h` prediction into its own rectangle of the CU-sized
/// `dst` buffer, one row at a time (`PERF-PROGRAMME.md` item B1: this used
/// to be a per-sample `get`/`get_mut` pair, `build_cu_prediction`'s own
/// 3.48% share of decode). Both buffers hold the same `i32` element type at
/// the same per-row stride they are copied at, so each row is one
/// `copy_from_slice` — a real `memcpy`, not sample-by-sample arithmetic —
/// rather than one bounds-checked lookup per sample.
fn blit(dst: &mut [i32], dst_stride: usize, x0: usize, y0: usize, w: usize, h: usize, src: &[i32]) {
    for row in 0..h {
        let dst_start = y0
            .saturating_add(row)
            .saturating_mul(dst_stride)
            .saturating_add(x0);
        let src_start = row.saturating_mul(w);
        let (Some(dst_row), Some(src_row)) = (
            dst.get_mut(dst_start..dst_start.saturating_add(w)),
            src.get(src_start..src_start.saturating_add(w)),
        ) else {
            continue;
        };
        dst_row.copy_from_slice(src_row);
    }
}

/// A non-skip merged `PART_2Nx2N` CU with `rqt_root_cbf == 0` (inferred, per
/// §7.3.8.5's own presence condition — never actually parsed) writes its MC
/// prediction straight to the picture, unmodified.
fn write_inter_cu_no_residual(
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    size: i32,
    pus: &[(PuRect, MotionInfo)],
) -> Result<()> {
    let pred = build_cu_prediction(s, x0, y0, size, pus)?;
    write_pred_block(&mut s.recon.y, x0, y0, pred.size, pred.size, &pred.y);
    let csize = (size >> 1).max(1);
    write_pred_block(&mut s.recon.cb, x0 >> 1, y0 >> 1, csize, csize, &pred.cb);
    write_pred_block(&mut s.recon.cr, x0 >> 1, y0 >> 1, csize, csize, &pred.cr);
    Ok(())
}

/// Write one CU's motion-compensated prediction straight to the picture
/// (`PERF-PROGRAMME.md` item B1: `write_inter_cu_no_residual`'s own 9.32%
/// share was this loop calling [`crate::framebuf::Plane::set`] once per
/// sample — a bounds-checked 2-D index plus a separate `ready`-bitmap write,
/// both recomputed every pixel). Row-wise instead: one bounds-checked slice
/// per row from [`crate::framebuf::Plane::row_mut`], a tight per-sample
/// clamp-and-convert loop over that slice (no per-sample `Option`), then one
/// [`crate::framebuf::Plane::mark_row_ready`] call for the whole row.
/// This crate's own largest coding-tree size (§7.4.3.2.1's `CtbLog2SizeY`
/// never exceeds 6) — the fixed conversion-buffer size `write_pred_block`/
/// `write_block` both need since `ReconPlane::write_row` takes an
/// already-converted `&[u8]` rather than handing back a raw, writable
/// slice the way `Plane::row_mut` used to.
const MAX_CTB: usize = 64;

fn write_pred_block(
    plane: &mut crate::framebuf::ReconPlane<'_>,
    x0: i32,
    y0: i32,
    w: i32,
    h: i32,
    src: &[i32],
) {
    let (wu, hu) = (
        usize::try_from(w).unwrap_or(0),
        usize::try_from(h).unwrap_or(0),
    );
    let Ok(x0u) = usize::try_from(x0) else { return };
    // `ReconPlane::write_row` takes an already-converted `&[u8]` (tile
    // storage cannot hand back a raw, picture-wide `&mut [u8]` the way
    // `Plane::row_mut` could — see that method's own doc), so the i32-to-u8
    // clamp happens into this fixed, stack-allocated buffer first. `MAX_CTB`
    // (module-level, shared with `write_block`) is this crate's own largest
    // coding-tree size (§7.4.3.2.1's `CtbLog2SizeY` never exceeds 6), so no
    // real `wu` ever exceeds it.
    let mut buf = [0u8; MAX_CTB];
    let wu_clamped = wu.min(MAX_CTB);
    for row in 0..hu {
        let Ok(py) = usize::try_from(y0.saturating_add(i32::try_from(row).unwrap_or(0))) else {
            continue;
        };
        let row_start = row.saturating_mul(wu);
        let Some(src_row) = src.get(row_start..row_start.saturating_add(wu_clamped)) else {
            continue;
        };
        let Some(dst_row) = buf.get_mut(..wu_clamped) else {
            continue;
        };
        for (d, &s) in dst_row.iter_mut().zip(src_row) {
            *d = u8::try_from(s.clamp(0, 255)).unwrap_or(0);
        }
        plane.write_row(x0u, py, dst_row);
        plane.mark_row_ready(py, x0u, wu_clamped);
    }
}

/// A non-skip, non-intra coding unit: `part_mode`, then `prediction_unit()`
/// per PU (merge, or `ref_idx_l0`/`mvd_coding`/`mvp_l0_flag`), then
/// `rqt_root_cbf` and either a residual-free write or the transform tree.
fn decode_inter_cu(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    depth: u32,
) -> Result<()> {
    let size = 1i32 << log2_size;
    let at_min_cb = log2_size == s.shared.log2_min_cb_size;
    let amp_enabled = s.inter()?.amp_enabled;
    let part_mode = parse_part_mode_inter(cabac, ctx, at_min_cb, size, amp_enabled)?;
    let num_pus = part_mode.num_pus();

    let mut pu_motion: Vec<(PuRect, MotionInfo)> = Vec::new();
    let mut all_merged = true;
    let depth_u8 = u8::try_from(depth).unwrap_or(u8::MAX);

    // §8.7.2's edge-filter flags are set at every prediction-block edge as
    // well as every transform-block edge (Table 8-12's `bS` derivation reads
    // motion/reference differences at a PU boundary regardless of whether
    // the transform tree happens to split there too) — a CU whose
    // `part_mode` is not `TwoNx2N` but whose transform tree stays unsplit at
    // this CU's own depth (`rqt_root_cbf`'s single, whole-CU-sized leaf) has
    // an internal PU boundary with no transform-unit leaf of its own to mark
    // it. Marked here, once per PU's own top-left edge, since PUs tile the
    // CU exactly and a PU's own left/top edge is precisely its shared
    // boundary with the previous PU (or, for `pu_idx == 0`, the CU's own
    // edge — already marked by the transform tree, so re-marking it here is
    // redundant but harmless, `EdgeMarks` being a plain boolean OR). Deliberately
    // the plain (non-`tu_`) mark: this is a filterable edge, not necessarily
    // a transform-block edge (§8.7.2.4's non-zero-coefficient `bS`
    // condition must not fire here unless a transform-unit leaf also marked
    // it).
    let deblock_grid = crate::deblock::DEBLOCK_GRID;
    for pu_idx in 0..num_pus {
        let pu = part_mode.pu_rect(x0, y0, size, pu_idx);
        s.edges.mark_vert(pu.x, pu.y, pu.h, deblock_grid);
        s.edges.mark_horiz(pu.x, pu.y, pu.w, deblock_grid);
        let cm = ctx
            .merge_flag
            .first_mut()
            .ok_or(Error::InvalidData("merge_flag ctx"))?;
        let merge_flag = cabac.decode_decision(cm) != 0;

        let info =
            if merge_flag {
                let max_num_merge_cand = s.inter()?.max_num_merge_cand;
                let merge_idx = parse_merge_index(cabac, ctx, max_num_merge_cand)?;
                resolve_merge_candidate(
                    s,
                    x0,
                    y0,
                    size,
                    pu,
                    pu_idx,
                    part_mode,
                    merge_idx,
                    max_num_merge_cand,
                )?
            } else {
                all_merged = false;
                let is_b = s.inter()?.is_b;
                let inter_pred_idc = if is_b {
                    parse_inter_pred_idc(cabac, ctx, part_mode, size, depth)?
                } else {
                    InterPredIdc::L0
                };

                let l0 = if inter_pred_idc == InterPredIdc::L1 {
                    None
                } else {
                    let num_ref_idx_l0 = s.inter()?.ref_pics_l0.len();
                    let ref_idx = parse_ref_idx(cabac, ctx, num_ref_idx_l0)?;
                    let mvd = parse_mvd(cabac, ctx)?;
                    let mvp_idx = parse_mvp_idx(cabac, ctx)?;
                    let (cur_poc, target_ref_poc, log2_pml, has_collocated) = {
                        let inter = s.inter()?;
                        let target_ref_poc = inter.ref_pics_l0.get(ref_idx).map(|r| r.poc).ok_or(
                            Error::InvalidData("vaco-codec-hevc: ref_idx_l0 out of range"),
                        )?;
                        (
                            inter.cur_poc,
                            target_ref_poc,
                            inter.log2_parallel_merge_level,
                            inter.collocated.is_some(),
                        )
                    };
                    let temporal = if has_collocated {
                        temporal_candidate(
                            s,
                            pu.x,
                            pu.y,
                            pu.w,
                            pu.h,
                            cur_poc,
                            target_ref_poc,
                            RefList::L0,
                        )?
                    } else {
                        None
                    };
                    let cands = motion::derive_amvp_candidates(
                        &s.cu_grid,
                        pu,
                        log2_pml,
                        cur_poc,
                        target_ref_poc,
                        RefList::L0,
                        temporal,
                        |x, y| s.in_current_slice(x, y),
                    );
                    let predictor = cands.get(mvp_idx).copied().unwrap_or(Mv::ZERO);
                    Some(UniMotion {
                        mv: predictor.add_mvd(mvd),
                        ref_poc: target_ref_poc,
                    })
                };

                let l1 = if inter_pred_idc == InterPredIdc::L0 {
                    None
                } else {
                    let (num_ref_idx_l1, mvd_l1_zero) = {
                        let inter = s.inter()?;
                        (inter.ref_pics_l1.len(), inter.mvd_l1_zero)
                    };
                    let ref_idx = parse_ref_idx(cabac, ctx, num_ref_idx_l1)?;
                    // §7.3.8.6's own presence condition: `mvd_coding(x0, y0, 1)`
                    // is skipped (`MvdL1` inferred `(0, 0)`) exactly when
                    // `mvd_l1_zero_flag` is set *and* this PU is bi-predictive —
                    // an L1-only PU (`inter_pred_idc == PRED_L1`) always reads
                    // its own `mvd_coding`, regardless of `mvd_l1_zero_flag`.
                    let mvd = if mvd_l1_zero && inter_pred_idc == InterPredIdc::Bi {
                        Mv::ZERO
                    } else {
                        parse_mvd(cabac, ctx)?
                    };
                    let mvp_idx = parse_mvp_idx(cabac, ctx)?;
                    let (cur_poc, target_ref_poc, log2_pml, has_collocated) = {
                        let inter = s.inter()?;
                        let target_ref_poc = inter.ref_pics_l1.get(ref_idx).map(|r| r.poc).ok_or(
                            Error::InvalidData("vaco-codec-hevc: ref_idx_l1 out of range"),
                        )?;
                        (
                            inter.cur_poc,
                            target_ref_poc,
                            inter.log2_parallel_merge_level,
                            inter.collocated.is_some(),
                        )
                    };
                    let temporal = if has_collocated {
                        temporal_candidate(
                            s,
                            pu.x,
                            pu.y,
                            pu.w,
                            pu.h,
                            cur_poc,
                            target_ref_poc,
                            RefList::L1,
                        )?
                    } else {
                        None
                    };
                    let cands = motion::derive_amvp_candidates(
                        &s.cu_grid,
                        pu,
                        log2_pml,
                        cur_poc,
                        target_ref_poc,
                        RefList::L1,
                        temporal,
                        |x, y| s.in_current_slice(x, y),
                    );
                    let predictor = cands.get(mvp_idx).copied().unwrap_or(Mv::ZERO);
                    Some(UniMotion {
                        mv: predictor.add_mvd(mvd),
                        ref_poc: target_ref_poc,
                    })
                };

                MotionInfo { l0, l1 }
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
        s.cu_grid
            .fill(bx0, by0, blocks_w, blocks_h, depth_u8, DC_IDX);
        s.cu_grid
            .fill_motion(bx0, by0, blocks_w, blocks_h, info, false);
        pu_motion.push((pu, info));
    }

    // §7.3.8.5's own presence condition: `rqt_root_cbf` is inferred `1`
    // (transform tree always runs) exactly when the CU is a single merged
    // `PART_2Nx2N` PU; otherwise it is genuinely parsed.
    let rqt_root_cbf = if part_mode == PartMode::TwoNx2N && all_merged {
        true
    } else {
        let cm = ctx
            .qt_root_cbf
            .first_mut()
            .ok_or(Error::InvalidData("qt_root_cbf ctx"))?;
        cabac.decode_decision(cm) != 0
    };

    if rqt_root_cbf {
        let pred = build_cu_prediction(s, x0, y0, size, &pu_motion)?;
        let max_depth = s.shared.max_transform_hierarchy_depth_inter;
        // §7.4.9.8's `interSplitFlag`, whose gate is
        // `max_transform_hierarchy_depth_inter == 0` — the SPS syntax element
        // itself, not HM's `QuadtreeTUMaxDepthInter`, which is that value plus
        // one and which HM therefore tests against `1`.
        let inter_split_flag = u32::from(max_depth == 0 && part_mode != PartMode::TwoNx2N);
        let quadtree_tu_log2_min =
            quadtree_tu_log2_min_in_cu(s, log2_size, max_depth, inter_split_flag);
        transform_tree_inter(
            cabac,
            ctx,
            s,
            x0,
            y0,
            log2_size,
            0,
            inter_split_flag != 0,
            &pred,
            quadtree_tu_log2_min,
            true,
            true,
        )?;
    } else {
        // Same gap `decode_skip_cu` has, and the same fix: no transform
        // tree runs on this path, so nothing else marks this CU's own
        // left/top boundary as a (trivially residual-free) transform-block
        // edge — see that function's own comment for why HM marks it
        // unconditionally regardless of `rqt_root_cbf`.
        let grid = crate::deblock::DEBLOCK_GRID;
        s.edges.mark_tu_vert(x0, y0, size, grid);
        s.edges.mark_tu_horiz(x0, y0, size, grid);
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
    s: &mut Ctx<'_, '_, '_, '_>,
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
    let split =
        if (force_split_at_root && trafo_depth == 0) || log2_size > s.shared.log2_max_tb_size {
            true
        } else if log2_size == s.shared.log2_min_tb_size || log2_size == quadtree_tu_log2_min {
            false
        } else {
            let ctx_idx = usize::try_from(5u32.saturating_sub(log2_size)).unwrap_or(0);
            let cm = ctx
                .trans_subdiv_flag
                .get_mut(ctx_idx)
                .ok_or(Error::InvalidData("trans_subdiv ctx"))?;
            cabac.decode_decision(cm) != 0
        };

    let chroma_splittable = log2_size > 2;
    let cbf_cb = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cb {
            let cm = ctx
                .qt_cbf
                .get_mut(5 + trafo_depth.min(4) as usize)
                .ok_or(Error::InvalidData("cbf_cb ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cb
    };
    let cbf_cr = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cr {
            let cm = ctx
                .qt_cbf
                .get_mut(5 + trafo_depth.min(4) as usize)
                .ok_or(Error::InvalidData("cbf_cr ctx"))?;
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
            transform_tree_inter(
                cabac,
                ctx,
                s,
                x0 + dx,
                y0 + dy,
                log2_size - 1,
                trafo_depth + 1,
                force_split_at_root,
                pred,
                quadtree_tu_log2_min,
                cbf_cb,
                cbf_cr,
            )?;
        }
        return Ok(());
    }

    // §7.3.8.8's own presence condition for an inter CU's leaf:
    // `cbf_luma` is only actually parsed when `trafoDepth != 0` or either
    // chroma cbf is set; a root-level (`trafoDepth == 0`), all-chroma-zero
    // leaf infers it `1` instead (it must be, since `rqt_root_cbf` — the
    // only reason this transform tree exists at all — said *some* residual
    // is present, and this leaf is the only place left to carry it). An
    // intra CU's own leaf (`transform_unit`/`transform_tree`, not this
    // function) has no such condition: `CuPredMode == MODE_INTRA` alone
    // already satisfies HM's OR, so it is unconditionally parsed there.
    let cbf_luma = if trafo_depth != 0 || cbf_cb || cbf_cr {
        let luma_ctx_idx = usize::from(trafo_depth == 0);
        let cm = ctx
            .qt_cbf
            .get_mut(luma_ctx_idx)
            .ok_or(Error::InvalidData("cbf_luma ctx"))?;
        cabac.decode_decision(cm) != 0
    } else {
        true
    };

    transform_unit_inter(
        cabac, ctx, s, x0, y0, log2_size, cbf_luma, cbf_cb, cbf_cr, pred,
    )
}

#[allow(clippy::too_many_arguments)]
fn transform_unit_inter(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    cbf_luma: bool,
    cbf_cb: bool,
    cbf_cr: bool,
    pred: &CuPrediction,
) -> Result<()> {
    let grid = crate::deblock::DEBLOCK_GRID;
    let size = 1i32 << log2_size;
    s.edges.mark_tu_vert(x0, y0, size, grid);
    s.edges.mark_tu_horiz(x0, y0, size, grid);

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
        let (cx0, cy0, clog2) = if log2_size > 2 {
            (x0 >> 1, y0 >> 1, log2_size - 1)
        } else {
            ((x0 - 4).max(0) >> 1, (y0 - 4).max(0) >> 1, 2u32)
        };
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
            let (Ok(bx), Ok(by)) = (
                usize::try_from(x0 + i32::try_from(col).unwrap_or(0)),
                usize::try_from(y0 + i32::try_from(row).unwrap_or(0)),
            ) else {
                continue;
            };
            let v = buf
                .get(by * stride + bx)
                .copied()
                .unwrap_or(0)
                .clamp(0, 255);
            if let Some(slot) = out.get_mut(row * size + col) {
                *slot = u16::try_from(v).unwrap_or(0);
            }
        }
    }
    out
}

fn reconstruct_luma_inter(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    cbf: bool,
    pred_cu: &CuPrediction,
) -> Result<()> {
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
        let skip = read_transform_skip_flag(cabac, ctx, s, log2_size, 0)?;
        // §7.4.9.11's mode-dependent scan order is an intra-only rule — an
        // inter TU's `scanIdx` is always `0` (diagonal), HM's own
        // `getCoefScanIdx` returning `SCAN_DIAG` whenever `CuPredMode !=
        // MODE_INTRA`.
        let coeffs = residual::residual_coding(
            cabac,
            ctx,
            log2_size,
            crate::scan::ScanOrder::Diag,
            false,
            s.shared.sign_data_hiding && !s.cu_transquant_bypass,
        );
        let residual = if s.cu_transquant_bypass {
            transform::transquant_bypass(&coeffs.values, size)
        } else {
            // §8.6.4.1: DST-VII only for 4x4 *intra* luma.
            let kind = transform_kind(skip, false);
            let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
            let dequantised = transform::dequant(
                &coeffs.values,
                size,
                qp_y,
                s.shared.bit_depth_luma,
                &s.shared.scaling_matrices,
                transform::ScalingListKind::InterY,
            );
            transform::inverse_transform(&dequantised, size, kind, s.shared.bit_depth_luma)
        };
        transform::add_residual_clip(&mut pred, &residual, size, s.shared.bit_depth_luma);
    }
    write_block(&mut s.recon.y, x0, y0, size, &pred);
    // §8.7.2.4's `bS == 1` non-zero-coefficient condition reads this leaf's
    // own `cbf_luma` at deblocking time — see `CuGrid::cbf_luma_at`'s own
    // doc for why only the inter path ever needs to record it.
    let blocks = (size >> 2).max(1);
    let bx0 = usize::try_from(x0 >> 2).unwrap_or(0);
    let by0 = usize::try_from(y0 >> 2).unwrap_or(0);
    s.cu_grid.fill_cbf_luma(bx0, by0, blocks, blocks, cbf);
    Ok(())
}

fn reconstruct_chroma_inter(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    cx0: i32,
    cy0: i32,
    log2_size: u32,
    is_cb: bool,
    pred_cu: &CuPrediction,
) -> Result<()> {
    let size = 1usize << log2_size;
    let (cu_x0, cu_y0) = cu_origin_of(cx0 << 1, cy0 << 1, pred_cu.size);
    let (ccu_x0, ccu_y0) = (cu_x0 >> 1, cu_y0 >> 1);
    let src = if is_cb { &pred_cu.cb } else { &pred_cu.cr };
    let csize = (pred_cu.size >> 1).max(1);
    let mut pred = pred_slice(src, csize, cx0 - ccu_x0, cy0 - ccu_y0, size);

    let skip = read_transform_skip_flag(cabac, ctx, s, log2_size, 1)?;
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let qp = transform::chroma_qp(
        qp_y,
        if is_cb {
            s.shared.cb_qp_offset
        } else {
            s.shared.cr_qp_offset
        },
    );
    let coeffs = residual::residual_coding(
        cabac,
        ctx,
        log2_size,
        crate::scan::ScanOrder::Diag,
        true,
        s.shared.sign_data_hiding && !s.cu_transquant_bypass,
    );
    let residual = if s.cu_transquant_bypass {
        transform::transquant_bypass(&coeffs.values, size)
    } else {
        let kind = if is_cb {
            transform::ScalingListKind::InterCb
        } else {
            transform::ScalingListKind::InterCr
        };
        let dequantised = transform::dequant(
            &coeffs.values,
            size,
            qp,
            s.shared.bit_depth_chroma,
            &s.shared.scaling_matrices,
            kind,
        );
        transform::inverse_transform(
            &dequantised,
            size,
            transform_kind(skip, false),
            s.shared.bit_depth_chroma,
        )
    };
    transform::add_residual_clip(&mut pred, &residual, size, s.shared.bit_depth_chroma);

    let plane = if is_cb {
        &mut s.recon.cb
    } else {
        &mut s.recon.cr
    };
    write_block(plane, cx0, cy0, size, &pred);
    Ok(())
}

fn write_pred_chroma_only(
    s: &mut Ctx<'_, '_, '_, '_>,
    cx0: i32,
    cy0: i32,
    log2_size: u32,
    is_cb: bool,
    pred_cu: &CuPrediction,
) {
    let size = 1usize << log2_size;
    let (cu_x0, cu_y0) = cu_origin_of(cx0 << 1, cy0 << 1, pred_cu.size);
    let (ccu_x0, ccu_y0) = (cu_x0 >> 1, cu_y0 >> 1);
    let src = if is_cb { &pred_cu.cb } else { &pred_cu.cr };
    let csize = (pred_cu.size >> 1).max(1);
    let pred = pred_slice(src, csize, cx0 - ccu_x0, cy0 - ccu_y0, size);
    let plane = if is_cb {
        &mut s.recon.cb
    } else {
        &mut s.recon.cr
    };
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

/// The smallest transform-block `log2` size this CU may reach: `log2CbSize`
/// minus however many splits §7.3.8.8 permits below it, clamped into
/// `[MinTbLog2SizeY, MaxTbLog2SizeY]`. `max_depth` is
/// `max_transform_hierarchy_depth_intra` or `..._inter` per the CU's own
/// `CuPredMode` (the caller chooses); `extra_split_flag` is §7.4.9.8's
/// `IntraSplitFlag` (`PartMode == PART_NxN`) or its `interSplitFlag`, each of
/// which buys exactly one more level on top of `max_depth`.
///
/// `max_depth` is the SPS syntax element, **not** HM's
/// `QuadtreeTUMaxDepth{Intra,Inter}`, which is that element plus one. HM's
/// own `getQuadtreeTULog2MinSizeInCU` therefore subtracts the one back off
/// and gates `interSplitFlag` on its stored value being `1`; transcribing
/// that shape while feeding it the spec's value made every CU with
/// `max_transform_hierarchy_depth > 0`, and every non-`PART_2Nx2N` inter CU,
/// stop splitting one transform level early — a CABAC desync, not a
/// reconstruction error, since the `split_transform_flag` bins the stream
/// spent then go unread.
fn quadtree_tu_log2_min_in_cu(
    s: &Ctx<'_, '_, '_, '_>,
    log2_cb_size: u32,
    max_depth: u32,
    extra_split_flag: u32,
) -> u32 {
    let denom = max_depth + extra_split_flag;
    if log2_cb_size < s.shared.log2_min_tb_size + denom {
        s.shared.log2_min_tb_size
    } else {
        (log2_cb_size.saturating_sub(denom)).min(s.shared.log2_max_tb_size)
    }
}

#[allow(clippy::too_many_arguments)]
fn transform_tree(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
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
    let split = if intra_split_and_root || log2_size > s.shared.log2_max_tb_size {
        true
    } else if log2_size == s.shared.log2_min_tb_size || log2_size == quadtree_tu_log2_min {
        false
    } else {
        let ctx_idx = usize::try_from(5u32.saturating_sub(log2_size)).unwrap_or(0);
        let cm = ctx
            .trans_subdiv_flag
            .get_mut(ctx_idx)
            .ok_or(Error::InvalidData("trans_subdiv ctx"))?;
        cabac.decode_decision(cm) != 0
    };

    let chroma_splittable = log2_size > 2;
    let cbf_cb = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cb {
            let cm = ctx
                .qt_cbf
                .get_mut(5 + trafo_depth.min(4) as usize)
                .ok_or(Error::InvalidData("cbf_cb ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cb
    };
    let cbf_cr = if chroma_splittable {
        if trafo_depth == 0 || parent_cbf_cr {
            let cm = ctx
                .qt_cbf
                .get_mut(5 + trafo_depth.min(4) as usize)
                .ok_or(Error::InvalidData("cbf_cr ctx"))?;
            cabac.decode_decision(cm) != 0
        } else {
            false
        }
    } else {
        parent_cbf_cr
    };

    if split {
        let half = 1i32 << (log2_size - 1);
        for (i, (dx, dy)) in [(0, 0), (half, 0), (0, half), (half, half)]
            .into_iter()
            .enumerate()
        {
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
    let cm = ctx
        .qt_cbf
        .get_mut(luma_ctx_idx)
        .ok_or(Error::InvalidData("cbf_luma ctx"))?;
    let cbf_luma = cabac.decode_decision(cm) != 0;

    transform_unit(
        cabac,
        ctx,
        s,
        x0,
        y0,
        log2_size,
        blk_idx,
        cbf_luma,
        cbf_cb,
        cbf_cr,
        pus,
        luma_modes,
        chroma_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn transform_unit(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
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
    let grid = crate::deblock::DEBLOCK_GRID;
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
    s: &mut Ctx<'_, '_, '_, '_>,
    x0: i32,
    y0: i32,
    log2_size: u32,
    mode: u8,
    cbf: bool,
) -> Result<()> {
    let size = 1usize << log2_size;
    let line = intra_pred::build_reference_line(
        &s.recon.y,
        x0,
        y0,
        size,
        s.shared.bit_depth_luma,
        |nx, ny| {
            s.in_current_slice(nx, ny)
                && (!s.shared.constrained_intra_pred || s.cu_grid.inter_at(nx, ny).is_none())
        },
    );
    let filtered;
    let ref_line = if intra_pred::should_filter(mode, size, true) {
        filtered = intra_pred::filter_reference_line(
            &line,
            size,
            s.shared.bit_depth_luma,
            s.shared.strong_intra_smoothing,
        );
        &filtered
    } else {
        &line
    };
    let mut pred = vec![0u16; size * size];
    intra_pred::predict(
        mode,
        ref_line,
        size,
        s.shared.bit_depth_luma,
        true,
        &mut pred,
    );

    if cbf {
        let skip = read_transform_skip_flag(cabac, ctx, s, log2_size, 0)?;
        let order = intra_mode::scan_order_for_mode(mode, log2_size, false);
        let coeffs: Coeffs = residual::residual_coding(
            cabac,
            ctx,
            log2_size,
            order,
            false,
            s.shared.sign_data_hiding && !s.cu_transquant_bypass,
        );
        let residual = if s.cu_transquant_bypass {
            transform::transquant_bypass(&coeffs.values, size)
        } else {
            let kind = transform_kind(skip, log2_size == 2);
            let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
            let dequantised = transform::dequant(
                &coeffs.values,
                size,
                qp_y,
                s.shared.bit_depth_luma,
                &s.shared.scaling_matrices,
                transform::ScalingListKind::IntraY,
            );
            transform::inverse_transform(&dequantised, size, kind, s.shared.bit_depth_luma)
        };
        transform::add_residual_clip(&mut pred, &residual, size, s.shared.bit_depth_luma);
    }

    write_block(&mut s.recon.y, x0, y0, size, &pred);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    s: &mut Ctx<'_, '_, '_, '_>,
    cx0: i32,
    cy0: i32,
    log2_size: u32,
    mode: u8,
    is_cb: bool,
) -> Result<()> {
    let size = 1usize << log2_size;
    let plane = if is_cb { &s.recon.cb } else { &s.recon.cr };
    // `<< 1`: chroma-to-luma coordinate scaling for the 4:2:0 collocated
    // block CuGrid is indexed by (see `cu_origin_of`'s own callers' `cx0 <<
    // 1` precedent above).
    let line = intra_pred::build_reference_line(
        plane,
        cx0,
        cy0,
        size,
        s.shared.bit_depth_chroma,
        |nx, ny| {
            let (lx, ly) = (nx << 1, ny << 1);
            s.in_current_slice(lx, ly)
                && (!s.shared.constrained_intra_pred || s.cu_grid.inter_at(lx, ly).is_none())
        },
    );
    let mut pred = vec![0u16; size * size];
    // Chroma never smooths its reference samples at 4:2:0 (see the crate
    // doc), so no `should_filter`/`filter_reference_line` call here.
    intra_pred::predict(
        mode,
        &line,
        size,
        s.shared.bit_depth_chroma,
        false,
        &mut pred,
    );

    let skip = read_transform_skip_flag(cabac, ctx, s, log2_size, 1)?;
    let order = intra_mode::scan_order_for_mode(mode, log2_size, true);
    let qp_y = derive_qp_y(s.qg_qp_pred, s.cu_qp_delta_val);
    let qp = transform::chroma_qp(
        qp_y,
        if is_cb {
            s.shared.cb_qp_offset
        } else {
            s.shared.cr_qp_offset
        },
    );
    let coeffs = residual::residual_coding(
        cabac,
        ctx,
        log2_size,
        order,
        true,
        s.shared.sign_data_hiding && !s.cu_transquant_bypass,
    );
    let residual = if s.cu_transquant_bypass {
        transform::transquant_bypass(&coeffs.values, size)
    } else {
        let kind = if is_cb {
            transform::ScalingListKind::IntraCb
        } else {
            transform::ScalingListKind::IntraCr
        };
        let dequantised = transform::dequant(
            &coeffs.values,
            size,
            qp,
            s.shared.bit_depth_chroma,
            &s.shared.scaling_matrices,
            kind,
        );
        transform::inverse_transform(
            &dequantised,
            size,
            transform_kind(skip, false),
            s.shared.bit_depth_chroma,
        )
    };
    transform::add_residual_clip(&mut pred, &residual, size, s.shared.bit_depth_chroma);

    let plane_mut = if is_cb {
        &mut s.recon.cb
    } else {
        &mut s.recon.cr
    };
    write_block(plane_mut, cx0, cy0, size, &pred);
    Ok(())
}

fn predict_chroma_only(
    s: &mut Ctx<'_, '_, '_, '_>,
    cx0: i32,
    cy0: i32,
    log2_size: u32,
    mode: u8,
    is_cb: bool,
) {
    let size = 1usize << log2_size;
    let plane = if is_cb { &s.recon.cb } else { &s.recon.cr };
    let line = intra_pred::build_reference_line(
        plane,
        cx0,
        cy0,
        size,
        s.shared.bit_depth_chroma,
        |nx, ny| {
            let (lx, ly) = (nx << 1, ny << 1);
            s.in_current_slice(lx, ly)
                && (!s.shared.constrained_intra_pred || s.cu_grid.inter_at(lx, ly).is_none())
        },
    );
    let mut pred = vec![0u16; size * size];
    intra_pred::predict(
        mode,
        &line,
        size,
        s.shared.bit_depth_chroma,
        false,
        &mut pred,
    );
    let plane_mut = if is_cb {
        &mut s.recon.cb
    } else {
        &mut s.recon.cr
    };
    write_block(plane_mut, cx0, cy0, size, &pred);
}

/// Write one intra-reconstructed transform block into the picture, one row
/// at a time (the same `PERF-PROGRAMME.md` B1 shape `write_pred_block`
/// already uses for inter prediction — this per-sample loop was the
/// remaining one B1's own profile pass did not name, since intra
/// reconstruction's cost is dominated elsewhere; converted here anyway
/// while B2 already touches every `Plane` write path). `block`'s own values
/// are already clamped to `[0, (1 << bit_depth) - 1]` by
/// `transform::add_residual_clip` (or are a raw prediction with cbf clear,
/// itself a weighted average of already-in-range reference samples), so the
/// `u8` narrowing below never actually clips.
fn write_block(
    plane: &mut crate::framebuf::ReconPlane<'_>,
    x0: i32,
    y0: i32,
    size: usize,
    block: &[u16],
) {
    let Ok(x0u) = usize::try_from(x0) else { return };
    // See `write_pred_block`'s own comment for why this goes through a
    // fixed conversion buffer rather than a raw `row_mut`-style slice.
    let mut buf = [0u8; MAX_CTB];
    let size_clamped = size.min(MAX_CTB);
    for row in 0..size {
        let Ok(py) = usize::try_from(y0.saturating_add(i32::try_from(row).unwrap_or(0))) else {
            continue;
        };
        let row_start = row.saturating_mul(size);
        let Some(src_row) = block.get(row_start..row_start.saturating_add(size_clamped)) else {
            continue;
        };
        let Some(dst_row) = buf.get_mut(..size_clamped) else {
            continue;
        };
        for (d, &s) in dst_row.iter_mut().zip(src_row) {
            *d = u8::try_from(s).unwrap_or(0);
        }
        plane.write_row(x0u, py, dst_row);
        plane.mark_row_ready(py, x0u, size_clamped);
    }
}
