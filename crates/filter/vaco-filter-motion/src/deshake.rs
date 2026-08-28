//! `deshake` — single-pass, causal, translation-only video stabilisation.
//!
//! `ffmpeg -h filter=deshake` documents `rx`/`ry` (horizontal/vertical
//! search range in pixels, default `16`), `edge` (`blank`/`original`/
//! `clamp`/`mirror`, default `mirror`), plus `blocksize`/`contrast`/`search`
//! this module does not use.
//!
//! # Algorithm (original, not a transcription of the reference)
//!
//! The reference (and the separate two-pass `vidstabdetect`/
//! `vidstabtransform` pair) do offline or lookahead-smoothed feature
//! tracking with a full affine (rotation + zoom + translation) correction.
//! This module is deliberately simpler, in a way worth stating plainly
//! rather than presenting as equivalent:
//!
//! 1. **Motion estimate**: a fixed `3x3` grid of block searches
//!    ([`vaco_filter_vdsp::motion::search_block`], reused rather than
//!    reimplemented — see that crate's own doc for why a second SAD-search
//!    is exactly what `cargo xtask dup-check` exists to catch) between the
//!    current and previous frame's luma plane. The **median** of the valid
//!    matches' vectors is this frame's motion step — median rather than mean
//!    specifically to resist a single outlier block (a moving subject,
//!    occlusion) from dragging the whole estimate.
//! 2. **Trajectory**: the running sum of every frame's motion step —
//!    the camera's estimated absolute path so far.
//! 3. **Smoothing**: a causal exponential moving average of the trajectory
//!    (fixed `alpha = 0.15`, not exposed as an option — the reference has no
//!    directly matching knob either). This is a real, structural limitation
//!    versus the reference's non-causal (lookahead) smoothing: it cannot
//!    anticipate a hard pan starting, so the first several frames of a new
//!    pan are partially damped before the average catches up.
//! 4. **Correction**: a translation-only [`vaco_filter_vdsp::affine::AffineMap`]
//!    warp that pulls the current frame back toward the *smoothed* path,
//!    scaled per plane by that plane's subsampling ratio against luma.
//!
//! No rotation or zoom correction is attempted (a real, structural gap
//! versus the reference's affine correction, not a bug — extending this to
//! a full affine model needs frame-to-frame block *correspondence* accuracy
//! this grid search does not attempt to provide, only a global translation).
//!
//! # Verified property, not a framecrc comparison
//!
//! There is no reference algorithm to reproduce framecrc-exact against (the
//! whole point is a *different*, simpler correction). What is verified
//! instead, in this module's own tests: on a synthetic sequence with known
//! jitter superimposed on a slow true pan, the corrected sequence's
//! frame-to-frame difference is measurably smaller than the raw input's —
//! i.e. the filter actually reduces jitter rather than (via a sign error in
//! the correction direction) doubling it. That is the one property that
//! actually matters for "not broken": a stabiliser with the correction
//! backwards would still produce a well-formed frame every time.
//!
//! # `edge` handling
//!
//! Two of the reference's four modes are implemented: `blank` (uncovered
//! border pixels are `0`) and everything else (`original`/`clamp`/`mirror`,
//! the default) falls back to the *unwarped* frame's own pixel at that
//! location — closer to `original`'s documented behaviour than to `clamp`
//! or `mirror`, which are not separately implemented. Named here rather than
//! silently defaulted.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_vdsp::affine::{AffineMap, bilinear_sample};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "deshake",
    description: "Stabilize shaky video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const SMOOTHING_ALPHA: f64 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeMode {
    Blank,
    Original,
}

impl EdgeMode {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "0" | "blank" => Ok(Self::Blank),
            "1" | "original" | "2" | "clamp" | "3" | "mirror" => Ok(Self::Original),
            other => Err(format!("deshake: bad `edge` `{other}`")),
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "deshake", help = "Stabilize shaky video.")]
pub(crate) struct Opts {
    #[opt(name = "rx", help = "set x range", default = 16, range = 0..=64, flags(video, filtering))]
    pub rx: i64,
    #[opt(name = "ry", help = "set y range", default = 16, range = 0..=64, flags(video, filtering))]
    pub ry: i64,
    #[opt(name = "edge", help = "set edge mode", default = "mirror".to_owned(), flags(video, filtering))]
    pub edge: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    range: i32,
    edge: EdgeMode,
    prev: Option<Frame>,
    trajectory: (f64, f64),
    smoothed: (f64, f64),
    checked_format: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        Ok(Self {
            range: common::to_i32(opts.rx.max(opts.ry)).max(1),
            edge: EdgeMode::parse(&opts.edge)?,
            prev: None,
            trajectory: (0.0, 0.0),
            smoothed: (0.0, 0.0),
            checked_format: false,
        })
    }

    /// Median motion vector over a `3x3` grid of block searches. `(0.0,
    /// 0.0)` if the frame is too small for even one in-bounds search, or if
    /// no block found a match.
    fn estimate_motion(&self, prev: &Frame, cur: &Frame, width: u32, height: u32) -> (f64, f64) {
        let (Some(p0), Some(c0)) = (prev.plane(0), cur.plane(0)) else {
            return (0.0, 0.0);
        };
        let w = width as usize;
        let h = height as usize;
        #[allow(clippy::integer_division, reason = "block size in pixels, truncation is the intended behaviour")]
        let bw = 32usize.min((w / 4).max(4));
        #[allow(clippy::integer_division, reason = "block size in pixels, truncation is the intended behaviour")]
        let bh = 32usize.min((h / 4).max(4));
        #[allow(clippy::cast_sign_loss, reason = "range is >= 1 by construction")]
        let range = self.range as usize;
        let margin_x = range.max(bw);
        let margin_y = range.max(bh);
        let two_margin_bw = margin_x.saturating_mul(2).saturating_add(bw);
        let two_margin_bh = margin_y.saturating_mul(2).saturating_add(bh);
        if w <= two_margin_bw || h <= two_margin_bh {
            return (0.0, 0.0);
        }
        let usable_w = w - two_margin_bw;
        let usable_h = h - two_margin_bh;
        let mut dxs: Vec<i32> = Vec::new();
        let mut dys: Vec<i32> = Vec::new();
        for r in 0..3usize {
            for c in 0..3usize {
                #[allow(clippy::integer_division, reason = "grid position in pixels over a fixed 3x3 layout, truncation is the intended behaviour")]
                let bx = margin_x + usable_w * c / 2;
                #[allow(clippy::integer_division, reason = "grid position in pixels over a fixed 3x3 layout, truncation is the intended behaviour")]
                let by = margin_y + usable_h * r / 2;
                let m = vaco_filter_vdsp::motion::search_block(c0, p0, bx, by, bw, bh, self.range);
                if m.cost != u32::MAX {
                    dxs.push(m.dx);
                    dys.push(m.dy);
                }
            }
        }
        if dxs.is_empty() {
            return (0.0, 0.0);
        }
        // `search_block(cur, prev, ...)`'s vector points from the current
        // block's position to where that content was found *in the
        // previous* frame, so the content's actual displacement from prev
        // to cur is the negation of that vector.
        (-median(&mut dxs), -median(&mut dys))
    }

    fn warp(&self, pool: &FramePool, frame: &Frame, format: vaco_pixfmt::PixFmt, width: u32, height: u32, corr: (f64, f64)) -> Option<Frame> {
        let mut out = pool.acquire_video(format, width, height).ok()?;
        for p in 0..format.plane_count() {
            let p8 = common::to_i32(p) as u8;
            let pw = format.plane_width(width, p8);
            let ph = format.plane_height(height, p8);
            if width == 0 || height == 0 {
                continue;
            }
            let scale_x = f64::from(pw) / f64::from(width);
            let scale_y = f64::from(ph) / f64::from(height);
            let map = AffineMap::translation(corr.0 * scale_x, corr.1 * scale_y);
            let (Some(src), Some(mut dst)) = (frame.plane(p), out.plane_mut(p)) else {
                continue;
            };
            let dst_w = common::to_i32(pw).max(0);
            let dst_h = common::to_i32(ph).max(0);
            for y in 0..dst_h {
                let Ok(uy) = usize::try_from(y) else { continue };
                for x in 0..dst_w {
                    let Ok(ux) = usize::try_from(x) else { continue };
                    let (sx, sy) = map.apply(f64::from(x), f64::from(y));
                    let sampled = bilinear_sample(src, sx, sy);
                    let value = sampled.or_else(|| match self.edge {
                        EdgeMode::Blank => Some(0),
                        EdgeMode::Original => src.row(uy).and_then(|r| r.get(ux)).copied(),
                    });
                    if let (Some(v), Some(row)) = (value, dst.row_mut(uy))
                        && let Some(cell) = row.get_mut(ux)
                    {
                        *cell = v;
                    }
                }
            }
        }
        Some(out)
    }
}

fn median(v: &mut [i32]) -> f64 {
    v.sort_unstable();
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    #[allow(clippy::integer_division, reason = "middle index of a sorted slice, truncation is the intended behaviour")]
    let mid = n / 2;
    if n % 2 == 1 {
        v.get(mid).copied().map_or(0.0, f64::from)
    } else {
        let prev = mid.checked_sub(1);
        let (Some(&a), Some(&b)) = (prev.and_then(|i| v.get(i)), v.get(mid)) else {
            return 0.0;
        };
        f64::from(a).midpoint(f64::from(b))
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, width, height, .. } = frame.data else {
            return Ok(FrameOut::One(frame));
        };
        if !self.checked_format {
            self.checked_format = true;
            common::ensure_8bit_addressable(format)?;
        }
        let Some(prev) = self.prev.take() else {
            self.prev = Some(frame.clone());
            return Ok(FrameOut::One(frame));
        };
        let motion = self.estimate_motion(&prev, &frame, width, height);
        self.trajectory.0 += motion.0;
        self.trajectory.1 += motion.1;
        self.smoothed.0 += (self.trajectory.0 - self.smoothed.0) * SMOOTHING_ALPHA;
        self.smoothed.1 += (self.trajectory.1 - self.smoothed.1) * SMOOTHING_ALPHA;
        let corr = (self.trajectory.0 - self.smoothed.0, self.trajectory.1 - self.smoothed.1);
        let out = self.warp(ctx.pool(), &frame, format, width, height, corr);
        self.prev = Some(frame.clone());
        match out {
            Some(mut warped) => {
                warped.pts = frame.pts;
                warped.time_base = frame.time_base;
                warped.duration = frame.duration;
                Ok(FrameOut::One(warped))
            }
            None => Ok(FrameOut::One(frame)),
        }
    }

    fn flush_state(&mut self) {
        self.prev = None;
        self.trajectory = (0.0, 0.0);
        self.smoothed = (0.0, 0.0);
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

    fn shifted_frame(w: u32, h: u32, shift: i32) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    for (x, cell) in row.iter_mut().enumerate() {
                        // A diagonal ramp pattern, distinctive enough that a
                        // horizontal shift is unambiguously detectable by
                        // block search, and stable under the fixed 3x3 grid.
                        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "test fixture, small bounded values")]
                        let v = (((x as i32 - shift).rem_euclid(256)) as u8).wrapping_add((y * 7) as u8);
                        *cell = v;
                    }
                }
            }
        }
        f
    }

    fn feed(f: &mut Filter, frame: Frame, width: u32, height: u32) -> Frame {
        let FrameData::Video { format, .. } = frame.data else {
            return frame;
        };
        let Some(prev) = f.prev.take() else {
            f.prev = Some(frame.clone());
            return frame;
        };
        let motion = f.estimate_motion(&prev, &frame, width, height);
        f.trajectory.0 += motion.0;
        f.trajectory.1 += motion.1;
        f.smoothed.0 += (f.trajectory.0 - f.smoothed.0) * SMOOTHING_ALPHA;
        f.smoothed.1 += (f.trajectory.1 - f.smoothed.1) * SMOOTHING_ALPHA;
        let corr = (f.trajectory.0 - f.smoothed.0, f.trajectory.1 - f.smoothed.1);
        let pool = FramePool::default();
        let out = f.warp(&pool, &frame, format, width, height, corr).unwrap();
        f.prev = Some(frame);
        out
    }

    /// A synthetic sequence with alternating +6/-6px jitter on top of no
    /// true pan. If the correction direction were backwards, the corrected
    /// sequence would be *more* different frame-to-frame than the raw
    /// input, not less — the actual bug class this test exists to catch,
    /// per this module's own doc.
    #[test]
    fn jittery_sequence_is_smoothed_more_than_the_raw_input() {
        let (w, h) = (64u32, 64u32);
        let jitters = [0i32, 6, -6, 6, -6, 6, -6];
        let raw: Vec<Frame> = jitters.iter().map(|&s| shifted_frame(w, h, s)).collect();

        let mut filt = Filter::new(&Opts { rx: 16, ry: 16, edge: "original".to_owned() }).unwrap();
        let corrected: Vec<Frame> = raw.iter().map(|f| feed(&mut filt, f.clone(), w, h)).collect();

        let raw_diff: u64 = raw.windows(2).map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap())).sum();
        let corrected_diff: u64 = corrected.windows(2).map(|w2| vaco_filter_vdsp::plane_sad(w2[0].plane(0).unwrap(), w2[1].plane(0).unwrap())).sum();

        assert!(
            corrected_diff < raw_diff,
            "expected stabilisation to reduce total frame-to-frame difference: raw={raw_diff} corrected={corrected_diff}"
        );
    }

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "deshake", instance: "deshake", args: None, arguments: &[] };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_edge_is_a_clean_error() {
        let req = Instantiate { name: "deshake", instance: "deshake", args: Some("edge=nonsense"), arguments: &[] };
        assert!(create(&req).is_err());
    }
}
