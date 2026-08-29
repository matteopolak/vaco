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

/// One reconstruction plane: samples plus a parallel "has this pixel been
/// written yet" bit, both addressed by the same `(x, y)`.
#[derive(Debug)]
pub(crate) struct Plane {
    width: usize,
    height: usize,
    data: Vec<u16>,
    ready: Vec<bool>,
}

impl Plane {
    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, width: usize, height: usize) -> Result<Self> {
        let len = width.saturating_mul(height);
        let data = budget.alloc(len)?;
        let ready = vec![false; len];
        Ok(Self { width, height, data, ready })
    }

    fn index(&self, x: usize, y: usize) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(y * self.width + x)
    }

    /// Whether `(x, y)` is inside the plane and has already been
    /// reconstructed. The single availability test every reference-sample
    /// read in intra prediction uses.
    #[must_use]
    pub(crate) fn is_ready(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        self.index(x, y).and_then(|i| self.ready.get(i)).copied().unwrap_or(false)
    }

    /// The sample at `(x, y)`, or `0` out of range or not yet written —
    /// callers that reach this must have checked [`Plane::is_ready`] first
    /// (or be reading their own just-written prediction buffer, which is
    /// always in range by construction).
    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> u16 {
        self.index(x, y).and_then(|i| self.data.get(i)).copied().unwrap_or(0)
    }

    /// Write a final reconstructed sample and mark it available.
    pub(crate) fn set(&mut self, x: usize, y: usize, v: u16) {
        if let Some(i) = self.index(x, y) {
            if let Some(slot) = self.data.get_mut(i) {
                *slot = v;
            }
            if let Some(slot) = self.ready.get_mut(i) {
                *slot = true;
            }
        }
    }

    /// [`Plane::set`] taking `i32` coordinates and an already-clamped `i32`
    /// value — [`crate::sao`] computes in `i32` throughout (clause 8.7.3's
    /// own arithmetic needs headroom past `u16` mid-formula) and only needs
    /// the final clip-then-store this wraps.
    pub(crate) fn set_i32(&mut self, x: i32, y: i32, v: i32) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else { return };
        let v = u16::try_from(v.max(0)).unwrap_or(0);
        self.set(x, y, v);
    }
}

/// One decoded picture's reconstruction planes: luma plus two 4:2:0 chroma
/// planes (this crate's whole scope is 4:2:0 — see the crate doc).
#[derive(Debug)]
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
}

/// Per-4x4-luma-block metadata the CTU walk needs from already-decoded
/// neighbours: the coding-quadtree depth (`split_cu_flag`'s `ctxInc`,
/// §9.3.4.2.2) and the luma intra prediction mode (the MPM derivation,
/// §8.4.2).
///
/// Indexed at 4-sample granularity because that is HEVC's minimum coding
/// block size; a coding unit's whole footprint is painted in one pass when
/// its final depth/mode is known, so a later neighbour query is a plain
/// lookup rather than a tree walk.
#[derive(Debug)]
pub(crate) struct CuGrid {
    cols: usize,
    rows: usize,
    depth: Vec<u8>,
    mode: Vec<u8>,
    written: Vec<bool>,
}

impl CuGrid {
    /// # Errors
    /// [`vaco_core::Error`] if either grid's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, luma_width: usize, luma_height: usize) -> Result<Self> {
        let cols = luma_width.div_ceil(4).max(1);
        let rows = luma_height.div_ceil(4).max(1);
        let len = cols.saturating_mul(rows);
        Ok(Self {
            cols,
            rows,
            depth: budget.alloc(len)?,
            mode: budget.alloc(len)?,
            written: vec![false; len],
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
#[derive(Debug)]
pub(crate) struct EdgeMarks {
    cols: usize,
    rows: usize,
    vert: Vec<bool>,
    horiz: Vec<bool>,
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
        Self { cols, rows, vert: vec![false; len], horiz: vec![false; len] }
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
}

impl Plane {
    /// The plane's own dimensions — [`crate::deblock`]'s picture-wide pass
    /// needs to know how far to scan without reaching into private fields.
    #[must_use]
    pub(crate) fn dims(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}
