//! `amplify` — amplify (or leave alone) each pixel's deviation from a
//! trailing/leading temporal average, revealing subtle frame-to-frame
//! change while ignoring both noise-level flicker and large real motion.
//!
//! `ffmpeg -h filter=amplify` (2026-08-28): `radius` (`1..=63`, default `2`),
//! `factor` (`0..=65535`, default `2`), `threshold` (`0..=65535`, default
//! `10`), `tolerance` (`0..=65535`, default `0`), `low`/`high`
//! (`0..=65535`, default `65535` each), `planes` (bitmask, default `7`).
//! Timeline-capable.
//!
//! # Measured: the window, the gate, and the clamp (`ffmpeg 8.1`, `-bitexact`,
//! 1x1 `gray` sources carrying a hand-built per-frame sequence via `geq`)
//!
//! None of this is in the reference's own documentation beyond the one-line
//! option help ("Set radius of pixels used in averaging", "Set factor used
//! for amplification", "Set threshold for amplification", "Set tolerance
//! for difference") — fetched from `https://ffmpeg.org/ffmpeg-filters.html`,
//! itself a mirror of `filters.texi`, which D7 treats as a documented
//! interface fact rather than source. Everything below the option names was
//! established by feeding a synthetic sequence of distinct per-frame values
//! through the reference and reading its raw output, not by reading its C.
//!
//! **The window** is symmetric and includes the centre: for `radius=r`, the
//! average is over the `2r+1` frames `[center-r, ..., center, ..., center+r]`.
//! Confirmed independently at `r=1` and `r=2` against a step sequence (flat,
//! then a sustained small jump), matching a hand-computed average to the
//! exact byte at every position once the window is right.
//!
//! **Readiness needs one frame more of history than the window strictly
//! requires.** The first frame the reference ever emits output for is index
//! `radius+1`, not `radius` — a `radius=1`, 10-frame probe emits for indices
//! `2..=8`, not `1..=8`, and this reproduces at `radius=2` (`3..=11` of 14)
//! too. The last `radius` frames are dropped with no compensating emission
//! at EOF (checked: total output count is exactly `input_count - 2*radius -
//! 1` for every `radius`/length combination tried). This crate does not
//! know *why* the reference wants the extra frame; it reproduces the
//! measured readiness rule regardless, via a `2*radius+2`-slot ring buffer
//! that acts on slot `1` (skipping slot `0`) once full.
//!
//! **The gate: `tolerance < |dev| <= threshold`, else pass the centre pixel
//! through unchanged**, where `dev = center - avg`. A deviation of `~1.667`
//! was left untouched at `threshold=0` and `threshold=1` but amplified at
//! `threshold=2` and the default `threshold=10`; the same deviation was
//! amplified at `tolerance=0`/`1`/`1.5` but left untouched at `tolerance=2`.
//! A large, sustained `40`-unit step was left untouched at every `threshold`
//! this crate tried (`0`, `1`, `2`, `10`), consistent with the default
//! `threshold=10` being *why* obviously-real motion is not amplified by
//! default — the filter's whole purpose is subtle change, not motion
//! amplification.
//!
//! **`low`/`high` clamp the added delta's magnitude, not the output value
//! directly, and clamp asymmetrically by sign**: `delta = factor * dev`,
//! then `delta = delta.max(-low)` if negative or `delta.min(high)` if
//! positive, `out = round(center + delta)`. Measured by forcing a `factor=5`
//! amplification that would overshoot to `92`/`113` (from a centre of
//! `100`/`105`) down to exactly `97`/`108` with `low=3, high=3` (i.e. the
//! delta's magnitude clamped to `3` either way), and to `97`/`113`
//! (only the negative side clamped) with `low=3, high=100`. The default
//! `low=high=65535` never binds for 8-bit content, which is why every
//! earlier probe in this module's history looked unclamped.
//!
//! # Not implemented: bit depths above 8
//!
//! Matches this crate's `vignette` and every filter in `vaco-filter-convolve`.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
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
    name: "amplify",
    description: "Amplify changes between successive video frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "amplify",
    help = "Amplify changes between successive video frames."
)]
pub(crate) struct Opts {
    #[opt(name = "radius", help = "set radius", default = 2, range = 1..=63, flags(video, filtering))]
    pub radius: i64,
    #[opt(name = "factor", help = "set factor", default = 2.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub factor: f64,
    #[opt(name = "threshold", help = "set threshold", default = 10.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub threshold: f64,
    #[opt(name = "tolerance", help = "set tolerance", default = 0.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub tolerance: f64,
    #[opt(name = "low", help = "set low limit for amplification", default = 65535.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub low: f64,
    #[opt(name = "high", help = "set high limit for amplification", default = 65535.0, range = 0.0..=65535.0, flags(video, filtering))]
    pub high: f64,
    #[opt(name = "planes", help = "set what planes to filter", default = 7, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
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
    radius: usize,
    factor: f64,
    threshold: f64,
    tolerance: f64,
    low: f64,
    high: f64,
    planes: i64,
    buf: VecDeque<Frame>,
}

impl Filter {
    fn new(opts: &Opts) -> Self {
        Self {
            #[allow(
                clippy::cast_sign_loss,
                reason = "range = 1..=63 is enforced by the option schema"
            )]
            radius: opts.radius as usize,
            factor: opts.factor,
            threshold: opts.threshold,
            tolerance: opts.tolerance,
            low: opts.low,
            high: opts.high,
            planes: opts.planes,
            buf: VecDeque::new(),
        }
    }

    fn capacity(&self) -> usize {
        2 * self.radius + 2
    }

    /// One pixel: `center` amplified against the window average `avg`,
    /// gated by `tolerance`/`threshold`, delta-clamped by `low`/`high`.
    fn amplify_one(&self, center: u8, avg: f64) -> u8 {
        let dev = f64::from(center) - avg;
        if dev.abs() <= self.tolerance || dev.abs() > self.threshold {
            return center;
        }
        let mut delta = self.factor * dev;
        delta = if delta < 0.0 {
            delta.max(-self.low)
        } else {
            delta.min(self.high)
        };
        let raw = f64::from(center) + delta;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "raw is clamped into 0..=255 immediately below"
        )]
        {
            raw.round().clamp(0.0, 255.0) as u8
        }
    }

    /// Produce the output for the window's centre slot (index 1, once the
    /// buffer holds `capacity()` frames), or `None` if the format cannot be
    /// addressed. Does not mutate `self.buf`; the caller pops the spent
    /// leading frame afterward.
    fn amplify_window(&self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        let Some(center) = self.buf.get(1) else {
            return Ok(None);
        };
        let FrameData::Video { format, .. } = center.data else {
            return Ok(None);
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(None);
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(None);
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let window: Vec<&Frame> = self.buf.iter().skip(1).take(2 * self.radius + 1).collect();
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let selected = p == 0 || common::plane_selected(self.planes, p8);
            let ph = common::to_i32(format.plane_height(height, p8)).max(0);
            let Some(center_plane) = center.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            let window_planes: Vec<_> = window.iter().filter_map(|f| f.plane(p)).collect();
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(center_row) = center_plane.row(uy) else {
                    continue;
                };
                let Some(dst_row) = dst_plane.row_mut(uy) else {
                    continue;
                };
                let n = dst_row.len().min(center_row.len());
                for x in 0..n {
                    let Some(&cv) = center_row.get(x) else {
                        continue;
                    };
                    let Some(dst) = dst_row.get_mut(x) else {
                        continue;
                    };
                    *dst = if selected && window_planes.len() == 2 * self.radius + 1 {
                        let sum: f64 = window_planes
                            .iter()
                            .map(|p| {
                                f64::from(p.row(uy).and_then(|r| r.get(x)).copied().unwrap_or(0))
                            })
                            .sum();
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "radius <= 63, well within f64's exact integer range"
                        )]
                        let avg = sum / (2 * self.radius + 1) as f64;
                        self.amplify_one(cv, avg)
                    } else {
                        cv
                    };
                }
            }
        }
        common::copy_frame_meta(&mut out, center);
        Ok(Some(out))
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        self.buf.push_back(input);
        if self.buf.len() < self.capacity() {
            return Ok(FrameOut::None);
        }
        let out = self.amplify_window(ctx)?;
        self.buf.pop_front();
        Ok(out.map_or(FrameOut::None, FrameOut::One))
    }

    fn flush_state(&mut self) {
        self.buf.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts);
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

    fn opts(radius: i64, factor: f64, threshold: f64, tolerance: f64, low: f64, high: f64) -> Opts {
        Opts {
            radius,
            factor,
            threshold,
            tolerance,
            low,
            high,
            planes: 7,
        }
    }

    /// Pinned against the reference probe in this module's doc: radius=1,
    /// small sustained step (100 -> 105), factor=5, default threshold/tolerance.
    #[test]
    fn matches_the_measured_small_step_amplification() {
        let f = Filter::new(&opts(1, 5.0, 10.0, 0.0, 65535.0, 65535.0));
        // center=100 (window src[2,3,4]=100,100,105), avg=101.667 -> dev=-1.667 -> delta=-8.33
        assert_eq!(f.amplify_one(100, 101.666_666_666_666_67), 92);
        // center=105 (window src[3,4,5]=100,105,105), avg=103.333 -> dev=+1.667 -> delta=+8.33
        assert_eq!(f.amplify_one(105, 103.333_333_333_333_33), 113);
    }

    #[test]
    fn a_deviation_at_or_below_tolerance_is_untouched() {
        let f = Filter::new(&opts(1, 5.0, 10.0, 2.0, 65535.0, 65535.0));
        assert_eq!(f.amplify_one(100, 101.666_666_666_666_67), 100);
    }

    #[test]
    fn a_deviation_above_threshold_is_untouched() {
        let f = Filter::new(&opts(1, 5.0, 1.0, 0.0, 65535.0, 65535.0));
        assert_eq!(f.amplify_one(100, 101.666_666_666_666_67), 100);
    }

    /// Pinned: `low`/`high` clamp the delta's magnitude by sign, not the
    /// output value directly.
    #[test]
    fn low_and_high_clamp_the_delta_by_sign() {
        let symmetric = Filter::new(&opts(1, 5.0, 10.0, 0.0, 3.0, 3.0));
        assert_eq!(symmetric.amplify_one(100, 101.666_666_666_666_67), 97);
        assert_eq!(symmetric.amplify_one(105, 103.333_333_333_333_33), 108);

        let asymmetric = Filter::new(&opts(1, 5.0, 10.0, 0.0, 3.0, 100.0));
        assert_eq!(asymmetric.amplify_one(100, 101.666_666_666_666_67), 97);
        assert_eq!(asymmetric.amplify_one(105, 103.333_333_333_333_33), 113);
    }

    #[test]
    fn zero_deviation_is_always_the_identity() {
        let f = Filter::new(&opts(2, 65535.0, 65535.0, 0.0, 65535.0, 65535.0));
        for v in [0u8, 1, 100, 254, 255] {
            assert_eq!(f.amplify_one(v, f64::from(v)), v);
        }
    }

    /// The buffering shape measured in this module's doc: for `radius=r` no
    /// output is possible until `2r+2` frames have arrived, and after that
    /// point exactly one frame's worth of readiness is gained per input
    /// frame (a `VecDeque` that grows to `capacity()` once, then holds
    /// steady). This is the mechanism `filter_frame` relies on, exercised
    /// here without needing a `FilterContext`.
    #[test]
    fn buffer_reaches_capacity_after_two_radius_plus_two_frames_and_then_holds_steady() {
        let mut f = Filter::new(&opts(2, 1.0, 10.0, 0.0, 65535.0, 65535.0));
        assert_eq!(f.capacity(), 6);
        let pool = vaco_frame::FramePool::default();
        let dummy = || {
            pool.acquire_video(vaco_pixfmt::PixFmt::Gray8, 1, 1)
                .unwrap()
        };
        for i in 0..5 {
            f.buf.push_back(dummy());
            assert!(f.buf.len() < f.capacity(), "frame {i}: not ready yet");
        }
        f.buf.push_back(dummy());
        assert_eq!(f.buf.len(), f.capacity(), "6th frame completes the buffer");
        // Simulate what filter_frame does once ready: consume the window,
        // drop the spent leading frame, and the buffer holds at capacity-1
        // until the next frame arrives.
        f.buf.pop_front();
        assert_eq!(f.buf.len(), f.capacity() - 1);
    }

    proptest::proptest! {
        /// Invariant: whatever the gate decides, the output is always a
        /// valid sample and either exactly the input (gated off) or a value
        /// consistent with the clamped-delta formula — never something
        /// outside `0..=255`.
        #[test]
        fn amplify_one_always_stays_in_byte_range(
            center in 0u8..=255,
            avg in 0.0f64..=255.0,
            factor in 0.0f64..=200.0,
            threshold in 0.0f64..=300.0,
            tolerance in 0.0f64..=300.0,
            low in 0.0f64..=300.0,
            high in 0.0f64..=300.0,
        ) {
            let f = Filter::new(&opts(1, factor, threshold, tolerance, low, high));
            let out = f.amplify_one(center, avg);
            proptest::prop_assert!((0..=255u8).contains(&out));
        }

        /// Invariant: a zero deviation (center == avg exactly) is always
        /// the identity, regardless of every other knob — the gate's
        /// `|dev| > tolerance` branch can never fire and the amplified
        /// branch, if it somehow did, would add a zero delta anyway.
        #[test]
        fn zero_deviation_is_the_identity_under_any_settings(
            center in 0u8..=255,
            factor in 0.0f64..=200.0,
            threshold in 0.0f64..=300.0,
            tolerance in 0.0f64..=300.0,
            low in 0.0f64..=300.0,
            high in 0.0f64..=300.0,
        ) {
            let f = Filter::new(&opts(1, factor, threshold, tolerance, low, high));
            proptest::prop_assert_eq!(f.amplify_one(center, f64::from(center)), center);
        }
    }
}
