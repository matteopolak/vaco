//! [`H264FrameTask`] — the parallel half of the decoder, and the only place reference *samples* are read.
//!
//! # The split, and why it falls here
//!
//! [`crate::decoder::H264Decoder::split_packet`] is the serial half: it parses the access unit, runs CABAC
//! over the slice, builds the reference lists, applies clause 8.2.5's reference-picture marking and decides
//! every output ordering question -- mutable decoder state, kept on one thread.
//!
//! This task is everything after that: clause 8.4/8.5 reconstruction, clause 8.7 deblocking, and the crop
//! into a [`Frame`]. It is where the time goes -- a profile puts `reconstruct_picture` +
//! `sample_luma_block` + the two deblocking passes at roughly 55% of self time before the long tail,
//! against about 9% for `decode_slice_cabac` and `residual_block_cabac` together -- and it is the only half
//! that needs reference *pixels*: entropy decoding needs only the co-located picture's motion field, which
//! the serial half already holds the moment that picture's slice was decoded. So the serial half can run
//! arbitrarily far ahead of the pixels, and the dependency graph this task waits on is exactly the
//! reference-picture graph.
//!
//! # How the waiting works, without `unsafe`
//!
//! Each reference is a [`PictureRef`]: a shared handle to a picture that may still be in production.
//! [`TaskCtx::wait_rows`] blocks until the rows are published and hands back a borrow of them. Publication
//! moves a band *out* of the writer and into an `OnceLock`, so a reader cannot observe a partially written
//! one -- the compiler rules out the race.
//!
//! # Granularity: rows, and what bounds the wait
//!
//! Reconstruction and clause 8.7's filter are interleaved a macroblock row at a time
//! ([`crate::reconstruct::PictureReconstructor`]), the filter one row behind, so a row becomes *final*
//! while the picture is still being produced. [`RowPublisher`] copies each band into the DPB entry and
//! publishes it the moment every row it holds is final, so the next picture can predict from it there
//! rather than waiting for the whole picture.
//!
//! The wait is derived, not guessed: before reconstructing macroblock row `my`,
//! [`crate::reconstruct::row_reference_reach`] walks that row's motion vectors and reports, per reference
//! and per plane, the deepest row clause 8.4.2.2's filters will actually read -- `y + (mv_y >> 2) + 6` for
//! luma's six-tap, `cy + (mv_y >> 3) + 2` for chroma's bilinear. A reference the row does not predict from
//! is not waited on, and reading past what was waited for is refused by `PlaneView::block` rather than
//! served, so a bound that was ever too small would be an error and never wrong pixels. At `-threads 1`
//! none of this runs: the DPB entry is one band, the whole picture is waited for once, and the planes are
//! read as plain slices.

use vaco_codec_core::picture::{PictureRef, PictureWriter};
use vaco_codec_core::{FrameTask, TaskCtx};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags, FrameSideData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

use crate::mb::MbSummary;
use crate::reconstruct::{
    BiPredMode, ImplicitWeights, PictureCtx, RefPicturePlanes, RefPlane, RowReach,
    SliceWeightTables, chroma_rows_final, luma_rows_final, macroblocks_in_raster_order,
    row_reference_reach,
};
use crate::task_pool::TaskBufferPools;

pub(crate) use crate::deblock::DeblockParams;

/// Everything about one slice of a picture that reconstruction and the
/// deblocking filter read out of its own slice header, rather than out of the
/// SPS/PPS the whole picture shares.
///
/// A picture may be coded as several slices (clause 7.4.3), each with its own
/// reference lists, weighted-prediction tables and filter knobs, so these
/// cannot be picture constants. [`crate::mb::MbSummary::slice_id`] indexes
/// this array.
#[derive(Debug)]
pub(crate) struct SliceContext {
    /// Clause 8.2.4's final `RefPicList0`/`RefPicList1` for this slice, as
    /// handles to pictures that may still be decoding.
    pub(crate) ref_list0: Vec<PictureRef>,
    pub(crate) ref_list1: Vec<PictureRef>,
    /// The same two lists as POCs, which is all `crate::deblock` needs to
    /// tell two references apart (it never touches a sample of them).
    pub(crate) ref_list0_poc: Vec<i32>,
    pub(crate) ref_list1_poc: Vec<i32>,
    pub(crate) weights: SliceWeightTables,
    pub(crate) bipred_mode: BiPredMode,
    pub(crate) implicit_weights: Option<ImplicitWeights>,
    pub(crate) deblock: DeblockParams,
}

/// What [`crate::decoder::build_frame`] needs to crop the coded picture down to
/// the SPS's displayed size and stamp the container's timing on it.
#[derive(Debug, Clone)]
pub(crate) struct FrameGeometry {
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) crop_unit: (u32, u32),
    pub(crate) crop: vaco_parse_h264::Crop,
    pub(crate) pts: vaco_core::Timestamp,
    pub(crate) duration: vaco_core::Duration,
    pub(crate) is_idr: bool,
    /// Raw `cc_data` triplet bytes from this access unit's own
    /// `user_data_registered_itu_t_t35` SEI (ATSC A/53), if any — empty for
    /// the ordinary case of a picture with no caption SEI. Attached to
    /// *this* picture's `Frame` in [`build_frame`], not accumulated
    /// elsewhere, so the decoder's own POC reordering carries it from
    /// decode order into presentation order for free — see
    /// `vaco_parse_h264::a53`'s module doc for why getting this wrong
    /// fails silently rather than loudly.
    pub(crate) closed_captions: Vec<u8>,
    /// `Sps::color_info()` at decode time, stamped onto this picture's
    /// `Frame::color` in [`build_frame`] — finding 22a
    /// (`planning/INTERFACE-GAPS.md`): the VUI already reaches
    /// `CodecParameters::color` via `vaco_parse_h264::params`, which is
    /// what `vaco-probe -show_streams` reads, but nothing wrote
    /// `Frame::color`, so a decoded frame carried the *default* `ColorInfo`
    /// while probe reported the real one on the same file. `Frame::alloc_video`
    /// leaves `color` as `ColorInfo::default()`, so skipping this field is
    /// indistinguishable from an unspecified stream rather than an error.
    pub(crate) color: vaco_color::ColorInfo,
    /// §D.1.29's mastering-display-colour-volume SEI, converted and
    /// permuted into `vaco_frame::MasteringDisplay`'s shared shape, if this
    /// access unit's SEI carried one — finding 22b
    /// (`planning/INTERFACE-GAPS.md`): parsed correctly by
    /// `vaco_parse_h264::sei` and then read by nothing in this crate.
    pub(crate) mastering_display: Option<vaco_frame::MasteringDisplay>,
    /// §D.1.31's content-light-level SEI as `(max_cll, max_fall)`, same
    /// finding as `mastering_display` above.
    pub(crate) content_light: Option<(u32, u32)>,
}

/// One picture's reconstruction, deblocking and crop.
///
/// `Send + 'static` by construction: every field is owned data or an `Arc`, so
/// there is no borrow of decoder state to leak across a thread boundary and
/// nothing to propagate back.
#[derive(Debug)]
pub(crate) struct H264FrameTask {
    pub(crate) macroblocks: Vec<MbSummary>,
    pub(crate) mbs_wide: u32,
    pub(crate) mbs_high: u32,
    pub(crate) chroma_qp_offset_cb: i32,
    pub(crate) chroma_qp_offset_cr: i32,
    /// One entry per slice of this picture, in the order the slices appeared
    /// in the access unit -- what [`crate::mb::MbSummary::slice_id`] indexes.
    pub(crate) slices: Vec<SliceContext>,
    /// Whether to publish this picture band by band as its rows become final,
    /// and to wait on references a macroblock row at a time.
    ///
    /// Set exactly when `-threads N` asked for more than one thread, because
    /// that is when the DPB entry is allocated with more than one band; at one
    /// thread a picture is one allocation and the whole-picture path reads it
    /// as one slice.
    pub(crate) row_progress: bool,
    /// The sole writer of this picture's DPB entry — `Some` exactly when this
    /// is a reference picture. Dropping it without finishing wakes every
    /// waiter with an error, which is what keeps a failed task from parking
    /// the pictures that referenced it.
    pub(crate) store: Option<PictureWriter>,
    pub(crate) geometry: FrameGeometry,
    /// Per-allocation guard for this task's own three sample planes and its
    /// output frame. The *aggregate* charge for those bytes is held by the
    /// decoder's own budget for as long as the task is in flight — see
    /// [`crate::decoder::H264Decoder::split_packet`]'s own charge — so this
    /// one exists to apply `max_alloc_single`/`max_frame_bytes` to each
    /// individual allocation, not to bound the total a second time.
    pub(crate) limits: Limits,
    /// The decoder's own free lists for this task's working reconstruction
    /// buffer, its `macroblocks` array and its output frame's storage —
    /// see `crate::task_pool`'s own doc. Cloning is a cheap `Arc` bump;
    /// every dispatched task shares the same pools as the decoder that
    /// made it.
    pub(crate) pools: TaskBufferPools,
}

/// The whole of one reference picture, waited for in full.
///
/// The non-row-threaded path: `-threads 1` publishes a DPB entry as one band,
/// so waiting once for every row costs one atomic load and hands back the plane
/// as one slice -- [`RefPlane::Flat`], which is the plain indexed fetch the
/// decoder has always used.
fn whole_planes_of<'a>(
    ctx: &TaskCtx<'_>,
    reference: &'a PictureRef,
) -> Result<RefPicturePlanes<'a>> {
    let mut out: [RefPlane<'a>; 3] = [
        RefPlane::Flat(&[]),
        RefPlane::Flat(&[]),
        RefPlane::Flat(&[]),
    ];
    for (plane, slot) in out.iter_mut().enumerate() {
        // `wait_rows` clamps the row it waits for to the plane's own height, so
        // `u32::MAX` is "every row".
        let view = ctx.wait_rows(reference, plane, u32::MAX)?;
        *slot = match view.contiguous_all() {
            Some(block) => RefPlane::Flat(block.data),
            // A banded picture that happens to be finished: still not one
            // allocation, so it is still read through the block API.
            None => RefPlane::Banded(view),
        };
    }
    let [luma, cb, cr] = out;
    Ok(RefPicturePlanes { luma, cb, cr })
}

/// Wait for exactly the rows reconstructing one macroblock row will read.
///
/// A reference the row does not predict from is not waited on at all; it is
/// refreshed without blocking instead, so that a read the reach derivation did
/// not anticipate is refused by `PlaneView::block` rather than served from a
/// stale watermark.
fn wait_row_planes<'r>(
    ctx: &TaskCtx<'_>,
    refs: &'r [PictureRef],
    reach: &RowReach,
    list: usize,
    out: &mut [RefPicturePlanes<'r>],
) -> Result<()> {
    for (i, reference) in refs.iter().enumerate() {
        let Some(slot) = out.get_mut(i) else { break };
        let luma_to = reach
            .luma
            .get(list)
            .and_then(|a| a.get(i))
            .copied()
            .flatten();
        let chroma_to = reach
            .chroma
            .get(list)
            .and_then(|a| a.get(i))
            .copied()
            .flatten();
        for (plane, want) in [(0usize, luma_to), (1, chroma_to), (2, chroma_to)] {
            let view = match want {
                Some(y) => Some(ctx.wait_rows(reference, plane, y)?),
                None => reference.try_rows(plane, 0),
            };
            let Some(view) = view else { continue };
            let cell = match plane {
                0 => &mut slot.luma,
                1 => &mut slot.cb,
                _ => &mut slot.cr,
            };
            *cell = RefPlane::Banded(view);
        }
    }
    Ok(())
}

/// Copies finished rows of the working picture into this picture's DPB entry,
/// publishing each band the moment it is complete.
///
/// A band is copied and published only once *every* row it contains is final,
/// which is what makes a reader's `OnceLock::get` an acquire load of finished
/// samples and nothing else. At one band per picture this is exactly the
/// publish-once-at-the-end behaviour it replaces.
struct RowPublisher {
    /// The next band to fill, per plane.
    next: [usize; 3],
}

impl RowPublisher {
    const fn new() -> Self {
        Self { next: [0; 3] }
    }

    fn advance(
        &mut self,
        writer: &mut PictureWriter,
        plane: usize,
        src: &[u8],
        stride: usize,
        final_rows: u32,
    ) -> Result<()> {
        let count = writer.band_count(plane);
        loop {
            let Some(&k) = self.next.get(plane) else {
                return Ok(());
            };
            if k >= count {
                return Ok(());
            }
            {
                let mut band = writer.band_mut(plane, k)?;
                let (first, rows) = (band.first_row(), band.rows());
                if first.saturating_add(rows) > final_rows {
                    return Ok(());
                }
                for r in 0..rows {
                    let Some(dst) = band.row_mut(r) else { break };
                    let start = (first.saturating_add(r) as usize).saturating_mul(stride);
                    let Some(row) = src.get(start..start.saturating_add(dst.len())) else {
                        return Err(Error::InvalidData(
                            "vaco-codec-h264: reference band geometry does not match the coded picture",
                        ));
                    };
                    dst.copy_from_slice(row);
                }
            }
            writer.publish_through(plane, k)?;
            if let Some(slot) = self.next.get_mut(plane) {
                *slot = k.saturating_add(1);
            }
        }
    }

    /// Publish every plane through its own watermark.
    fn publish(
        &mut self,
        store: Option<&mut PictureWriter>,
        planes: (&[u8], &[u8], &[u8]),
        strides: (usize, usize),
        rows: (u32, u32),
    ) -> Result<()> {
        let Some(writer) = store else { return Ok(()) };
        self.advance(writer, 0, planes.0, strides.0, rows.0)?;
        self.advance(writer, 1, planes.1, strides.1, rows.1)?;
        self.advance(writer, 2, planes.2, strides.1, rows.1)
    }
}

impl FrameTask for H264FrameTask {
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame> {
        let Self {
            macroblocks,
            mbs_wide,
            mbs_high,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            slices,
            row_progress,
            mut store,
            geometry,
            limits,
            pools,
        } = *self;
        let mut budget = Budget::new(limits);

        // Clause 8.7's filter is skipped entirely only when *every* slice of
        // this picture switched it off; `disable_deblocking_filter_idc == 1`
        // on one slice of several leaves the others still filtering, which
        // `DeblockCtx` handles per macroblock.
        let deblock_slices: Vec<crate::deblock::SliceDeblock> = slices
            .iter()
            .map(|sl| crate::deblock::SliceDeblock {
                params: sl.deblock,
                ref_list0_poc: sl.ref_list0_poc.clone(),
                ref_list1_poc: sl.ref_list1_poc.clone(),
            })
            .collect();
        let any_filtering = deblock_slices.iter().any(|s| s.params.disable_idc != 1);
        let deblock_ctx = any_filtering.then(|| {
            crate::deblock::DeblockCtx::new(&macroblocks, mbs_wide, mbs_high, &deblock_slices)
        });
        let strides = (
            (mbs_wide as usize).saturating_mul(16),
            (mbs_wide as usize).saturating_mul(8),
        );
        let heights = (mbs_high.saturating_mul(16), mbs_high.saturating_mul(8));
        let mut publisher = RowPublisher::new();
        let mut recon = pools.acquire_reconstructor(mbs_wide, mbs_high, &mut budget)?;

        // Row-level threading stays on the one-slice fast path. A multi-slice
        // picture still decodes; it just takes the whole-picture path below,
        // which waits for its references once rather than a row at a time.
        // Reference lists are per slice, so the row schedule's single
        // `planes0`/`planes1` pair (and `wait_row_planes`'s single reach
        // derivation) would have to be replicated per slice to keep the
        // per-row waits correct -- not done, rather than done approximately.
        let row_wise = row_progress
            && slices.len() == 1
            && macroblocks_in_raster_order(&macroblocks, mbs_wide, mbs_high);
        if row_wise {
            let empty = RefPicturePlanes {
                luma: RefPlane::Flat(&[]),
                cb: RefPlane::Flat(&[]),
                cr: RefPlane::Flat(&[]),
            };
            #[allow(
                clippy::indexing_slicing,
                reason = "`row_wise` above is only true for a one-slice picture"
            )]
            let only = &slices[0];
            let mut planes0: Vec<RefPicturePlanes<'_>> = vec![empty; only.ref_list0.len()];
            let mut planes1: Vec<RefPicturePlanes<'_>> = vec![empty; only.ref_list1.len()];
            let mbw = mbs_wide as usize;
            for my in 0..mbs_high {
                let start = (my as usize).saturating_mul(mbw);
                let row = macroblocks
                    .get(start..start.saturating_add(mbw))
                    .unwrap_or(&[]);
                let reach = row_reference_reach(row);
                wait_row_planes(ctx, &only.ref_list0, &reach, 0, &mut planes0)?;
                wait_row_planes(ctx, &only.ref_list1, &reach, 1, &mut planes1)?;
                {
                    let pctx = [PictureCtx::new(
                        mbs_wide,
                        mbs_high,
                        chroma_qp_offset_cb,
                        chroma_qp_offset_cr,
                        &planes0,
                        &planes1,
                        &only.weights,
                        only.bipred_mode,
                        only.implicit_weights.as_ref(),
                    )];
                    recon.reconstruct_row(&macroblocks, my, &pctx)?;
                }
                let final_rows = match &deblock_ctx {
                    Some(d) => {
                        if my == 0 {
                            continue;
                        }
                        let done = my - 1;
                        recon.deblock_row(d, done, chroma_qp_offset_cb, chroma_qp_offset_cr);
                        (
                            luma_rows_final(done).min(heights.0),
                            chroma_rows_final(done).min(heights.1),
                        )
                    }
                    None => (
                        my.saturating_add(1).saturating_mul(16).min(heights.0),
                        my.saturating_add(1).saturating_mul(8).min(heights.1),
                    ),
                };
                publisher.publish(store.as_mut(), recon.planes(), strides, final_rows)?;
            }
            if let Some(d) = &deblock_ctx
                && mbs_high > 0
            {
                recon.deblock_row(d, mbs_high - 1, chroma_qp_offset_cb, chroma_qp_offset_cr);
            }
        } else {
            // Block here, and only here, on the pictures this one predicts
            // from. This is the whole-picture path: `-threads 1`, and any
            // macroblock order the row schedule cannot assume.
            let planes: Vec<(Vec<RefPicturePlanes<'_>>, Vec<RefPicturePlanes<'_>>)> = slices
                .iter()
                .map(|sl| {
                    Ok((
                        sl.ref_list0
                            .iter()
                            .map(|r| whole_planes_of(ctx, r))
                            .collect::<Result<Vec<_>>>()?,
                        sl.ref_list1
                            .iter()
                            .map(|r| whole_planes_of(ctx, r))
                            .collect::<Result<Vec<_>>>()?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let pctx: Vec<PictureCtx<'_>> = slices
                .iter()
                .zip(planes.iter())
                .map(|(sl, (planes0, planes1))| {
                    PictureCtx::new(
                        mbs_wide,
                        mbs_high,
                        chroma_qp_offset_cb,
                        chroma_qp_offset_cr,
                        planes0,
                        planes1,
                        &sl.weights,
                        sl.bipred_mode,
                        sl.implicit_weights.as_ref(),
                    )
                })
                .collect();
            recon.reconstruct_all(&macroblocks, &pctx)?;
            if let Some(d) = &deblock_ctx {
                for my in 0..mbs_high {
                    recon.deblock_row(d, my, chroma_qp_offset_cb, chroma_qp_offset_cr);
                }
            }
        }
        drop(deblock_ctx);
        // Nothing borrows `macroblocks` past `deblock_ctx` (reconstruction
        // above is the only other reader, and it is long done) -- hand it
        // back to the decoder's free list rather than dropping it, so the
        // next picture at this geometry can `push` into it without growing
        // from empty. See `crate::task_pool`'s own doc (item A0).
        pools.release_macroblocks(macroblocks);

        // Publish before building the output frame: every picture waiting on
        // this one is blocked until this line runs, and the crop below is not.
        publisher.publish(store.as_mut(), recon.planes(), strides, heights)?;
        if let Some(writer) = store.take() {
            writer.finish()?;
        }

        let frame = build_frame(&mut budget, mbs_wide, recon.planes(), &geometry, &pools)?;
        // `recon`'s three sample planes are already copied into `frame` by
        // `build_frame`'s own blit -- safe to recycle the working buffer for
        // the next picture at this geometry instead of dropping it.
        pools.release_reconstructor(recon);
        Ok(frame)
    }
}

/// Crop `pic` from its coded (macroblock-aligned) size down to the SPS's own
/// displayed size (clause 7.4.2.1.1's `frame_crop_*`) and pack it into a real
/// [`Frame`], `yuv420p`.
///
/// # Errors
///
/// [`Error::InvalidData`] when the crop leaves nothing visible, or the pixel
/// format is missing from the registry; [`Error::LimitExceeded`] from the frame
/// allocation.
pub(crate) fn build_frame(
    budget: &mut Budget,
    mbs_wide: u32,
    pic: (&[u8], &[u8], &[u8]),
    geometry: &FrameGeometry,
    pools: &TaskBufferPools,
) -> Result<Frame> {
    let (luma, cb, cr) = pic;
    let (width, height) = geometry.dimensions.ok_or(Error::InvalidData(
        "vaco-codec-h264: SPS crop leaves no visible picture area",
    ))?;
    let (unit_x, unit_y) = geometry.crop_unit;
    // Luma offsets are in luma samples (crop units scaled by
    // `CropUnitX`/`CropUnitY`); chroma offsets are the raw crop values
    // themselves, since `CropUnitX`/`CropUnitY` already fold in
    // `SubWidthC`/`SubHeightC` -- one crop unit horizontally is exactly one
    // chroma sample for `ChromaArrayType == 1`.
    let luma_x0 = (geometry.crop.left.saturating_mul(unit_x)) as usize;
    let luma_y0 = (geometry.crop.top.saturating_mul(unit_y)) as usize;
    let chroma_x0 = geometry.crop.left as usize;
    let chroma_y0 = geometry.crop.top as usize;

    let fmt = PixFmt::from_name("yuv420p").map_err(|_| {
        Error::InvalidData("vaco-codec-h264: yuv420p pixel format is not registered")
    })?;
    // The same dimension/size check `Frame::alloc_video` would have made --
    // kept explicit because the allocation itself now goes through the
    // decoder's own `FramePool` (`crate::task_pool`, item A0), which has no
    // `Budget` of its own to check against.
    let bpp = u32::from(fmt.bits_per_pixel()).div_ceil(8).max(1);
    budget.check_frame(width, height, bpp)?;
    let mut frame = pools.acquire_frame(fmt, width, height)?;
    if geometry.is_idr {
        frame.flags |= FrameFlags::KEY;
    }

    let luma_stride = (mbs_wide * 16) as usize;
    let chroma_stride = (mbs_wide * 8) as usize;
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    crate::decoder::blit_plane(luma, luma_stride, luma_x0, luma_y0, &mut frame, 0, w, h);
    crate::decoder::blit_plane(
        cb,
        chroma_stride,
        chroma_x0,
        chroma_y0,
        &mut frame,
        1,
        cw,
        ch,
    );
    crate::decoder::blit_plane(
        cr,
        chroma_stride,
        chroma_x0,
        chroma_y0,
        &mut frame,
        2,
        cw,
        ch,
    );

    frame.pts = geometry.pts;
    frame.duration = geometry.duration;
    frame.color = geometry.color;
    if !geometry.closed_captions.is_empty() {
        let buffer = vaco_pool::Buffer::from_slice(budget, &geometry.closed_captions)?;
        frame.set_side_data(FrameSideData::ClosedCaptions(buffer));
    }
    if let Some(mastering_display) = geometry.mastering_display {
        frame.set_side_data(FrameSideData::MasteringDisplay(Box::new(mastering_display)));
    }
    if let Some((max_cll, max_fall)) = geometry.content_light {
        frame.set_side_data(FrameSideData::ContentLightLevel { max_cll, max_fall });
    }
    Ok(frame)
}
