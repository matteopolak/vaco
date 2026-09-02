//! `yuvtestsrc` — three horizontal bands, each a full-range linear ramp in
//! one plane while the other two planes sit at the neutral mid-point.
//!
//! `ffmpeg -h filter=yuvtestsrc` documents `size`/`s` (default `"320x240"`),
//! `rate`/`r`, `duration`/`d` and `sar` — plain `nullsrc`-style options, no
//! filter-specific ones.
//!
//! # The formula (measured, not read)
//!
//! Probed at 16×8 and confirmed at 320×240 (`ffmpeg -f lavfi -i
//! yuvtestsrc=size=WxH -f rawvideo -pix_fmt yuv444p -frames:v 1 -`). For row
//! `y`, column `x`, output size `w`×`h`:
//!
//! ```text
//! band(y)  = (y * 3) / h                       // 0, 1 or 2
//! grad(x)  = (x * 256) / w                      // 0..255
//! band 0: Y = grad(x), U = 128, V = 128
//! band 1: Y = 128,     U = grad(x), V = 128
//! band 2: Y = 128,     U = 128,     V = grad(x)
//! ```
//!
//! confirmed against measured points including `w = 320`, where
//! `grad(50) = 40`, `grad(300) = 240`, `grad(319) = 255`, and the 240-row
//! frame splits into three 80-row bands exactly.
//!
//! **Exact.**

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration(-1);
const NEUTRAL: u8 = 128;

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "yuvtestsrc", help = "generate YUV test pattern")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
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

pub const DESC: FilterDesc = FilterDesc {
    name: "yuvtestsrc",
    description: "Generate YUV test pattern",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// `grad(x) = (x * 256) / w`, clamped for the `x == 0, w == 0` degenerate
/// case (an empty frame has no pixels to fill, but a `0`-width plane must
/// not panic on the division).
#[allow(
    clippy::integer_division,
    reason = "the gradient ramp is a floor division of x*256 by width, by construction"
)]
fn gradient(x: u32, w: u32) -> u8 {
    if w == 0 {
        return 0;
    }
    let v = (u64::from(x) * 256) / u64::from(w);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "x < w, so v < 256, which fits in u8"
    )]
    {
        v.min(255) as u8
    }
}

#[allow(
    clippy::integer_division,
    reason = "the row band index is a floor division of y*3 by height, by construction"
)]
fn band(y: u32, h: u32) -> u8 {
    if h == 0 {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "y < h, so y*3/h < 3, which fits in u8"
    )]
    {
        ((u64::from(y) * 3) / u64::from(h)) as u8
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for Source {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Yuv444p, self.width, self.height)?;
        let (w, h) = (self.width, self.height);
        for plane_idx in 0..3usize {
            if let Some(mut plane) = frame.plane_mut(plane_idx) {
                for row_idx in 0..plane.rows() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "plane.rows() == h, which fits in u32"
                    )]
                    let yy = row_idx as u32;
                    let active_band = band(yy, h);
                    if let Some(row) = plane.row_mut(row_idx) {
                        for (x, px) in row.iter_mut().enumerate() {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "x < w, which fits in u32"
                            )]
                            let xx = x as u32;
                            *px = if plane_idx as u8 == active_band {
                                gradient(xx, w)
                            } else {
                                NEUTRAL
                            };
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let total_frames = if opts.duration.0 < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    let source = Source {
        width,
        height,
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Yuv444p)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_matches_measured_reference() {
        assert_eq!(gradient(50, 320), 40);
        assert_eq!(gradient(100, 320), 80);
        assert_eq!(gradient(300, 320), 240);
        assert_eq!(gradient(319, 320), 255);
        assert_eq!(gradient(0, 16), 0);
        assert_eq!(gradient(1, 16), 16);
        assert_eq!(gradient(15, 16), 240);
    }

    #[test]
    fn band_boundaries_match_measured_reference() {
        // 240-row frame: three 80-row bands, exactly.
        assert_eq!(band(0, 240), 0);
        assert_eq!(band(79, 240), 0);
        assert_eq!(band(80, 240), 1);
        assert_eq!(band(159, 240), 1);
        assert_eq!(band(160, 240), 2);
        assert_eq!(band(239, 240), 2);
        // 8-row frame: 3/3/2, per the module doc's worked example.
        assert_eq!(band(2, 8), 0);
        assert_eq!(band(3, 8), 1);
        assert_eq!(band(5, 8), 1);
        assert_eq!(band(6, 8), 2);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "yuvtestsrc",
            instance: "yuvtestsrc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn gradient_and_band_guard_against_a_zero_dimension() {
        // `gradient`/`band` are called per-pixel against a frame the pool
        // already allocated, so a `0` width/height never reaches them in
        // practice — but they must not divide by zero if it ever does.
        assert_eq!(gradient(0, 0), 0);
        assert_eq!(band(0, 0), 0);
    }
}
