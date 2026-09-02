//! Frame threading without `unsafe`: guard-padded bands published through
//! `OnceLock`, banded by row for cross-picture pipelining or by row *and*
//! column for intra-picture wavefronts.
//!
//! Frame threading needs "frame N+1 may proceed once frame N has produced row
//! R", but the conventional answer — one contiguous buffer, a raw pointer and
//! an atomic row counter — needs `unsafe` (D2), and ordinary borrow rules
//! cannot express "`&mut` above row R and `&` below it, with R moving over
//! time".
//!
//! A plane is instead a grid of **bands** (one column by default; more than
//! one when a plane asks for it — see "Column bands" below), each a block of
//! `band_h` rows preceded by `guard` rows of context copied from the band
//! above. The writer owns a band exclusively while filling it, then *moves*
//! it into a [`OnceLock`] — exactly where it stops being mutable and starts
//! being shared. `OnceLock::set` is a release store and `OnceLock::get` an
//! acquire load, so no lock is needed on the fast path and no partially
//! written band is ever observable:
//!
//! ```text
//! band_mut(k) -> exclusive &mut [u8], ...fill...
//! publish_through(k): copy guard rows from band k-1, bands[k].set(band)
//!   (release) ──► reader: bands[k].get() (acquire), ready.load()/wait_rows()
//! ```
//!
//! [`PictureWriter`] is neither `Sync` nor `Clone`: exactly one frame task
//! holds it. [`PictureRef`] is cheap to clone and `Send + Sync`, held by tasks
//! for their reference pictures. The compiler proves the absence of a data
//! race, not a convention.
//!
//! # Column bands
//!
//! [`PlaneSpec::with_bands`] additionally splits a plane's width into
//! `band_w`-wide column bands, giving a 2-D grid instead of one column of
//! full-width row bands. This exists for a different dependency shape than
//! cross-picture pipelining's: a wavefront where row `r + 1`'s worker needs
//! row `r`'s column `c` done *while row `r`'s own worker is still only a few
//! columns in* — a dependency on partial width at a fixed height, which a
//! full-width row band cannot express at any `band_h` (a band publishes, and
//! becomes readable, only as one atomic release over its *entire* row).
//! [`PictureWriter::publish_tile`]/[`PictureRef::wait_tile`] are the
//! column-banded counterparts of `publish_through`/`wait_rows`, keyed by
//! `(row_band, col_band)` instead of a row number.
//!
//! This is the same mechanism generalised, not a second one (D19/D23): a
//! full-width row band is exactly a one-column-band plane (`band_w` left at
//! its default, `0`, meaning "whole width"), and every row-oriented method
//! below is defined only for that case — `PictureSpec::new` without
//! `PlaneSpec::with_bands` reproduces today's behaviour exactly, byte for
//! byte. What does **not** generalise is the *read* side: [`PlaneView::row`]
//! and [`PlaneView::block`] promise one contiguous borrow per row, which is
//! true for a full-width band but cannot be true once a row's bytes are
//! split across independently-allocated column bands — there is no single
//! slice to hand back without copying. Column-banded planes therefore use
//! [`BlockRef`]-per-tile reads ([`PictureRef::wait_tile`]/`try_tile`) instead
//! of [`PlaneView`]; [`PictureRef::ready_rows`]/`wait_rows`/`finished` still
//! report a genuine "every column of these rows is done" count (useful for
//! "is the whole picture finished" queries) even for a column-banded plane,
//! but [`PlaneView::row`]/`block` refuse to serve one, rather than silently
//! returning only the first column's bytes.
//!
//! Column bands carry no guard rows (`PlaneSpec::with_bands` forces
//! `guard = 0`): a guard row is a physical copy from the band *directly*
//! above, which only ever covers one neighbour, while a wavefront's
//! above-right reference reaches into a second, diagonally adjacent tile.
//! Cross-tile reads instead look up whichever neighbour tile they need
//! directly, once its own publish is confirmed — the same "read past what
//! was waited for must be refused, not served" discipline `wait_rows`
//! already gives row-banded planes, applied per tile.
//!
//! Notable choices: these primitives live here rather than in `vaco-frame` (a
//! frozen layer-1 crate owned elsewhere, and a decoder-only concept fits the
//! codec framework better anyway — moving it later is a re-export). A band
//! carries a *top* guard only: a bottom guard would need rows from the band
//! below, which is written after the band above is already immutable, so it
//! could never be filled — reads past a band's last row are served by the
//! *next* band's top guard instead. `ready` therefore advances to the last row
//! of the published band rather than lagging by `guard`; the caller adds its
//! own filter reach when deciding which row to wait for. [`PlaneView::block`]
//! returns a `Result` because the copy path needs scratch space and an
//! undersized buffer must be reported, not silently truncated — codecs size
//! scratch from their largest block, so this never fires in practice.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Rows of context a band carries above its own first row.
///
/// Eight rows covers the maximum inter-prediction filter reach of H.264, HEVC,
/// VP9 and AV1, so a motion-compensated read that strays above a band's first
/// row still lands inside one contiguous allocation. Column-banded planes
/// ignore this — see the module doc's "Column bands" section.
pub const DEFAULT_GUARD: u32 = 8;

/// Default rows per band.
///
/// Large enough that the overwhelming majority of 8×8 and 16×16 block reads sit
/// inside one band, small enough that a consumer does not wait long for the
/// producer to publish.
pub const DEFAULT_BAND_HEIGHT: u32 = 256;

/// One plane's geometry, plus how it is banded.
///
/// Banding used to live on [`PictureSpec`] as one pair of values shared by
/// every plane; it is per-plane now, because a picture's planes are not
/// always the same size (chroma is typically half-resolution) and a
/// column-banded codec generally wants each plane's own tile aligned to its
/// own coding-tree grid — HEVC's chroma CTB is half its luma CTB's width and
/// height, not the same absolute size. `PictureSpec::with_band_height`/
/// `with_guard`/`single_band` still exist and still take one value each; they
/// now just apply it to every plane already added, which reproduces the old
/// picture-wide behaviour exactly for a caller that never calls
/// `PlaneSpec::with_bands`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneSpec {
    /// Meaningful bytes per row. May be less than `stride`.
    pub width_bytes: u32,
    /// Rows in the plane.
    pub height: u32,
    /// Bytes between the start of consecutive rows.
    pub stride: usize,
    band_w: u32,
    band_h: u32,
    guard: u32,
}

impl PlaneSpec {
    /// A plane whose stride is its width.
    #[must_use]
    pub const fn new(width_bytes: u32, height: u32) -> Self {
        Self {
            width_bytes,
            height,
            stride: width_bytes as usize,
            band_w: 0,
            band_h: DEFAULT_BAND_HEIGHT,
            guard: DEFAULT_GUARD,
        }
    }

    /// A plane with an explicit stride, clamped to at least the width.
    #[must_use]
    pub const fn with_stride(width_bytes: u32, height: u32, stride: usize) -> Self {
        Self {
            width_bytes,
            height,
            stride: if stride < width_bytes as usize {
                width_bytes as usize
            } else {
                stride
            },
            band_w: 0,
            band_h: DEFAULT_BAND_HEIGHT,
            guard: DEFAULT_GUARD,
        }
    }

    /// Band this plane into a 2-D grid of `band_w`-wide, `band_h`-tall tiles
    /// instead of one column of full-width row bands — see the module doc's
    /// "Column bands" section for why a full-width band cannot express a
    /// wavefront's own dependency shape at any height.
    ///
    /// Forces this plane's guard to `0`: see the module doc for why a
    /// column-banded plane's cross-tile reads do not use guard rows at all.
    /// `band_h` is clamped to at least one row; `band_w` left at `0` keeps
    /// this plane single-column (today's behaviour) while still letting the
    /// row height differ from `PictureSpec::with_band_height`'s picture-wide
    /// default.
    #[must_use]
    pub const fn with_bands(mut self, band_w: u32, band_h: u32) -> Self {
        self.band_w = band_w;
        self.band_h = if band_h == 0 { 1 } else { band_h };
        self.guard = 0;
        self
    }
}

/// How a picture is banded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureSpec {
    planes: Vec<PlaneSpec>,
}

impl PictureSpec {
    /// Bands of [`DEFAULT_BAND_HEIGHT`] rows with [`DEFAULT_GUARD`] rows of
    /// context, one column wide — the defaults every [`PlaneSpec`] already
    /// carries.
    #[must_use]
    pub fn new(planes: Vec<PlaneSpec>) -> Self {
        Self { planes }
    }

    /// Set every plane's own band height. Clamped to at least one row.
    ///
    /// Applies to the planes already added, the same way it always has —
    /// this is a picture-wide convenience over [`PlaneSpec`]'s own per-plane
    /// field, not a separate value, so calling this after
    /// [`PlaneSpec::with_bands`] on one of `planes` overwrites that plane's
    /// own row height too. A caller that wants different planes banded
    /// differently sets each `PlaneSpec` up first and does not call this.
    #[must_use]
    pub fn with_band_height(mut self, rows: u32) -> Self {
        let h = if rows == 0 { 1 } else { rows };
        for p in &mut self.planes {
            p.band_h = h;
        }
        self
    }

    /// Set every plane's own guard depth.
    #[must_use]
    pub fn with_guard(mut self, rows: u32) -> Self {
        for p in &mut self.planes {
            p.guard = rows;
        }
        self
    }

    /// One band per plane, covering the whole picture.
    ///
    /// The first escape hatch of plan 15 §1.8.3: whenever frame threading is
    /// off or the codec is intra-only, a single band means every
    /// [`PlaneView::block`] takes the contiguous path and the non-threaded case
    /// pays nothing at all for this machinery.
    ///
    /// Each plane's own height is rounded up to a power of two so that the
    /// row-to-band mapping is a shift rather than a division here too — one
    /// band means the answer is always zero, and there is no reason to pay a
    /// `udiv` to learn that. Rounding up cannot add a band:
    /// `height.div_ceil(band_h)` is still one for any `band_h >= height`.
    #[must_use]
    pub fn single_band(mut self) -> Self {
        for p in &mut self.planes {
            let tallest = p.height.max(1);
            p.band_h = tallest.checked_next_power_of_two().unwrap_or(tallest);
            p.guard = 0;
            p.band_w = 0;
        }
        self
    }

    /// The planes, in order.
    #[must_use]
    pub fn planes(&self) -> &[PlaneSpec] {
        &self.planes
    }
}

/// One published, immutable band: `guard` rows of context (row-banded planes
/// only — always `0` for a column-banded one) followed by `body` rows of
/// `body_w` bytes each.
#[derive(Debug)]
struct Band {
    rows: Box<[u8]>,
    body: u32,
    body_w: u32,
}

#[derive(Debug, Default)]
struct PlaneWait {
    rows: u32,
    failed: bool,
}

/// One plane's publication state — a 2-D grid of bands, `n_row_bands` tall by
/// `n_col_bands` wide, flattened row-major (`row_band * n_col_bands +
/// col_band`). `n_col_bands == 1` (the default: `PlaneSpec::with_bands` was
/// never called) is exactly the row-banded plane this module always had; the
/// row-oriented API (`row`/`ready`/`wait_rows`) is defined only for that case.
#[derive(Debug)]
struct ProgressPlane {
    bands: Box<[OnceLock<Band>]>,
    n_row_bands: usize,
    n_col_bands: usize,
    /// Columns published so far, per row band — the column-banded
    /// counterpart of `ready`, `n_row_bands` long. Always `0` or `1` per
    /// entry when `n_col_bands == 1`.
    col_ready: Box<[AtomicU32]>,
    /// Rows guaranteed readable *across the plane's full width*. Monotonic;
    /// the fast path is one atomic load. For a column-banded plane this only
    /// advances once every column of a row band has published — it is a
    /// legitimate "is the whole picture done through here" signal even then,
    /// just not one [`PlaneView::row`] can serve contiguously.
    ready: AtomicU32,
    state: Mutex<PlaneWait>,
    wake: Condvar,
    width_bytes: u32,
    height: u32,
    band_h: u32,
    band_w: u32,
    /// `band_h.trailing_zeros()` when `band_h` is a power of two, so
    /// [`ProgressPlane::band_of`] — which every block read runs — is a shift
    /// instead of a division by a runtime value.
    band_shift: Option<u32>,
    /// [`ProgressPlane::band_shift`]'s column-axis counterpart, `None`
    /// whenever this plane is not column-banded.
    col_shift: Option<u32>,
    guard: u32,
    /// Each band's own internal row stride. Equal to the plane's `stride`
    /// (which may exceed `width_bytes` — custom padding) when `n_col_bands ==
    /// 1`, preserving that feature exactly; equal to each band's own tight
    /// `body_w` when column-banded, since a column-banded plane's bands are
    /// independent allocations with no reason to pad.
    stride: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means a producer panicked. The invariant this mutex
    // protects — a monotonic row count and a failure flag — survives that, and
    // recovering keeps every waiter live, which is the whole point of the
    // failure path.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProgressPlane {
    #[expect(
        clippy::integer_division,
        reason = "band index from a row number; band_h is clamped to at least 1 at construction"
    )]
    const fn band_of(&self, row: u32) -> usize {
        match self.band_shift {
            Some(sh) => (row >> sh) as usize,
            None => (row / self.band_h) as usize,
        }
    }

    const fn band_first_row(&self, k: usize) -> u32 {
        (k as u32).saturating_mul(self.band_h)
    }

    fn band_body_rows(&self, k: usize) -> u32 {
        let first = self.band_first_row(k);
        self.height.saturating_sub(first).min(self.band_h)
    }

    const fn col_first_x(&self, c: usize) -> u32 {
        if self.n_col_bands <= 1 {
            0
        } else {
            (c as u32).saturating_mul(self.band_w)
        }
    }

    /// The column band containing byte offset `x` — [`ProgressPlane::band_of`]'s
    /// column-axis counterpart, for a caller with a pixel position rather
    /// than an already-known tile index (an above-right reference sample
    /// that may spill into the next column's tile, say).
    #[expect(
        clippy::integer_division,
        reason = "column band index from a byte offset; band_w is only divided by when it is confirmed non-zero"
    )]
    const fn col_of(&self, x: u32) -> usize {
        if self.n_col_bands <= 1 {
            return 0;
        }
        match self.col_shift {
            Some(sh) => (x >> sh) as usize,
            None => (x / self.band_w) as usize,
        }
    }

    fn col_body_w(&self, c: usize) -> u32 {
        if self.n_col_bands <= 1 {
            return self.width_bytes;
        }
        let first = self.col_first_x(c);
        self.width_bytes.saturating_sub(first).min(self.band_w)
    }

    /// The flattened index of tile `(row_band, col_band)`, or `None` out of
    /// range.
    const fn flat(&self, row_band: usize, col_band: usize) -> Option<usize> {
        if row_band >= self.n_row_bands || col_band >= self.n_col_bands {
            return None;
        }
        Some(row_band * self.n_col_bands + col_band)
    }

    /// Bytes of one published row, or `None` if its band is not published,
    /// the row is outside the plane, or this plane is column-banded (a row's
    /// bytes are then split across more than one independent allocation —
    /// see the module doc's "Column bands" section).
    fn row(&self, gy: u32) -> Option<&[u8]> {
        if gy >= self.height || self.n_col_bands != 1 {
            return None;
        }
        let k = self.band_of(gy);
        let band = self.bands.get(self.flat(k, 0)?)?.get()?;
        let local = (gy - self.band_first_row(k)) as usize + self.guard as usize;
        let start = local.checked_mul(self.stride)?;
        let end = start.checked_add(self.stride)?;
        band.rows.get(start..end)
    }

    fn mark_failed(&self) {
        let mut st = lock(&self.state);
        st.failed = true;
        self.wake.notify_all();
    }
}

/// A picture being produced by one frame task and read by later ones.
#[derive(Debug)]
pub struct ProgressPicture {
    decode_index: u64,
    planes: Box<[ProgressPlane]>,
}

impl ProgressPicture {
    /// Allocate a picture and split it into the single writer and the shared
    /// reader.
    ///
    /// `decode_index` must increase with decode order; it is what the
    /// deadlock-freedom assertion in [`PictureRef::wait_rows_for`] checks
    /// against.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] when the picture does not fit the budget.
    pub fn allocate(
        spec: &PictureSpec,
        decode_index: u64,
        budget: &mut Budget,
    ) -> Result<(PictureWriter, PictureRef)> {
        let mut planes = Vec::new();
        let mut writers = Vec::new();
        for p in &spec.planes {
            let n_row_bands = if p.height == 0 {
                0
            } else {
                p.height.div_ceil(p.band_h) as usize
            };
            let n_col_bands = if p.band_w == 0 || p.width_bytes == 0 {
                1
            } else {
                p.width_bytes.div_ceil(p.band_w).max(1) as usize
            };
            let tiled = n_col_bands > 1;
            let guard = if tiled { 0 } else { p.guard };
            let mut staging = Vec::new();
            let mut bands = Vec::new();
            for rk in 0..n_row_bands {
                let row_first = (rk as u32).saturating_mul(p.band_h);
                let row_body = p.height.saturating_sub(row_first).min(p.band_h);
                for ck in 0..n_col_bands {
                    let (own_stride, own_w) = if tiled {
                        let col_first = (ck as u32).saturating_mul(p.band_w);
                        let w = p.width_bytes.saturating_sub(col_first).min(p.band_w);
                        (w as usize, w)
                    } else {
                        (p.stride, p.width_bytes)
                    };
                    let rows = (row_body as usize)
                        .checked_add(guard as usize)
                        .and_then(|r| r.checked_mul(own_stride))
                        .ok_or(Error::LimitExceeded {
                            limit: "picture_band",
                            requested: u64::MAX,
                            cap: u64::MAX,
                        })?;
                    let buf: Vec<u8> = budget.alloc(rows)?;
                    staging.push(Some(Band {
                        rows: buf.into_boxed_slice(),
                        body: row_body,
                        body_w: own_w,
                    }));
                    bands.push(OnceLock::new());
                }
            }
            let col_ready: Vec<AtomicU32> = (0..n_row_bands).map(|_| AtomicU32::new(0)).collect();
            planes.push(ProgressPlane {
                bands: bands.into_boxed_slice(),
                n_row_bands,
                n_col_bands,
                col_ready: col_ready.into_boxed_slice(),
                ready: AtomicU32::new(0),
                state: Mutex::new(PlaneWait::default()),
                wake: Condvar::new(),
                width_bytes: p.width_bytes,
                height: p.height,
                band_h: p.band_h,
                band_w: p.band_w,
                band_shift: p
                    .band_h
                    .is_power_of_two()
                    .then(|| p.band_h.trailing_zeros()),
                col_shift: (tiled && p.band_w.is_power_of_two()).then(|| p.band_w.trailing_zeros()),
                guard,
                stride: if tiled { 0 } else { p.stride },
            });
            writers.push(PlaneWriter {
                staging,
                published: 0,
                next_col: vec![0; n_row_bands],
            });
        }
        let picture = Arc::new(Self {
            decode_index,
            planes: planes.into_boxed_slice(),
        });
        Ok((
            PictureWriter {
                picture: Arc::clone(&picture),
                planes: writers,
                complete: false,
            },
            PictureRef(picture),
        ))
    }

    /// Position in decode order.
    #[must_use]
    pub const fn decode_index(&self) -> u64 {
        self.decode_index
    }

    /// Number of planes.
    #[must_use]
    pub const fn plane_count(&self) -> usize {
        self.planes.len()
    }

    fn plane(&self, index: usize) -> Result<&ProgressPlane> {
        self.planes
            .get(index)
            .ok_or(Error::InvalidData("plane index out of range"))
    }
}

#[derive(Debug)]
struct PlaneWriter {
    /// Bands still owned exclusively by the writer, flattened the same way as
    /// `ProgressPlane::bands`. `None` once published.
    staging: Vec<Option<Band>>,
    /// Row bands published so far, in strict order — `publish_through`'s own
    /// bookkeeping (row-banded planes only).
    published: usize,
    /// Next column band expected in each row band — `publish_tile`'s own
    /// bookkeeping (column-banded planes only), one entry per row band.
    next_col: Vec<usize>,
}

/// Exclusive access to one band's body rows.
#[derive(Debug)]
pub struct BandMut<'a> {
    data: &'a mut [u8],
    stride: usize,
    rows: u32,
    first_row: u32,
    first_col: u32,
    width_bytes: u32,
}

impl BandMut<'_> {
    /// One row of the band body.
    #[must_use]
    pub fn row_mut(&mut self, index: u32) -> Option<&mut [u8]> {
        if index >= self.rows {
            return None;
        }
        let start = (index as usize).checked_mul(self.stride)?;
        let end = start.checked_add(self.width_bytes as usize)?;
        self.data.get_mut(start..end)
    }

    /// Every body row, back to back at `stride`.
    #[must_use]
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.data
    }

    /// Bytes between consecutive rows.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Rows in this band's body.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// The picture row this band starts at.
    #[must_use]
    pub const fn first_row(&self) -> u32 {
        self.first_row
    }

    /// The picture column this band starts at — always `0` for a row-banded
    /// (single-column) plane.
    #[must_use]
    pub const fn first_col(&self) -> u32 {
        self.first_col
    }
}

impl<'a> BandMut<'a> {
    /// Consumes the exclusive access, returning one specific row's own
    /// mutable slice for the rest of the *band*'s lifetime rather than
    /// [`BandMut::row_mut`]'s own call-scoped one.
    ///
    /// [`BandMut::row_mut`] takes `&mut self` so a caller can fetch several
    /// rows of the *same* band one after another without re-acquiring it —
    /// its returned slice's lifetime is tied to that borrow, not to `'a`,
    /// which is correct for that shape but too short for a caller that
    /// re-derives a fresh, single-use `BandMut` per row (a picture wrapper
    /// whose own `row_mut(y)` maps `y` to a tile and returns straight
    /// through, say) and needs the slice to outlive the local `BandMut`
    /// that produced it.
    #[must_use]
    pub fn into_row_mut(self, index: u32) -> Option<&'a mut [u8]> {
        if index >= self.rows {
            return None;
        }
        let start = (index as usize).checked_mul(self.stride)?;
        let end = start.checked_add(self.width_bytes as usize)?;
        self.data.get_mut(start..end)
    }
}

/// A disjoint run of row bands, handed to one slice or tile job.
///
/// Row-banded (single-column) planes only — a column-banded plane's own
/// concurrent-writer story is one row's worth of tiles per worker via
/// [`PictureWriter::tile_mut`]/[`PictureWriter::publish_tile`], not this.
#[derive(Debug)]
pub struct BandRangeMut<'a> {
    bands: &'a mut [Option<Band>],
    first: usize,
    stride: usize,
    band_h: u32,
    guard: u32,
    width_bytes: u32,
}

impl BandRangeMut<'_> {
    /// The absolute band index this range starts at.
    #[must_use]
    pub const fn first_band(&self) -> usize {
        self.first
    }

    /// Bands in this range.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bands.len()
    }

    /// Whether the range is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// Exclusive access to one band of this range, by absolute index.
    #[must_use]
    pub fn band_mut(&mut self, k: usize) -> Option<BandMut<'_>> {
        let local = k.checked_sub(self.first)?;
        let band = self.bands.get_mut(local)?.as_mut()?;
        let skip = (self.guard as usize).checked_mul(self.stride)?;
        let rows = band.body;
        let data = band.rows.get_mut(skip..)?;
        Some(BandMut {
            data,
            stride: self.stride,
            rows,
            first_row: (k as u32).saturating_mul(self.band_h),
            first_col: 0,
            width_bytes: self.width_bytes,
        })
    }
}

/// The sole writer of a picture. Not `Sync`, not `Clone`: one frame task holds
/// it, which is what makes exclusive access to an unpublished band a fact the
/// compiler can check.
#[derive(Debug)]
pub struct PictureWriter {
    picture: Arc<ProgressPicture>,
    planes: Vec<PlaneWriter>,
    complete: bool,
}

impl PictureWriter {
    /// Position in decode order.
    #[must_use]
    pub fn decode_index(&self) -> u64 {
        self.picture.decode_index
    }

    /// Number of planes.
    #[must_use]
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    /// Bands in a plane — the flattened `row_bands(plane) * col_bands(plane)`
    /// total for a column-banded plane, or simply the row-band count for a
    /// row-banded one, matching this method's pre-existing meaning exactly.
    #[must_use]
    pub fn band_count(&self, plane: usize) -> usize {
        self.planes.get(plane).map_or(0, |p| p.staging.len())
    }

    /// Row bands in a plane.
    #[must_use]
    pub fn row_bands(&self, plane: usize) -> usize {
        self.picture.plane(plane).map_or(0, |p| p.n_row_bands)
    }

    /// Column bands in a plane — `1` unless [`PlaneSpec::with_bands`] set a
    /// non-zero `band_w`.
    #[must_use]
    pub fn col_bands(&self, plane: usize) -> usize {
        self.picture.plane(plane).map_or(1, |p| p.n_col_bands)
    }

    /// The tile `(row_band, col_band)` containing pixel `(x, y)` of `plane`.
    #[must_use]
    pub fn tile_of(&self, plane: usize, x: u32, y: u32) -> Option<(usize, usize)> {
        let p = self.picture.plane(plane).ok()?;
        Some((p.band_of(y), p.col_of(x)))
    }

    /// Exclusive access to band `k` of `plane` — row band `k`, column band
    /// `0`. [`PictureWriter::tile_mut`]'s row-only special case, and the only
    /// form a single-column plane ever needs.
    ///
    /// # Errors
    ///
    /// As [`PictureWriter::tile_mut`].
    pub fn band_mut(&mut self, plane: usize, k: usize) -> Result<BandMut<'_>> {
        self.tile_mut(plane, k, 0)
    }

    /// Exclusive access to the tile at `(row_band, col_band)` of `plane` —
    /// the two-dimensional generalisation of [`PictureWriter::band_mut`],
    /// which is exactly `tile_mut(plane, k, 0)`. There is one implementation
    /// underneath either name (D19/D23).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or tile does not exist, or if the
    /// tile has already been published — at which point it is immutable and
    /// shared, and writing to it would be exactly the race this design exists
    /// to prevent.
    pub fn tile_mut(
        &mut self,
        plane: usize,
        row_band: usize,
        col_band: usize,
    ) -> Result<BandMut<'_>> {
        let geom = self.picture.plane(plane)?;
        let flat = geom
            .flat(row_band, col_band)
            .ok_or(Error::InvalidData("tile index out of range"))?;
        let (guard, own_stride, own_w) = (
            geom.guard,
            geom.stride_for(row_band, col_band),
            geom.col_body_w(col_band),
        );
        let pw = self
            .planes
            .get_mut(plane)
            .ok_or(Error::InvalidData("plane index out of range"))?;
        let slot = pw
            .staging
            .get_mut(flat)
            .ok_or(Error::InvalidData("band index out of range"))?;
        let band = slot
            .as_mut()
            .ok_or(Error::InvalidData("band was already published"))?;
        let rows = band.body;
        let skip = (guard as usize)
            .checked_mul(own_stride)
            .ok_or(Error::InvalidData("band geometry overflow"))?;
        let data = band
            .rows
            .get_mut(skip..)
            .ok_or(Error::InvalidData("band geometry overflow"))?;
        Ok(BandMut {
            data,
            stride: own_stride,
            rows,
            first_row: geom.band_first_row(row_band),
            first_col: geom.col_first_x(col_band),
            width_bytes: own_w,
        })
    }

    /// Read-only access to band `k` of `plane` while it is still staged (not
    /// yet published) — [`PictureWriter::tile_ref`]'s row-only special case.
    ///
    /// `None` if the plane or band does not exist, or if the band has
    /// already been published (at which point [`PictureRef::try_rows`]/
    /// [`PictureRef::try_tile`] are the way to read it, not this).
    #[must_use]
    pub fn band_ref(&self, plane: usize, k: usize) -> Option<BlockRef<'_>> {
        self.tile_ref(plane, k, 0)
    }

    /// Read-only access to the tile at `(row_band, col_band)` of `plane`
    /// while it is still staged (not yet published) — the immutable
    /// counterpart of [`PictureWriter::tile_mut`], for a caller that needs
    /// to read back what it has already written to a tile it has not
    /// finished yet (same-CTU intra reference samples, say) without forcing
    /// every read-only call site to also require `&mut`. One
    /// implementation underneath `tile_mut`/`tile_ref` either (D19/D23):
    /// both index the same `staging` slot, one exclusively, one shared.
    ///
    /// `None` if the plane or tile does not exist, or if it has already
    /// been published.
    #[must_use]
    pub fn tile_ref(&self, plane: usize, row_band: usize, col_band: usize) -> Option<BlockRef<'_>> {
        let geom = self.picture.plane(plane).ok()?;
        let flat = geom.flat(row_band, col_band)?;
        let guard = geom.guard;
        let own_stride = geom.stride_for(row_band, col_band);
        let pw = self.planes.get(plane)?;
        let band = pw.staging.get(flat)?.as_ref()?;
        let skip = (guard as usize).checked_mul(own_stride)?;
        let data = band.rows.get(skip..)?;
        Some(BlockRef {
            data,
            stride: own_stride,
        })
    }

    /// Publish every band of `plane` through row band `k`, then advertise the
    /// rows they contain and wake anyone waiting for them.
    ///
    /// Each band's guard rows are filled from the tail of its predecessor
    /// first, so a read that strays above a band's first row still lands in one
    /// contiguous allocation.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or band does not exist, if bands
    /// are published out of order, or if `plane` is column-banded (use
    /// [`PictureWriter::publish_tile`] instead — there is no single "through
    /// row `k`" order across a 2-D grid multiple workers fill concurrently).
    pub fn publish_through(&mut self, plane: usize, k: usize) -> Result<()> {
        let geom = self.picture.plane(plane)?;
        if geom.n_col_bands != 1 {
            return Err(Error::InvalidData(
                "publish_through needs a row-banded (single-column) plane; use publish_tile",
            ));
        }
        let pw = self
            .planes
            .get_mut(plane)
            .ok_or(Error::InvalidData("plane index out of range"))?;
        if k >= pw.staging.len() {
            return Err(Error::InvalidData("band index out of range"));
        }
        while pw.published <= k {
            let j = pw.published;
            let mut band = pw
                .staging
                .get_mut(j)
                .and_then(Option::take)
                .ok_or(Error::InvalidData("band was already published"))?;
            if j > 0 && geom.guard > 0 {
                fill_guard(geom, &mut band, j);
            }
            let slot = geom
                .bands
                .get(j)
                .ok_or(Error::InvalidData("band index out of range"))?;
            if slot.set(band).is_err() {
                return Err(Error::InvalidData("band was already published"));
            }
            pw.published = j + 1;
        }
        let rows = geom
            .band_first_row(k)
            .saturating_add(geom.band_body_rows(k))
            .min(geom.height);
        self.advance_ready(plane, rows)
    }

    /// Publish the tile at `(row_band, col_band)` of a column-banded `plane`.
    ///
    /// Tiles in one row band must publish left to right, in order — the same
    /// "no gaps, no re-ordering" discipline `publish_through` already
    /// enforces along the row axis, applied along the column axis here. Once
    /// every column of `row_band` has published, this also advances the
    /// plane's whole-row readiness exactly the way `publish_through`'s own
    /// tail does, so [`PictureRef::ready_rows`]/`wait_rows`/`finished` keep
    /// meaning "every column of these rows is done" even for a column-banded
    /// plane.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or tile does not exist, if `plane`
    /// is not column-banded (use [`PictureWriter::publish_through`] instead),
    /// or if this tile is not the next one due in its row band.
    pub fn publish_tile(&mut self, plane: usize, row_band: usize, col_band: usize) -> Result<()> {
        let geom = self.picture.plane(plane)?;
        if geom.n_col_bands <= 1 {
            return Err(Error::InvalidData(
                "publish_tile needs a column-banded plane; use publish_through",
            ));
        }
        let flat = geom
            .flat(row_band, col_band)
            .ok_or(Error::InvalidData("tile index out of range"))?;
        let pw = self
            .planes
            .get_mut(plane)
            .ok_or(Error::InvalidData("plane index out of range"))?;
        let expected = pw
            .next_col
            .get(row_band)
            .copied()
            .ok_or(Error::InvalidData("row band out of range"))?;
        if col_band != expected {
            return Err(Error::InvalidData(
                "tiles in a row band must publish left to right, in order",
            ));
        }
        let band = pw
            .staging
            .get_mut(flat)
            .and_then(Option::take)
            .ok_or(Error::InvalidData("band was already published"))?;
        let slot = geom
            .bands
            .get(flat)
            .ok_or(Error::InvalidData("band index out of range"))?;
        if slot.set(band).is_err() {
            return Err(Error::InvalidData("band was already published"));
        }
        let next = expected + 1;
        if let Some(c) = pw.next_col.get_mut(row_band) {
            *c = next;
        }
        let next_u32 = u32::try_from(next).unwrap_or(u32::MAX);
        if let Some(counter) = geom.col_ready.get(row_band) {
            counter.store(next_u32, Ordering::Release);
        }
        // The notify must happen while holding `state`'s lock, even though
        // `col_ready` itself is a lock-free atomic: a waiter's own check-then-
        // wait sequence (`wait_tile`) holds this same lock across both steps,
        // so acquiring it here guarantees the notify cannot fire in the gap
        // between the waiter's check and its `wake.wait()` call — the classic
        // lost-wakeup race `advance_ready`'s own lock-then-notify already
        // avoids for the row axis.
        let st = lock(&geom.state);
        geom.wake.notify_all();
        drop(st);
        if next == geom.n_col_bands {
            let rows = geom
                .band_first_row(row_band)
                .saturating_add(geom.band_body_rows(row_band))
                .min(geom.height);
            self.advance_ready(plane, rows)?;
        }
        Ok(())
    }

    /// Shared tail of `publish_through`/`publish_tile`: advertise `rows` as
    /// ready and wake anyone waiting on `wait_rows`.
    fn advance_ready(&mut self, plane: usize, rows: u32) -> Result<()> {
        let geom = self.picture.plane(plane)?;
        geom.ready.store(rows, Ordering::Release);
        let mut st = lock(&geom.state);
        if rows > st.rows {
            st.rows = rows;
            geom.wake.notify_all();
        }
        Ok(())
    }

    /// Hand out disjoint band ranges of one row-banded plane to concurrent
    /// slice or tile jobs.
    ///
    /// Safety here is `split_at_mut`-style disjointness and nothing exotic:
    /// ranges must be ascending and non-overlapping, and each job gets a
    /// `&mut` that cannot alias any other. Publication stays with the owning
    /// thread after the jobs join, because it is the writer that knows the
    /// order.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane does not exist, is column-banded,
    /// or the ranges are not ascending, disjoint and in range.
    pub fn split_bands_mut(
        &mut self,
        plane: usize,
        ranges: &[std::ops::Range<usize>],
    ) -> Result<Vec<BandRangeMut<'_>>> {
        let geom = self.picture.plane(plane)?;
        if geom.n_col_bands != 1 {
            return Err(Error::InvalidData(
                "split_bands_mut needs a row-banded (single-column) plane",
            ));
        }
        let (stride, guard, band_h, width_bytes) =
            (geom.stride, geom.guard, geom.band_h, geom.width_bytes);
        let pw = self
            .planes
            .get_mut(plane)
            .ok_or(Error::InvalidData("plane index out of range"))?;
        let total = pw.staging.len();
        let mut cursor = 0usize;
        for r in ranges {
            if r.start < cursor || r.end < r.start || r.end > total {
                return Err(Error::InvalidData(
                    "band ranges must be ascending, disjoint and within the plane",
                ));
            }
            cursor = r.end;
        }
        let mut rest: &mut [Option<Band>] = &mut pw.staging;
        let mut base = 0usize;
        let mut out = Vec::new();
        for r in ranges {
            let skip = r.start - base;
            let (_, tail) = rest.split_at_mut(skip);
            let (mine, tail) = tail.split_at_mut(r.end - r.start);
            out.push(BandRangeMut {
                bands: mine,
                first: r.start,
                stride,
                band_h,
                guard,
                width_bytes,
            });
            base = r.end;
            rest = tail;
        }
        Ok(out)
    }

    /// Publish everything that is left and mark the picture complete.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if a band is in an inconsistent state.
    pub fn finish(mut self) -> Result<()> {
        for plane in 0..self.planes.len() {
            let (n_row_bands, n_col_bands) = {
                let geom = self.picture.plane(plane)?;
                (geom.n_row_bands, geom.n_col_bands)
            };
            if n_col_bands <= 1 {
                let n = self.band_count(plane);
                if n > 0 {
                    self.publish_through(plane, n - 1)?;
                }
            } else {
                for r in 0..n_row_bands {
                    loop {
                        let next = self
                            .planes
                            .get(plane)
                            .and_then(|p| p.next_col.get(r).copied())
                            .ok_or(Error::InvalidData("row band out of range"))?;
                        if next >= n_col_bands {
                            break;
                        }
                        self.publish_tile(plane, r, next)?;
                    }
                }
            }
        }
        self.complete = true;
        Ok(())
    }
}

/// Copy the tail of band `j - 1` into band `j`'s guard rows.
fn fill_guard(geom: &ProgressPlane, band: &mut Band, j: usize) {
    let Some(prev) = geom.bands.get(j - 1).and_then(|b| b.get()) else {
        return;
    };
    let guard = geom.guard as usize;
    let stride = geom.stride;
    let take = (prev.body as usize).min(guard);
    for i in 0..take {
        // Source: the last `take` body rows of the previous band.
        let src_row = (prev.body as usize) - take + i + guard;
        let dst_row = guard - take + i;
        let (Some(src_start), Some(dst_start)) =
            (src_row.checked_mul(stride), dst_row.checked_mul(stride))
        else {
            return;
        };
        let (Some(src), Some(dst)) = (
            prev.rows.get(src_start..src_start + stride),
            band.rows.get_mut(dst_start..dst_start + stride),
        ) else {
            return;
        };
        dst.copy_from_slice(src);
    }
}

impl ProgressPlane {
    /// This tile's own internal row stride: the plane's shared `stride`
    /// (which may exceed `width_bytes` — custom padding) for a row-banded
    /// plane, or this specific column band's own tight width otherwise.
    fn stride_for(&self, _row_band: usize, col_band: usize) -> usize {
        if self.n_col_bands <= 1 {
            self.stride
        } else {
            self.col_body_w(col_band) as usize
        }
    }
}

impl Drop for PictureWriter {
    /// The deadlock guard.
    ///
    /// A writer dropped before the picture is complete — a panicking task, a
    /// cancelled one, an early return — marks every plane failed and wakes each
    /// waiter with an error. Every [`PictureRef::wait_rows`] therefore
    /// terminates: either progress arrives or the picture fails.
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        for plane in &self.picture.planes {
            plane.mark_failed();
        }
    }
}

/// A shared, read-only handle to a picture that may still be in production.
///
/// Cheap to clone, `Send + Sync`. This is what a frame task holds for each of
/// its reference pictures.
#[derive(Debug, Clone)]
pub struct PictureRef(Arc<ProgressPicture>);

impl PictureRef {
    /// Position in decode order.
    #[must_use]
    pub fn decode_index(&self) -> u64 {
        self.0.decode_index
    }

    /// Number of planes.
    #[must_use]
    pub fn plane_count(&self) -> usize {
        self.0.plane_count()
    }

    /// Row bands in `plane`.
    #[must_use]
    pub fn row_bands(&self, plane: usize) -> usize {
        self.0.plane(plane).map_or(0, |p| p.n_row_bands)
    }

    /// Column bands in `plane` — `1` unless it was allocated with
    /// [`PlaneSpec::with_bands`].
    #[must_use]
    pub fn col_bands(&self, plane: usize) -> usize {
        self.0.plane(plane).map_or(1, |p| p.n_col_bands)
    }

    /// The tile `(row_band, col_band)` containing pixel `(x, y)` of `plane`.
    #[must_use]
    pub fn tile_of(&self, plane: usize, x: u32, y: u32) -> Option<(usize, usize)> {
        let p = self.0.plane(plane).ok()?;
        Some((p.band_of(y), p.col_of(x)))
    }

    /// Rows of `plane` readable right now, without blocking — "every column
    /// of these rows is done", which stays meaningful even for a
    /// column-banded plane (see the module doc).
    #[must_use]
    pub fn ready_rows(&self, plane: usize) -> u32 {
        self.0
            .plane(plane)
            .map_or(0, |p| p.ready.load(Ordering::Acquire))
    }

    /// Columns of row band `row_band` in `plane` readable right now, without
    /// blocking. Always `0` or `1` for a row-banded plane.
    #[must_use]
    pub fn ready_cols(&self, plane: usize, row_band: usize) -> u32 {
        self.0
            .plane(plane)
            .ok()
            .and_then(|p| p.col_ready.get(row_band))
            .map_or(0, |c| c.load(Ordering::Acquire))
    }

    /// Block until rows `..=y` of `plane` are readable.
    ///
    /// The fast path is one acquire load and no syscall.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane does not exist, if `y` is outside
    /// the plane, or if the task producing this picture failed — in which case
    /// every waiter is woken rather than left blocked.
    pub fn wait_rows(&self, plane: usize, y: u32) -> Result<PlaneView<'_>> {
        let p = self.0.plane(plane)?;
        let need = y.saturating_add(1).min(p.height);
        let ready = p.ready.load(Ordering::Acquire);
        if ready >= need {
            return Ok(PlaneView {
                plane: p,
                rows: ready,
            });
        }
        let mut st = lock(&p.state);
        loop {
            if st.failed {
                return Err(Error::InvalidData("reference picture producer failed"));
            }
            if st.rows >= need {
                let rows = st.rows;
                return Ok(PlaneView { plane: p, rows });
            }
            st = p.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// [`PictureRef::wait_rows`], with the acyclicity assertion.
    ///
    /// A task waits only on pictures that precede it in decode order, and the
    /// header stage emits tasks in decode order, so the wait graph is a DAG.
    /// This is the debug assertion that checks it (plan 15 §1.8.4).
    ///
    /// # Errors
    ///
    /// As [`PictureRef::wait_rows`].
    pub fn wait_rows_for(&self, waiter: u64, plane: usize, y: u32) -> Result<PlaneView<'_>> {
        debug_assert!(
            self.decode_index() < waiter,
            "a frame task may only wait on pictures earlier in decode order"
        );
        self.wait_rows(plane, y)
    }

    /// Rows `..=y` if they are already readable, without blocking.
    #[must_use]
    pub fn try_rows(&self, plane: usize, y: u32) -> Option<PlaneView<'_>> {
        let p = self.0.plane(plane).ok()?;
        let ready = p.ready.load(Ordering::Acquire);
        if ready >= y.saturating_add(1).min(p.height) {
            Some(PlaneView {
                plane: p,
                rows: ready,
            })
        } else {
            None
        }
    }

    /// The whole plane, once complete. The non-threaded and post-decode path.
    ///
    /// # Errors
    ///
    /// As [`PictureRef::wait_rows`].
    pub fn finished(&self, plane: usize) -> Result<PlaneView<'_>> {
        let height = self.0.plane(plane)?.height;
        self.wait_rows(plane, height.saturating_sub(1))
    }

    /// Block until the tile at `(row_band, col_band)` of a column-banded
    /// `plane` is readable, and borrow it.
    ///
    /// The fast path is one acquire load and no syscall, the same shape as
    /// [`PictureRef::wait_rows`]'s. A wait past what the producer will ever
    /// publish (an out-of-range tile) is refused rather than hung — the same
    /// "must be refused, not tolerated" discipline as an out-of-range row.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or tile does not exist, if `plane`
    /// is not column-banded, or if the producing task failed.
    pub fn wait_tile(
        &self,
        plane: usize,
        row_band: usize,
        col_band: usize,
    ) -> Result<BlockRef<'_>> {
        let p = self.0.plane(plane)?;
        if p.n_col_bands <= 1 {
            return Err(Error::InvalidData("wait_tile needs a column-banded plane"));
        }
        if p.flat(row_band, col_band).is_none() {
            return Err(Error::InvalidData("tile index out of range"));
        }
        let col_band_u32 = u32::try_from(col_band).unwrap_or(u32::MAX);
        let ready = p
            .col_ready
            .get(row_band)
            .map_or(0, |c| c.load(Ordering::Acquire));
        if ready > col_band_u32 {
            return Self::tile_block(p, row_band, col_band);
        }
        let mut st = lock(&p.state);
        loop {
            if st.failed {
                return Err(Error::InvalidData("reference tile producer failed"));
            }
            let ready = p
                .col_ready
                .get(row_band)
                .map_or(0, |c| c.load(Ordering::Acquire));
            if ready > col_band_u32 {
                return Self::tile_block(p, row_band, col_band);
            }
            st = p.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// [`PictureRef::wait_tile`], with the acyclicity assertion — the
    /// column-banded counterpart of [`PictureRef::wait_rows_for`].
    ///
    /// # Errors
    ///
    /// As [`PictureRef::wait_tile`].
    pub fn wait_tile_for(
        &self,
        waiter: u64,
        plane: usize,
        row_band: usize,
        col_band: usize,
    ) -> Result<BlockRef<'_>> {
        debug_assert!(
            self.decode_index() < waiter,
            "a frame task may only wait on pictures earlier in decode order"
        );
        self.wait_tile(plane, row_band, col_band)
    }

    /// The tile at `(row_band, col_band)`, if it is already readable, without
    /// blocking.
    #[must_use]
    pub fn try_tile(&self, plane: usize, row_band: usize, col_band: usize) -> Option<BlockRef<'_>> {
        let p = self.0.plane(plane).ok()?;
        if p.n_col_bands <= 1 {
            return None;
        }
        let col_band_u32 = u32::try_from(col_band).unwrap_or(u32::MAX);
        let ready = p.col_ready.get(row_band)?.load(Ordering::Acquire);
        if ready > col_band_u32 {
            Self::tile_block(p, row_band, col_band).ok()
        } else {
            None
        }
    }

    fn tile_block(p: &ProgressPlane, row_band: usize, col_band: usize) -> Result<BlockRef<'_>> {
        let flat = p
            .flat(row_band, col_band)
            .ok_or(Error::InvalidData("tile index out of range"))?;
        let band = p
            .bands
            .get(flat)
            .and_then(OnceLock::get)
            .ok_or(Error::InvalidData("tile not published"))?;
        let stride = band.body_w as usize;
        let span = (band.body as usize).saturating_mul(stride);
        let data = band
            .rows
            .get(..span)
            .ok_or(Error::InvalidData("tile geometry overflow"))?;
        Ok(BlockRef { data, stride })
    }

    /// Whether the producing task failed.
    #[must_use]
    pub fn failed(&self, plane: usize) -> bool {
        self.0.plane(plane).is_ok_and(|p| lock(&p.state).failed)
    }
}

/// A read-only window onto the rows of a plane that have been published.
///
/// Row-banded (single-column) planes only — see the module doc's "Column
/// bands" section for why a column-banded plane's rows cannot be served as
/// one contiguous borrow, and use [`PictureRef::wait_tile`] instead.
#[derive(Debug, Clone, Copy)]
pub struct PlaneView<'a> {
    plane: &'a ProgressPlane,
    rows: u32,
}

impl<'a> PlaneView<'a> {
    /// Rows readable through this view.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// Rows in the whole plane.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.plane.height
    }

    /// Meaningful bytes per row.
    #[must_use]
    pub const fn width_bytes(&self) -> u32 {
        self.plane.width_bytes
    }

    /// Bytes between consecutive rows within one band.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.plane.stride
    }

    /// One published row, or `None` if `y` is unpublished, out of range, or
    /// this plane is column-banded.
    #[must_use]
    pub fn row(&self, y: u32) -> Option<&'a [u8]> {
        if y >= self.rows {
            return None;
        }
        self.plane.row(y)
    }

    /// The whole published extent of this plane as one contiguous borrow, when
    /// a single band holds it.
    ///
    /// This is the whole-picture face of [`PlaneView::block`]'s own fast path,
    /// for a caller whose reference-sample reads are written against a plain
    /// `(&[u8], stride)` pair rather than against per-block borrows — a
    /// frame-threaded decoder that publishes at *picture* granularity
    /// (`PictureSpec::single_band`) waits once for the whole plane and then
    /// wants exactly that pair. `None` whenever the extent spans more than one
    /// band, which is the row-granularity case [`PlaneView::block`] is for.
    #[must_use]
    pub fn contiguous_all(&self) -> Option<BlockRef<'a>> {
        self.contiguous(0, 0, self.plane.width_bytes, self.rows)
    }

    /// A contiguous borrow of a `w × h` region at `(x, y)`.
    ///
    /// **Fast path**: the region lies inside one band's allocation, so the
    /// borrow is that band's own memory at its natural stride — the same cost
    /// as a raw `(ptr, stride)` pair.
    ///
    /// **Copy path**: the region straddles a band seam or falls outside the
    /// picture. It is copied into `scratch` with edge replication and borrowed
    /// from there. This is the same cold path that already exists for
    /// out-of-picture motion vectors, so it costs one extra condition rather
    /// than a new mechanism.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the region needs rows that have not been
    /// published, when this plane is column-banded, or when `scratch` is
    /// smaller than the region. Codecs size the scratch once from their
    /// largest block, so the last case cannot happen at run time.
    pub fn block(
        &self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        scratch: &'a mut BlockScratch,
    ) -> Result<BlockRef<'a>> {
        let plane = self.plane;
        if plane.n_col_bands != 1 {
            return Err(Error::InvalidData(
                "PlaneView::block is not valid on a column-banded plane; use PictureRef::wait_tile",
            ));
        }
        if w == 0 || h == 0 {
            return Ok(BlockRef {
                data: &[],
                stride: 0,
            });
        }
        let inside = x >= 0
            && y >= 0
            && (x as u32).saturating_add(w) <= plane.width_bytes
            && (y as u32).saturating_add(h) <= self.rows;
        if inside {
            let (ux, uy) = (x as u32, y as u32);
            if let Some(b) = self.contiguous(ux, uy, w, h) {
                return Ok(b);
            }
        } else if i64::from(y) + i64::from(h) > i64::from(self.rows) && self.rows < plane.height {
            // Rows below the published watermark are not "outside the picture",
            // they are "not written yet". Replicating them would silently
            // fabricate picture content.
            return Err(Error::InvalidData(
                "picture region extends past the published rows",
            ));
        }
        self.copy_block(x, y, w, h, scratch)
    }

    /// The region as one contiguous borrow, if some single band holds all of
    /// it: either the band the first row belongs to, or — when the region
    /// begins within `guard` rows of a seam — the next band, whose guard rows
    /// hold exactly those rows.
    fn contiguous(&self, x: u32, y: u32, w: u32, h: u32) -> Option<BlockRef<'a>> {
        let plane = self.plane;
        let own = plane.band_of(y);
        for k in [own, own + 1] {
            let first = plane.band_first_row(k);
            let body = plane.band_body_rows(k);
            if body == 0 {
                continue;
            }
            let top = first.saturating_sub(plane.guard);
            if y < top || y.saturating_add(h) > first.saturating_add(body) {
                continue;
            }
            if k > 0 && y < first && (first - y) > plane.guard {
                continue;
            }
            let band = plane.bands.get(plane.flat(k, 0)?)?.get()?;
            let local = (y + plane.guard).checked_sub(first)? as usize;
            let start = local.checked_mul(plane.stride)?.checked_add(x as usize)?;
            let span = (h as usize - 1)
                .checked_mul(plane.stride)?
                .checked_add(w as usize)?;
            let data = band.rows.get(start..start.checked_add(span)?)?;
            return Some(BlockRef {
                data,
                stride: plane.stride,
            });
        }
        None
    }

    fn copy_block(
        &self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        scratch: &'a mut BlockScratch,
    ) -> Result<BlockRef<'a>> {
        let plane = self.plane;
        let need = (w as usize)
            .checked_mul(h as usize)
            .ok_or(Error::InvalidData("block geometry overflow"))?;
        if scratch.buf.len() < need {
            return Err(Error::LimitExceeded {
                limit: "block_scratch",
                requested: need as u64,
                cap: scratch.buf.len() as u64,
            });
        }
        if self.rows == 0 || plane.width_bytes == 0 {
            return Err(Error::InvalidData("picture region has no published rows"));
        }
        let last_row = self.rows - 1;
        let last_col = plane.width_bytes - 1;
        for j in 0..h {
            let gy = (i64::from(y) + i64::from(j)).clamp(0, i64::from(last_row)) as u32;
            let src = plane
                .row(gy)
                .ok_or(Error::InvalidData("published row is missing"))?;
            let dst_start = (j as usize) * (w as usize);
            let Some(dst) = scratch.buf.get_mut(dst_start..dst_start + w as usize) else {
                return Err(Error::InvalidData("block scratch too small"));
            };
            for i in 0..w {
                let gx = (i64::from(x) + i64::from(i)).clamp(0, i64::from(last_col)) as usize;
                let Some(out) = dst.get_mut(i as usize) else {
                    break;
                };
                *out = src.get(gx).copied().unwrap_or(0);
            }
        }
        let data = scratch
            .buf
            .get(..need)
            .ok_or(Error::InvalidData("block scratch too small"))?;
        Ok(BlockRef {
            data,
            stride: w as usize,
        })
    }
}

/// A contiguous borrow of a picture region, plus the stride to walk it with.
#[derive(Debug, Clone, Copy)]
pub struct BlockRef<'a> {
    /// The region's bytes. The first row starts at index zero.
    pub data: &'a [u8],
    /// Bytes between consecutive rows of `data`.
    pub stride: usize,
}

/// Scratch space for the copy path of [`PlaneView::block`].
///
/// Sized once, from the codec's largest block plus its filter reach, and reused
/// for every block after that: the copy path must not allocate.
#[derive(Debug)]
pub struct BlockScratch {
    buf: Vec<u8>,
}

impl BlockScratch {
    /// Scratch for a region of at most `max_w × max_h` bytes.
    ///
    /// # Errors
    ///
    /// [`Error::LimitExceeded`] when the budget refuses.
    pub fn new(budget: &mut Budget, max_w: u32, max_h: u32) -> Result<Self> {
        let n = (max_w as usize)
            .checked_mul(max_h as usize)
            .ok_or(Error::InvalidData("scratch geometry overflow"))?;
        Ok(Self {
            buf: budget.alloc(n)?,
        })
    }

    /// Bytes available.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
}
