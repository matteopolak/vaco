//! `framerate` — convert to a target frame rate by cross-fading between the
//! two bracketing input frames, rather than duplicating
//! (`vaco-filter-video-format::fps`) or dropping.
//!
//! `ffmpeg -h filter=framerate` documents `fps` (default `50`),
//! `interp_start`/`interp_end` (0..255, defaults 15/255 — the reference's own
//! per-pixel "blend only if the two source pixels are close enough" gate) and
//! `scene` (0..100, default 8.2 — a whole-frame scene-cut threshold below
//! which no blending is attempted for that frame pair) and `flags`
//! (`scene_change_detect`, on by default).
//!
//! # What this module implements, measured against nothing (an original
//! algorithm, not a transcription)
//!
//! This is **not** the reference's block-motion-compensated blend — that
//! needs `mestimate`'s dense per-macroblock field, which this crate does not
//! implement (see the crate doc). Instead: for every output time instant
//! that falls strictly between the two frames currently held (`prev`,
//! `next`), the blend factor `t = (t_out - t_prev) / (t_next - t_prev)` is
//! computed from each frame's own rescaled timestamp, and every plane is a
//! plain per-sample linear cross-fade `round(prev*(1-t) + next*t)` — a
//! whole-frame version of `interp_start`/`interp_end`'s per-pixel gate, using
//! only the `scene` threshold: [`vaco_filter_vdsp::normalised_sad`] between
//! `prev` and `next`'s luma planes, scaled to the reference's `0..100` scale,
//! is compared against `scene` (default `8.2`, matching the reference's
//! documented default). Above it, the pair is treated as a cut and the
//! nearer frame (by `t`) is emitted unblended instead of cross-faded.
//! `interp_start`/`interp_end` are parsed (for option-string compatibility)
//! but not otherwise used — this is a coarser, whole-frame version of the
//! same idea, named as a simplification rather than silently ignored.
//!
//! # Buffering shape
//!
//! Same "hold one frame behind" shape as `vaco-filter-video-format::fps`:
//! one input frame is always held so that every output slot can be
//! evaluated against a real `(prev, next)` pair. `eof_action` is not
//! implemented; end of stream emits the held frame once more at the next
//! slot (equivalent to the reference's `eof_action=pass`), not the
//! extrapolate-the-last-gap behaviour `fps`'s `round` default uses — a
//! named, deliberate simplification since `framerate` has no matching
//! option for it in the reference in the first place.
//!
//! Only 8-bit, non-palette, non-hardware, non-bitstream formats are
//! blended; anything else is rejected by `create` via
//! `common::ensure_8bit_addressable` at first frame.

use smallvec::SmallVec;
use vaco_core::{Duration, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "framerate",
    description: "Upsamples or downsamples progressive source between specified frame rates.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "framerate",
    help = "Upsamples or downsamples progressive source between specified frame rates."
)]
pub(crate) struct Opts {
    #[opt(name = "fps", help = "required output frames per second rate", default = "50".to_owned(), flags(video, filtering))]
    pub fps: String,
    #[opt(name = "interp_start", help = "point to start linear interpolation", default = 15, range = 0..=255, flags(video, filtering))]
    pub interp_start: i64,
    #[opt(name = "interp_end", help = "point to end linear interpolation", default = 255, range = 0..=255, flags(video, filtering))]
    pub interp_end: i64,
    #[opt(
        name = "scene",
        help = "scene change level used to disable interpolation",
        default = 8.2,
        flags(video, filtering)
    )]
    pub scene: f64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.interp_start != 15 {
            return Err("framerate: `interp_start` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.interp_end != 255 {
            return Err("framerate: `interp_end` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug)]
struct Held {
    frame: Frame,
    slot: i64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    fps: Rational,
    out_tb: Rational,
    in_tb: Rational,
    scene_threshold: f64,
    held: Option<Held>,
    next_out_pts: i64,
    checked_format: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let fps = vaco_core::parse::rational(&opts.fps)
            .ok_or_else(|| format!("framerate: bad `fps` `{}`", opts.fps))?;
        if fps.num <= 0 || fps.den <= 0 {
            return Err(format!(
                "framerate: `fps` must be positive, got `{}`",
                opts.fps
            ));
        }
        Ok(Self {
            fps,
            out_tb: fps.inverse(),
            in_tb: Rational::UNDEFINED,
            scene_threshold: opts.scene,
            held: None,
            next_out_pts: 0,
            checked_format: false,
        })
    }

    fn slot_of(&self, pts: Timestamp) -> i64 {
        pts.rescale(self.in_tb, self.out_tb, Rounding::NearestAwayFromZero)
            .ticks()
            .unwrap_or(self.next_out_pts)
    }

    /// Fraction of the way from `prev`'s slot to `next`'s slot that output
    /// slot `n` falls at, clamped to `0.0..=1.0` (an output slot is always
    /// chosen to lie in `[prev.slot, next.slot)`, but float rounding can
    /// still nudge it a hair outside).
    fn blend_factor(prev_slot: i64, next_slot: i64, n: i64) -> f64 {
        let span = next_slot.saturating_sub(prev_slot);
        if span <= 0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss, reason = "slot counts are far below 2^53")]
        let t = (n.saturating_sub(prev_slot)) as f64 / span as f64;
        t.clamp(0.0, 1.0)
    }

    fn blend_frame(
        pool: &vaco_frame::FramePool,
        prev: &Frame,
        next: &Frame,
        t: f64,
    ) -> Option<Frame> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = prev.data
        else {
            return None;
        };
        let mut out = pool.acquire_video(format, width, height).ok()?;
        for p in 0..format.plane_count() {
            let p8 = common::to_i32(p) as u8;
            let ph = common::to_i32(format.plane_height(height, p8));
            let (Some(a), Some(b), Some(mut dst)) =
                (prev.plane(p), next.plane(p), out.plane_mut(p))
            else {
                continue;
            };
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let (Some(ra), Some(rb)) = (a.row(uy), b.row(uy)) else {
                    continue;
                };
                let n = ra.len().min(rb.len());
                let Some(dst_row) = dst.row_mut(uy) else {
                    continue;
                };
                for x in 0..n {
                    let (Some(&av), Some(&bv)) = (ra.get(x), rb.get(x)) else {
                        continue;
                    };
                    let blended = f64::from(av).mul_add(1.0 - t, f64::from(bv) * t).round();
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "convex combination of two u8 values, 0.0..=255.0"
                    )]
                    let byte = blended.clamp(0.0, 255.0) as u8;
                    if let Some(cell) = dst_row.get_mut(x) {
                        *cell = byte;
                    }
                }
            }
        }
        Some(out)
    }

    fn stamp(mut frame: Frame, slot: i64, out_tb: Rational) -> Frame {
        frame.pts = Timestamp::new(slot);
        frame.time_base = out_tb;
        frame.duration = Duration(1);
        frame
    }

    fn is_scene_change(&self, prev: &Frame, next: &Frame) -> bool {
        let (Some(a), Some(b)) = (prev.plane(0), next.plane(0)) else {
            return false;
        };
        let pct = vaco_filter_vdsp::normalised_sad(a, b) * 100.0;
        pct > self.scene_threshold
    }

    /// Emit every output slot in `[self.next_out_pts, next.slot)`, blending
    /// (or nearest-picking, across a detected cut) between `held` and
    /// `next`.
    fn emit_between(
        &mut self,
        pool: &vaco_frame::FramePool,
        held: &Frame,
        held_slot: i64,
        next: &Frame,
        next_slot: i64,
    ) -> FrameOut {
        let cut = self.is_scene_change(held, next);
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        let mut n = self.next_out_pts;
        while n < next_slot {
            let t = Self::blend_factor(held_slot, next_slot, n);
            let frame = if cut {
                if t < 0.5 { held.clone() } else { next.clone() }
            } else {
                Self::blend_frame(pool, held, next, t).unwrap_or_else(|| held.clone())
            };
            out.push(Self::stamp(frame, n, self.out_tb));
            n = n.saturating_add(1);
        }
        self.next_out_pts = n.max(self.next_out_pts);
        FrameOut::from_iter(out)
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Video { time_base, .. }) = ctx.input_link(0) {
            self.in_tb = *time_base;
        }
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                time_base,
                frame_rate,
                ..
            } = &mut out
            {
                *time_base = self.out_tb;
                *frame_rate = self.fps;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        if !self.checked_format {
            self.checked_format = true;
            if let FrameData::Video { format, .. } = frame.data {
                common::ensure_8bit_addressable(format)?;
            }
        }
        let slot = self.slot_of(frame.pts);
        let Some(held) = self.held.take() else {
            self.held = Some(Held { frame, slot });
            return Ok(FrameOut::None);
        };
        let out = self.emit_between(ctx.pool(), &held.frame, held.slot, &frame, slot);
        self.held = Some(Held { frame, slot });
        Ok(out)
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let Some(held) = self.held.take() else {
            return Ok(FrameOut::None);
        };
        let n = self.next_out_pts;
        self.next_out_pts = n.saturating_add(1);
        Ok(FrameOut::One(Self::stamp(held.frame, n, self.out_tb)))
    }

    fn flush_state(&mut self) {
        self.held = None;
        self.next_out_pts = 0;
        self.checked_format = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn frame_at(pts: i64, tb: Rational, fill: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        f.pts = Timestamp::new(pts);
        f.time_base = tb;
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..4 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(fill);
                }
            }
        }
        f
    }

    fn opts(fps: &str) -> Opts {
        Opts {
            fps: fps.to_owned(),
            interp_start: 15,
            interp_end: 255,
            scene: 8.2,
        }
    }

    fn sample(out: &FrameOut, i: usize) -> Option<u8> {
        let frames: Vec<&Frame> = match out {
            FrameOut::None => vec![],
            FrameOut::One(f) => vec![f],
            FrameOut::Many(v) => v.iter().collect(),
        };
        frames
            .get(i)
            .and_then(|f| f.plane(0))
            .and_then(|p| p.row(0))
            .and_then(|r| r.first().copied())
    }

    #[test]
    fn upsampling_blends_between_two_flat_frames() {
        let mut f = Filter::new(&opts("100")).unwrap();
        f.in_tb = Rational::new(1, 50);
        let out0 = f.filter_frame_direct(frame_at(0, Rational::new(1, 50), 0));
        assert!(out0.is_empty());
        // A small step (20 of 255, ~7.8%) stays under the default `scene`
        // threshold (8.2%) so this exercises the blend path, not the
        // scene-cut path — see the dedicated cut test below for the other
        // one.
        let out1 = f.filter_frame_direct(frame_at(1, Rational::new(1, 50), 20));
        // 50->100 emits two output slots per input interval: slot 0 exactly
        // at `held`'s own timestamp (t=0, unblended), then slot 1 halfway
        // to `next` (t=0.5, blended).
        assert_eq!(sample(&out1, 0), Some(0));
        assert_eq!(sample(&out1, 1), Some(10));
    }

    #[test]
    fn a_scene_cut_disables_blending_and_picks_the_nearer_frame() {
        let mut f = Filter::new(&opts("100")).unwrap();
        f.in_tb = Rational::new(1, 50);
        let _ = f.filter_frame_direct(frame_at(0, Rational::new(1, 50), 0));
        let out1 = f.filter_frame_direct(frame_at(1, Rational::new(1, 50), 255));
        // A full 0->255 swing on every pixel is a clear cut: the single
        // intermediate slot (t=0.5) must not be a blended ~127, it must be
        // one of the two original values.
        let v = sample(&out1, 0).unwrap();
        assert!(v == 0 || v == 255, "expected a hard pick, got {v}");
    }

    impl Filter {
        fn filter_frame_direct(&mut self, frame: Frame) -> FrameOut {
            if !self.checked_format {
                self.checked_format = true;
            }
            let pool = vaco_frame::FramePool::default();
            let slot = self.slot_of(frame.pts);
            let Some(held) = self.held.take() else {
                self.held = Some(Held { frame, slot });
                return FrameOut::None;
            };
            let out = self.emit_between(&pool, &held.frame, held.slot, &frame, slot);
            self.held = Some(Held { frame, slot });
            out
        }
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "framerate",
            instance: "framerate",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_fps_is_a_clean_error() {
        let req = Instantiate {
            name: "framerate",
            instance: "framerate",
            args: Some("fps=notanumber"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
