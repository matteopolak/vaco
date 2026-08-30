//! [`H264FrameTask`] — the parallel half of the decoder, and the only place
//! reference *samples* are read.
//!
//! # The split, and why it falls here
//!
//! [`crate::decoder::H264Decoder::split_packet`] is the serial half: it parses
//! the access unit, runs CABAC over the slice, builds the reference lists,
//! applies clause 8.2.5's reference-picture marking and decides every output
//! ordering question. All of that is mutable decoder state and stays on one
//! thread, exactly as `vaco_codec_core::threading`'s module doc argues it
//! should.
//!
//! This is everything after that: clause 8.4/8.5 reconstruction, clause 8.7
//! deblocking, and the crop into a [`Frame`]. Two facts make it the right
//! seam:
//!
//! * **It is where the time goes.** The profile in `planning/E2E-GAPS.md` §19
//!   puts `reconstruct_picture` + `sample_luma_block` + the two deblocking
//!   passes at roughly 55% of self time before the long tail, against about 9%
//!   for `decode_slice_cabac` and `residual_block_cabac` together.
//! * **It is the only half that needs reference pixels.** Entropy decoding
//!   needs the co-located picture's *motion field*, which is metadata the
//!   serial half already holds the moment that picture's slice was decoded —
//!   never its samples. So the serial half can run arbitrarily far ahead of
//!   the pixels, and the dependency graph this task waits on is exactly the
//!   reference-picture graph and nothing else.
//!
//! # How the waiting works, without `unsafe`
//!
//! Each reference is a [`PictureRef`]: a shared handle to a picture that may
//! still be in production. [`TaskCtx::wait_rows`] blocks until the rows are
//! published and hands back a borrow of them. Publication moves a band *out* of
//! the writer and into an `OnceLock`, so a reader cannot observe a partially
//! written one — the compiler, not a convention, is what rules out the race.
//!
//! This decoder publishes at **picture** granularity ([`PictureSpec::single_band`]),
//! not row granularity, for one concrete reason: [`crate::deblock`] runs as two
//! whole-picture passes *after* reconstruction, so no row of this picture is
//! final until the last macroblock row has been reconstructed and both passes
//! have swept it. Publishing rows early would publish undeblocked samples,
//! which is not what a later picture's motion compensation must read. Moving to
//! row granularity means interleaving deblocking into the macroblock-row loop
//! first; the band machinery here already supports it, and switching is a
//! change to one `PictureSpec` plus a `publish_through` call per band.

use vaco_codec_core::picture::{PictureRef, PictureWriter};
use vaco_codec_core::{FrameTask, TaskCtx};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

use crate::mb::MbSummary;
use crate::reconstruct::{
    BiPredMode, ImplicitWeights, ReconstructedPicture, RefPicturePlanes, SliceWeightTables,
    reconstruct_picture,
};

/// The slice-header knobs clause 8.7's filter reads, carried whole rather than
/// as four loose arguments.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeblockParams {
    pub(crate) disable_idc: u32,
    pub(crate) alpha_c0_offset_div2: i32,
    pub(crate) beta_offset_div2: i32,
}

/// What [`crate::decoder::build_frame`] needs to crop the coded picture down to
/// the SPS's displayed size and stamp the container's timing on it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameGeometry {
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) crop_unit: (u32, u32),
    pub(crate) crop: vaco_parse_h264::Crop,
    pub(crate) pts: vaco_core::Timestamp,
    pub(crate) duration: vaco_core::Duration,
    pub(crate) is_idr: bool,
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
    /// Clause 8.2.4's final `RefPicList0`/`RefPicList1`, as handles to
    /// pictures that may still be decoding.
    pub(crate) ref_list0: Vec<PictureRef>,
    pub(crate) ref_list1: Vec<PictureRef>,
    /// The same two lists as POCs, which is all `crate::deblock` needs to tell
    /// two references apart (it never touches a sample of them).
    pub(crate) ref_list0_poc: Vec<i32>,
    pub(crate) ref_list1_poc: Vec<i32>,
    pub(crate) weights: SliceWeightTables,
    pub(crate) bipred_mode: BiPredMode,
    pub(crate) implicit_weights: Option<ImplicitWeights>,
    pub(crate) deblock: DeblockParams,
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
}

/// One reference picture's three planes, borrowed from the published bands.
fn planes_of<'a>(
    ctx: &TaskCtx<'_>,
    reference: &'a PictureRef,
) -> Result<RefPicturePlanes<'a>> {
    let mut out: [&'a [u8]; 3] = [&[], &[], &[]];
    for (plane, slot) in out.iter_mut().enumerate() {
        // `wait_rows` clamps the row it waits for to the plane's own height, so
        // `u32::MAX` is "every row" -- which is what picture-granularity
        // publication means. Row granularity would ask for the highest row this
        // picture's motion vectors can reach instead; see the module doc.
        let view = ctx.wait_rows(reference, plane, u32::MAX)?;
        let block = view.contiguous_all().ok_or(Error::InvalidData(
            "vaco-codec-h264: a reference plane was not published as one band",
        ))?;
        *slot = block.data;
    }
    let [luma, cb, cr] = out;
    Ok(RefPicturePlanes { luma, cb, cr })
}

/// Copy a finished plane into this picture's DPB entry.
fn publish_plane(writer: &mut PictureWriter, plane: usize, src: &[u8]) -> Result<()> {
    {
        let mut band = writer.band_mut(plane, 0)?;
        let dst = band.data_mut();
        let n = dst.len().min(src.len());
        let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) else {
            return Err(Error::InvalidData(
                "vaco-codec-h264: reference band geometry does not match the coded picture",
            ));
        };
        d.copy_from_slice(s);
    }
    writer.publish_through(plane, 0)
}

impl FrameTask for H264FrameTask {
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame> {
        let Self {
            macroblocks,
            mbs_wide,
            mbs_high,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            ref_list0,
            ref_list1,
            ref_list0_poc,
            ref_list1_poc,
            weights,
            bipred_mode,
            implicit_weights,
            deblock,
            mut store,
            geometry,
            limits,
        } = *self;
        let mut budget = Budget::new(limits);

        // Block here, and only here, on the pictures this one predicts from.
        // Everything above is owned; everything below reads only these borrows.
        let planes0: Vec<RefPicturePlanes<'_>> = ref_list0
            .iter()
            .map(|r| planes_of(ctx, r))
            .collect::<Result<Vec<_>>>()?;
        let planes1: Vec<RefPicturePlanes<'_>> = ref_list1
            .iter()
            .map(|r| planes_of(ctx, r))
            .collect::<Result<Vec<_>>>()?;

        let mut pic: ReconstructedPicture = reconstruct_picture(
            &macroblocks,
            mbs_wide,
            mbs_high,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            &planes0,
            &planes1,
            &weights,
            bipred_mode,
            implicit_weights.as_ref(),
            &mut budget,
        )?;
        drop(planes0);
        drop(planes1);

        crate::deblock::deblock_picture_luma(
            &mut pic.luma,
            &macroblocks,
            mbs_wide,
            mbs_high,
            deblock.disable_idc,
            deblock.alpha_c0_offset_div2,
            deblock.beta_offset_div2,
            &ref_list0_poc,
            &ref_list1_poc,
        )?;
        for (chroma, offset) in [
            (&mut pic.cb, chroma_qp_offset_cb),
            (&mut pic.cr, chroma_qp_offset_cr),
        ] {
            crate::deblock::deblock_picture_chroma(
                chroma,
                &macroblocks,
                mbs_wide,
                mbs_high,
                offset,
                deblock.disable_idc,
                deblock.alpha_c0_offset_div2,
                deblock.beta_offset_div2,
                &ref_list0_poc,
                &ref_list1_poc,
            );
        }

        // Publish before building the output frame: every picture waiting on
        // this one is blocked until this line runs, and the crop below is not.
        if let Some(writer) = store.as_mut() {
            publish_plane(writer, 0, &pic.luma)?;
            publish_plane(writer, 1, &pic.cb)?;
            publish_plane(writer, 2, &pic.cr)?;
        }
        if let Some(writer) = store.take() {
            writer.finish()?;
        }

        build_frame(&mut budget, mbs_wide, &pic, &geometry)
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
    pic: &ReconstructedPicture,
    geometry: &FrameGeometry,
) -> Result<Frame> {
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
    let mut frame = Frame::alloc_video(budget, fmt, width, height)?;
    if geometry.is_idr {
        frame.flags |= FrameFlags::KEY;
    }

    let luma_stride = (mbs_wide * 16) as usize;
    let chroma_stride = (mbs_wide * 8) as usize;
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    crate::decoder::blit_plane(&pic.luma, luma_stride, luma_x0, luma_y0, &mut frame, 0, w, h);
    crate::decoder::blit_plane(&pic.cb, chroma_stride, chroma_x0, chroma_y0, &mut frame, 1, cw, ch);
    crate::decoder::blit_plane(&pic.cr, chroma_stride, chroma_x0, chroma_y0, &mut frame, 2, cw, ch);

    frame.pts = geometry.pts;
    frame.duration = geometry.duration;
    Ok(frame)
}
