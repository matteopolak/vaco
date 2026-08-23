//! `color` — generate a solid-colour video source.
//!
//! `ffmpeg -h filter=color` documents `color`/`c` (default `"black"`),
//! `size`/`s` (default `"320x240"`), `rate`/`r` (default `"25"`),
//! `duration`/`d` and `sar`. `vaco_core::parse::color` already speaks the
//! reference's exact colour grammar (named colours, `#rrggbb[aa]`,
//! `name@alpha`) — see plan `00-decisions.md` D17 for why our colour table
//! matches the reference's two known-wrong entries rather than a published
//! standard. Output format is `rgb24`; the reference's `color` source can
//! also emit `yuv420p` (its actual default), but a solid fill converts
//! losslessly either way and `rgb24` is what this filter can build directly
//! from `vaco_core::parse::Rgba` without a colour-matrix decision.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration(-1);

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "color", help = "provide an uniformly coloured input")]
pub(crate) struct Opts {
    #[opt(name = "color", alias = "c", help = "set color", default = "black".to_owned(), flags(filtering))]
    pub color: String,
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
    name: "color",
    description: "Provide an uniformly colored input, syntax is 'color=color:size:rate'",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    rgb: [u8; 3],
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
            .acquire_video(PixFmt::Rgb24, self.width, self.height)?;
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for px in row.chunks_exact_mut(3) {
                        if let Some(dst) = px.get_mut(..3) {
                            dst.copy_from_slice(&self.rgb);
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
    let rgba = vaco_core::parse::color(&opts.color)
        .ok_or_else(|| format!("color: bad color `{}`", opts.color))?;
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
        rgb: [rgba.r, rgba.g, rgba.b],
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
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb24)),
                ..FormatSet::default()
            }],
            ties: Vec::new(),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Sourced::new(source)),
    })
}
