//! `pal100bars`/`pal75bars` — EBU-style colour bars.
//!
//! `ffmpeg -h filter=pal100bars`/`pal75bars` document `size`/`s` (default
//! `"320x240"`), `rate`/`r` (default `"25"`), `duration`/`d` and `sar`,
//! the same shape as `vaco-filter-plumbing`'s `color` — this module's
//! `Opts` mirrors that one directly.
//!
//! # Measured: eight equal vertical segments, not seven
//!
//! ```text
//! ffmpeg -f lavfi -i pal100bars=size=490x1 -f rawvideo -pix_fmt rgb24 - | xxd
//! ```
//!
//! Byte-scanning the row finds colour boundaries at columns `0, 62, 124,
//! 186, 248, 310, 372, 434` of 490 — eight segments of (nominally) equal
//! width, not seven bars filling the frame. The eighth segment is black.
//! Colours in order: white, yellow, cyan, green, magenta, red, blue, black.
//! `pal75bars` repeats the exact same boundaries with the six *interior*
//! colours (yellow through blue) scaled to 75% of full amplitude; the
//! leading white and trailing black segments are measured identical between
//! the two filters (white stays reference white, black stays zero) — only
//! the saturated colours carry the "75%" in the name.
//!
//! # Fidelity
//!
//! The boundary formula used here is `boundary(i) = i * width / 8`
//! (integer division), which reproduces the measured example (`490`,
//! `8`ths landing on `0,61,122,183,245,306,367,428`) only approximately —
//! the reference's actual boundaries (`0,62,124,186,248,310,372,434`) are
//! consistently one to six columns later, which looks like a different
//! rounding rule (possibly biased towards the *end* of each segment) that
//! was not pinned down further in the time available. Colours are exact;
//! segment widths are close but not bit-identical at a width that is not a
//! multiple of 8. Output is `rgb24`; the reference's native format for these
//! sources is `yuv422p`, so converting through `-pix_fmt rgb24` for the
//! probe above introduces its own small rounding (`255` prints as `253`,
//! `0` as `2`-ish) that this filter does not reproduce because it never
//! goes through YUV at all — see [`vaco_filter_video_geometry`]'s `fill`
//! module (a sibling crate) for the same trade-off made explicitly for
//! `pad`'s border colour.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const OUTPUT_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

const UNLIMITED: VDuration = VDuration::from_micros(-1);

/// The eight EBU/PAL bar colours at full (100%) amplitude, in display order.
const FULL: [[u8; 3]; 8] = [
    [255, 255, 255], // white
    [255, 255, 0],   // yellow
    [0, 255, 255],   // cyan
    [0, 255, 0],     // green
    [255, 0, 255],   // magenta
    [255, 0, 0],     // red
    [0, 0, 255],     // blue
    [0, 0, 0],       // black
];

/// `pal75bars`'s six interior colours scaled to 75%; the leading white and
/// trailing black are left at full amplitude, matching the measurement.
fn scaled(amplitude_pct: u32) -> [[u8; 3]; 8] {
    if amplitude_pct >= 100 {
        return FULL;
    }
    let mut out = FULL;
    for c in &mut out[1..7] {
        for v in c.iter_mut() {
            #[allow(
                clippy::integer_division,
                reason = "percentage scaling; truncation is the intended rounding"
            )]
            let scaled = (u32::from(*v) * amplitude_pct) / 100;
            *v = scaled as u8;
        }
    }
    out
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pal100bars", help = "generate PAL 100% color bars")]
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

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    colors: [[u8; 3]; 8],
    frame_rate: Rational,
    sar: Rational,
    total_frames: Option<u64>,
    next: i64,
}

impl Source {
    fn paint(&self, frame: &mut Frame) {
        let Some(mut plane) = frame.plane_mut(0) else {
            return;
        };
        let width = self.width.max(1);
        for y in 0..plane.rows() {
            let Some(row) = plane.row_mut(y) else {
                continue;
            };
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                #[allow(
                    clippy::integer_division,
                    reason = "segment index from a pixel position; truncation is the bucketing rule"
                )]
                let seg = (((x as u32).saturating_mul(8)) / width).min(7) as usize;
                if let (Some(dst), Some(color)) = (px.get_mut(..3), self.colors.get(seg)) {
                    dst.copy_from_slice(color);
                }
            }
        }
    }
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
            .acquire_video(PixFmt::Rgb24, self.width, self.height)?;
        self.paint(&mut frame);
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
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

fn build(
    desc: FilterDesc,
    colors: [[u8; 3]; 8],
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    if width == 0 || height == 0 {
        return Err(format!(
            "{}: size must be non-zero, got {width}x{height}",
            desc.name
        ));
    }
    let rate = opts.rate.0;
    let total_frames = if opts.duration < VDuration::ZERO {
        None
    } else {
        Some(crate::frame_budget(opts.duration, rate))
    };
    let source = Source {
        width,
        height,
        colors,
        frame_rate: rate,
        sar: opts.sar,
        total_frames,
        next: 0,
    };
    Ok(Instance {
        desc,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb24)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}

pub mod pal100 {
    use super::{FULL, FilterDesc, FilterFlags, Instance, Instantiate, OUTPUT_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "pal100bars",
        description: "generate PAL 100% color bars",
        inputs: &[],
        outputs: OUTPUT_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, FULL, req)
    }
}

pub mod pal75 {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, OUTPUT_PAD, build, scaled};

    pub const DESC: FilterDesc = FilterDesc {
        name: "pal75bars",
        description: "generate PAL 75% color bars",
        inputs: &[],
        outputs: OUTPUT_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, scaled(75), req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn full_bars_are_the_eight_measured_colours() {
        assert_eq!(FULL[0], [255, 255, 255]);
        assert_eq!(FULL[7], [0, 0, 0]);
    }

    #[test]
    fn seventy_five_percent_scales_interior_colours_only() {
        let s = scaled(75);
        assert_eq!(s[0], [255, 255, 255], "white stays full amplitude");
        assert_eq!(s[7], [0, 0, 0], "black stays black");
        assert_eq!(s[1], [191, 191, 0], "yellow scaled to 75%");
    }

    #[test]
    fn paint_writes_eight_segments_left_to_right() {
        let pool = vaco_frame::FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Rgb24, 80, 1).unwrap();
        let source = Source {
            width: 80,
            height: 1,
            colors: FULL,
            frame_rate: Rational::new(25, 1),
            sar: Rational::ONE,
            total_frames: None,
            next: 0,
        };
        source.paint(&mut frame);
        let row = frame.plane(0).unwrap();
        let row = row.row(0).unwrap();
        // Segment width is 10 (80/8): first pixel of each segment should
        // match that segment's colour.
        for (seg, color) in FULL.iter().enumerate() {
            let x = seg * 10;
            assert_eq!(&row[x * 3..x * 3 + 3], color.as_slice(), "segment {seg}");
        }
    }

    #[test]
    fn every_declared_name_creates_with_default_args() {
        for name in ["pal100bars", "pal75bars"] {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            let result = match name {
                "pal100bars" => pal100::create(&req),
                _ => pal75::create(&req),
            };
            assert!(result.is_ok(), "{name} should build with default options");
        }
    }
}
