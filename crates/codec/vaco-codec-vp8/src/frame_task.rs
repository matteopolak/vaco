//! [`Vp8FrameTask`] — the parallel half of frame threading.
//!
//! # Why VP8 threads at picture granularity, not row granularity
//!
//! `vaco-codec-h264` and `vaco-codec-hevc` publish a picture band by band as
//! rows become final, so a later picture can start predicting from a still-
//! producing reference. VP8 does not need that here: its frames are far
//! cheaper to reconstruct (no CABAC, no 8-tap luma filter, a much simpler
//! loop filter), and — unlike a coding format with B-frames and picture
//! reordering — decode order is always display order, so there is no
//! multi-frame reorder buffer whose *depth* this decoder needs to hide
//! behind row-level overlap. Two-stage pipelining (this frame's
//! reconstruction and loop filter overlapped with the *next* frame's token
//! decode, [`crate::decode::split_frame`]'s doc has the full argument) gets
//! the real, measured win at a fraction of the design and review cost a
//! row-banded rewrite of every predictor and the loop filter would have
//! carried; the crate-level docs record the selected threading boundary.
//!
//! Each reference is therefore published as a single band
//! (`PictureSpec::single_band`) and a task waits for the *whole* picture
//! once, through [`crate::framebuf::materialize`], rather than per-row. RFC
//! 6386 has no notion of a partially available reference frame either —
//! motion compensation can land anywhere in it — so there is no coarser or
//! finer grain that would help.
//!
//! # The cost this design accepts, measured rather than assumed
//!
//! `materialize` copies a whole reference picture into an owned buffer
//! before this task can read it, at *every* thread count including one
//! (`-threads 1` still runs a task through the same `FrameTask::run`, just
//! inline and synchronously — see `vaco_codec_core::threading::FrameRunner`'s
//! own doc). That is a real, deliberate trade: it buys a decoder whose every
//! reconstruction call site (`crate::decode::apply_intra`/`apply_inter`,
//! `crate::loopfilter::apply_frame`) is untouched from the single-threaded
//! implementation already verified byte-exact against `ffmpeg` on 58 of 60
//! real VP8 test vectors, instead of threading a zero-copy borrow through
//! them and re-verifying every one. See this crate's docs for the measured
//! size of that cost.

use vaco_codec_core::picture::PictureWriter;
use vaco_codec_core::{FrameTask, TaskCtx};
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameFlags};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

use crate::decode::{MbInfo, ParsedMb, apply_inter, apply_intra, apply_loop_filter_task, blit};
use crate::framebuf::{Picture, Plane, RefFrames, materialize};

/// This frame's three reference slots, already waited-for and copied into
/// owned buffers — see this module's doc for why that copy is paid instead
/// of threaded around.
#[derive(Debug, Default)]
struct MaterializedRefs {
    last: Option<Picture>,
    golden: Option<Picture>,
    altref: Option<Picture>,
}

impl MaterializedRefs {
    fn get(&self, which: u8) -> Option<&Picture> {
        match which {
            1 => self.last.as_ref(),
            2 => self.golden.as_ref(),
            3 => self.altref.as_ref(),
            _ => None,
        }
    }
}

/// One frame's reconstruction and loop filter — the parallel half of frame
/// threading. `Send + 'static` by construction: every field is
/// owned data, so there is nothing borrowed from decoder state to leak
/// across the thread boundary.
#[derive(Debug)]
pub(crate) struct Vp8FrameTask {
    pub(crate) mb_cols: usize,
    pub(crate) mb_rows: usize,
    /// Every macroblock's decoded mode/motion/residual, from
    /// [`crate::decode::split_frame`]'s serial walk.
    pub(crate) parsed: Vec<ParsedMb>,
    /// The same frame's loop-filter/neighbour-context records, for the loop
    /// filter's per-macroblock filter level and skip-inner-edges decision.
    pub(crate) mbs: Vec<MbInfo>,
    /// The reference slots *this* frame predicts from, as handles — see
    /// `crate::framebuf`'s module doc.
    pub(crate) refs: RefFrames,
    pub(crate) version: u8,
    pub(crate) filter_level: i32,
    pub(crate) sharpness_level: i32,
    pub(crate) filter_simple: bool,
    pub(crate) key_frame: bool,
    pub(crate) width: u16,
    pub(crate) height: u16,
    /// This frame's own output — published once, at the end of [`FrameTask::run`].
    pub(crate) writer: PictureWriter,
    pub(crate) limits: Limits,
}

impl FrameTask for Vp8FrameTask {
    fn run(self: Box<Self>, ctx: &TaskCtx<'_>) -> Result<Frame> {
        let Self {
            mb_cols,
            mb_rows,
            parsed,
            mbs,
            refs,
            version,
            filter_level,
            sharpness_level,
            filter_simple,
            key_frame,
            width,
            height,
            mut writer,
            limits,
        } = *self;

        let mut budget = Budget::new(limits);

        let materialized = MaterializedRefs {
            last: refs
                .last
                .as_ref()
                .map(|r| materialize(r, ctx.decode_index(), mb_cols, mb_rows, &mut budget))
                .transpose()?,
            golden: refs
                .golden
                .as_ref()
                .map(|r| materialize(r, ctx.decode_index(), mb_cols, mb_rows, &mut budget))
                .transpose()?,
            altref: refs
                .altref
                .as_ref()
                .map(|r| materialize(r, ctx.decode_index(), mb_cols, mb_rows, &mut budget))
                .transpose()?,
        };

        let mut y = Plane::new(&mut budget, mb_cols * 16, mb_rows * 16)?;
        let mut u = Plane::new(&mut budget, mb_cols * 8, mb_rows * 8)?;
        let mut v = Plane::new(&mut budget, mb_cols * 8, mb_rows * 8)?;

        for row in 0..mb_rows {
            for col in 0..mb_cols {
                match parsed.get(row * mb_cols + col) {
                    Some(ParsedMb::Intra(p)) => {
                        apply_intra(&mut y, &mut u, &mut v, mb_cols, col, row, p);
                    }
                    Some(ParsedMb::Inter(p)) => {
                        apply_inter(
                            &mut y,
                            &mut u,
                            &mut v,
                            materialized.get(p.ref_frame),
                            version,
                            col,
                            row,
                            p,
                        );
                    }
                    None => {}
                }
            }
            ctx.check_cancelled()?;
        }

        apply_loop_filter_task(
            &mut y,
            &mut u,
            &mut v,
            mb_cols,
            mb_rows,
            &mbs,
            filter_level,
            sharpness_level,
            key_frame,
            filter_simple,
        );

        // Single band per plane (`PictureSpec::single_band`), so band 0 is
        // the whole plane and this is the one place these bytes are ever
        // copied for the picture's own consumers.
        writer
            .band_mut(0, 0)?
            .data_mut()
            .copy_from_slice(y.as_bytes());
        writer
            .band_mut(1, 0)?
            .data_mut()
            .copy_from_slice(u.as_bytes());
        writer
            .band_mut(2, 0)?
            .data_mut()
            .copy_from_slice(v.as_bytes());
        writer.finish()?;

        let fmt = PixFmt::from_name("yuv420p")
            .map_err(|_| Error::InvalidData("vp8: yuv420p pixel format is not registered"))?;
        let mut frame = Frame::alloc_video(&mut budget, fmt, u32::from(width), u32::from(height))?;
        if key_frame {
            frame.flags |= FrameFlags::KEY;
        }
        blit(&y, &mut frame, 0, usize::from(width), usize::from(height));
        blit(
            &u,
            &mut frame,
            1,
            usize::from(width).div_ceil(2),
            usize::from(height).div_ceil(2),
        );
        blit(
            &v,
            &mut frame,
            2,
            usize::from(width).div_ceil(2),
            usize::from(height).div_ceil(2),
        );
        Ok(frame)
    }
}
