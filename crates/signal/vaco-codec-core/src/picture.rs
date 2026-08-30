//! Frame threading without `unsafe`: guard-padded row bands published through
//! `OnceLock`.
//!
//! # The problem
//!
//! Frame threading needs "frame N+1 may proceed once frame N has produced row
//! R". The conventional solution is one contiguous picture buffer, a raw
//! pointer and an atomic row counter, with readers racing ahead of the writer
//! into the same allocation. We cannot write that (D2), and ordinary borrow
//! rules cannot express "this allocation is `&mut` above row R and `&` below
//! it, and R moves over time".
//!
//! # The solution: ownership transfer at band granularity
//!
//! A plane is allocated as a sequence of **bands**, each a block of `band_h`
//! rows preceded by `guard` rows of context copied from the band above. The
//! writer owns a band exclusively while filling it, then *moves* it into an
//! [`OnceLock`], which is exactly where it stops being mutable and starts being
//! shared. `OnceLock::set` is a release store and `OnceLock::get` is an acquire
//! load, so no lock is needed on the fast path and no type in this module can
//! observe a partially written band.
//!
//! ```text
//!   writer                                  reader
//!   ------                                  ------
//!   band_mut(k)   exclusive &mut [u8]
//!   ...fill...
//!   publish_through(k):
//!       copy guard rows from band k-1
//!       bands[k].set(band)   ── release ──►  bands[k].get()  ── acquire
//!       ready.store(rows)                    ready.load() / wait_rows()
//! ```
//!
//! [`PictureWriter`] is neither `Sync` nor `Clone`: exactly one frame task
//! holds it. [`PictureRef`] is cheap to clone and `Send + Sync`: it is what a
//! task holds for each of its reference pictures. The compiler, not a
//! convention, is what proves the absence of a data race.
//!
//! # Deviations from plan 15 §1.8.2, and why
//!
//! * The primitives live here rather than in `vaco-frame`. `vaco-frame` is a
//!   layer-1 crate owned by someone else and is still frozen; a decoder-only
//!   concept is also a better fit for the codec framework than for the frame
//!   model. Moving it later is a re-export.
//! * A band carries a *top* guard only, not a top and a bottom one. A bottom
//!   guard would have to be filled from the band below, which is written after
//!   the band above has already been published and become immutable — so it
//!   could never be filled. Reads that extend below a band's last row are
//!   served by the *next* band's top guard, which contains exactly those rows,
//!   and fall back to the copy path when they extend further.
//! * `ready` therefore advances to the last row of the published band rather
//!   than lagging by `guard`. The caller already adds its filter reach when it
//!   decides which row to wait for, and there is no unwritable padding to
//!   protect.
//! * [`PlaneView::block`] returns a `Result`. The copy path needs scratch
//!   space, and a scratch buffer too small for the request has to be reported
//!   rather than silently truncated. Codecs size the scratch once, from their
//!   largest block, so it never fires in practice.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Rows of context a band carries above its own first row.
///
/// Eight rows covers the maximum inter-prediction filter reach of H.264, HEVC,
/// VP9 and AV1, so a motion-compensated read that strays above a band's first
/// row still lands inside one contiguous allocation.
pub const DEFAULT_GUARD: u32 = 8;

/// Default rows per band.
///
/// Large enough that the overwhelming majority of 8×8 and 16×16 block reads sit
/// inside one band, small enough that a consumer does not wait long for the
/// producer to publish.
pub const DEFAULT_BAND_HEIGHT: u32 = 256;

/// One plane's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneSpec {
    /// Meaningful bytes per row. May be less than `stride`.
    pub width_bytes: u32,
    /// Rows in the plane.
    pub height: u32,
    /// Bytes between the start of consecutive rows.
    pub stride: usize,
}

impl PlaneSpec {
    /// A plane whose stride is its width.
    #[must_use]
    pub const fn new(width_bytes: u32, height: u32) -> Self {
        Self {
            width_bytes,
            height,
            stride: width_bytes as usize,
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
        }
    }
}

/// How a picture is banded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureSpec {
    planes: Vec<PlaneSpec>,
    band_h: u32,
    guard: u32,
}

impl PictureSpec {
    /// Bands of [`DEFAULT_BAND_HEIGHT`] rows with [`DEFAULT_GUARD`] rows of
    /// context.
    #[must_use]
    pub fn new(planes: Vec<PlaneSpec>) -> Self {
        Self {
            planes,
            band_h: DEFAULT_BAND_HEIGHT,
            guard: DEFAULT_GUARD,
        }
    }

    /// Set the band height. Clamped to at least one row.
    #[must_use]
    pub const fn with_band_height(mut self, rows: u32) -> Self {
        self.band_h = if rows == 0 { 1 } else { rows };
        self
    }

    /// Set the guard depth.
    #[must_use]
    pub const fn with_guard(mut self, rows: u32) -> Self {
        self.guard = rows;
        self
    }

    /// One band per plane, covering the whole picture.
    ///
    /// The first escape hatch of plan 15 §1.8.3: whenever frame threading is
    /// off or the codec is intra-only, a single band means every
    /// [`PlaneView::block`] takes the contiguous path and the non-threaded case
    /// pays nothing at all for this machinery.
    #[must_use]
    pub fn single_band(mut self) -> Self {
        let tallest = self.planes.iter().map(|p| p.height).max().unwrap_or(1);
        self.band_h = tallest.max(1);
        self.guard = 0;
        self
    }

    /// The planes, in order.
    #[must_use]
    pub fn planes(&self) -> &[PlaneSpec] {
        &self.planes
    }

    /// Rows per band.
    #[must_use]
    pub const fn band_height(&self) -> u32 {
        self.band_h
    }

    /// Guard rows per band.
    #[must_use]
    pub const fn guard(&self) -> u32 {
        self.guard
    }
}

/// One published, immutable band: `guard` rows of context followed by `body`
/// rows of picture.
#[derive(Debug)]
struct Band {
    rows: Box<[u8]>,
    body: u32,
}

#[derive(Debug, Default)]
struct PlaneWait {
    rows: u32,
    failed: bool,
}

/// One plane's publication state.
#[derive(Debug)]
struct ProgressPlane {
    bands: Box<[OnceLock<Band>]>,
    /// Rows guaranteed readable. Monotonic; the fast path is one atomic load.
    ready: AtomicU32,
    state: Mutex<PlaneWait>,
    wake: Condvar,
    width_bytes: u32,
    height: u32,
    band_h: u32,
    guard: u32,
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
        (row / self.band_h) as usize
    }

    const fn band_first_row(&self, k: usize) -> u32 {
        (k as u32).saturating_mul(self.band_h)
    }

    fn band_body_rows(&self, k: usize) -> u32 {
        let first = self.band_first_row(k);
        self.height.saturating_sub(first).min(self.band_h)
    }

    /// Bytes of one published row, or `None` if its band is not published or
    /// the row is outside the plane.
    fn row(&self, gy: u32) -> Option<&[u8]> {
        if gy >= self.height {
            return None;
        }
        let k = self.band_of(gy);
        let band = self.bands.get(k)?.get()?;
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
            let n_bands = if p.height == 0 {
                0
            } else {
                p.height.div_ceil(spec.band_h) as usize
            };
            let mut staging = Vec::new();
            let mut bands = Vec::new();
            for k in 0..n_bands {
                let first = (k as u32).saturating_mul(spec.band_h);
                let body = p.height.saturating_sub(first).min(spec.band_h);
                let rows = (body as usize)
                    .checked_add(spec.guard as usize)
                    .and_then(|r| r.checked_mul(p.stride))
                    .ok_or(Error::LimitExceeded {
                        limit: "picture_band",
                        requested: u64::MAX,
                        cap: u64::MAX,
                    })?;
                let buf: Vec<u8> = budget.alloc(rows)?;
                staging.push(Some(Band {
                    rows: buf.into_boxed_slice(),
                    body,
                }));
                bands.push(OnceLock::new());
            }
            planes.push(ProgressPlane {
                bands: bands.into_boxed_slice(),
                ready: AtomicU32::new(0),
                state: Mutex::new(PlaneWait::default()),
                wake: Condvar::new(),
                width_bytes: p.width_bytes,
                height: p.height,
                band_h: spec.band_h,
                guard: spec.guard,
                stride: p.stride,
            });
            writers.push(PlaneWriter {
                staging,
                published: 0,
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
    /// Bands still owned exclusively by the writer. `None` once published.
    staging: Vec<Option<Band>>,
    published: usize,
}

/// Exclusive access to one band's body rows.
#[derive(Debug)]
pub struct BandMut<'a> {
    data: &'a mut [u8],
    stride: usize,
    rows: u32,
    first_row: u32,
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
}

/// A disjoint run of bands, handed to one slice or tile job.
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

    /// Bands in a plane.
    #[must_use]
    pub fn band_count(&self, plane: usize) -> usize {
        self.planes.get(plane).map_or(0, |p| p.staging.len())
    }

    /// Exclusive access to band `k` of `plane`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or band does not exist, or if the
    /// band has already been published — at which point it is immutable and
    /// shared, and writing to it would be exactly the race this design exists
    /// to prevent.
    pub fn band_mut(&mut self, plane: usize, k: usize) -> Result<BandMut<'_>> {
        let geom = self.picture.plane(plane)?;
        let (stride, guard, band_h, width_bytes) =
            (geom.stride, geom.guard, geom.band_h, geom.width_bytes);
        let pw = self
            .planes
            .get_mut(plane)
            .ok_or(Error::InvalidData("plane index out of range"))?;
        let slot = pw
            .staging
            .get_mut(k)
            .ok_or(Error::InvalidData("band index out of range"))?;
        let band = slot
            .as_mut()
            .ok_or(Error::InvalidData("band was already published"))?;
        let rows = band.body;
        let skip = (guard as usize)
            .checked_mul(stride)
            .ok_or(Error::InvalidData("band geometry overflow"))?;
        let data = band
            .rows
            .get_mut(skip..)
            .ok_or(Error::InvalidData("band geometry overflow"))?;
        Ok(BandMut {
            data,
            stride,
            rows,
            first_row: (k as u32).saturating_mul(band_h),
            width_bytes,
        })
    }

    /// Publish every band of `plane` through `k`, then advertise the rows they
    /// contain and wake anyone waiting for them.
    ///
    /// Each band's guard rows are filled from the tail of its predecessor
    /// first, so a read that strays above a band's first row still lands in one
    /// contiguous allocation.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane or band does not exist, or if bands
    /// are published out of order.
    pub fn publish_through(&mut self, plane: usize, k: usize) -> Result<()> {
        let geom = self.picture.plane(plane)?;
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
        // Release: everything above must be visible before the row count is.
        geom.ready.store(rows, Ordering::Release);
        let mut st = lock(&geom.state);
        if rows > st.rows {
            st.rows = rows;
            geom.wake.notify_all();
        }
        Ok(())
    }

    /// Hand out disjoint band ranges of one plane to concurrent slice or tile
    /// jobs.
    ///
    /// Safety here is `split_at_mut`-style disjointness and nothing exotic:
    /// ranges must be ascending and non-overlapping, and each job gets a
    /// `&mut` that cannot alias any other. Publication stays with the owning
    /// thread after the jobs join, because it is the writer that knows the
    /// order.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the plane does not exist or the ranges are not
    /// ascending, disjoint and in range.
    pub fn split_bands_mut(
        &mut self,
        plane: usize,
        ranges: &[std::ops::Range<usize>],
    ) -> Result<Vec<BandRangeMut<'_>>> {
        let geom = self.picture.plane(plane)?;
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
            let n = self.band_count(plane);
            if n > 0 {
                self.publish_through(plane, n - 1)?;
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

    /// Rows of `plane` readable right now, without blocking.
    #[must_use]
    pub fn ready_rows(&self, plane: usize) -> u32 {
        self.0
            .plane(plane)
            .map_or(0, |p| p.ready.load(Ordering::Acquire))
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

    /// Whether the producing task failed.
    #[must_use]
    pub fn failed(&self, plane: usize) -> bool {
        self.0.plane(plane).is_ok_and(|p| lock(&p.state).failed)
    }
}

/// A read-only window onto the rows of a plane that have been published.
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

    /// One published row.
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
    /// published, or when `scratch` is smaller than the region. Codecs size the
    /// scratch once from their largest block, so the second cannot happen at
    /// run time.
    pub fn block(
        &self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        scratch: &'a mut BlockScratch,
    ) -> Result<BlockRef<'a>> {
        let plane = self.plane;
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
            let band = plane.bands.get(k)?.get()?;
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
