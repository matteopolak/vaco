//! Safe slice-parallel filtering over disjoint output bands.
//!
//! [`SliceFilter`] keeps the input planes immutable and gives each invocation
//! exclusive ownership of its output bands. The adapter joins the jobs before
//! folding their scratch values in index order, so a reduction cannot depend on
//! Rayon scheduling.

use std::collections::VecDeque;

#[cfg(not(target_arch = "wasm32"))]
use rayon::ThreadPool;
#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, PlaneMut, PlaneRef};

use crate::timeline::Timeline;
use crate::{Activity, Filter, FilterContext};

/// Per-slice scratch storage.
///
/// Scratch is private to one job. The adapter hands completed values to
/// [`SliceFilter::reduce`] in increasing slice-index order. The byte buffer is
/// intentionally generic; the little-endian `u64` helpers cover scalar
/// reductions without forcing every filter to allocate its own per-band type.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scratch {
    bytes: Vec<u8>,
}

impl Scratch {
    /// Create empty scratch storage.
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// The bytes written by this slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Mutable storage for a filter-specific accumulator.
    pub fn as_bytes_mut(&mut self) -> &mut Vec<u8> {
        &mut self.bytes
    }

    /// Store one little-endian `u64` reduction value.
    pub fn set_u64(&mut self, value: u64) {
        self.bytes.clear();
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Read a little-endian `u64` reduction value, if one was stored.
    #[must_use]
    pub fn u64(&self) -> Option<u64> {
        let bytes: [u8; 8] = self.bytes.get(..8)?.try_into().ok()?;
        Some(u64::from_le_bytes(bytes))
    }
}

/// One output plane band owned by a [`SliceJob`].
#[derive(Debug)]
pub struct PlaneBandMut<'a> {
    plane: PlaneMut<'a>,
    y0: u32,
    y1: u32,
}

impl<'a> PlaneBandMut<'a> {
    fn new(plane: PlaneMut<'a>, y0: u32, y1: u32) -> Self {
        Self { plane, y0, y1 }
    }

    /// First row of this band in its plane's coordinate system.
    #[must_use]
    pub const fn y0(&self) -> u32 {
        self.y0
    }

    /// Exclusive end row of this band in its plane's coordinate system.
    #[must_use]
    pub const fn y1(&self) -> u32 {
        self.y1
    }

    /// Number of rows in this band.
    #[must_use]
    pub const fn rows(&self) -> usize {
        self.plane.rows()
    }

    /// Meaningful bytes per row.
    #[must_use]
    pub const fn row_bytes(&self) -> usize {
        self.plane.row_bytes()
    }

    /// Read one row at a band-local index.
    #[must_use]
    pub fn row(&self, y: usize) -> Option<&[u8]> {
        self.plane.row(y)
    }

    /// Mutate one row at a band-local index.
    pub fn row_mut(&mut self, y: usize) -> Option<&mut [u8]> {
        self.plane.row_mut(y)
    }

    /// Read-only view of the band.
    #[must_use]
    pub fn as_ref(&self) -> PlaneRef<'_> {
        self.plane.as_ref()
    }
}

/// One independently executable horizontal slice.
#[derive(Debug)]
pub struct SliceJob<'job, 'frame> {
    /// Zero-based slice index.
    pub index: u32,
    /// Number of slices for this frame.
    pub count: u32,
    /// First output luma row owned by this job.
    pub y0: u32,
    /// Exclusive output luma row owned by this job.
    pub y1: u32,
    /// Whole input planes, immutable so neighbouring rows are safe to read.
    pub input: &'job [PlaneRef<'frame>],
    /// One output band per plane, with chroma bands in their own row units.
    pub output: &'job mut [PlaneBandMut<'frame>],
    /// Scratch private to this job.
    pub scratch: &'job mut Scratch,
    /// Timeline result evaluated once for the frame.
    pub enabled: bool,
}

/// Per-frame work for the [`Slice`] adapter.
pub trait SliceFilter: Send + Sync {
    /// Process the output band owned by `job`.
    ///
    /// # Errors
    ///
    /// Returns the filter's processing failure.
    fn slice(&self, job: SliceJob<'_, '_>) -> Result<()>;

    /// Choose the number of slices for a frame.
    fn slice_count(&self, height: u32, threads: u32) -> u32 {
        threads.min(height).max(1)
    }

    /// Fold one completed slice after all workers have joined.
    ///
    /// Calls are strictly ordered by `index`; the default ignores scratch for
    /// filters whose output is purely per-pixel.
    ///
    /// # Errors
    ///
    /// Returns the filter's reduction failure.
    fn reduce(&mut self, _index: u32, _scratch: Scratch) -> Result<()> {
        Ok(())
    }

    /// Clear state discarded by a seek.
    fn flush_state(&mut self) {}
}

fn run_slice<'frame, F: SliceFilter>(
    inner: &F,
    input: &[PlaneRef<'frame>],
    output: &mut [PlaneBandMut<'frame>],
    scratch: &mut Scratch,
    index: usize,
    count: u32,
    height: u32,
    luma_band_rows: usize,
    enabled: bool,
) -> Result<()> {
    let y0 = index
        .saturating_mul(luma_band_rows)
        .min(usize::try_from(height).unwrap_or(usize::MAX));
    let y1 = y0
        .saturating_add(luma_band_rows)
        .min(usize::try_from(height).unwrap_or(usize::MAX));
    inner.slice(SliceJob {
        index: u32::try_from(index).unwrap_or(u32::MAX),
        count,
        y0: u32::try_from(y0).unwrap_or(u32::MAX),
        y1: u32::try_from(y1).unwrap_or(u32::MAX),
        input,
        output,
        scratch,
        enabled,
    })
}

fn run_serial<'frame, F: SliceFilter>(
    inner: &F,
    input: &[PlaneRef<'frame>],
    bands: &mut [Vec<PlaneBandMut<'frame>>],
    scratch: &mut [Scratch],
    count: u32,
    height: u32,
    luma_band_rows: usize,
    enabled: bool,
) -> Result<()> {
    for (index, (output, scratch)) in bands.iter_mut().zip(scratch.iter_mut()).enumerate() {
        run_slice(
            inner,
            input,
            output,
            scratch,
            index,
            count,
            height,
            luma_band_rows,
            enabled,
        )?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn run_parallel<'frame, F: SliceFilter>(
    pool: &ThreadPool,
    inner: &F,
    input: &[PlaneRef<'frame>],
    bands: &mut [Vec<PlaneBandMut<'frame>>],
    scratch: &mut [Scratch],
    count: u32,
    height: u32,
    luma_band_rows: usize,
    enabled: bool,
) -> Result<()> {
    pool.install(|| {
        bands
            .par_iter_mut()
            .zip(scratch.par_iter_mut())
            .enumerate()
            .try_for_each(|(index, (output, scratch))| {
                run_slice(
                    inner,
                    input,
                    output,
                    scratch,
                    index,
                    count,
                    height,
                    luma_band_rows,
                    enabled,
                )
            })
    })
}

/// One-input/one-output adapter that runs [`SliceFilter`] bands in parallel.
#[derive(Debug)]
pub struct Slice<F> {
    inner: F,
    #[cfg(not(target_arch = "wasm32"))]
    pool: Option<ThreadPool>,
    threads: u32,
    pending: VecDeque<Frame>,
    timeline: Timeline,
    done: bool,
    frame_index: u64,
}

impl<F: SliceFilter> Slice<F> {
    /// Construct a pool using this host's available parallelism.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if Rayon cannot create the native worker
    /// pool. WebAssembly uses the serial path.
    pub fn new(inner: F) -> Result<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
        #[cfg(target_arch = "wasm32")]
        let threads = 0;
        Self::with_threads(inner, threads)
    }

    /// Construct an adapter with an explicit pool size.
    ///
    /// A zero-worker configuration runs serially without constructing a pool.
    /// On `wasm32` every configuration is serial because the target has no
    /// native worker threads.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if Rayon cannot create a requested native
    /// worker pool.
    pub fn with_threads(inner: F, threads: u32) -> Result<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        let pool = if threads == 0 {
            None
        } else {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(usize::try_from(threads).unwrap_or(usize::MAX))
                    .build()
                    .map_err(|_| Error::Unsupported("could not create slice thread pool"))?,
            )
        };
        #[cfg(target_arch = "wasm32")]
        let threads = {
            let _ = threads;
            0
        };
        Ok(Self {
            inner,
            #[cfg(not(target_arch = "wasm32"))]
            pool,
            threads,
            pending: VecDeque::new(),
            timeline: Timeline::always(),
            done: false,
            frame_index: 0,
        })
    }

    /// Gate the filter on an `enable=` expression.
    #[must_use]
    pub fn with_timeline(mut self, timeline: Timeline) -> Self {
        self.timeline = timeline;
        self
    }

    /// Borrow the wrapped filter.
    #[must_use]
    pub const fn inner(&self) -> &F {
        &self.inner
    }

    /// Recover the wrapped filter.
    pub fn into_inner(self) -> F {
        self.inner
    }

    /// Number of workers in this adapter's pool.
    #[must_use]
    pub const fn threads(&self) -> u32 {
        self.threads
    }

    fn run_frame(&mut self, input: &Frame, enabled: bool) -> Result<Frame> {
        let height = match &input.data {
            FrameData::Video { height, .. } => *height,
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => 1,
        };
        let input_planes: Vec<_> = (0..16).filter_map(|i| input.plane(i)).collect();
        let mut output = input.clone();
        let mut output_planes = output.planes_mut();
        let requested = self.inner.slice_count(height, self.threads).max(1);
        let plane_limit = output_planes
            .iter()
            .map(PlaneMut::rows)
            .filter(|&rows| rows > 0)
            .min()
            .and_then(|rows| u32::try_from(rows).ok())
            .unwrap_or(1);
        let count = requested.min(height.max(1)).min(plane_limit).max(1);
        let count_usize = usize::try_from(count).unwrap_or(1);
        let luma_band_rows = usize::try_from(height)
            .unwrap_or(usize::MAX)
            .div_ceil(count_usize);
        let mut bands: Vec<Vec<PlaneBandMut<'_>>> = (0..count_usize).map(|_| Vec::new()).collect();
        for plane in output_planes.drain(..) {
            let rows = plane.rows();
            let band_rows = rows.div_ceil(count_usize);
            for (index, band) in plane.split_bands(count_usize).into_iter().enumerate() {
                let y0 = index.saturating_mul(band_rows).min(rows);
                let y1 = y0.saturating_add(band.rows()).min(rows);
                if let Some(job_bands) = bands.get_mut(index) {
                    job_bands.push(PlaneBandMut::new(
                        band,
                        u32::try_from(y0).unwrap_or(u32::MAX),
                        u32::try_from(y1).unwrap_or(u32::MAX),
                    ));
                }
            }
        }
        drop(output_planes);
        let mut scratch: Vec<Scratch> = (0..count_usize).map(|_| Scratch::new()).collect();
        let input_ref = input_planes.as_slice();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(pool) = &self.pool {
            run_parallel(
                pool,
                &self.inner,
                input_ref,
                &mut bands,
                &mut scratch,
                count,
                height,
                luma_band_rows,
                enabled,
            )?;
        } else {
            run_serial(
                &self.inner,
                input_ref,
                &mut bands,
                &mut scratch,
                count,
                height,
                luma_band_rows,
                enabled,
            )?;
        }
        #[cfg(target_arch = "wasm32")]
        run_serial(
            &self.inner,
            input_ref,
            &mut bands,
            &mut scratch,
            count,
            height,
            luma_band_rows,
            enabled,
        )?;
        for (index, scratch) in scratch.into_iter().enumerate() {
            self.inner
                .reduce(u32::try_from(index).unwrap_or(u32::MAX), scratch)?;
        }
        drop(bands);
        Ok(output)
    }
}

impl<F: SliceFilter> Filter for Slice<F> {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if self.done {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        while let Some(frame) = self.pending.pop_front() {
            if !ctx.output_has_room(0) {
                self.pending.push_front(frame);
                return Ok(Activity::Blocked);
            }
            ctx.push_output(0, frame)?;
        }
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }
        if let Some(frame) = ctx.take_input(0) {
            let enabled = self.timeline.evaluate(&frame, self.frame_index);
            self.frame_index = self.frame_index.saturating_add(1);
            let output = self.run_frame(&frame, enabled)?;
            self.pending.push_back(output);
            while let Some(frame) = self.pending.pop_front() {
                if !ctx.output_has_room(0) {
                    self.pending.push_front(frame);
                    break;
                }
                ctx.push_output(0, frame)?;
            }
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            self.done = true;
            return Ok(Activity::Eof);
        }
        Ok(Activity::NeedInput)
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.timeline.configure(ctx);
        Ok(())
    }

    fn flush(&mut self) {
        self.pending.clear();
        self.done = false;
        self.frame_index = 0;
        self.inner.flush_state();
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        if name == "enable" {
            return self.timeline.set_expression(value);
        }
        Err(Error::Unsupported("filter accepts no runtime commands"))
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::panic,
    clippy::unwrap_used,
    reason = "the focused fixture assertions make the test's expected frame layout explicit"
)]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[derive(Debug, Default)]
    struct Probe {
        reductions: Vec<u64>,
    }

    impl SliceFilter for Probe {
        fn slice(&self, job: SliceJob<'_, '_>) -> Result<()> {
            job.scratch.set_u64(u64::from(job.index));
            let value = if job.enabled {
                u8::try_from(job.index).unwrap()
            } else {
                u8::MAX
            };
            for plane in job.output.iter_mut() {
                for y in 0..plane.rows() {
                    plane.row_mut(y).unwrap().fill(value);
                }
            }
            Ok(())
        }

        fn reduce(&mut self, _index: u32, scratch: Scratch) -> Result<()> {
            self.reductions.push(scratch.u64().unwrap());
            Ok(())
        }
    }

    #[test]
    fn bands_and_reductions_are_deterministic() {
        let input = FramePool::default()
            .acquire_video(PixFmt::Yuv420p, 32, 16)
            .unwrap();
        let mut adapter = Slice::with_threads(Probe::default(), 4).unwrap();
        let output = adapter.run_frame(&input, true).unwrap();
        assert_eq!(adapter.inner().reductions, [0, 1, 2, 3]);
        let FrameData::Video { planes, .. } = output.data else {
            panic!("probe output was not video");
        };
        assert_eq!(planes.len(), 3);
        for (plane_index, plane) in planes.iter().enumerate() {
            let rows = if plane_index == 0 { 16 } else { 8 };
            for y in 0..rows {
                let row = plane
                    .data
                    .as_slice()
                    .get(y * plane.stride..(y + 1) * plane.stride)
                    .unwrap();
                assert_eq!(
                    row.first().copied(),
                    Some(u8::try_from(y / (rows.div_ceil(4))).unwrap())
                );
            }
        }
    }

    #[test]
    fn zero_threads_still_runs_one_slice() {
        let input = FramePool::default()
            .acquire_video(PixFmt::Gray8, 8, 2)
            .unwrap();
        let mut adapter = Slice::with_threads(Probe::default(), 0).unwrap();
        assert_eq!(adapter.threads(), 0);
        let _ = adapter.run_frame(&input, true).unwrap();
        assert_eq!(adapter.inner().reductions, [0]);
    }

    #[test]
    fn timeline_result_reaches_every_slice() {
        let input = FramePool::default()
            .acquire_video(PixFmt::Gray8, 8, 2)
            .unwrap();
        let mut adapter = Slice::with_threads(Probe::default(), 2).unwrap();
        let output = adapter.run_frame(&input, false).unwrap();
        let FrameData::Video { planes, .. } = output.data else {
            panic!("probe output was not video");
        };
        assert!(
            planes[0]
                .data
                .as_slice()
                .chunks(planes[0].stride)
                .take(2)
                .all(|row| row.first().copied() == Some(u8::MAX))
        );
    }

    #[test]
    fn scratch_u64_is_little_endian_and_bounded() {
        let mut scratch = Scratch::new();
        scratch.set_u64(0x0102_0304_0506_0708);
        assert_eq!(scratch.as_bytes(), &[8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(scratch.u64(), Some(0x0102_0304_0506_0708));
        scratch.as_bytes_mut().truncate(7);
        assert_eq!(scratch.u64(), None);
    }
}
