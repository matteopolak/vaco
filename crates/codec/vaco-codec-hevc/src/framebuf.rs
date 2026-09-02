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
use vaco_core::Result;
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
/// every *read* of availability ([`Plane::is_ready`]) is a per-pixel
/// reference-sample query that only ever needs "has the 4x4 block
/// containing this pixel been written", never finer. Collapsing `ready` to
/// that grid is therefore not an approximation — within this crate's scope
/// it answers the identical question the old per-pixel array did, at 1/16
/// the memory and update cost.
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

    /// Whether `(x, y)` is inside the plane and its containing 4x4 block has
    /// already been fully reconstructed. The single availability test every
    /// reference-sample read in intra prediction uses.
    #[must_use]
    pub(crate) fn is_ready(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        if x >= self.width || y >= self.height {
            return false;
        }
        self.ready_index(block_of(x), block_of(y)).and_then(|i| self.ready.get(i)).copied().unwrap_or(false)
    }

    /// The sample at `(x, y)`, or `0` out of range or not yet written —
    /// callers that reach this must have checked [`Plane::is_ready`] first
    /// (or be reading their own just-written prediction buffer, which is
    /// always in range by construction). Widened to `u16` so every caller
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
#[derive(Debug, Clone)]
pub(crate) struct EdgeMarks {
    cols: usize,
    rows: usize,
    vert: Vec<bool>,
    horiz: Vec<bool>,
    /// The subset of `vert`/`horiz` that is *also* a transform-block edge
    /// (as opposed to a prediction-unit-only boundary interior to one,
    /// unsplit transform unit — see `ctu::decode_inter_cu`'s own
    /// [`EdgeMarks::mark_vert`]/[`EdgeMarks::mark_horiz`] calls for where
    /// that PU-only case comes from). `crate::deblock`'s §8.7.2.4 `bS == 1`
    /// derivation needs this distinction: its non-zero-coefficient condition
    /// applies only "when the edge is also a transform block edge" — a
    /// PU-only edge never qualifies, regardless of what either side's
    /// (necessarily larger, unsplit) transform block coded.
    tu_vert: Vec<bool>,
    tu_horiz: Vec<bool>,
}

impl EdgeMarks {
    /// One `bool` per 4x4 luma block, in each of the two directions — not
    /// `Budget`-tracked, matching this module's own [`Plane::ready`]/
    /// [`CuGrid`]'s own `written` precedent for boolean occupancy grids.
    #[must_use]
    pub(crate) fn new(luma_width: usize, luma_height: usize) -> Self {
        let cols = luma_width.div_ceil(4).max(1);
        let rows = luma_height.div_ceil(4).max(1);
        let len = cols.saturating_mul(rows);
        Self { cols, rows, vert: vec![false; len], horiz: vec![false; len], tu_vert: vec![false; len], tu_horiz: vec![false; len] }
    }

    fn index(&self, bx: usize, by: usize) -> Option<usize> {
        if bx >= self.cols || by >= self.rows {
            return None;
        }
        Some(by * self.cols + bx)
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
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for by in by0..by0.saturating_add(blocks) {
            if let Some(i) = self.index(bx, by)
                && let Some(slot) = self.vert.get_mut(i)
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
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for bx in bx0..bx0.saturating_add(blocks) {
            if let Some(i) = self.index(bx, by)
                && let Some(slot) = self.horiz.get_mut(i)
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
        self.index(block_of(x), block_of(y)).and_then(|i| self.vert.get(i)).copied().unwrap_or(false)
    }

    /// Whether a horizontal edge was marked at luma pixel row `y`, for the
    /// 4x4 block column containing `x`.
    #[must_use]
    pub(crate) fn horiz_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        self.index(block_of(x), block_of(y)).and_then(|i| self.horiz.get(i)).copied().unwrap_or(false)
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
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for by in by0..by0.saturating_add(blocks) {
            if let Some(i) = self.index(bx, by)
                && let Some(slot) = self.tu_vert.get_mut(i)
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
        let blocks = usize::try_from((size >> 2).max(1)).unwrap_or(1);
        for bx in bx0..bx0.saturating_add(blocks) {
            if let Some(i) = self.index(bx, by)
                && let Some(slot) = self.tu_horiz.get_mut(i)
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
        self.index(block_of(x), block_of(y)).and_then(|i| self.tu_vert.get(i)).copied().unwrap_or(false)
    }

    /// Whether the horizontal edge at `(x, y)` (as addressed by
    /// [`EdgeMarks::horiz_at`]) is also a transform-block edge.
    #[must_use]
    pub(crate) fn tu_horiz_at(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return false };
        self.index(block_of(x), block_of(y)).and_then(|i| self.tu_horiz.get(i)).copied().unwrap_or(false)
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
