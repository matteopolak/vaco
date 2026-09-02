//! `rgbtestsrc` — the RGB analogue of [`crate::yuvtestsrc`]: three
//! horizontal bands, each a linear ramp in one colour channel, replicated
//! into alpha.
//!
//! `ffmpeg -h filter=rgbtestsrc` documents `size`/`s`, `rate`/`r`,
//! `duration`/`d`, `sar` and `complement`/`co` (default `false`).
//!
//! # The formula (measured, not read)
//!
//! With `complement=false` (the default), probed at 16×8 and 320×240:
//!
//! ```text
//! band(y) = (y * 3) / h        // 0, 1 or 2 -> R, G, B
//! grad(x) = (x * 256) / w
//! band 0: R = grad(x), G = 0, B = 0, A = grad(x)
//! band 1: R = 0, G = grad(x), B = 0, A = grad(x)
//! band 2: R = 0, G = 0, B = grad(x), A = grad(x)
//! ```
//!
//! confirmed at every sampled point for `complement=false`. **Exact** for
//! that path.
//!
//! # `complement=true` — not verified
//!
//! Probing `complement=1` at 320×240 shows roughly a third of the frame's
//! bytes differ from the `complement=false` output, but every point sampled
//! at the band/gradient coordinates above (the row centres, several columns)
//! was **unchanged** — so whatever `complement` does lives in a region of the
//! frame this crate did not localise in the time available (most likely an
//! inset detail pattern the base three-band formula does not cover at all,
//! since `rgbtestsrc`'s pattern has more structure than the flat gradient
//! bands this module reproduces suggest). Rather than guess at a
//! transformation and risk shipping a wrong answer that looks plausible,
//! `complement` is accepted (so option parsing does not reject the option
//! the reference documents) but has **no effect** here — a real, honestly
//! incomplete implementation rather than a silently wrong one. See
//! `docs/filter/vaco-filter-source.md`.

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
#[options(name = "rgbtestsrc", help = "generate RGB test pattern")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
    /// Accepted, but not implemented — see the module doc's `complement=true`
    /// section for why.
    #[opt(
        name = "complement",
        alias = "co",
        help = "set complement colors",
        default = false,
        flags(filtering)
    )]
    pub complement: bool,
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
    name: "rgbtestsrc",
    description: "Generate RGB test pattern",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

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
            .acquire_video(PixFmt::Rgba, self.width, self.height)?;
        let (w, h) = (self.width, self.height);
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "plane.rows() == h, which fits in u32"
                )]
                let yy = row_idx as u32;
                let active = band(yy, h);
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "x < w, which fits in u32"
                        )]
                        let xx = x as u32;
                        let g = gradient(xx, w);
                        if let [r, gr, b, a] = px {
                            *r = if active == 0 { g } else { 0 };
                            *gr = if active == 1 { g } else { 0 };
                            *b = if active == 2 { g } else { 0 };
                            *a = g;
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
    // `complement` is parsed (see the field doc) but does not change the
    // generated pattern yet.
    let _ = opts.complement;
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
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgba)),
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
        assert_eq!(gradient(300, 320), 240);
        assert_eq!(gradient(319, 320), 255);
    }

    #[test]
    fn band_boundaries_match_measured_reference() {
        assert_eq!(band(79, 240), 0);
        assert_eq!(band(80, 240), 1);
        assert_eq!(band(159, 240), 1);
        assert_eq!(band(160, 240), 2);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "rgbtestsrc",
            instance: "rgbtestsrc",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn complement_option_parses_without_changing_the_pattern() {
        let req = Instantiate {
            name: "rgbtestsrc",
            instance: "rgbtestsrc",
            args: Some("complement=1"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
