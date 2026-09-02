//! A private, plain reconstruction buffer, plus the per-4x4 metadata grids
//! the CTU walk needs for neighbour derivation.
//!
//! Same reasoning as `vaco-codec-av1`/`vaco-codec-vp8`/`vaco-codec-vp9`'s
//! identically-named `Plane`/`Picture` types (`xtask/src/dup_check.rs`'s
//! `DISTINCT` list covers this shape generically): intra prediction needs to
//! read already-written pixels of the very buffer being written, which does
//! not fit `vaco_frame::Plane`'s borrow shape. Copied into a real
//! `vaco_frame::Frame` once, at emission.
//!
//! # Availability, without re-deriving 6.4.1's z-scan addresses
//!
//! ITU-T H.265 §6.4.1 defines neighbour availability in terms of z-scan
//! address order within one slice and one tile. This crate supports exactly
//! one independent slice segment and no tiles per picture (see the crate
//! doc), so "already decoded, in this picture" and "available per §6.4.1"
//! coincide exactly. [`Plane::ready`] tracks the former directly — a bit set
//! the instant a pixel's final reconstructed value is written — which is
//! simpler than reconstructing z-scan addresses and cannot disagree with the
//! process it is standing in for, because within that scope it is not an
//! approximation of "already decoded", it *is* "already decoded".
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::intra_mode::DC_IDX;

/// A luma pixel coordinate's 4-sample block index — the whole reason
/// [`CuGrid`] exists at this granularity.
#[allow(clippy::integer_division, reason = "block index = pixel coordinate / the fixed 4-sample block size")]
const fn block_of(x: usize) -> usize {
    x / 4
}

/// One reconstruction plane: `u8` samples (this crate's whole scope is
/// 8-bit — see `check_scope`) plus a per-4x4-block "has this block been
/// written yet" grid, addressed at HEVC's own minimum transform-block
/// granularity rather than per pixel.
///
/// `PERF-PROGRAMME.md` item B2: storing `u16` here charged every sample
/// twice the memory traffic an 8-bit-only crate ever needed, and the
/// per-pixel `ready: Vec<bool>` was sixteen times larger than the
/// granularity anything ever queried it at. Every write this crate makes —
/// `write_block`/`write_pred_block`'s transform/prediction blocks, deblocking
/// and SAO's touch-ups to an already-fully-written picture — is at least a
/// 4x4 transform block and lands on that grid exactly (`transform_tree` never
/// splits smaller, and `pic_width`/`pic_height_in_luma_samples` are
/// themselves always CTB-grid- (hence 4x4-grid-) aligned per §7.4.3.2.1, so
/// there is no partial block at the plane's own edge to round awkwardly);
/// every *read* of availability ([`ReconPlane::is_ready`] — the CTU walk's
/// own in-progress reconstruction buffer, not this now-write-only `Plane`;
/// see `ReconPlane`'s own module doc below) is a per-pixel reference-sample
/// query that only ever needs "has the 4x4 block containing this pixel
/// been written", never finer. Collapsing `ready` to that grid is
/// therefore not an approximation — within this crate's scope it answers
/// the identical question the old per-pixel array did, at 1/16 the memory
/// and update cost.
#[derive(Debug, Clone)]
pub(crate) struct Plane {
    width: usize,
    height: usize,
    data: Vec<u8>,
    ready_cols: usize,
    ready_rows: usize,
    ready: Vec<bool>,
}

impl Plane {
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, width: usize, height: usize) -> Result<Self> {
        let len = width.saturating_mul(height);
        let data = budget.alloc(len)?;
        let ready_cols = width.div_ceil(4).max(1);
        let ready_rows = height.div_ceil(4).max(1);
        let ready = vec![false; ready_cols.saturating_mul(ready_rows)];
        Ok(Self { width, height, data, ready_cols, ready_rows, ready })
    }

    fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y * self.width + x)
    }

    fn ready_index(&self, bx: usize, by: usize) -> Option<usize> {
        if bx >= self.ready_cols || by >= self.ready_rows {
            return None;
        }
        Some(by * self.ready_cols + bx)
    }

    /// The sample at `(x, y)`, or `0` out of range or not yet written —
    /// callers reach this after the CTU walk's own
    /// `ReconPicture::materialize_into` has already marked the whole
    /// picture ready — deblocking, SAO and motion compensation all read
    /// reference pictures this way, never a still-in-progress one, so
    /// there is no readiness check left to make here. Widened to `u16` so
    /// every caller
    /// written against the crate's pre-B2 accessor keeps working unchanged;
    /// the plane's own storage is `u8`.
    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).and_then(|i| self.data.get(i)).copied().map_or(0, u16::from)
    }

    /// Write a final reconstructed sample and mark its 4x4 block available.
    pub(crate) fn set(&mut self, x: usize, y: usize, v: u16) {
        let byte = u8::try_from(v).unwrap_or(0);
        if let Some(i) = self.index(x, y)
            && let Some(slot) = self.data.get_mut(i)
        {
            *slot = byte;
        }
        self.mark_block_ready(x, y, 1, 1);
    }

    /// Mark every 4x4 block the pixel rectangle `[x0, x0+w) x [y0, y0+h)`
    /// touches as reconstructed — the single definition
    /// [`Plane::set`]/[`Plane::mark_row_ready`] both build on (D19), so the
    /// pixel-to-block-grid mapping exists in exactly one place.
    pub(crate) fn mark_block_ready(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        if w == 0 || h == 0 || x0 >= self.width || y0 >= self.height {
            return;
        }
        let x1 = x0.saturating_add(w).saturating_sub(1).min(self.width.saturating_sub(1));
        let y1 = y0.saturating_add(h).saturating_sub(1).min(self.height.saturating_sub(1));
        let (bx0, by0, bx1, by1) = (block_of(x0), block_of(y0), block_of(x1), block_of(y1));
        for by in by0..=by1 {
            for bx in bx0..=bx1 {
                if let Some(i) = self.ready_index(bx, by)
                    && let Some(slot) = self.ready.get_mut(i)
                {
                    *slot = true;
                }
            }
        }
    }
}

/// One decoded picture's reconstruction planes: luma plus two 4:2:0 chroma
/// planes (this crate's whole scope is 4:2:0 — see the crate doc).
#[derive(Debug, Clone)]
pub(crate) struct Picture {
    pub y: Plane,
    pub cb: Plane,
    pub cr: Plane,
}

impl Picture {
    /// # Errors
    /// [`vaco_core::Error`] if any plane's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, luma_width: usize, luma_height: usize) -> Result<Self> {
        let cw = luma_width.div_ceil(2);
        let ch = luma_height.div_ceil(2);
        Ok(Self {
            y: Plane::new(budget, luma_width, luma_height)?,
            cb: Plane::new(budget, cw, ch)?,
            cr: Plane::new(budget, cw, ch)?,
        })
    }

    /// The total bytes [`Budget::alloc`] charged for this picture's three
    /// planes — what [`crate::dpb::Dpb`] gives back via [`Budget::release`]
    /// the moment a picture is actually dropped from the DPB (evicted by
    /// `remove_unused`, or the whole buffer cleared for an IRAP), so a long
    /// sequence's running `Budget` total reflects pictures genuinely still
    /// held, not every picture ever decoded. See `dpb.rs`'s own module doc
    /// for the real fixture this was measured against: a 640x480, 25-frame,
    /// hierarchical-B `libx265` stream — whose own DPB legitimately holds
    /// more simultaneous pictures than any P-slice fixture ever did — hit
    /// `Budget`'s `max_alloc_total` cap before this existed, because nothing
    /// in this crate ever released a dropped picture's own charge.
    #[must_use]
    pub(crate) fn budget_bytes(&self) -> u64 {
        self.y.byte_len() + self.cb.byte_len() + self.cr.byte_len()
    }
}

/// Per-4x4-luma-block metadata the CTU walk needs from already-decoded
/// neighbours: the coding-quadtree depth (`split_cu_flag`'s `ctxInc`,
/// §9.3.4.2.2), the luma intra prediction mode (the MPM derivation,
/// §8.4.2), and the coding unit's own finalised luma `QpY` (§8.6.1's
/// `qPY_A`/`qPY_B` neighbour derivation, and [`crate::deblock`]'s own
/// per-edge `qP_P`/`qP_Q`).
///
/// Indexed at 4-sample granularity because that is HEVC's minimum coding
/// block size; a coding unit's whole footprint is painted in one pass when
/// its final depth/mode is known, so a later neighbour query is a plain
/// lookup rather than a tree walk. `QpY` is painted separately
/// ([`CuGrid::fill_qp`]) and later than depth/mode ([`CuGrid::fill`]):
/// unlike depth/mode, which are known before a coding unit's own transform
/// tree is walked, `QpY` depends on that coding unit's own `cu_qp_delta`
/// (if any is coded at all — see `ctu::maybe_parse_cu_qp_delta`), which is
/// only known once the whole transform tree has been read.
#[derive(Debug, Clone)]
pub(crate) struct CuGrid {
    cols: usize,
    rows: usize,
    depth: Vec<u8>,
    mode: Vec<u8>,
    written: Vec<bool>,
    qp: Vec<i8>,
    qp_written: Vec<bool>,
    /// Per-4x4-block motion, one optional [`crate::motion::UniMotion`] per
    /// reference list, for merge/AMVP spatial-neighbour derivation
    /// (§8.5.3.2) — both lists empty for every intra block, or a picture
    /// that has not reached this stage of the crate's scope yet (see
    /// `crate::motion`'s own module doc). `pred_l0`/`pred_l1` mirror
    /// `predFlagL0`/`predFlagL1`; a P-slice PU only ever sets `pred_l0`.
    /// `mv0_x`/`mv0_y`/`mv1_x`/`mv1_y` are quarter-pel, `i16` like HM's own
    /// `TComMv`; `ref_poc0`/`ref_poc1` are `i64` to match
    /// `crate::motion::UniMotion`'s own field, not because a POC needs more
    /// than 32 bits.
    pred_l0: Vec<bool>,
    pred_l1: Vec<bool>,
    is_skip: Vec<bool>,
    mv0_x: Vec<i16>,
    mv0_y: Vec<i16>,
    ref_poc0: Vec<i64>,
    mv1_x: Vec<i16>,
    mv1_y: Vec<i16>,
    ref_poc1: Vec<i64>,
    /// Per-4x4-block "this position's own luma transform block has one or
    /// more non-zero coefficient levels" — §8.7.2.4's `bS == 1` condition
    /// needs exactly this, restricted (by `crate::deblock`'s own caller) to
    /// positions that are *also* a transform-block edge; a plain
    /// prediction-unit-only boundary never consults it. Only ever written by
    /// an inter CU's own transform-unit leaf (`ctu::reconstruct_luma_inter`)
    /// — intra edges never read it, since either side being intra already
    /// forces `bS == 2` before this grid would matter.
    cbf_luma: Vec<bool>,
}

impl CuGrid {
    /// `has_l1` is `InterSliceParams::is_b` (`false` for an I or P slice's own
    /// grid) — a P/I slice's own `l1` arrays are charged at length `0`
    /// (`fill_motion`/`inter_at` degrade to "never populated" automatically:
    /// an empty `Vec`'s `get`/`get_mut` always return `None`, which reads
    /// back exactly as "this list unused", the correct answer for every
    /// block a P/I slice ever writes). This is not an optimisation of
    /// convenience: doubling every per-4x4-block motion array's footprint
    /// for a slice kind that can never populate `l1` at all is exactly the
    /// `Vec::with_capacity`-shaped budget hazard `AGENT-CONSTRAINTS.md` warns
    /// about applied to a genuine, measured case — a real `libx265` 640x480
    /// stock fixture (25 P-only frames, the exact "must not regress"
    /// fixture) started failing `Budget`'s `max_alloc_total` cap the moment
    /// `l1` support was added unconditionally, and stopped failing the
    /// moment this gating did.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if any grid's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, luma_width: usize, luma_height: usize, has_l1: bool) -> Result<Self> {
        let cols = luma_width.div_ceil(4).max(1);
        let rows = luma_height.div_ceil(4).max(1);
        let len = cols.saturating_mul(rows);
        let len_l1 = if has_l1 { len } else { 0 };
        Ok(Self {
            cols,
            rows,
            depth: budget.alloc(len)?,
            mode: budget.alloc(len)?,
            written: vec![false; len],
            qp: budget.alloc(len)?,
            qp_written: vec![false; len],
            pred_l0: vec![false; len],
            pred_l1: vec![false; len],
            is_skip: vec![false; len],
            mv0_x: budget.alloc(len)?,
            mv0_y: budget.alloc(len)?,
            ref_poc0: budget.alloc(len)?,
            mv1_x: budget.alloc(len_l1)?,
            mv1_y: budget.alloc(len_l1)?,
            ref_poc1: budget.alloc(len_l1)?,
            cbf_luma: vec![false; len],
        })
    }

    fn index(&self, bx: usize, by: usize) -> Option<usize> {
        if bx >= self.cols || by >= self.rows {
            return None;
        }
        Some(by * self.cols + bx)
    }

    /// Paint one coding unit's whole footprint (in 4-sample blocks) with its
    /// final quadtree depth and, for intra, its luma mode.
    pub(crate) fn fill(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, depth: u8, mode: u8) {
        for by in by0..by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = self.index(bx, by) {
                    if let Some(slot) = self.depth.get_mut(i) {
                        *slot = depth;
                    }
                    if let Some(slot) = self.mode.get_mut(i) {
                        *slot = mode;
                    }
                    if let Some(slot) = self.written.get_mut(i) {
                        *slot = true;
                    }
                }
            }
        }
    }

    /// The quadtree depth of the 4x4 block at luma pixel `(px, py)`, or
    /// `None` if it is out of picture bounds or not yet decoded (both cases
    /// contribute `0` to a `ctxInc` sum, per §9.3.4.2.2's "unavailable"
    /// clause).
    #[must_use]
    pub(crate) fn depth_at(&self, px: i32, py: i32) -> Option<u8> {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return None;
        };
        let i = self.index(block_of(px), block_of(py))?;
        if !self.written.get(i).copied().unwrap_or(false) {
            return None;
        }
        self.depth.get(i).copied()
    }

    /// The luma intra mode of the 4x4 block at luma pixel `(px, py)`, or
    /// [`DC_IDX`] if unavailable — §8.4.2's own fallback, folded in here so
    /// every caller gets it for free rather than re-deriving it.
    #[must_use]
    pub(crate) fn mode_at(&self, px: i32, py: i32) -> u8 {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return DC_IDX;
        };
        let Some(i) = self.index(block_of(px), block_of(py)) else {
            return DC_IDX;
        };
        if !self.written.get(i).copied().unwrap_or(false) {
            return DC_IDX;
        }
        self.mode.get(i).copied().unwrap_or(DC_IDX)
    }

    /// Paint one coding unit's whole footprint (in 4-sample blocks) with its
    /// finalised luma `QpY` — called once per coding unit, after its whole
    /// transform tree has been walked (see this struct's own doc for why
    /// that timing differs from [`CuGrid::fill`]'s).
    pub(crate) fn fill_qp(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, qp: i8) {
        for by in by0..by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = self.index(bx, by) {
                    if let Some(slot) = self.qp.get_mut(i) {
                        *slot = qp;
                    }
                    if let Some(slot) = self.qp_written.get_mut(i) {
                        *slot = true;
                    }
                }
            }
        }
    }

    /// The finalised luma `QpY` of the 4x4 block at luma pixel `(px, py)`, or
    /// `None` if it is out of picture bounds or not yet decoded — every
    /// caller (§8.6.1's `qPY_A`/`qPY_B`, and [`crate::deblock`]'s `qP_P`/
    /// `qP_Q`) has its own well-defined fallback for "unavailable" (§8.6.1's
    /// `qPY_PREV`, `slice_qp`, respectively), so this returns `Option`
    /// rather than picking one of those fallbacks itself.
    #[must_use]
    pub(crate) fn qp_at(&self, px: i32, py: i32) -> Option<i8> {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return None;
        };
        let i = self.index(block_of(px), block_of(py))?;
        if !self.qp_written.get(i).copied().unwrap_or(false) {
            return None;
        }
        self.qp.get(i).copied()
    }

    /// Paint one PU's footprint (in 4-sample blocks) with its finalised
    /// motion — called as soon as a PU's own `mvLX`/reference POC is known,
    /// before the *next* PU of the same CU (or a later CU) can read it as a
    /// spatial merge/AMVP neighbour (§8.5.3.2's own "already decoded in
    /// z-scan order" availability, which for an inter PU includes an earlier
    /// PU of its own CU). `info.l1` is `None` for a P-slice PU (or a
    /// uni-predictive B-slice one) and simply leaves `pred_l1` clear.
    pub(crate) fn fill_motion(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, info: crate::motion::MotionInfo, is_skip: bool) {
        let l0 = info.l0.map(|u| (i16::try_from(u.mv.x).unwrap_or(0), i16::try_from(u.mv.y).unwrap_or(0), u.ref_poc));
        let l1 = info.l1.map(|u| (i16::try_from(u.mv.x).unwrap_or(0), i16::try_from(u.mv.y).unwrap_or(0), u.ref_poc));
        for by in by0..by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                let Some(i) = self.index(bx, by) else { continue };
                if let Some(slot) = self.is_skip.get_mut(i) {
                    *slot = is_skip;
                }
                if let Some(slot) = self.pred_l0.get_mut(i) {
                    *slot = l0.is_some();
                }
                if let Some((x, y, poc)) = l0 {
                    if let Some(slot) = self.mv0_x.get_mut(i) {
                        *slot = x;
                    }
                    if let Some(slot) = self.mv0_y.get_mut(i) {
                        *slot = y;
                    }
                    if let Some(slot) = self.ref_poc0.get_mut(i) {
                        *slot = poc;
                    }
                }
                if let Some(slot) = self.pred_l1.get_mut(i) {
                    *slot = l1.is_some();
                }
                if let Some((x, y, poc)) = l1 {
                    if let Some(slot) = self.mv1_x.get_mut(i) {
                        *slot = x;
                    }
                    if let Some(slot) = self.mv1_y.get_mut(i) {
                        *slot = y;
                    }
                    if let Some(slot) = self.ref_poc1.get_mut(i) {
                        *slot = poc;
                    }
                }
            }
        }
    }

    /// The motion recorded at luma pixel `(px, py)`, or `None` if it is out
    /// of picture bounds, not yet decoded, or intra-coded (§8.5.3.2's own
    /// "predFlagL0/L1 equal to 0" unavailability, collapsed with the other
    /// two "unavailable" cases the way every other neighbour query in this
    /// crate already does — see `CuGrid::mode_at`'s own precedent).
    #[must_use]
    pub(crate) fn inter_at(&self, px: i32, py: i32) -> Option<crate::motion::MotionInfo> {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return None;
        };
        let i = self.index(block_of(px), block_of(py))?;
        if !self.written.get(i).copied().unwrap_or(false) {
            return None;
        }
        let pred_l0 = self.pred_l0.get(i).copied().unwrap_or(false);
        let pred_l1 = self.pred_l1.get(i).copied().unwrap_or(false);
        if !pred_l0 && !pred_l1 {
            return None;
        }
        let l0 = pred_l0.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(self.mv0_x.get(i).copied().unwrap_or(0)), y: i32::from(self.mv0_y.get(i).copied().unwrap_or(0)) },
            ref_poc: self.ref_poc0.get(i).copied().unwrap_or(0),
        });
        let l1 = pred_l1.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(self.mv1_x.get(i).copied().unwrap_or(0)), y: i32::from(self.mv1_y.get(i).copied().unwrap_or(0)) },
            ref_poc: self.ref_poc1.get(i).copied().unwrap_or(0),
        });
        Some(crate::motion::MotionInfo { l0, l1 })
    }

    /// Whether the 4x4 block at `(px, py)` is marked `cu_skip_flag` — used
    /// only for `skip_flag`'s own `ctxInc` derivation (§9.3.4.2.2), which
    /// treats "unavailable" the same as "not skipped" (both contribute `0`).
    #[must_use]
    pub(crate) fn is_skip_at(&self, px: i32, py: i32) -> bool {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return false;
        };
        let Some(i) = self.index(block_of(px), block_of(py)) else {
            return false;
        };
        if !self.written.get(i).copied().unwrap_or(false) {
            return false;
        }
        self.is_skip.get(i).copied().unwrap_or(false)
    }

    /// Paint one inter luma transform-unit leaf's own footprint (in
    /// 4-sample blocks) with whether it coded any non-zero coefficient —
    /// called once per leaf from `ctu::reconstruct_luma_inter`, mirroring
    /// [`CuGrid::fill_motion`]'s own per-leaf timing.
    pub(crate) fn fill_cbf_luma(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, cbf: bool) {
        for by in by0..by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = self.index(bx, by)
                    && let Some(slot) = self.cbf_luma.get_mut(i)
                {
                    *slot = cbf;
                }
            }
        }
    }

    /// Whether the luma transform block containing `(px, py)` coded any
    /// non-zero coefficient — `false` for an out-of-bounds or never-written
    /// position, the same "unavailable reads as the harmless default" shape
    /// every other query on this grid already has.
    #[must_use]
    pub(crate) fn cbf_luma_at(&self, px: i32, py: i32) -> bool {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return false;
        };
        let Some(i) = self.index(block_of(px), block_of(py)) else {
            return false;
        };
        self.cbf_luma.get(i).copied().unwrap_or(false)
    }

    /// The total bytes [`Budget::alloc`] charged across this grid's nine
    /// tracked arrays (`depth`/`mode`/`qp`/`mv0_x`/`mv0_y`/`ref_poc0` always,
    /// `mv1_x`/`mv1_y`/`ref_poc1` at their real length — `0` for a P/I
    /// slice's grid, `len` for a B slice's, exactly as [`CuGrid::new`]
    /// charged them) — what `decoder.rs` gives back via [`Budget::release`]
    /// once a slice's own CTU walk is done with this grid.
    ///
    /// Unlike [`Picture::budget_bytes`], which a picture still held live in
    /// the `Dpb` needs released only at eviction, this grid is a pure
    /// per-slice working buffer: nothing outside one `decode_ctu_slice` call
    /// ever reads it again once that slice's own deblocking/SAO passes and
    /// `CollocatedMotionField::build` are done with it. Before this existed,
    /// every single decoded picture — not just ones the `Dpb` kept as a
    /// reference — added `O(picture size)` to `committed` and never gave it
    /// back, which is why `max_alloc_total` was reached in lockstep with
    /// frame *count* rather than `Dpb` occupancy: a stock `libx265` encode
    /// past roughly 640x480 crossed the 64 MiB `strict` cap within the same
    /// 25-frame, one-second clip that a smaller-resolution fixture survived
    /// only because its own per-frame `CuGrid` charge was small enough that
    /// 25 frames' worth of it, plus the (correctly bounded) `Dpb` occupancy,
    /// still fit under the ceiling by coincidence.
    #[must_use]
    pub(crate) fn budget_bytes(&self) -> u64 {
        let bytes = |len: usize, size: usize| u64::try_from(len.saturating_mul(size)).unwrap_or(u64::MAX);
        bytes(self.depth.len(), 1)
            .saturating_add(bytes(self.mode.len(), 1))
            .saturating_add(bytes(self.qp.len(), 1))
            .saturating_add(bytes(self.mv0_x.len(), 2))
            .saturating_add(bytes(self.mv0_y.len(), 2))
            .saturating_add(bytes(self.ref_poc0.len(), 8))
            .saturating_add(bytes(self.mv1_x.len(), 2))
            .saturating_add(bytes(self.mv1_y.len(), 2))
            .saturating_add(bytes(self.ref_poc1.len(), 8))
    }
}

/// Per-4x4-luma-block "is there a transform/coding-unit boundary starting
/// here" flags, one grid per edge direction — the input [`crate::deblock`]'s
/// picture-wide filtering pass needs to know which 8-sample-grid lines are
/// real boundaries at all, since HEVC's deblocking filter (§8.7.2) never
/// filters below that grid regardless of how finely the transform tree
/// actually splits (see `deblock`'s own module doc for why: HM's own
/// `TComLoopFilter::xSetEdgefilterTU` addresses edge-filter flags at
/// `MinCbSizeY` (never below §8.7.2's fixed 8-sample floor) granularity, so a
/// transform split finer than that grid is folded into its enclosing grid
/// cell's boundary rather than creating a new, independently-filtered edge).
///
/// [`EdgeMarks::mark_vert`]/[`EdgeMarks::mark_horiz`] are called once per
/// reconstructed transform-unit leaf (`ctu::transform_unit`'s luma
/// reconstruction) with that leaf's own top-left corner and size, and do the
/// grid-alignment check themselves so the caller never has to reason about
/// it.
///
/// PERF-PROGRAMME.md item B4, Stage 1 step 3: row-banded the same way
/// [`ReconPlane`] is, and for the same reason (see that struct's own module
/// doc) — Stage 2's wavefront needs a later CTU row's own edge marks
/// writable while an earlier row's are still being read by deblocking. Every
/// mark/read call is, by construction, either within the one row band
/// currently being written (a CU/TU footprint never crosses a CTU
/// boundary, and CTU rows and edge-mark row bands are the same height) or
/// into an *already-finished* earlier row band — never a row band still
/// being written by someone else — so unlike [`ReconPlane`]'s per-CTU-tile
/// publish, a coarser once-per-row freeze is enough here: no caller ever
/// needs partial, sub-row visibility into a row still in progress. That
/// means this can stay a small, hand-rolled "current owned/mutable band,
/// finished bands frozen into `published`" split rather than going through
/// `vaco_codec_core::picture` itself — that primitive's per-tile publish
/// machinery would be solving a problem this data does not have. Stage 2's
/// real thread dispatch is what will need to make `published` shareable
/// across threads (a `Vec<OnceLock<EdgeBand>>` or similar); Stage 1 stays
/// single-threaded, so a plain `Vec<EdgeBand>` already proves the shape and
/// its cost.
#[derive(Debug, Clone)]
pub(crate) struct EdgeMarks {
    cols: usize,
    /// 4x4-block rows per CTU row band — the same quantity
    /// [`ReconPlane::band_h`] tracks in luma samples, here in block units.
    band_rows: usize,
    /// Total row bands in the picture — [`EdgeMarks::finish`] advances
    /// `current_band` past this so every read routes to `published`
    /// afterward, the same trick [`ReconPlane::finish`] uses.
    n_bands: usize,
    /// The row band [`EdgeMarks::mark_vert`] and friends currently write
    /// into; every earlier band already lives in `published`.
    current_band: usize,
    current: EdgeBand,
    /// Every row band strictly before `current_band`, frozen the moment
    /// [`EdgeMarks::begin_row`]/[`EdgeMarks::finish`] moved past it — the
    /// read side ([`EdgeMarks::vert_at`] and friends) for any row not in
    /// `current`.
    published: Vec<EdgeBand>,
}

/// One CTU row band's own share of [`EdgeMarks`]'s four boolean grids —
/// broken out as its own type purely so `current`/each `published` entry is
/// one value to move, rather than four parallel `Vec`s that would need to
/// travel together by convention.
#[derive(Debug, Clone)]
struct EdgeBand {
    vert: Vec<bool>,
    horiz: Vec<bool>,
    /// The subset of `vert` that is *also* a transform-block edge (as
    /// opposed to a prediction-unit-only boundary interior to one, unsplit
    /// transform unit — see `ctu::decode_inter_cu`'s own
    /// [`EdgeMarks::mark_vert`]/[`EdgeMarks::mark_horiz`] calls for where
    /// that PU-only case comes from). `crate::deblock`'s §8.7.2.4 `bS == 1`
    /// derivation needs this distinction: its non-zero-coefficient
    /// condition applies only "when the edge is also a transform block
    /// edge" — a PU-only edge never qualifies, regardless of what either
    /// side's (necessarily larger, unsplit) transform block coded.
    tu_vert: Vec<bool>,
    tu_horiz: Vec<bool>,
}

impl EdgeBand {
    fn new(len: usize) -> Self {
        Self { vert: vec![false; len], horiz: vec![false; len], tu_vert: vec![false; len], tu_horiz: vec![false; len] }
    }
}

impl EdgeMarks {
    /// One `bool` per 4x4 luma block, in each of the two directions, per
    /// row band — not `Budget`-tracked, matching this module's own
    /// [`Plane::ready`]/[`CuGrid`]'s own `written` precedent for boolean
    /// occupancy grids. `ctb_size` (in luma samples) sets the row-band
    /// height, the same quantity [`ReconPlane::new`]'s own caller passes.
    #[must_use]
    pub(crate) fn new(luma_width: usize, luma_height: usize, ctb_size: usize) -> Self {
        let cols = luma_width.div_ceil(4).max(1);
        let total_rows = luma_height.div_ceil(4).max(1);
        let band_rows = ctb_size.max(1).div_ceil(4).max(1);
        let n_bands = total_rows.div_ceil(band_rows).max(1);
        let band_len = cols.saturating_mul(band_rows);
        Self {
            cols,
            band_rows,
            n_bands,
            current_band: 0,
            current: EdgeBand::new(band_len),
            // Not `Vec::with_capacity` (disallowed workspace-wide — every
            // reservation goes through `Budget::alloc` instead): `published`
            // grows by exactly one `EdgeBand` per `begin_row`/`finish` call,
            // never resized in bulk, so amortised `push` growth is the
            // right shape rather than a single upfront reservation this
            // struct's boolean grids (like `Plane::ready`/`CuGrid::written`
            // before it) are already exempt from tracking anyway.
            published: Vec::new(),
        }
    }

    /// The row band containing 4x4-block row `by`.
    #[allow(clippy::integer_division, reason = "row band index = block row / the fixed CTB row-band height")]
    const fn band_of(&self, by: usize) -> usize {
        by / self.band_rows
    }

    /// `by`'s own row within whichever band [`EdgeMarks::band_of`] says it
    /// belongs to.
    #[allow(clippy::integer_division, reason = "same fixed CTB row-band height as band_of, its own remainder")]
    const fn local_of(&self, by: usize) -> usize {
        by % self.band_rows
    }

    fn index_in(&self, bx: usize, local_by: usize) -> Option<usize> {
        if bx >= self.cols || local_by >= self.band_rows {
            return None;
        }
        Some(local_by * self.cols + bx)
    }

    /// Advance to CTU row `row_band`: freeze every row band strictly before
    /// it into `published` and reset `current` for the new one — the
    /// same-shaped counterpart of [`ReconPlane::begin_row`], called from the
    /// same call sites right alongside it. Idempotent for a `row_band`
    /// already current, including once, harmlessly, for row `0`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row_band` goes backward.
    pub(crate) fn begin_row(&mut self, row_band: usize) -> Result<()> {
        if row_band < self.current_band {
            return Err(Error::InvalidData("vaco-codec-hevc: edge marks rows must advance in order"));
        }
        let band_len = self.cols.saturating_mul(self.band_rows);
        while self.published.len() < row_band {
            self.published.push(std::mem::replace(&mut self.current, EdgeBand::new(band_len)));
        }
        self.current_band = row_band;
        Ok(())
    }

    /// Freeze the last row band once the whole CTU walk is done, and
    /// advance `current_band` one past the last real band — mirroring
    /// [`ReconPlane::finish`] exactly, and for the same reason: every read
    /// after this point must route to `published` (the `Equal` branch in
    /// [`EdgeMarks::vert_at`] and friends would otherwise still match the
    /// last row band and see the fresh, empty `current` this leaves
    /// behind, not the data [`EdgeMarks::finish`] just moved out of it).
    /// Called once, right alongside [`ReconPlane::finish`], before
    /// deblocking or SAO ever read an [`EdgeMarks`] query.
    pub(crate) fn finish(&mut self) {
        let band_len = self.cols.saturating_mul(self.band_rows);
        while self.published.len() < self.n_bands {
            self.published.push(std::mem::replace(&mut self.current, EdgeBand::new(band_len)));
        }
        self.current_band = self.n_bands;
    }

    /// Record a vertical edge (a left-side transform/CU boundary) at `x0`
    /// spanning `[y0, y0 + size)`, but only when `x0` actually falls on the
    /// deblocking grid (`grid` luma samples, `x0 > 0` — never the picture's
    /// own left edge) — a transform leaf smaller than the grid, or one whose
    /// left edge does not land on a grid line, contributes nothing, which is
    /// exactly HEVC's own "never filter below the 8-sample grid" rule.
    pub(crate) fn mark_vert(&mut self, x0: i32, y0: i32, size: i32, grid: i32) {
        if x0 <= 0 || x0 % grid != 0 {
            return;
        }
        let Ok(bx) = usize::try_from(x0 >> 2) else { return };
        let Ok(by0) = usize::try_from(y0 >> 2) else { return };
        if self.band_of(by0) != self.current_band {
            return;
        }
        let local_by0 = self.local_of(by0);
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for local_by in local_by0..local_by0.saturating_add(blocks) {
            if let Some(i) = self.index_in(bx, local_by)
                && let Some(slot) = self.current.vert.get_mut(i)
            {
                *slot = true;
            }
        }
    }

    /// The horizontal-direction mirror of [`EdgeMarks::mark_vert`]: a
    /// top-side boundary at `y0` spanning `[x0, x0 + size)`.
    pub(crate) fn mark_horiz(&mut self, x0: i32, y0: i32, size: i32, grid: i32) {
        if y0 <= 0 || y0 % grid != 0 {
            return;
        }
        let Ok(by) = usize::try_from(y0 >> 2) else { return };
        let Ok(bx0) = usize::try_from(x0 >> 2) else { return };
        if self.band_of(by) != self.current_band {
            return;
        }
        let local_by = self.local_of(by);
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for bx in bx0..bx0.saturating_add(blocks) {
            if let Some(i) = self.index_in(bx, local_by)
                && let Some(slot) = self.current.horiz.get_mut(i)
            {
                *slot = true;
            }
        }
    }

    /// Whether a vertical edge was marked at luma pixel column `x`, for the
    /// 4x4 block row containing `y`.
    #[must_use]
    pub(crate) fn vert_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        let (bx, by) = (block_of(x), block_of(y));
        let local_by = self.local_of(by);
        let Some(i) = self.index_in(bx, local_by) else { return false };
        match self.band_of(by).cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.vert.get(i).copied().unwrap_or(false),
            std::cmp::Ordering::Less => self.published.get(self.band_of(by)).and_then(|b| b.vert.get(i)).copied().unwrap_or(false),
            std::cmp::Ordering::Greater => false,
        }
    }

    /// Whether a horizontal edge was marked at luma pixel row `y`, for the
    /// 4x4 block column containing `x`.
    #[must_use]
    pub(crate) fn horiz_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        let (bx, by) = (block_of(x), block_of(y));
        let local_by = self.local_of(by);
        let Some(i) = self.index_in(bx, local_by) else { return false };
        match self.band_of(by).cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.horiz.get(i).copied().unwrap_or(false),
            std::cmp::Ordering::Less => self.published.get(self.band_of(by)).and_then(|b| b.horiz.get(i)).copied().unwrap_or(false),
            std::cmp::Ordering::Greater => false,
        }
    }

    /// [`EdgeMarks::mark_vert`], plus also recording that this specific
    /// vertical edge is a transform-block edge (not merely a filterable
    /// one) — the call an inter transform-unit leaf makes, as opposed to the
    /// plain [`EdgeMarks::mark_vert`] a prediction-unit-only interior
    /// boundary (§8.7.2's own filterable-but-not-a-transform-edge case) uses.
    pub(crate) fn mark_tu_vert(&mut self, x0: i32, y0: i32, size: i32, grid: i32) {
        self.mark_vert(x0, y0, size, grid);
        if x0 <= 0 || x0 % grid != 0 {
            return;
        }
        let Ok(bx) = usize::try_from(x0 >> 2) else { return };
        let Ok(by0) = usize::try_from(y0 >> 2) else { return };
        if self.band_of(by0) != self.current_band {
            return;
        }
        let local_by0 = self.local_of(by0);
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for local_by in local_by0..local_by0.saturating_add(blocks) {
            if let Some(i) = self.index_in(bx, local_by)
                && let Some(slot) = self.current.tu_vert.get_mut(i)
            {
                *slot = true;
            }
        }
    }

    /// The horizontal-direction mirror of [`EdgeMarks::mark_tu_vert`].
    pub(crate) fn mark_tu_horiz(&mut self, x0: i32, y0: i32, size: i32, grid: i32) {
        self.mark_horiz(x0, y0, size, grid);
        if y0 <= 0 || y0 % grid != 0 {
            return;
        }
        let Ok(by) = usize::try_from(y0 >> 2) else { return };
        let Ok(bx0) = usize::try_from(x0 >> 2) else { return };
        if self.band_of(by) != self.current_band {
            return;
        }
        let local_by = self.local_of(by);
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for bx in bx0..bx0.saturating_add(blocks) {
            if let Some(i) = self.index_in(bx, local_by)
                && let Some(slot) = self.current.tu_horiz.get_mut(i)
            {
                *slot = true;
            }
        }
    }

    /// Whether the vertical edge at `(x, y)` (as addressed by
    /// [`EdgeMarks::vert_at`]) is also a transform-block edge.
    #[must_use]
    pub(crate) fn tu_vert_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        let (bx, by) = (block_of(x), block_of(y));
        let local_by = self.local_of(by);
        let Some(i) = self.index_in(bx, local_by) else { return false };
        match self.band_of(by).cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.tu_vert.get(i).copied().unwrap_or(false),
            std::cmp::Ordering::Less => self.published.get(self.band_of(by)).and_then(|b| b.tu_vert.get(i)).copied().unwrap_or(false),
            std::cmp::Ordering::Greater => false,
        }
    }

    /// Whether the horizontal edge at `(x, y)` (as addressed by
    /// [`EdgeMarks::horiz_at`]) is also a transform-block edge.
    #[must_use]
    pub(crate) fn tu_horiz_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        let (bx, by) = (block_of(x), block_of(y));
        let local_by = self.local_of(by);
        let Some(i) = self.index_in(bx, local_by) else { return false };
        match self.band_of(by).cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.tu_horiz.get(i).copied().unwrap_or(false),
            std::cmp::Ordering::Less => self.published.get(self.band_of(by)).and_then(|b| b.tu_horiz.get(i)).copied().unwrap_or(false),
            std::cmp::Ordering::Greater => false,
        }
    }
}


impl Plane {
    /// The plane's own dimensions — [`crate::deblock`]'s picture-wide pass
    /// needs to know how far to scan without reaching into private fields.
    #[must_use]
    pub(crate) fn dims(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The exact byte count [`Budget::alloc`] charged for `self.data` —
    /// `width * height` `u8` samples, one byte each since item B2 (was two)
    /// — for [`Picture::budget_bytes`] to sum across all three planes.
    #[must_use]
    fn byte_len(&self) -> u64 {
        u64::try_from(self.width.saturating_mul(self.height)).unwrap_or(u64::MAX)
    }

    /// One full row of already-reconstructed samples, `None` past the last
    /// row — the read side of the row-wise copy shape `PERF-PROGRAMME.md`'s
    /// item B1 replaces per-sample [`Plane::get`] loops with: a single
    /// length-checked slice instead of one bounds check (and one `Option`
    /// unwrap) per sample. Item B2 changed the element type to `u8`, the
    /// plane's own storage type, so a caller copying into another `u8`
    /// buffer (`decoder::blit`, emission into a `vaco_frame::Frame`) can now
    /// use [`slice::copy_from_slice`] directly instead of a per-sample
    /// narrowing conversion.
    #[must_use]
    pub(crate) fn row(&self, y: usize) -> Option<&[u8]> {
        let start = y.checked_mul(self.width)?;
        self.data.get(start..start.saturating_add(self.width))
    }

    /// The mutable counterpart of [`Plane::row`] — callers writing a whole
    /// row still owe [`Plane::mark_row_ready`] afterward, since this does
    /// not touch the parallel `ready` grid (a caller writing only part of a
    /// row, e.g. one CTU's own width inside a wider plane, marks only that
    /// sub-range as ready, which a combined "write and mark" method could
    /// not express as cleanly).
    pub(crate) fn row_mut(&mut self, y: usize) -> Option<&mut [u8]> {
        let start = y.checked_mul(self.width)?;
        self.data.get_mut(start..start.saturating_add(self.width))
    }

    /// Mark `len` samples starting at `(x0, y)` as reconstructed — the
    /// row-wise-write counterpart of [`Plane::mark_block_ready`] (one
    /// definition, D19: this just fixes `h` at `1`), for a caller that
    /// writes its own rows one at a time and marks readiness once per row
    /// rather than once for the whole block.
    pub(crate) fn mark_row_ready(&mut self, y: usize, x0: usize, len: usize) {
        self.mark_block_ready(x0, y, len, 1);
    }

    /// A `Budget`-charged copy of every sample in this plane, in the same
    /// row-major layout — one [`slice::copy_from_slice`] rather than
    /// [`crate::sao::Snapshot::capture`]'s old per-sample [`Plane::get`]
    /// loop (item B1). The two-array shape (`data` allocated fresh, then
    /// filled) mirrors [`Plane::new`]'s own `Budget::alloc`-then-fill
    /// pattern, which every other `Budget`-tracked buffer in this crate
    /// already uses.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub(crate) fn clone_samples(&self, budget: &mut Budget) -> Result<Vec<u8>> {
        let mut data: Vec<u8> = budget.alloc(self.data.len())?;
        if let Some(dst) = data.get_mut(..self.data.len()) {
            dst.copy_from_slice(&self.data);
        }
        Ok(data)
    }
}

// --- Stage 1: the CTU walk's own in-progress reconstruction buffer --------
//
// `Plane`/`Picture` above are unchanged: the shape every already-finished
// reference picture in the `Dpb` is stored as, and the shape deblocking, SAO
// and emission all already read and write. `ReconPlane`/`ReconPicture`
// below are new, additive types for *this* picture's own CTU walk
// specifically — see `docs/codec/hevc-wavefront-threading.md`'s "Concrete
// Stage 1 plan" for why the reconstruction buffer needs its own type rather
// than `Plane` itself growing a `vaco_codec_core::picture::PictureWriter`:
// once one of that primitive's bands publishes it is immutable forever (the
// whole point of the mechanism — see that module's own doc), but
// deblocking and SAO both need to modify pixels the CTU walk already
// finished. `ReconPicture::materialize_into` is the one-time hand-off
// between the two: read every published row band back into a plain,
// mutable `Picture` once the whole walk is done, which the existing
// deblock/SAO/emission code then keeps using exactly as it always has.
//
// Row-banded (one band per CTU row, full picture width) via
// `vaco_codec_core::picture`'s existing 1-D API (`band_mut`/
// `publish_through`/`band_ref`), not the 2-D per-CTU tile grid
// `PlaneSpec::with_bands` also supports: Stage 1 is still single-threaded,
// so there is no wavefront dependency yet that needs column granularity for
// (see the design doc's own "What is not yet known"). A row band's own
// `row_mut` stays exactly as fast as `Plane::row_mut` always was — one
// contiguous, full-width slice — because a full-width row never spans more
// than one band either way; only Stage 2's real per-CTU-column publish
// needs `with_bands`, at which point `row_mut`'s own single-row-at-a-time
// shape stops being expressible and every caller moves to `wait_tile`-
// mediated reads instead (see the design doc).

use vaco_codec_core::picture::{PictureRef, PictureSpec, PictureWriter, PlaneSpec, ProgressPicture};

/// The CTU walk's own in-progress reconstruction buffer for one component
/// plane — see this module's own "Stage 1" section doc above.
pub(crate) struct ReconPlane {
    writer: PictureWriter,
    reader: PictureRef,
    width: usize,
    height: usize,
    band_h: usize,
    n_row_bands: usize,
    /// The row band writes currently target. `get`/`is_ready` for a row
    /// strictly before it read the already-published `PictureRef` instead.
    current: usize,
    /// Per-4x4-block "has this block been written yet", scoped to `current`
    /// only and reset whenever it advances — everything before `current` is
    /// published, hence fully ready, by construction; only the band still
    /// being filled can be partially ready. Same reasoning as `Plane::ready`
    /// above, re-derived fresh per row band instead of covering the whole
    /// picture at once.
    ready_cols: usize,
    ready_rows: usize,
    ready: Vec<bool>,
}

impl ReconPlane {
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, width: usize, height: usize, ctb_size: usize) -> Result<Self> {
        let band_h = ctb_size.max(1);
        let spec = PictureSpec::new(vec![PlaneSpec::new(
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
        )])
        .with_band_height(u32::try_from(band_h).unwrap_or(1))
        .with_guard(0);
        let (writer, reader) = ProgressPicture::allocate(&spec, 0, budget)?;
        let n_row_bands = height.div_ceil(band_h).max(1);
        let ready_cols = width.div_ceil(4).max(1);
        let ready_rows = band_h.div_ceil(4).max(1);
        let ready = vec![false; ready_cols.saturating_mul(ready_rows)];
        Ok(Self {
            writer,
            reader,
            width,
            height,
            band_h,
            n_row_bands,
            current: 0,
            ready_cols,
            ready_rows,
            ready,
        })
    }

    fn ready_index(&self, bx: usize, by_in_band: usize) -> Option<usize> {
        if bx >= self.ready_cols || by_in_band >= self.ready_rows {
            return None;
        }
        Some(by_in_band * self.ready_cols + bx)
    }

    /// The row band containing picture row `y`.
    #[allow(clippy::integer_division, reason = "row band index = picture row / the fixed CTB row-band height")]
    const fn row_band_of(&self, y: usize) -> usize {
        y / self.band_h
    }

    /// `y`'s own row within whichever band [`ReconPlane::row_band_of`] says
    /// it belongs to.
    #[allow(clippy::integer_division, reason = "same fixed CTB row-band height as row_band_of, its own remainder")]
    const fn local_row_of(&self, y: usize) -> usize {
        y % self.band_h
    }

    /// Advance to CTU row `row_band`: publish every row band strictly
    /// before it (a no-op for ones already published — `publish_through`'s
    /// own idempotence) and reset the per-block ready grid for the new one.
    /// Called once per CTU row by the walk's own outer loop, including
    /// once, harmlessly, for row `0`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row_band` goes backward, or publishing
    /// fails.
    pub(crate) fn begin_row(&mut self, row_band: usize) -> Result<()> {
        if row_band < self.current {
            return Err(Error::InvalidData(
                "vaco-codec-hevc: recon plane rows must advance in order",
            ));
        }
        if row_band > 0 {
            self.writer.publish_through(0, row_band - 1)?;
        }
        self.current = row_band;
        self.ready.fill(false);
        Ok(())
    }

    /// Publish every remaining row band — the walk is done; deblocking and
    /// SAO read the materialized flat [`Picture`] instead of this one from
    /// here on.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if publishing fails.
    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.n_row_bands > 0 {
            self.writer.publish_through(0, self.n_row_bands - 1)?;
        }
        self.current = self.n_row_bands;
        Ok(())
    }

    #[must_use]
    pub(crate) fn dims(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Whether `(x, y)`'s containing 4x4 block has already been fully
    /// reconstructed — [`Plane::is_ready`]'s exact counterpart.
    #[must_use]
    pub(crate) fn is_ready(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        if x >= self.width || y >= self.height {
            return false;
        }
        let row_band = self.row_band_of(y);
        if row_band < self.current {
            return true;
        }
        if row_band > self.current {
            return false;
        }
        let local_y = self.local_row_of(y);
        self.ready_index(block_of(x), block_of(local_y))
            .and_then(|i| self.ready.get(i))
            .copied()
            .unwrap_or(false)
    }

    /// The sample at `(x, y)`, or `0` out of range or not yet written —
    /// [`Plane::get`]'s exact counterpart, reading through whichever of the
    /// still-staged current band or an already-published one owns `y`.
    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> u16 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let row_band = self.row_band_of(y);
        match row_band.cmp(&self.current) {
            std::cmp::Ordering::Equal => {
                let Some(blk) = self.writer.band_ref(0, row_band) else {
                    return 0;
                };
                let local_y = self.local_row_of(y);
                let idx = local_y.checked_mul(blk.stride).and_then(|s| s.checked_add(x));
                idx.and_then(|i| blk.data.get(i)).copied().map_or(0, u16::from)
            }
            std::cmp::Ordering::Less => {
                let yu = u32::try_from(y).unwrap_or(0);
                self.reader
                    .try_rows(0, yu)
                    .and_then(|view| view.row(yu))
                    .and_then(|row| row.get(x))
                    .copied()
                    .map_or(0, u16::from)
            }
            std::cmp::Ordering::Greater => 0,
        }
    }

    /// [`Plane::mark_block_ready`]'s exact counterpart, scoped to the
    /// current row band — marking a position in an earlier, already-
    /// published band is a silent no-op (nothing left to mark; it is
    /// already fully ready by construction).
    pub(crate) fn mark_block_ready(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        if w == 0 || h == 0 || x0 >= self.width || y0 >= self.height {
            return;
        }
        let row_band = self.row_band_of(y0);
        if row_band != self.current {
            return;
        }
        let local_y0 = self.local_row_of(y0);
        let x1 = x0.saturating_add(w).saturating_sub(1).min(self.width.saturating_sub(1));
        let local_y1 = local_y0
            .saturating_add(h)
            .saturating_sub(1)
            .min(self.band_h.saturating_sub(1));
        let (bx0, by0, bx1, by1) = (block_of(x0), block_of(local_y0), block_of(x1), block_of(local_y1));
        for by in by0..=by1 {
            for bx in bx0..=bx1 {
                if let Some(i) = self.ready_index(bx, by)
                    && let Some(slot) = self.ready.get_mut(i)
                {
                    *slot = true;
                }
            }
        }
    }

    /// [`Plane::mark_row_ready`]'s exact counterpart.
    pub(crate) fn mark_row_ready(&mut self, y: usize, x0: usize, len: usize) {
        self.mark_block_ready(x0, y, len, 1);
    }

    /// [`Plane::row_mut`]'s exact counterpart: one full, contiguous row of
    /// the band currently being written, `None` for any other row (the
    /// walk never asks for one; deblocking/SAO/emission all read the
    /// materialized flat [`Picture`] instead once the walk is done).
    pub(crate) fn row_mut(&mut self, y: usize) -> Option<&mut [u8]> {
        if y >= self.height {
            return None;
        }
        let row_band = self.row_band_of(y);
        if row_band != self.current {
            return None;
        }
        let local_y = u32::try_from(self.local_row_of(y)).ok()?;
        let band = self.writer.band_mut(0, row_band).ok()?;
        band.into_row_mut(local_y)
    }

    /// Copy every published row into `dst`, and mark it ready there too —
    /// the one-time hand-off `ReconPicture::materialize_into` uses. Must be
    /// called after [`ReconPlane::finish`]; a row band that never published
    /// (should not happen — `finish` publishes everything) is silently
    /// skipped rather than panicking, matching this module's own "missing
    /// reads as zero/unready, never as a crash" convention throughout.
    fn materialize_into(&self, dst: &mut Plane) {
        let (w, h) = self.dims();
        for y in 0..h {
            let yu = u32::try_from(y).unwrap_or(0);
            let Some(row) = self.reader.try_rows(0, yu).and_then(|view| view.row(yu)) else {
                continue;
            };
            if let Some(dst_row) = dst.row_mut(y) {
                let n = dst_row.len().min(row.len()).min(w);
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
            dst.mark_row_ready(y, 0, w);
        }
    }
}

/// One decoding picture's three reconstruction planes, as the CTU walk sees
/// them — see [`ReconPlane`]'s own doc for why this is a separate type from
/// [`Picture`] rather than [`Picture`] itself.
pub(crate) struct ReconPicture {
    pub y: ReconPlane,
    pub cb: ReconPlane,
    pub cr: ReconPlane,
}

impl ReconPicture {
    /// `ctb_size` is the *luma* CTB size; chroma's own band height is half
    /// that (rounded up), matching this crate's 4:2:0-only scope exactly
    /// the way [`Picture::new`]'s own `cw`/`ch` halving already does.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if any plane's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, luma_width: usize, luma_height: usize, ctb_size: usize) -> Result<Self> {
        let cw = luma_width.div_ceil(2);
        let ch = luma_height.div_ceil(2);
        let cctb = ctb_size.div_ceil(2).max(1);
        Ok(Self {
            y: ReconPlane::new(budget, luma_width, luma_height, ctb_size)?,
            cb: ReconPlane::new(budget, cw, ch, cctb)?,
            cr: ReconPlane::new(budget, cw, ch, cctb)?,
        })
    }

    /// [`ReconPlane::begin_row`], across all three planes at once — the
    /// call site the CTU walk's own outer loop makes once per CTU row.
    ///
    /// # Errors
    /// As [`ReconPlane::begin_row`].
    pub(crate) fn begin_ctu_row(&mut self, row: usize) -> Result<()> {
        self.y.begin_row(row)?;
        self.cb.begin_row(row)?;
        self.cr.begin_row(row)?;
        Ok(())
    }

    /// [`ReconPlane::finish`], across all three planes.
    ///
    /// # Errors
    /// As [`ReconPlane::finish`].
    pub(crate) fn finish(&mut self) -> Result<()> {
        self.y.finish()?;
        self.cb.finish()?;
        self.cr.finish()?;
        Ok(())
    }

    /// The one-time hand-off to a plain, mutable [`Picture`] — call once,
    /// after [`ReconPicture::finish`], before deblocking runs.
    pub(crate) fn materialize_into(&self, dst: &mut Picture) {
        self.y.materialize_into(&mut dst.y);
        self.cb.materialize_into(&mut dst.cb);
        self.cr.materialize_into(&mut dst.cr);
    }
}
