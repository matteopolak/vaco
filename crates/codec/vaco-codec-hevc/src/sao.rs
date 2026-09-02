//! Sample Adaptive Offset: the per-CTU `sao()` bitstream syntax (§7.3.8.3)
//! and the filtering process (§8.7.3).
//!
//! Cross-checked against HM 18.0 (BSD-3-Clause, Tier A — see `cabac_ctx`'s
//! module doc for the clean-room posture): `TDecSbac::parseSAOBlkParam` for
//! the syntax (`TLibDecoder/TDecSbac.cpp`) and
//! `TComSampleAdaptiveOffset::offsetBlock`/`invertQuantOffsets`
//! (`TLibCommon/TComSampleAdaptiveOffset.cpp`) for the filtering process.
//!
//! # Why the filtering process here does not look like HM's
//!
//! HM's `offsetBlock` reuses per-row sign buffers (`m_signLineBuf1/2`)
//! across lines purely as a performance optimisation — a diagonal edge
//! class's `sgn(a - b)` for one row is the *negated* comparison the row
//! above already computed, so HM caches it rather than recomputing. That
//! optimisation only changes *how many times* a given sign comparison is
//! evaluated, never *which* comparison — every sign this module computes is
//! bit-for-bit the same comparison HM's cached one stands in for. Recomputing
//! it per pixel instead is simpler to get right and, at this crate's
//! scope (test-fixture-sized frames, correctness-first per the crate doc),
//! not worth the bookkeeping.
//!
//! The other simplification is real, not just cosmetic: HM's boundary
//! availability (`isLeftAvail`/`isAboveRightAvail`/etc., from
//! `deriveLoopFilterBoundaryAvailibility`) accounts for slice and tile
//! boundaries as well as the picture edge. This crate supports exactly one
//! slice segment and no tiles per picture (the crate doc), so "a
//! neighbouring sample is available" and "that sample's coordinate is
//! inside the picture" coincide exactly — the same reasoning
//! `framebuf`'s own module doc already applies to intra-prediction
//! neighbour availability. So every edge-offset computation below just
//! bounds-checks both neighbour coordinates directly, which reproduces
//! HM's own CTU-boundary-narrowing exactly within this crate's scope,
//! without needing to re-derive it per CTU.
//!
//! # Merge semantics
//!
//! `sao_merge_left_flag`/`sao_merge_up_flag` (read from **one** shared
//! CABAC context, matching HM's own `m_cSaoMergeSCModel`) copy a whole
//! CTU's already-resolved [`CtuSao`] (all three components at once) from
//! the raster-adjacent CTU to its left or above — never both; a left merge
//! makes the above-merge flag itself absent (`isLeftMerge` short-circuits
//! it in HM, and the spec's own `sao()` syntax table does the same). Both
//! are only present at all when the corresponding neighbour exists in this
//! picture (`rx > 0` / `ry > 0`), which in this crate's one-slice/no-tile
//! scope is the whole presence condition — no slice/tile-boundary check
//! needed on top.

use vaco_codec_cabac::CabacDecoder;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::cabac_ctx::ContextBank;
use crate::ctu::Ctx;

/// `getMaxOffsetQVal`, Table 9-32: this crate's 8-bit-only scope (the crate
/// doc) collapses `min(bitDepth, 10)` to the literal `8`.
const MAX_OFFSET_Q_VAL: u32 = (1 << (8 - 5)) - 1;

/// One component's resolved SAO parameters for one CTU — always fully
/// resolved (never a raw "merge" pointer): a merge copies the neighbour's
/// already-resolved [`SaoMode`] outright, exactly like HM's
/// `reconstructBlkSAOParam` folding `SAO_MODE_MERGE` away immediately
/// rather than keeping it as a live reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SaoMode {
    #[default]
    Off,
    /// Band offset: the 5-bit band position `sao_band_position` names,
    /// already applied (§7.4.9.3's `(bandPos + i) % 32` mapping) to the
    /// full 32-band offset array — index directly by `sample >> 3` (8-bit).
    Bo { offsets: [i32; 32] },
    /// Edge offset: `class` is `sao_eo_class` (0..=3: 0°, 90°, 135°, 45°),
    /// `offsets` is indexed by `edgeType + 2` (`edgeType` in `-2..=2`),
    /// i.e. `[FULL_VALLEY, HALF_VALLEY, PLAIN(=0), HALF_PEAK, FULL_PEAK]` —
    /// clause 8.7.3.2's own `SaoOffsetVal` layout, matching HM's
    /// `SAOEOClasses` enum order exactly.
    Eo { class: u8, offsets: [i32; 5] },
}

/// One CTU's fully-resolved SAO parameters, all three components.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CtuSao {
    pub y: SaoMode,
    pub cb: SaoMode,
    pub cr: SaoMode,
}

/// `sao_params`'s own row-banded storage — PERF-PROGRAMME.md item B4,
/// Stage 1 step 3's third and last piece, after `EdgeMarks` and `CuGrid`
/// (see `framebuf.rs`'s own "Stage 1" section doc for the shared reasoning:
/// every read here targets either the same CTU row currently being decoded
/// or an earlier, already-finished one, so a coarse once-per-row freeze is
/// enough, same as those two).
///
/// Simpler than either: `sao_params` is naturally CTU-granularity, not
/// 4x4-block granularity, so one row band *is* one CTU row outright —
/// there is no block-within-a-band remainder to track, hence no
/// `band_of`/`local_of` pair here the way `EdgeMarks`/`CuGrid` both need.
/// `current`/`published` are `Option<Vec<CtuSao>>`/[`crate::wavefront::
/// RowPublish<Vec<CtuSao>>`](crate::wavefront::RowPublish) rather than a
/// bespoke band type, since one CTU row's own data already *is* a flat
/// `Vec<CtuSao>` with no other fields to bundle alongside it. `current` is
/// `Option`, not a plain value, for the same reason `CuGrid::current` is:
/// `CtuSao` is `Budget`-tracked (`Ctx::new`'s own `budget.alloc` used to
/// charge the whole grid up front; `begin_row` now charges one row at a
/// time instead), so `finish` must not need to allocate a throwaway
/// replacement it would have to charge against a `Budget` right as the
/// whole grid is about to be released.
///
/// `published` is `RowPublish`, not a plain `Vec` (Stage 2b step 1b,
/// `docs/codec/hevc-wavefront-threading.md`): the same latent data race
/// that document names for `EdgeMarks`/`CuGrid` applied here too — a
/// plain `Vec<Vec<CtuSao>>` is safe only because exactly one worker calls
/// `begin_row`/`set`/`finish` today.
///
/// Step 3's first commit splits `shared` (geometry plus `published`) into
/// its own type, [`SaoParamsGridShared`], separate from `current`/
/// `current_band` — the same move `EdgeMarks` made, for the same reason
/// (`docs/codec/hevc-wavefront-threading.md`'s "step 1 closed only half of
/// each race"): `RowPublish` alone fixed reads; a future `Arc` around
/// `SaoParamsGridShared` alone, with no `current` inside it, is what makes
/// the write side shareable. `SaoParamsGrid` still bundles both today
/// (single-threaded), but every method already routes through
/// `self.shared`/`self.current` explicitly.
#[derive(Debug, Clone)]
struct SaoParamsGridShared {
    ctbs_x: usize,
    /// Total row bands (CTU rows) in the picture.
    n_bands: usize,
    /// Every CTU row strictly before `current_band`, published the moment
    /// [`SaoParamsGrid::begin_row`]/[`SaoParamsGrid::finish`] moved past
    /// it — the read side ([`SaoParamsGrid::get`]) for any row not in
    /// `current`.
    published: crate::wavefront::RowPublish<Vec<CtuSao>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SaoParamsGrid {
    shared: SaoParamsGridShared,
    /// The CTU row [`SaoParamsGrid::set`] currently writes into; every
    /// earlier row already lives in `shared.published`.
    current_band: usize,
    current: Option<Vec<CtuSao>>,
}

impl SaoParamsGrid {
    /// # Errors
    /// [`vaco_core::Error`] if the first row's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, ctbs_x: u32, ctbs_y: u32) -> Result<Self> {
        let ctbs_x = usize::try_from(ctbs_x).unwrap_or(0).max(1);
        let n_bands = usize::try_from(ctbs_y).unwrap_or(0).max(1);
        let current: Vec<CtuSao> = budget.alloc(ctbs_x)?;
        Ok(Self {
            shared: SaoParamsGridShared { ctbs_x, n_bands, published: crate::wavefront::RowPublish::new(n_bands) },
            current_band: 0,
            current: Some(current),
        })
    }

    /// Total addressable CTUs (`ctbs_x * ctbs_y`) — every raster address
    /// [`SaoParamsGrid::get`]/[`SaoParamsGrid::set`] can name, matching
    /// what [`Ctx::new`]'s own `total_ctbs` used to size the flat
    /// `Vec<CtuSao>` this replaces.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.shared.ctbs_x.saturating_mul(self.shared.n_bands)
    }

    /// Advance to CTU row `row`: publish `current` and allocate a fresh
    /// one, once, for the new row — the same-shaped counterpart of
    /// [`crate::framebuf::EdgeMarks::begin_row`]/
    /// [`crate::framebuf::CuGrid::begin_row`], called from the same call
    /// sites right alongside them. Idempotent for a `row` already current,
    /// including once, harmlessly, for row `0`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row` goes backward, the new row's
    /// allocation exceeds `budget`, or (unreachable in practice, for the
    /// same reason [`crate::framebuf::EdgeMarks::begin_row`]'s own
    /// `Errors` section gives) [`crate::wavefront::RowPublish`] itself
    /// refuses a publish.
    pub(crate) fn begin_row(&mut self, budget: &mut Budget, row: usize) -> Result<()> {
        if row < self.current_band {
            return Err(Error::InvalidData("vaco-codec-hevc: sao params rows must advance in order"));
        }
        while self.current_band < row {
            if let Some(band) = self.current.take() {
                self.shared.published.publish(self.current_band, band)?;
            }
            self.current = Some(budget.alloc(self.shared.ctbs_x)?);
            self.current_band = self.current_band.saturating_add(1);
        }
        Ok(())
    }

    /// Publish the last CTU row once the whole CTU walk is done, and
    /// advance `current_band` one past the last real row — see
    /// `framebuf.rs`'s own "Stage 1" section doc for why every type built
    /// this way needs exactly this move. Called once, right alongside
    /// [`crate::framebuf::EdgeMarks::finish`]/
    /// [`crate::framebuf::CuGrid::finish`], before [`filter_picture`] ever
    /// reads this grid.
    ///
    /// # Errors
    /// [`vaco_core::Error`], unreachable in practice for the same reason
    /// [`SaoParamsGrid::begin_row`]'s own `Errors` section gives.
    pub(crate) fn finish(&mut self) -> Result<()> {
        while self.current_band < self.shared.n_bands {
            let Some(band) = self.current.take() else { break };
            self.shared.published.publish(self.current_band, band)?;
            self.current_band = self.current_band.saturating_add(1);
        }
        self.current_band = self.shared.n_bands;
        Ok(())
    }

    /// The CTU at raster address `addr`'s already-resolved SAO parameters,
    /// or [`CtuSao::default`] (every mode `Off`) if `addr` is out of range
    /// or not yet decoded — the same fallback
    /// [`prev.get(i).copied().unwrap_or_default()`](Self::get) calls used
    /// to spell out at each of its two call sites before this existed.
    #[must_use]
    /// `addr`'s own (row, col) — the CTU row it lives in, and its column
    /// within that row.
    #[allow(clippy::integer_division, reason = "row/col = raster address / the fixed CTU row width, its own remainder")]
    fn row_col(&self, addr: usize) -> (usize, usize) {
        let ctbs_x = self.shared.ctbs_x.max(1);
        (addr / ctbs_x, addr % ctbs_x)
    }

    #[must_use]
    pub(crate) fn get(&self, addr: u32) -> CtuSao {
        let Ok(addr) = usize::try_from(addr) else { return CtuSao::default() };
        let (row, col) = self.row_col(addr);
        let band = match row.cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.as_ref(),
            std::cmp::Ordering::Less => self.shared.published.get(row),
            std::cmp::Ordering::Greater => None,
        };
        band.and_then(|b| b.get(col)).copied().unwrap_or_default()
    }

    /// Record CTU `addr`'s resolved SAO parameters — called once per CTU,
    /// from [`decode_ctu`](crate::ctu::decode_ctu), right after
    /// [`parse_ctu_sao`] resolves them. A write targeting any row but the
    /// one currently open is a caller error this degrades from silently
    /// (every real call targets the CTU just decoded, always in
    /// `current`'s own row).
    pub(crate) fn set(&mut self, addr: u32, value: &CtuSao) {
        let Ok(addr) = usize::try_from(addr) else { return };
        let (row, col) = self.row_col(addr);
        if row != self.current_band {
            return;
        }
        if let Some(slot) = self.current.as_mut().and_then(|b| b.get_mut(col)) {
            *slot = *value;
        }
    }

    /// The total bytes [`Budget::alloc`] charged across every CTU row's own
    /// array — summed the same way [`crate::framebuf::CuGrid::budget_bytes`]
    /// sums its own bands, for the same reason: self-consistent with
    /// whatever [`SaoParamsGrid::new`]/[`SaoParamsGrid::begin_row`] have
    /// actually charged so far, at any point in this grid's lifetime.
    #[must_use]
    pub(crate) fn budget_bytes(&self) -> u64 {
        let size = u64::try_from(std::mem::size_of::<CtuSao>()).unwrap_or(u64::MAX);
        let band_bytes = |b: &Vec<CtuSao>| u64::try_from(b.len()).unwrap_or(u64::MAX).saturating_mul(size);
        let published: u64 = self.shared.published.iter().map(band_bytes).fold(0u64, u64::saturating_add);
        published.saturating_add(self.current.as_ref().map_or(0, band_bytes))
    }
}
/// `sgn`, HM `TComRom.h` — used identically for every edge-offset sign
/// comparison.
fn sgn(v: i32) -> i32 {
    v.signum()
}

/// `parseSaoMaxUvlc`: a truncated-unary-coded, bypass-only value in
/// `0..=max_symbol`.
fn parse_max_uvlc(cabac: &mut CabacDecoder<'_>, max_symbol: u32) -> u32 {
    if max_symbol == 0 {
        return 0;
    }
    if cabac.decode_bypass() == 0 {
        return 0;
    }
    let mut i = 1u32;
    loop {
        if cabac.decode_bypass() == 0 {
            break;
        }
        i += 1;
        if i == max_symbol {
            break;
        }
    }
    i
}

/// Parse one component's `sao_offset_abs`/`sao_offset_sign`/
/// `sao_band_position` or `sao_eo_class` (§7.3.8.3, the `SAO_MODE_NEW`
/// branch of HM's `parseSAOBlkParam`), given the already-decided top-level
/// `sao_type_idx` (`None` = off, `Some(false)` = BO, `Some(true)` = EO) and,
/// for a chroma component that is not the channel's first
/// (`compIdx == firstCompOfChType` in HM), the already-decoded sibling's
/// mode to copy the type/class from without reading any more bits.
fn parse_component(cabac: &mut CabacDecoder<'_>, is_new: bool, is_bo: bool, reads_class: bool, shared_class: u8) -> SaoMode {
    if !is_new {
        return SaoMode::Off;
    }
    let mut raw = [0i32; 4];
    for slot in &mut raw {
        *slot = i32::try_from(parse_max_uvlc(cabac, MAX_OFFSET_Q_VAL)).unwrap_or(0);
    }
    if is_bo {
        for v in &mut raw {
            if *v != 0 && cabac.decode_bypass() != 0 {
                *v = -*v;
            }
        }
        let band_position = cabac.decode_bypass_bits(5);
        let mut offsets = [0i32; 32];
        for (i, &v) in raw.iter().enumerate() {
            let band_position = usize::try_from(band_position).unwrap_or(0);
            let idx = (band_position + i) % 32;
            if let Some(slot) = offsets.get_mut(idx) {
                *slot = v;
            }
        }
        SaoMode::Bo { offsets }
    } else {
        let class = if reads_class { u8::try_from(cabac.decode_bypass_bits(2)).unwrap_or(0) } else { shared_class };
        // §7.3.8.3's own EO layout: offset[PLAIN] is always inferred 0 (never
        // coded), and the two "peak" classes negate the coded magnitude —
        // HM's `parseSAOBlkParam` assigns exactly this shape.
        let get = |i: usize| raw.get(i).copied().unwrap_or(0);
        let offsets = [get(0), get(1), 0, -get(2), -get(3)];
        SaoMode::Eo { class, offsets }
    }
}

/// Parse one CTU's whole `sao()` syntax element, resolving any merge
/// against `prev` (every already-decoded CTU's [`CtuSao`], indexed by
/// raster address — only `addr - 1` (left) and `addr - ctbs_x` (above) are
/// ever read).
pub(crate) fn parse_ctu_sao(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    addr: u32,
    ctbs_x: u32,
    sao_luma: bool,
    sao_chroma: bool,
    prev: &SaoParamsGrid,
) -> Result<CtuSao> {
    let ctu_x = addr.checked_rem(ctbs_x).unwrap_or(0);
    let ctu_y = addr.checked_div(ctbs_x).unwrap_or(0);

    let left_merge = if ctu_x > 0 { decode_merge_flag(cabac, ctx)? } else { false };
    let above_merge = if ctu_y > 0 && !left_merge { decode_merge_flag(cabac, ctx)? } else { false };

    if left_merge || above_merge {
        let src_addr = if left_merge { addr.saturating_sub(1) } else { addr.saturating_sub(ctbs_x) };
        let src = prev.get(src_addr);
        // A merge still forces a channel's mode to `Off` when this slice
        // has that channel's SAO disabled entirely (`sliceEnabled[compIdx]`
        // in HM) — a slice can turn SAO off for chroma while keeping it on
        // for luma (`slice_sao_luma_flag`/`slice_sao_chroma_flag` are
        // independent bits), and a merge must not resurrect the disabled
        // side from a neighbour decoded before the flags changed... though
        // in this crate's single-slice scope the flags cannot change
        // mid-picture; kept for fidelity to the spec's own rule regardless.
        return Ok(CtuSao {
            y: if sao_luma { src.y } else { SaoMode::Off },
            cb: if sao_chroma { src.cb } else { SaoMode::Off },
            cr: if sao_chroma { src.cr } else { SaoMode::Off },
        });
    }

    // New-or-off mode: luma first, then Cb (which may read its own
    // type/class), then Cr (which never reads `sao_type_idx` or
    // `sao_eo_class` — it copies Cb's, per HM's `firstCompOfChType` rule —
    // but reads its own offsets/sign/band-position exactly like Cb does).
    let y = if sao_luma {
        let (is_new, is_bo) = read_type_idx(cabac, ctx)?;
        parse_component(cabac, is_new, is_bo, true, 0)
    } else {
        SaoMode::Off
    };

    let (cb, cr) = if sao_chroma {
        let (is_new, is_bo) = read_type_idx(cabac, ctx)?;
        let cb = parse_component(cabac, is_new, is_bo, true, 0);
        let cb_class = if let SaoMode::Eo { class, .. } = cb { class } else { 0 };
        let cr = parse_component(cabac, is_new, is_bo, false, cb_class);
        (cb, cr)
    } else {
        (SaoMode::Off, SaoMode::Off)
    };

    Ok(CtuSao { y, cb, cr })
}

/// `parseSaoMerge`: the single shared context both `sao_merge_left_flag` and
/// `sao_merge_up_flag` are coded with.
fn decode_merge_flag(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank) -> Result<bool> {
    let cm = ctx.sao_merge_flag.first_mut().ok_or(vaco_core::Error::InvalidData("sao_merge_flag ctx"))?;
    Ok(cabac.decode_decision(cm) != 0)
}

/// `parseSaoTypeIdx`: one context-coded bin (0 = off), then, only if set, one
/// bypass bin distinguishing BO (0) from EO (1). Returns `(is_new, is_bo)`.
fn read_type_idx(cabac: &mut CabacDecoder<'_>, ctx: &mut ContextBank) -> Result<(bool, bool)> {
    let cm = ctx.sao_type_idx.first_mut().ok_or(vaco_core::Error::InvalidData("sao_type_idx ctx"))?;
    if cabac.decode_decision(cm) == 0 {
        return Ok((false, false));
    }
    let is_bo = cabac.decode_bypass() == 0;
    Ok((true, is_bo))
}

/// A read-only, bounds-checked copy of one plane's samples — the "source"
/// side of §8.7.3's filtering process, since every CTU's SAO output must be
/// computed from the *pre-SAO* (post-deblocked) picture, including where
/// it reads a neighbouring CTU's samples that this same pass may also be
/// about to rewrite (HM's own `m_tempPicYuv` snapshot serves the identical
/// purpose).
struct Snapshot {
    width: i32,
    data: Vec<u8>,
}

impl Snapshot {
    /// `PERF-PROGRAMME.md` item B1: this used to be a per-sample
    /// [`crate::framebuf::Plane::get`] loop (`Snapshot::capture`'s own 8.08%
    /// share of decode). `Plane::clone_samples` copies the same row-major
    /// data in one `Budget`-charged allocation plus one `copy_from_slice` —
    /// a whole-plane `memcpy`, since a snapshot's layout is exactly the
    /// source plane's own layout.
    fn capture(budget: &mut Budget, plane: &crate::framebuf::Plane) -> Result<Self> {
        let (width, _height) = plane.dims();
        let data = plane.clone_samples(budget)?;
        Ok(Self { width: i32::try_from(width).unwrap_or(0), data })
    }

    /// One full row of captured samples, `None` past the last row (including
    /// every row past `data.len() / width`, this struct's own implicit
    /// height) — [`offset_block`]'s row-wise replacement for a per-sample
    /// 2-D bounds check (`PERF-PROGRAMME.md` item B1): fetched once per row
    /// instead of once per pixel (twice more for a diagonal edge-offset
    /// class's two neighbour rows), it amortises the check that used to be
    /// repeated at every pixel.
    fn row(&self, y: i32) -> Option<&[u8]> {
        let (Ok(yu), Ok(width)) = (usize::try_from(y), usize::try_from(self.width)) else { return None };
        let start = yu.checked_mul(width)?;
        self.data.get(start..start.saturating_add(width))
    }

    /// The exact byte count [`Budget::alloc`] charged for `self.data` — for
    /// [`filter_picture`] to give back once its own per-CTU loop is done
    /// reading every snapshot, the same "working buffer, not a `Dpb`-held
    /// picture" reasoning [`crate::framebuf::CuGrid::budget_bytes`]'s own
    /// doc explains. Three of these (one per plane) are built on every slice
    /// SAO actually has syntax for (`slice_sao_luma_flag ||
    /// slice_sao_chroma_flag` — `libx265`'s own default), each the same size
    /// as one of `Picture`'s own three planes; left unreleased, this was a
    /// second, independent per-picture leak on top of `CuGrid`'s.
    #[must_use]
    fn byte_len(&self) -> u64 {
        u64::try_from(self.data.len()).unwrap_or(u64::MAX)
    }
}

/// Filter one component's plane for one CTU's rectangle, per §8.7.3.2/.3
/// (`TComSampleAdaptiveOffset::offsetBlock`).
#[allow(clippy::too_many_arguments, reason = "mirrors HM's own offsetBlock signature")]
fn offset_block(
    plane: &mut crate::framebuf::Plane,
    snapshot: &Snapshot,
    mode: SaoMode,
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
    bit_depth: u32,
) {
    let max_value = (1i32 << bit_depth) - 1;
    let (Ok(x0u), Ok(width_u)) = (usize::try_from(x0), usize::try_from(width)) else { return };
    match mode {
        SaoMode::Off => {}
        // `PERF-PROGRAMME.md` item B1: `offset_block`'s own 5.35% share was
        // three per-sample `Plane::get`-shaped lookups (each a full 2-D bounds
        // check) plus a per-sample [`crate::framebuf::Plane::set_i32`]
        // write. Row-wise instead: [`Snapshot::row`]/
        // [`crate::framebuf::Plane::row_mut`] amortise the y-bounds check
        // across a whole row, and the write goes through one
        // bounds-checked slice instead of `set_i32`'s own per-element
        // `index()` call plus separate `ready`-bitmap write.
        SaoMode::Bo { offsets } => {
            let shift = bit_depth.saturating_sub(5);
            for y in y0..y0 + height {
                let Ok(yu) = usize::try_from(y) else { continue };
                let Some(src_row) = snapshot.row(y).and_then(|r| r.get(x0u..x0u.saturating_add(width_u))) else {
                    continue;
                };
                if let Some(dst_row) = plane.row_mut(yu).and_then(|r| r.get_mut(x0u..x0u.saturating_add(width_u))) {
                    for (d, &sv) in dst_row.iter_mut().zip(src_row) {
                        let v = i32::from(sv);
                        let band = usize::try_from(v >> shift).unwrap_or(0);
                        let off = offsets.get(band).copied().unwrap_or(0);
                        *d = u8::try_from((v + off).clamp(0, max_value)).unwrap_or(0);
                    }
                }
                plane.mark_row_ready(yu, x0u, width_u);
            }
        }
        SaoMode::Eo { class, offsets } => {
            let (dx0, dy0, dx1, dy1): (i32, i32, i32, i32) = match class {
                0 => (-1, 0, 1, 0),
                1 => (0, -1, 0, 1),
                2 => (-1, -1, 1, 1),
                _ => (1, -1, -1, 1),
            };
            for y in y0..y0 + height {
                // Fetched once per row rather than once per pixel (the old
                // shape called that per-sample lookup three times, each
                // re-deriving the same row's y-bounds check): a diagonal
                // class's neighbour row is constant across the whole row,
                // so `y + dy0`/`y + dy1` only need resolving here.
                let (Some(cur_row), Some(row_a), Some(row_b)) = (snapshot.row(y), snapshot.row(y + dy0), snapshot.row(y + dy1)) else {
                    continue;
                };
                let Ok(yu) = usize::try_from(y) else { continue };
                let Some(dst_row) = plane.row_mut(yu) else { continue };
                for x in x0..x0 + width {
                    let Ok(xu) = usize::try_from(x) else { continue };
                    let Some(&sv) = cur_row.get(xu) else { continue };
                    let (Some(&av), Some(&bv)) = (
                        usize::try_from(x + dx0).ok().and_then(|i| row_a.get(i)),
                        usize::try_from(x + dx1).ok().and_then(|i| row_b.get(i)),
                    ) else {
                        continue;
                    };
                    let v = i32::from(sv);
                    let edge_type = sgn(v - i32::from(av)) + sgn(v - i32::from(bv));
                    let idx = usize::try_from(edge_type + 2).unwrap_or(2);
                    let off = offsets.get(idx).copied().unwrap_or(0);
                    if let Some(slot) = dst_row.get_mut(xu) {
                        *slot = u8::try_from((v + off).clamp(0, max_value)).unwrap_or(0);
                    }
                }
                plane.mark_row_ready(yu, x0u, width_u);
            }
        }
    }
}

/// Run SAO over the whole (already deblocked) picture, one CTU at a time in
/// raster order, using `s.sao_params.get(addr)` — the same [`SaoParamsGrid`]
/// [`parse_ctu_sao`] filled in during entropy decode, by now fully
/// published (`decoder.rs` calls [`SaoParamsGrid::finish`] before this).
///
/// # Errors
/// [`vaco_core::Error`] if the read-only snapshot copies exceed `budget`.
pub(crate) fn filter_picture(budget: &mut Budget, s: &mut Ctx<'_>) -> Result<()> {
    if !s.sao_luma && !s.sao_chroma {
        return Ok(());
    }
    let ctb_size = 1i32 << s.log2_ctb_size;
    let ctbs_x = s.ctbs_x;

    let snap_y = Snapshot::capture(budget, &s.pic.y)?;
    let snap_cb = Snapshot::capture(budget, &s.pic.cb)?;
    let snap_cr = Snapshot::capture(budget, &s.pic.cr)?;

    for addr in 0..s.sao_params.len() {
        let addr = u32::try_from(addr).unwrap_or(0);
        let params = s.sao_params.get(addr);
        let col = addr.checked_rem(ctbs_x).unwrap_or(0);
        let row = addr.checked_div(ctbs_x).unwrap_or(0);
        let x0 = i32::try_from(col).unwrap_or(0) * ctb_size;
        let y0 = i32::try_from(row).unwrap_or(0) * ctb_size;
        let width = ctb_size.min(s.pic_width - x0);
        let height = ctb_size.min(s.pic_height - y0);
        if width <= 0 || height <= 0 {
            continue;
        }
        offset_block(&mut s.pic.y, &snap_y, params.y, x0, y0, width, height, s.bit_depth_luma);

        let (cx0, cy0, cw, ch) = (x0 >> 1, y0 >> 1, (width + 1) >> 1, (height + 1) >> 1);
        offset_block(&mut s.pic.cb, &snap_cb, params.cb, cx0, cy0, cw, ch, s.bit_depth_chroma);
        offset_block(&mut s.pic.cr, &snap_cr, params.cr, cx0, cy0, cw, ch, s.bit_depth_chroma);
    }
    // The three snapshots are pure working state for the loop just above —
    // give their charge back before they drop, rather than letting it ride
    // on `budget.committed()` until `Budget` itself is dropped. See
    // `Snapshot::byte_len`'s own doc.
    budget.release(snap_y.byte_len().saturating_add(snap_cb.byte_len()).saturating_add(snap_cr.byte_len()));
    Ok(())
}
