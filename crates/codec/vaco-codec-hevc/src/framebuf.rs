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
///
/// PERF-PROGRAMME.md item B4, Stage 1 step 3: row-banded the same way
/// [`EdgeMarks`] is, for the same reason (see that struct's own module doc,
/// and this module's own "Stage 1" section doc above, for why a coarse
/// once-per-CTU-row freeze is enough here and `vaco_codec_core::picture`'s
/// per-tile publish machinery is not needed). `CuGridBand` bundles this
/// struct's nine per-block arrays the same way `EdgeBand` bundles
/// `EdgeMarks`'s four; `current`/`published` and `begin_row`/`finish` follow
/// the identical shape, including the same one-past-the-end advance on
/// `finish` (see this module's own "Stage 1" section doc for why that is
/// not incidental to `EdgeMarks` alone).
///
/// Stage 2b step 1c (`docs/codec/hevc-wavefront-threading.md`): `published`
/// is [`crate::wavefront::RowPublish`], not a plain `Vec` — the same latent
/// data race that document names for `EdgeMarks`/`SaoParamsGrid` applied
/// here too, last of the three to move since it is the largest (nine
/// heterogeneous arrays plus its own `Budget` accounting to keep
/// self-consistent through the change).
#[derive(Debug, Clone)]
struct CuGridShared {
    cols: usize,
    /// 4x4-block rows per CTU row band — see [`EdgeMarks::band_rows`]'s own
    /// doc for the identical quantity there.
    band_rows: usize,
    /// Total row bands in the picture — see [`EdgeMarks::n_bands`]'s own
    /// doc.
    n_bands: usize,
    /// `InterSliceParams::is_b` (`false` for an I or P slice's own grid) —
    /// carried here (rather than only at construction) so [`CuGrid::begin_row`]
    /// can size each new band's `mv1_x`/`mv1_y`/`ref_poc1` the same way
    /// [`CuGrid::new`] originally sized the whole grid's. See
    /// [`CuGrid::new`]'s own doc for why this gating exists at all.
    has_l1: bool,
    /// Every row band strictly before `current_band`, published the moment
    /// [`CuGrid::begin_row`]/[`CuGrid::finish`] moved past it. See
    /// [`CuGrid`]'s own doc for why this is [`crate::wavefront::RowPublish`]
    /// rather than a plain `Vec`.
    published: crate::wavefront::RowPublish<CuGridBand>,
}

/// Step 3's first commit splits `shared` (geometry, `has_l1`, and
/// `published`) into its own type, [`CuGridShared`], separate from
/// `current`/`current_band` — the same move `EdgeMarks`/`SaoParamsGrid`
/// made, for the same reason (`docs/codec/hevc-wavefront-threading.md`'s
/// "step 1 closed only half of each race"): a future `Arc` around
/// `CuGridShared` alone, with no `current` inside it, is what makes the
/// write side shareable. `CuGrid` still bundles both today
/// (single-threaded), but every method already routes through
/// `self.shared`/`self.current` explicitly.
#[derive(Debug, Clone)]
pub(crate) struct CuGrid {
    shared: std::sync::Arc<CuGridShared>,
    /// The row band [`CuGrid::fill`] and friends currently write into;
    /// every earlier band already lives in `shared.published`.
    current_band: usize,
    /// `Some` at every point in this grid's lifetime except between
    /// [`CuGrid::finish`] and drop, where it stays `None` — `finish` takes
    /// it into `published` without needing a replacement value the way
    /// [`CuGrid::begin_row`] does, since nothing writes to a `CuGrid` again
    /// once the walk that owns it is done with it.
    current: Option<CuGridBand>,
}

/// One CTU row band's own share of [`CuGrid`]'s nine per-4x4-block arrays —
/// broken out as its own type for the same reason [`EdgeBand`] is: one
/// value to move into `published`, not nine parallel `Vec`s that would need
/// to travel together by convention.
#[derive(Debug, Clone)]
struct CuGridBand {
    depth: Vec<u8>,
    mode: Vec<u8>,
    written: Vec<bool>,
    qp: Vec<i8>,
    qp_written: Vec<bool>,
    pred_l0: Vec<bool>,
    pred_l1: Vec<bool>,
    is_skip: Vec<bool>,
    mv0_x: Vec<i16>,
    mv0_y: Vec<i16>,
    ref_poc0: Vec<i64>,
    mv1_x: Vec<i16>,
    mv1_y: Vec<i16>,
    ref_poc1: Vec<i64>,
    cbf_luma: Vec<bool>,
}

impl CuGridBand {
    /// # Errors
    /// [`vaco_core::Error`] if this band's own allocation exceeds `budget`.
    fn new(budget: &mut Budget, cols: usize, band_rows: usize, has_l1: bool) -> Result<Self> {
        let len = cols.saturating_mul(band_rows);
        let len_l1 = if has_l1 { len } else { 0 };
        Ok(Self {
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

    /// The total bytes [`Budget::alloc`] charged for this one band — see
    /// [`CuGrid::budget_bytes`]'s own doc for why the whole grid's total is
    /// just this summed across every band.
    fn budget_bytes(&self) -> u64 {
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

/// `bx`/`local_by` (already band-relative) to a flat index into any of
/// [`CuGridBand`]'s arrays, or `None` out of range — a free function, not a
/// method, so callers already holding a `&mut CuGridBand` borrowed out of
/// [`CuGrid::current`] can still compute it without a second, conflicting
/// borrow of `self`.
fn cu_index_in(cols: usize, band_rows: usize, bx: usize, local_by: usize) -> Option<usize> {
    if bx >= cols || local_by >= band_rows {
        return None;
    }
    Some(local_by * cols + bx)
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
    /// `ctb_size` (in luma samples) sets the row-band height, the same
    /// quantity [`ReconPlane::new`]/[`EdgeMarks::new`]'s own caller already
    /// passes. Only the first band is allocated here; [`CuGrid::begin_row`]
    /// allocates each later one as the walk reaches it, so a picture whose
    /// full grid would exceed `budget` fails when the CTU walk actually
    /// reaches the row that pushes it over, not necessarily at this call —
    /// the same incremental-charge shape [`EdgeMarks`] already has for its
    /// (untracked) bands, extended here to the arrays that are
    /// `Budget`-tracked. The running total charged is unaffected: the sum
    /// across every band ends up the same handful of bytes [`CuGrid::new`]
    /// used to charge in one call (plus, at most, one row band's worth of
    /// rounding from the last, possibly-short band — see
    /// [`EdgeMarks::new`]'s own identical rounding).
    ///
    /// # Errors
    /// [`vaco_core::Error`] if the first band's allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, luma_width: usize, luma_height: usize, has_l1: bool, ctb_size: usize) -> Result<Self> {
        let cols = luma_width.div_ceil(4).max(1);
        let total_rows = luma_height.div_ceil(4).max(1);
        let band_rows = ctb_size.max(1).div_ceil(4).max(1);
        let n_bands = total_rows.div_ceil(band_rows).max(1);
        let current = CuGridBand::new(budget, cols, band_rows, has_l1)?;
        Ok(Self {
            shared: std::sync::Arc::new(CuGridShared { cols, band_rows, n_bands, has_l1, published: crate::wavefront::RowPublish::new(n_bands) }),
            current_band: 0,
            current: Some(current),
        })
    }

    /// The row band containing 4x4-block row `by`.
    #[allow(clippy::integer_division, reason = "row band index = block row / the fixed CTB row-band height")]
    fn band_of(&self, by: usize) -> usize {
        by / self.shared.band_rows
    }

    /// `by`'s own row within whichever band [`CuGrid::band_of`] says it
    /// belongs to.
    #[allow(clippy::integer_division, reason = "same fixed CTB row-band height as band_of, its own remainder")]
    fn local_of(&self, by: usize) -> usize {
        by % self.shared.band_rows
    }

    /// The band that block row `by` currently lives in — `current` if it is
    /// the row being written, a `published` entry if it is an earlier,
    /// already-finished row, or `None` for a row not reached yet.
    fn band_for(&self, by: usize) -> Option<&CuGridBand> {
        match self.band_of(by).cmp(&self.current_band) {
            std::cmp::Ordering::Equal => self.current.as_ref(),
            std::cmp::Ordering::Less => self.shared.published.get(self.band_of(by)),
            std::cmp::Ordering::Greater => None,
        }
    }

    /// `current`, only if block row `by` is the one it holds — the write
    /// side's counterpart of [`CuGrid::band_for`]'s `Equal` arm; a write
    /// targeting any other row is a caller error this degrades from
    /// silently (a coding unit's footprint never crosses a CTU boundary, so
    /// every real write already satisfies this).
    fn current_band_mut(&mut self, by: usize) -> Option<&mut CuGridBand> {
        if self.band_of(by) != self.current_band {
            return None;
        }
        self.current.as_mut()
    }

    /// Advance to CTU row `row_band`: freeze `current` into `published` and
    /// allocate a fresh one, once, for the new row — the same-shaped
    /// counterpart of [`EdgeMarks::begin_row`]/[`ReconPlane::begin_row`],
    /// called from the same call sites right alongside them. Idempotent for
    /// a `row_band` already current, including once, harmlessly, for row
    /// `0`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row_band` goes backward, the new band's
    /// allocation exceeds `budget`, or (unreachable in practice, for the
    /// same reason [`EdgeMarks::begin_row`]'s own `Errors` section gives)
    /// [`crate::wavefront::RowPublish`] itself refuses a publish.
    pub(crate) fn begin_row(&mut self, budget: &mut Budget, row_band: usize) -> Result<()> {
        if row_band < self.current_band {
            return Err(Error::InvalidData("vaco-codec-hevc: cu grid rows must advance in order"));
        }
        while self.current_band < row_band {
            if let Some(band) = self.current.take() {
                self.shared.published.publish(self.current_band, band)?;
            }
            self.current = Some(CuGridBand::new(budget, self.shared.cols, self.shared.band_rows, self.shared.has_l1)?);
            self.current_band = self.current_band.saturating_add(1);
        }
        Ok(())
    }

    /// Freeze the last row band once the whole CTU walk is done, and
    /// advance `current_band` one past the last real band — see this
    /// module's own "Stage 1" section doc for why every type built this way
    /// needs exactly this move, not merely freezing the last band in place.
    /// Called once, right alongside [`EdgeMarks::finish`]/
    /// [`ReconPlane::finish`], before deblocking, SAO or
    /// `CollocatedMotionField::build` ever query this grid.
    ///
    /// # Errors
    /// [`vaco_core::Error`], unreachable in practice for the same reason
    /// [`CuGrid::begin_row`]'s own `Errors` section gives.
    pub(crate) fn finish(&mut self) -> Result<()> {
        while self.current_band < self.shared.n_bands {
            let Some(band) = self.current.take() else { break };
            self.shared.published.publish(self.current_band, band)?;
            self.current_band = self.current_band.saturating_add(1);
        }
        self.current_band = self.shared.n_bands;
        Ok(())
    }

    /// Paint one coding unit's whole footprint (in 4-sample blocks) with its
    /// final quadtree depth and, for intra, its luma mode.
    pub(crate) fn fill(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, depth: u8, mode: u8) {
        let cols = self.shared.cols;
        let band_rows = self.shared.band_rows;
        let local_by0 = self.local_of(by0);
        let Some(band) = self.current_band_mut(by0) else { return };
        for local_by in local_by0..local_by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = cu_index_in(cols, band_rows, bx, local_by) {
                    if let Some(slot) = band.depth.get_mut(i) {
                        *slot = depth;
                    }
                    if let Some(slot) = band.mode.get_mut(i) {
                        *slot = mode;
                    }
                    if let Some(slot) = band.written.get_mut(i) {
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
        let (bx, by) = (block_of(px), block_of(py));
        let band = self.band_for(by)?;
        let i = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by))?;
        if !band.written.get(i).copied().unwrap_or(false) {
            return None;
        }
        band.depth.get(i).copied()
    }

    /// The luma intra mode of the 4x4 block at luma pixel `(px, py)`, or
    /// [`DC_IDX`] if unavailable — §8.4.2's own fallback, folded in here so
    /// every caller gets it for free rather than re-deriving it.
    #[must_use]
    pub(crate) fn mode_at(&self, px: i32, py: i32) -> u8 {
        let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
            return DC_IDX;
        };
        let (bx, by) = (block_of(px), block_of(py));
        let Some(band) = self.band_for(by) else {
            return DC_IDX;
        };
        let Some(i) = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by)) else {
            return DC_IDX;
        };
        if !band.written.get(i).copied().unwrap_or(false) {
            return DC_IDX;
        }
        band.mode.get(i).copied().unwrap_or(DC_IDX)
    }

    /// Paint one coding unit's whole footprint (in 4-sample blocks) with its
    /// finalised luma `QpY` — called once per coding unit, after its whole
    /// transform tree has been walked (see this struct's own doc for why
    /// that timing differs from [`CuGrid::fill`]'s).
    pub(crate) fn fill_qp(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, qp: i8) {
        let cols = self.shared.cols;
        let band_rows = self.shared.band_rows;
        let local_by0 = self.local_of(by0);
        let Some(band) = self.current_band_mut(by0) else { return };
        for local_by in local_by0..local_by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = cu_index_in(cols, band_rows, bx, local_by) {
                    if let Some(slot) = band.qp.get_mut(i) {
                        *slot = qp;
                    }
                    if let Some(slot) = band.qp_written.get_mut(i) {
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
        let (bx, by) = (block_of(px), block_of(py));
        let band = self.band_for(by)?;
        let i = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by))?;
        if !band.qp_written.get(i).copied().unwrap_or(false) {
            return None;
        }
        band.qp.get(i).copied()
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
        let cols = self.shared.cols;
        let band_rows = self.shared.band_rows;
        let local_by0 = self.local_of(by0);
        let Some(band) = self.current_band_mut(by0) else { return };
        for local_by in local_by0..local_by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                let Some(i) = cu_index_in(cols, band_rows, bx, local_by) else { continue };
                if let Some(slot) = band.is_skip.get_mut(i) {
                    *slot = is_skip;
                }
                if let Some(slot) = band.pred_l0.get_mut(i) {
                    *slot = l0.is_some();
                }
                if let Some((x, y, poc)) = l0 {
                    if let Some(slot) = band.mv0_x.get_mut(i) {
                        *slot = x;
                    }
                    if let Some(slot) = band.mv0_y.get_mut(i) {
                        *slot = y;
                    }
                    if let Some(slot) = band.ref_poc0.get_mut(i) {
                        *slot = poc;
                    }
                }
                if let Some(slot) = band.pred_l1.get_mut(i) {
                    *slot = l1.is_some();
                }
                if let Some((x, y, poc)) = l1 {
                    if let Some(slot) = band.mv1_x.get_mut(i) {
                        *slot = x;
                    }
                    if let Some(slot) = band.mv1_y.get_mut(i) {
                        *slot = y;
                    }
                    if let Some(slot) = band.ref_poc1.get_mut(i) {
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
        let (bx, by) = (block_of(px), block_of(py));
        let band = self.band_for(by)?;
        let i = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by))?;
        if !band.written.get(i).copied().unwrap_or(false) {
            return None;
        }
        let pred_l0 = band.pred_l0.get(i).copied().unwrap_or(false);
        let pred_l1 = band.pred_l1.get(i).copied().unwrap_or(false);
        if !pred_l0 && !pred_l1 {
            return None;
        }
        let l0 = pred_l0.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(band.mv0_x.get(i).copied().unwrap_or(0)), y: i32::from(band.mv0_y.get(i).copied().unwrap_or(0)) },
            ref_poc: band.ref_poc0.get(i).copied().unwrap_or(0),
        });
        let l1 = pred_l1.then(|| crate::motion::UniMotion {
            mv: crate::motion::Mv { x: i32::from(band.mv1_x.get(i).copied().unwrap_or(0)), y: i32::from(band.mv1_y.get(i).copied().unwrap_or(0)) },
            ref_poc: band.ref_poc1.get(i).copied().unwrap_or(0),
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
        let (bx, by) = (block_of(px), block_of(py));
        let Some(band) = self.band_for(by) else {
            return false;
        };
        let Some(i) = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by)) else {
            return false;
        };
        if !band.written.get(i).copied().unwrap_or(false) {
            return false;
        }
        band.is_skip.get(i).copied().unwrap_or(false)
    }

    /// Paint one inter luma transform-unit leaf's own footprint (in
    /// 4-sample blocks) with whether it coded any non-zero coefficient —
    /// called once per leaf from `ctu::reconstruct_luma_inter`, mirroring
    /// [`CuGrid::fill_motion`]'s own per-leaf timing.
    pub(crate) fn fill_cbf_luma(&mut self, bx0: usize, by0: usize, blocks_w: usize, blocks_h: usize, cbf: bool) {
        let cols = self.shared.cols;
        let band_rows = self.shared.band_rows;
        let local_by0 = self.local_of(by0);
        let Some(band) = self.current_band_mut(by0) else { return };
        for local_by in local_by0..local_by0.saturating_add(blocks_h) {
            for bx in bx0..bx0.saturating_add(blocks_w) {
                if let Some(i) = cu_index_in(cols, band_rows, bx, local_by)
                    && let Some(slot) = band.cbf_luma.get_mut(i)
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
        let (bx, by) = (block_of(px), block_of(py));
        let Some(band) = self.band_for(by) else {
            return false;
        };
        let Some(i) = cu_index_in(self.shared.cols, self.shared.band_rows, bx, self.local_of(by)) else {
            return false;
        };
        band.cbf_luma.get(i).copied().unwrap_or(false)
    }

    /// The total bytes [`Budget::alloc`] charged across every row band's own
    /// nine tracked arrays (`depth`/`mode`/`qp`/`mv0_x`/`mv0_y`/`ref_poc0`
    /// always, `mv1_x`/`mv1_y`/`ref_poc1` at their real length — `0` for a
    /// P/I slice's grid, non-zero for a B slice's, exactly as
    /// [`CuGridBand::new`] charged them) — what `decoder.rs` gives back via
    /// [`Budget::release`] once a slice's own CTU walk is done with this
    /// grid. Sums `published` (every finished band) plus `current` (only
    /// non-empty between construction and [`CuGrid::finish`]), so this
    /// always matches whatever [`CuGrid::new`]/[`CuGrid::begin_row`] have
    /// actually charged so far, at any point in the grid's lifetime, not
    /// only after [`CuGrid::finish`].
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
        let published: u64 = self.shared.published.iter().map(CuGridBand::budget_bytes).fold(0u64, u64::saturating_add);
        published.saturating_add(self.current.as_ref().map_or(0, CuGridBand::budget_bytes))
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
/// machinery would be solving a problem this data does not have.
///
/// `published` is [`crate::wavefront::RowPublish`] (not a plain `Vec`):
/// `docs/codec/hevc-wavefront-threading.md`'s "Stage 2b's concrete
/// prerequisites" section found that a plain `Vec<EdgeBand>` is *not*
/// `Sync`-safe the moment Stage 2 has more than one row worker in flight.
///
/// Stage 2b step 3's first commit goes one step further: `shared` (the
/// geometry plus `published`) is now its own type, [`EdgeMarksShared`],
/// separate from `current`/`current_band`. `RowPublish` alone fixed the
/// *read* side (`docs/codec/hevc-wavefront-threading.md`'s own "step 1
/// closed only half of each race"); this split is what makes the *write*
/// side expressible for real dispatch: `EdgeMarksShared` is what a future
/// `Arc` wraps and hands to every row worker (no `current` inside it to
/// race over), while `current`/`current_band` become the per-row-owned
/// value each worker keeps to itself — that move is `Ctx`'s own split
/// (step 3's second commit), not this one. For now, `EdgeMarks` still
/// bundles both — Stage 1/2b step 2 are still single-threaded, so there is
/// exactly one owner of the whole thing either way — but every method
/// below already routes through `self.shared`/`self.current` explicitly,
/// so splitting them across two owners later is a move, not a rewrite.
#[derive(Debug, Clone)]
struct EdgeMarksShared {
    cols: usize,
    /// 4x4-block rows per CTU row band — the same quantity
    /// [`ReconPlane::band_h`] tracks in luma samples, here in block units.
    band_rows: usize,
    /// Total row bands in the picture — [`EdgeMarks::finish`] advances
    /// `current_band` past this so every read routes to `published`
    /// afterward, the same trick [`ReconPlane::finish`] uses.
    n_bands: usize,
    /// Every row band strictly before `current_band`, published the moment
    /// [`EdgeMarks::begin_row`]/[`EdgeMarks::finish`] moved past it — the
    /// read side ([`EdgeMarks::vert_at`] and friends) for any row not in
    /// `current`. See [`EdgeMarks`]'s own doc for why this is
    /// [`crate::wavefront::RowPublish`] rather than a plain `Vec`.
    published: crate::wavefront::RowPublish<EdgeBand>,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeMarks {
    shared: std::sync::Arc<EdgeMarksShared>,
    /// The row band [`EdgeMarks::mark_vert`] and friends currently write
    /// into; every earlier band already lives in `shared.published`.
    current_band: usize,
    current: EdgeBand,
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
            shared: std::sync::Arc::new(EdgeMarksShared {
                cols,
                band_rows,
                n_bands,
                // Fixed-size at construction, one slot per row band — see
                // this struct's own doc for why this is `RowPublish` rather
                // than a plain, amortised-growth `Vec`.
                published: crate::wavefront::RowPublish::new(n_bands),
            }),
            current_band: 0,
            current: EdgeBand::new(band_len),
        }
    }

    /// The row band containing 4x4-block row `by`.
    #[allow(clippy::integer_division, reason = "row band index = block row / the fixed CTB row-band height")]
    fn band_of(&self, by: usize) -> usize {
        by / self.shared.band_rows
    }

    /// `by`'s own row within whichever band [`EdgeMarks::band_of`] says it
    /// belongs to.
    #[allow(clippy::integer_division, reason = "same fixed CTB row-band height as band_of, its own remainder")]
    fn local_of(&self, by: usize) -> usize {
        by % self.shared.band_rows
    }

    fn index_in(&self, bx: usize, local_by: usize) -> Option<usize> {
        if bx >= self.shared.cols || local_by >= self.shared.band_rows {
            return None;
        }
        Some(local_by * self.shared.cols + bx)
    }

    /// Advance to CTU row `row_band`: publish every row band strictly
    /// before it and reset `current` for the new one — the same-shaped
    /// counterpart of [`ReconPlane::begin_row`], called from the same call
    /// sites right alongside it. Idempotent for a `row_band` already
    /// current, including once, harmlessly, for row `0`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `row_band` goes backward, or (unreachable in
    /// practice: `current_band` only ever advances, one slot at a time,
    /// strictly within `[0, n_bands)`) if [`crate::wavefront::RowPublish`]
    /// itself refuses a publish.
    pub(crate) fn begin_row(&mut self, row_band: usize) -> Result<()> {
        if row_band < self.current_band {
            return Err(Error::InvalidData("vaco-codec-hevc: edge marks rows must advance in order"));
        }
        let band_len = self.shared.cols.saturating_mul(self.shared.band_rows);
        while self.current_band < row_band {
            let finished = std::mem::replace(&mut self.current, EdgeBand::new(band_len));
            self.shared.published.publish(self.current_band, finished)?;
            self.current_band = self.current_band.saturating_add(1);
        }
        Ok(())
    }

    /// Publish the last row band once the whole CTU walk is done, and
    /// advance `current_band` one past the last real band — mirroring
    /// [`ReconPlane::finish`] exactly, and for the same reason: every read
    /// after this point must route to `published` (the `Equal` branch in
    /// [`EdgeMarks::vert_at`] and friends would otherwise still match the
    /// last row band and see the fresh, empty `current` this leaves
    /// behind, not the data [`EdgeMarks::finish`] just moved out of it).
    /// Called once, right alongside [`ReconPlane::finish`], before
    /// deblocking or SAO ever read an [`EdgeMarks`] query.
    ///
    /// # Errors
    /// [`vaco_core::Error`], unreachable in practice for the same reason
    /// [`EdgeMarks::begin_row`]'s own `Errors` section gives.
    pub(crate) fn finish(&mut self) -> Result<()> {
        let band_len = self.shared.cols.saturating_mul(self.shared.band_rows);
        while self.current_band < self.shared.n_bands {
            let finished = std::mem::replace(&mut self.current, EdgeBand::new(band_len));
            self.shared.published.publish(self.current_band, finished)?;
            self.current_band = self.current_band.saturating_add(1);
        }
        Ok(())
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
            std::cmp::Ordering::Less => self.shared.published.get(self.band_of(by)).and_then(|b| b.vert.get(i)).copied().unwrap_or(false),
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
            std::cmp::Ordering::Less => self.shared.published.get(self.band_of(by)).and_then(|b| b.horiz.get(i)).copied().unwrap_or(false),
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
            std::cmp::Ordering::Less => self.shared.published.get(self.band_of(by)).and_then(|b| b.tu_vert.get(i)).copied().unwrap_or(false),
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
            std::cmp::Ordering::Less => self.shared.published.get(self.band_of(by)).and_then(|b| b.tu_horiz.get(i)).copied().unwrap_or(false),
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

// --- Stage 1/2: the CTU walk's own in-progress reconstruction buffer -----
//
// `Plane`/`Picture` above are unchanged: the shape every already-finished
// reference picture in the `Dpb` is stored as, and the shape deblocking, SAO
// and emission all already read and write. `ReconPlane`/`ReconPicture`
// below are new, additive types for *this* picture's own CTU walk
// specifically — see `docs/codec/hevc-wavefront-threading.md`'s "Concrete
// Stage 1 plan" for why the reconstruction buffer needs its own type rather
// than `Plane` itself growing a publish mechanism of its own: once a tile
// publishes it is immutable forever (the whole point of the mechanism),
// but deblocking and SAO both need to modify pixels the CTU walk already
// finished. `ReconPicture::materialize_into` is the one-time hand-off
// between the two: read every published tile back into a plain, mutable
// `Picture` once the whole walk is done, which the existing deblock/SAO/
// emission code then keeps using exactly as it always has.
//
// `ReconPlane` was row-banded for Stage 1, deliberately: single-threaded,
// it had no wavefront dependency that needed column granularity, and
// paying a 2-D grid's cost before anything needed it would have been the
// same mistake this design doc's own "Correction" section describes
// elsewhere. Stage 2 is exactly the "anything" that needs it: row `r + 1`'s
// worker must be able to read row `r`'s own CTU `c + 1` while row `r`'s
// worker is still only two CTUs in, which a full-width row band cannot
// express at any band height. `ReconPlane` therefore moved to a 2-D
// per-CTU tile grid (`3ac859f`) — first built on
// `vaco_codec_core::picture`'s own `PlaneSpec::with_bands`/`tile_mut`/
// `publish_tile`/`tile_ref`, then rebuilt again here (Stage 2b step 4's own
// prerequisite, `docs/codec/hevc-wavefront-threading.md`'s "don't share the
// writer" finding) once it became clear that primitive's `tile_mut`/
// `publish_tile` both require `&mut self`: fine for one worker checking out
// tiles one at a time in sequence, but no shape at all for N workers each
// wanting to own a *different* tile concurrently — Rust's own borrow
// checker forbids two live `&mut` borrows of the same `PictureWriter`
// regardless of which tiles they would touch.
//
// The fix is the same shape `EdgeMarks`/`CuGrid`/`SaoParamsGrid` already
// proved (Stage 2b step 1, `planning/E2E-GAPS.md` §§41-43): `current` is
// *owned outright* by whoever is filling it — a private `TileBuffer`, a
// plain `Vec<u8>` with no shared writer behind it at all, so there is
// nothing for two workers to race over or borrow-check against — and
// `published` is `RowPublish`, the same write-once-per-slot board every
// other structure in this crate now publishes through. A worker decoding
// CTU `(row, col)` owns its own `TileBuffer` free and clear, writes into it
// with ordinary indexing, and hands it to the shared board exactly once,
// on completion; every other worker reads a finished tile through
// `RowPublish::get` and never touches one still being filled. Nothing here
// needs `&mut self` on a structure two workers share, so the constraint
// that blocked `vaco_codec_core::picture`'s own writer never binds, and no
// interior mutability is needed anywhere in `#![forbid(unsafe_code)]`. The
// one operation that genuinely needs whole-picture mutable access —
// deblocking and SAO, modifying already-reconstructed pixels — stays
// outside this concurrent region entirely: `materialize_into` hands the
// finished, fully-published reconstruction to a plain `Picture` after
// every tile joins, exactly as Stage 1 already arranged, and that pass
// stays serial until it is itself made row-lagged as separate later work.
//
// A subtlety every "current owned/mutable band, earlier bands published
// read-only" type in this module shares, worth stating once rather than
// re-discovering per type: on `finish`, the *last* tile must still be
// published, and whatever plays the role of `current` afterward must stop
// being reachable by a read for that last tile's own index — not merely
// have its old, real contents moved out. `ReconPlane` does this by
// advancing `current_row`/`current_col` past the last valid tile once
// `finish` publishes through the end, mirroring `EdgeMarks`/`CuGrid`/
// `SaoParamsGrid`'s identical move in their own `finish`. Skipping it is a
// silent bug, not a loud one: a read for the last tile after `finish`
// still finds a same-shaped, in-range answer — the freshly emptied
// `current` left behind — so nothing panics or errors, it is simply wrong.

/// One CTU tile's own pixel buffer — owned outright by whoever is filling
/// it (`ReconPlane::current`), until [`ReconPlane::publish_ctu`] hands it
/// to the shared board. Row-major, `w * h` samples, no stride padding: a
/// private buffer with exactly one reader (its own owner) while unfinished
/// has no guard row to reserve for a cross-boundary read the way
/// `vaco_codec_core::picture`'s own bands do — every cross-tile read in
/// this crate already goes through `ReconPlane::get`, which checks
/// `is_ready`/`is_published` itself rather than trusting padding.
#[derive(Debug, Default)]
struct TileBuffer {
    data: Vec<u8>,
    w: usize,
    h: usize,
}

impl TileBuffer {
    fn zeroed(w: usize, h: usize) -> Self {
        Self { data: vec![0u8; w.saturating_mul(h)], w, h }
    }

    fn get(&self, lx: usize, ly: usize) -> Option<u8> {
        if lx >= self.w || ly >= self.h {
            return None;
        }
        self.data.get(ly.saturating_mul(self.w).saturating_add(lx)).copied()
    }

    fn row_mut(&mut self, ly: usize) -> Option<&mut [u8]> {
        if ly >= self.h {
            return None;
        }
        let start = ly.saturating_mul(self.w);
        self.data.get_mut(start..start.saturating_add(self.w))
    }
}

/// The CTU walk's own in-progress reconstruction buffer for one component
/// plane — see this module's own "Stage 1/2" section doc above.
struct ReconPlaneShared {
    width: usize,
    height: usize,
    ctb_size: usize,
    n_row_bands: usize,
    n_col_bands: usize,
    /// The fixed dimension of the per-tile `ready` grid (`ReconPlane`'s own
    /// `current` side) — derived once from `ctb_size`, constant for the
    /// picture's whole lifetime even though `ready`'s own *contents* reset
    /// every tile.
    ready_dim: usize,
    /// Every CTU tile strictly before the one currently open, published the
    /// moment [`ReconPlane::publish_ctu`]/[`ReconPlane::finish`] moved past
    /// it — indexed by raster CTU address (`row * n_col_bands + col`), the
    /// read side ([`ReconPlane::get`]) for any tile not currently open.
    published: crate::wavefront::RowPublish<TileBuffer>,
}

/// See this module's own "Stage 1/2" section doc above for `shared`
/// (geometry plus the `RowPublish` board) versus `current` (this plane's
/// own in-progress tile, owned outright, no shared writer behind it).
pub(crate) struct ReconPlane {
    shared: std::sync::Arc<ReconPlaneShared>,
    /// The CTU tile most recently opened for writes via
    /// [`ReconPlane::begin_ctu`] — every tile strictly before it in raster
    /// CTU order (a full row above, or an earlier column of this same row)
    /// is either already published or, for [`ReconPlane::finish`]'s own
    /// "picture ended early" case, about to be. `current_published` tracks
    /// whether *this* tile itself has been [`ReconPlane::publish_ctu`]-ed
    /// yet, since a read can arrive between `begin_ctu` and `publish_ctu`
    /// (same-CTU reference-line/reconstruction reads) as well as after.
    current_row: usize,
    current_col: usize,
    current_published: bool,
    current: TileBuffer,
    /// Per-4x4-block "has this block been written yet", scoped to the
    /// current tile only and reset whenever it advances — a tile is at
    /// most `ctb_size` square, so this is far smaller than Stage 1's own
    /// whole-row-band `ready` grid was. Same reasoning as `Plane::ready`
    /// above, re-derived fresh per tile instead of per row band.
    ready: Vec<bool>,
}
impl ReconPlane {
    /// `(row, col)`'s own real pixel dimensions — `ctb_size` square except
    /// at the picture's right/bottom edge, where the last row/column of
    /// tiles is whatever remains.
    fn tile_dims(width: usize, height: usize, ctb_size: usize, row: usize, col: usize) -> (usize, usize) {
        let w = ctb_size.min(width.saturating_sub(col.saturating_mul(ctb_size)));
        let h = ctb_size.min(height.saturating_sub(row.saturating_mul(ctb_size)));
        (w, h)
    }

    /// `(row, col)`'s own raster CTU address — the index [`RowPublish`]
    /// addresses it by.
    fn tile_addr(&self, row: usize, col: usize) -> usize {
        row.saturating_mul(self.shared.n_col_bands).saturating_add(col)
    }

    /// # Errors
    /// [`vaco_core::Error`] if the allocation exceeds `budget`.
    pub(crate) fn new(budget: &mut Budget, width: usize, height: usize, ctb_size: usize) -> Result<Self> {
        let ctb_size = ctb_size.max(1);
        let n_row_bands = height.div_ceil(ctb_size).max(1);
        let n_col_bands = width.div_ceil(ctb_size).max(1);
        // One upfront accounting charge for the whole plane, matching the
        // single charge `vaco_codec_core::picture::ProgressPicture::allocate`
        // used to make at this same construction point -- `Budget::charge`
        // rather than `Budget::alloc`, since nothing needs the bytes
        // themselves yet: each tile's own `Vec<u8>` is allocated as the
        // walk actually reaches it, in `begin_ctu` below, the same
        // incremental-charge shape `CuGrid`'s own doc explains. This is a
        // pure accounting reservation, not a buffer -- never released
        // within this crate today, matching `ReconPicture`'s existing
        // charge lifetime exactly (unchanged by this commit either way).
        // Slightly less than the old row-banded, single-column case could
        // charge (that path reserved extra guard rows this tile-only
        // design has no use for and never allocates), never more.
        budget.charge(u64::try_from(width.saturating_mul(height)).unwrap_or(u64::MAX))?;
        let ready_dim = ctb_size.div_ceil(4).max(1);
        let ready = vec![false; ready_dim.saturating_mul(ready_dim)];
        let (w0, h0) = Self::tile_dims(width, height, ctb_size, 0, 0);
        Ok(Self {
            shared: std::sync::Arc::new(ReconPlaneShared {
                width,
                height,
                ctb_size,
                n_row_bands,
                n_col_bands,
                ready_dim,
                published: crate::wavefront::RowPublish::new(n_row_bands.saturating_mul(n_col_bands)),
            }),
            current_row: 0,
            current_col: 0,
            current_published: false,
            current: TileBuffer::zeroed(w0, h0),
            ready,
        })
    }

    /// The CTU tile containing luma-grid pixel `(x, y)`.
    #[allow(clippy::integer_division, reason = "tile index = pixel coordinate / the fixed CTB tile size")]
    fn tile_of(&self, x: usize, y: usize) -> (usize, usize) {
        (y / self.shared.ctb_size, x / self.shared.ctb_size)
    }

    /// `(x, y)`'s own position within whichever tile [`ReconPlane::tile_of`]
    /// says it belongs to.
    #[allow(clippy::integer_division, reason = "same fixed CTB tile size as tile_of, its own remainder")]
    fn local_of(&self, x: usize, y: usize) -> (usize, usize) {
        (x % self.shared.ctb_size, y % self.shared.ctb_size)
    }

    fn ready_index(&self, bx: usize, by: usize) -> Option<usize> {
        if bx >= self.shared.ready_dim || by >= self.shared.ready_dim {
            return None;
        }
        Some(by * self.shared.ready_dim + bx)
    }

    /// Open CTU `(row, col)` for writes: allocate a fresh, freely owned
    /// tile buffer, reset the per-tile ready grid, and record it as
    /// current. Idempotent for a tile already current (including once,
    /// harmlessly, for CTU `(0, 0)`); called once per CTU by the walk's
    /// own outer loop, immediately before that CTU's own `decode_ctu`.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `(row, col)` is not the next CTU in raster
    /// order after whichever one is currently open.
    pub(crate) fn begin_ctu(&mut self, row: usize, col: usize) -> Result<()> {
        if (row, col) == (self.current_row, self.current_col) {
            return Ok(());
        }
        let is_next = if self.current_published {
            // Either the next column of the same row, or column 0 of the
            // next row — the two raster-order successors of a published
            // tile.
            (row == self.current_row && col == self.current_col.saturating_add(1))
                || (row == self.current_row.saturating_add(1) && col == 0 && self.current_row.saturating_add(1) < self.shared.n_row_bands)
        } else {
            false
        };
        if !is_next {
            return Err(Error::InvalidData("vaco-codec-hevc: recon plane CTUs must advance one at a time in raster order"));
        }
        let (w, h) = Self::tile_dims(self.shared.width, self.shared.height, self.shared.ctb_size, row, col);
        self.current = TileBuffer::zeroed(w, h);
        self.current_row = row;
        self.current_col = col;
        self.current_published = false;
        self.ready.fill(false);
        Ok(())
    }

    /// Publish the CTU tile currently open — must be called exactly once
    /// per CTU, after that CTU's own reconstruction
    /// ([`ctu::decode_ctu`](crate::ctu::decode_ctu)) is done, before the
    /// next CTU's [`ReconPlane::begin_ctu`]. Hands `current` to the shared
    /// board outright (`std::mem::take` leaves an empty placeholder behind,
    /// harmless since nothing reads `current` again once `current_published`
    /// is set) — no lock, no shared writer, just a move.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if `(row, col)` is not the tile currently open,
    /// or if publishing fails.
    pub(crate) fn publish_ctu(&mut self, row: usize, col: usize) -> Result<()> {
        if (row, col) != (self.current_row, self.current_col) {
            return Err(Error::InvalidData("vaco-codec-hevc: recon plane publish targeted a CTU that is not open"));
        }
        let addr = self.tile_addr(row, col);
        let finished = std::mem::take(&mut self.current);
        self.shared.published.publish(addr, finished)?;
        self.current_published = true;
        Ok(())
    }

    /// Publish every CTU tile the walk never explicitly published —
    /// everything from the currently-open one (if [`ReconPlane::publish_ctu`]
    /// was never called for it — an early `end_of_slice_segment_flag`/
    /// malformed-stream exit) through the last tile in the picture, each
    /// with whatever it holds (all-zero for one never even opened). Advances
    /// past the last real tile afterward — see this module's own "Stage 1"
    /// section doc for why every type built this way needs exactly this
    /// move, not merely publishing what remains in place.
    ///
    /// # Errors
    /// [`vaco_core::Error`] if publishing fails.
    pub(crate) fn finish(&mut self) -> Result<()> {
        let col_start = if self.current_published { self.current_col.saturating_add(1) } else { self.current_col };
        for row in self.current_row..self.shared.n_row_bands {
            let start = if row == self.current_row { col_start } else { 0 };
            for col in start..self.shared.n_col_bands {
                let addr = self.tile_addr(row, col);
                let buf = if (row, col) == (self.current_row, self.current_col) && !self.current_published {
                    std::mem::take(&mut self.current)
                } else {
                    let (w, h) = Self::tile_dims(self.shared.width, self.shared.height, self.shared.ctb_size, row, col);
                    TileBuffer::zeroed(w, h)
                };
                self.shared.published.publish(addr, buf)?;
            }
        }
        self.current_row = self.shared.n_row_bands;
        self.current_col = 0;
        self.current_published = true;
        Ok(())
    }

    /// Whether `(row, col)` is strictly before the tile currently open, in
    /// raster CTU order — the "already published, safe to read read-only"
    /// test every read method below shares.
    fn is_published(&self, row: usize, col: usize) -> bool {
        (row, col) < (self.current_row, self.current_col) || ((row, col) == (self.current_row, self.current_col) && self.current_published)
    }

    /// Whether `(x, y)`'s containing CTU is the one currently open for
    /// writes.
    fn is_current(&self, row: usize, col: usize) -> bool {
        (row, col) == (self.current_row, self.current_col) && !self.current_published
    }

    /// Whether `(x, y)`'s containing 4x4 block has already been fully
    /// reconstructed — [`Plane::is_ready`]'s exact counterpart.
    #[must_use]
    pub(crate) fn is_ready(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        if x >= self.shared.width || y >= self.shared.height {
            return false;
        }
        let (row, col) = self.tile_of(x, y);
        if self.is_published(row, col) {
            return true;
        }
        if !self.is_current(row, col) {
            return false;
        }
        let (lx, ly) = self.local_of(x, y);
        self.ready_index(block_of(lx), block_of(ly)).and_then(|i| self.ready.get(i)).copied().unwrap_or(false)
    }

    /// The sample at `(x, y)`, or `0` out of range or not yet written —
    /// [`Plane::get`]'s exact counterpart, reading through whichever of the
    /// still-open current tile or an already-published one owns it.
    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> u16 {
        if x >= self.shared.width || y >= self.shared.height {
            return 0;
        }
        let (row, col) = self.tile_of(x, y);
        let (lx, ly) = self.local_of(x, y);
        if self.is_current(row, col) {
            return self.current.get(lx, ly).map_or(0, u16::from);
        }
        if self.is_published(row, col) {
            let addr = self.tile_addr(row, col);
            let Some(tile) = self.shared.published.get(addr) else { return 0 };
            return tile.get(lx, ly).map_or(0, u16::from);
        }
        0
    }

    /// [`Plane::mark_block_ready`]'s exact counterpart, scoped to the
    /// current tile — marking a position outside it is a silent no-op
    /// (either already fully ready, by construction, or not this tile's
    /// own write to make at all).
    pub(crate) fn mark_block_ready(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        if w == 0 || h == 0 || x0 >= self.shared.width || y0 >= self.shared.height {
            return;
        }
        let (row, col) = self.tile_of(x0, y0);
        if !self.is_current(row, col) {
            return;
        }
        let (lx0, ly0) = self.local_of(x0, y0);
        let lx1 = lx0.saturating_add(w).saturating_sub(1).min(self.shared.ctb_size.saturating_sub(1));
        let ly1 = ly0.saturating_add(h).saturating_sub(1).min(self.shared.ctb_size.saturating_sub(1));
        let (bx0, by0, bx1, by1) = (block_of(lx0), block_of(ly0), block_of(lx1), block_of(ly1));
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

    /// Write `src` into the plane starting at `(x0, y0)`, one row,
    /// `src.len()` samples — [`Plane::row_mut`]'s counterpart, reshaped for
    /// tile storage: a tile-banded plane cannot hand back a raw
    /// `&mut [u8]` spanning picture-wide coordinates the way a row-banded
    /// one could (the row above `(x0, y0)`'s own tile boundary is a
    /// *different* tile, possibly not even allocated the way this one is
    /// mid-write), so every write goes through this instead of
    /// `row_mut`/`into_row_mut` directly. The whole `[x0, x0 + src.len())`
    /// range must lie within the CTU currently open (true of every real
    /// caller: a coding unit's footprint never crosses a CTU boundary) —
    /// silently does nothing otherwise, matching this module's own
    /// "malformed input degrades, never panics" convention.
    pub(crate) fn write_row(&mut self, x0: usize, y0: usize, src: &[u8]) {
        if y0 >= self.shared.height || src.is_empty() {
            return;
        }
        let (row, col) = self.tile_of(x0, y0);
        if !self.is_current(row, col) {
            return;
        }
        let (lx0, ly0) = self.local_of(x0, y0);
        let Some(dst) = self.current.row_mut(ly0).and_then(|r| r.get_mut(lx0..lx0.saturating_add(src.len()))) else { return };
        dst.copy_from_slice(src);
    }

    /// Copy every published CTU tile into `dst`, and mark it ready there
    /// too — the one-time hand-off `ReconPicture::materialize_into` uses.
    /// Must be called after [`ReconPlane::finish`]; a tile that never
    /// published (should not happen — `finish` publishes everything) is
    /// silently skipped rather than panicking, matching this module's own
    /// "missing reads as zero/unready, never as a crash" convention
    /// throughout.
    fn materialize_into(&self, dst: &mut Plane) {
        for row in 0..self.shared.n_row_bands {
            for col in 0..self.shared.n_col_bands {
                let addr = self.tile_addr(row, col);
                let Some(tile) = self.shared.published.get(addr) else { continue };
                let x0 = col.saturating_mul(self.shared.ctb_size);
                let y0 = row.saturating_mul(self.shared.ctb_size);
                for ly in 0..tile.h {
                    let Some(src_row) = tile.data.get(ly.saturating_mul(tile.w)..).and_then(|r| r.get(..tile.w)) else { continue };
                    let y = y0.saturating_add(ly);
                    if let Some(dst_row) = dst.row_mut(y).and_then(|r| r.get_mut(x0..x0.saturating_add(tile.w))) {
                        dst_row.copy_from_slice(src_row);
                    }
                    dst.mark_row_ready(y, x0, tile.w);
                }
            }
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
    /// `ctb_size` is the *luma* CTB size; chroma's own tile size is half
    /// that (rounded up), matching this crate's 4:2:0-only scope exactly
    /// the way [`Picture::new`]'s own `cw`/`ch` halving already does. Luma
    /// and chroma end up with the same CTU grid (same `n_row_bands`/
    /// `n_col_bands`), just at half the sample density, so one `(row, col)`
    /// CTU address addresses all three planes' tiles at once.
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

    /// [`ReconPlane::begin_ctu`], across all three planes at once — the
    /// call the CTU walk's own outer loop makes once per CTU, immediately
    /// before that CTU's own `decode_ctu`.
    ///
    /// # Errors
    /// As [`ReconPlane::begin_ctu`].
    pub(crate) fn begin_ctu(&mut self, row: usize, col: usize) -> Result<()> {
        self.y.begin_ctu(row, col)?;
        self.cb.begin_ctu(row, col)?;
        self.cr.begin_ctu(row, col)?;
        Ok(())
    }

    /// [`ReconPlane::publish_ctu`], across all three planes — the call the
    /// CTU walk's own outer loop makes once per CTU, immediately after
    /// `decode_ctu` returns.
    ///
    /// # Errors
    /// As [`ReconPlane::publish_ctu`].
    pub(crate) fn publish_ctu(&mut self, row: usize, col: usize) -> Result<()> {
        self.y.publish_ctu(row, col)?;
        self.cb.publish_ctu(row, col)?;
        self.cr.publish_ctu(row, col)?;
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
