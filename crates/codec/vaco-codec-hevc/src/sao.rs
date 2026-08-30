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
use vaco_core::Result;
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
    prev: &[CtuSao],
) -> Result<CtuSao> {
    let ctu_x = addr.checked_rem(ctbs_x).unwrap_or(0);
    let ctu_y = addr.checked_div(ctbs_x).unwrap_or(0);

    let left_merge = if ctu_x > 0 { decode_merge_flag(cabac, ctx)? } else { false };
    let above_merge = if ctu_y > 0 && !left_merge { decode_merge_flag(cabac, ctx)? } else { false };

    if left_merge || above_merge {
        let src_addr = if left_merge { addr.saturating_sub(1) } else { addr.saturating_sub(ctbs_x) };
        let src = usize::try_from(src_addr).ok().and_then(|i| prev.get(i)).copied().unwrap_or_default();
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
    height: i32,
    data: Vec<u16>,
}

impl Snapshot {
    fn capture(budget: &mut Budget, plane: &crate::framebuf::Plane) -> Result<Self> {
        let (width, height) = plane.dims();
        let mut data: Vec<u16> = budget.alloc(width.saturating_mul(height))?;
        let mut i = 0usize;
        for y in 0..height {
            for x in 0..width {
                if let Some(slot) = data.get_mut(i) {
                    *slot = plane.get(x, y);
                }
                i = i.saturating_add(1);
            }
        }
        Ok(Self { width: i32::try_from(width).unwrap_or(0), height: i32::try_from(height).unwrap_or(0), data })
    }

    /// `None` exactly when `(x, y)` is outside the picture — this crate's
    /// only "neighbour unavailable" case, per the module doc.
    fn get(&self, x: i32, y: i32) -> Option<i32> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let (Ok(xu), Ok(yu)) = (usize::try_from(x), usize::try_from(y)) else { return None };
        let idx = yu.saturating_mul(usize::try_from(self.width).unwrap_or(0)).saturating_add(xu);
        self.data.get(idx).copied().map(i32::from)
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
        u64::try_from(self.data.len()).unwrap_or(u64::MAX).saturating_mul(2)
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
    match mode {
        SaoMode::Off => {}
        SaoMode::Bo { offsets } => {
            let shift = bit_depth.saturating_sub(5);
            for y in y0..y0 + height {
                for x in x0..x0 + width {
                    let Some(v) = snapshot.get(x, y) else { continue };
                    let band = usize::try_from(v >> shift).unwrap_or(0);
                    let off = offsets.get(band).copied().unwrap_or(0);
                    plane.set_i32(x, y, (v + off).clamp(0, max_value));
                }
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
                for x in x0..x0 + width {
                    let Some(v) = snapshot.get(x, y) else { continue };
                    let (Some(a), Some(b)) = (snapshot.get(x + dx0, y + dy0), snapshot.get(x + dx1, y + dy1)) else {
                        continue;
                    };
                    let edge_type = sgn(v - a) + sgn(v - b);
                    let idx = usize::try_from(edge_type + 2).unwrap_or(2);
                    let off = offsets.get(idx).copied().unwrap_or(0);
                    plane.set_i32(x, y, (v + off).clamp(0, max_value));
                }
            }
        }
    }
}

/// Run SAO over the whole (already deblocked) picture, one CTU at a time in
/// raster order, using `s.sao_params[addr]` — the same array
/// [`parse_ctu_sao`] filled in during entropy decode.
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

    for (addr, params) in s.sao_params.iter().enumerate() {
        let addr = u32::try_from(addr).unwrap_or(0);
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
