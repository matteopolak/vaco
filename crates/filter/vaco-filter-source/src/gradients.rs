//! `gradients` — a multi-stop colour gradient (linear, radial, circular,
//! spiral or square) drawn across the frame, in `rgba`.
//!
//! `ffmpeg -h filter=gradients` documents `size`/`s`, `rate`/`r`, up to
//! eight colour stops `c0`..`c7` (each default `"random"`), `x0`/`y0`/`x1`/
//! `y1` (each default `-1`, meaning "pick one"), `nb_colors`/`n` (2..8,
//! default 2), `seed`, `duration`/`d`, `speed` (rotation, default 0.01) and
//! `type`/`t` (0 = `linear`, default; 1 = `radial`; 2 = `circular`; 3 =
//! `spiral`; 4 = `square`).
//!
//! # What is implemented, and what is not calibrated
//!
//! The colour blend along a gradient parameter `t in [0, 1)` — piecewise
//! linear across `nb_colors` evenly spaced stops — is exact given explicit
//! colours and explicit endpoints, and is this module's real content. What
//! is **not** measured against the reference:
//!
//! * `x0`/`y0`/`x1`/`y1 = -1` ("auto") — this crate resolves `-1` to the
//!   frame's centre and a corner, its own reasonable choice, not the
//!   reference's.
//! * `c0..c7 = "random"` — resolved via this crate's own [`crate::rng`],
//!   not the reference's RNG.
//! * `speed` (per-frame rotation) — not animated; every frame renders the
//!   same static gradient.
//! * `radial`/`circular`/`spiral`/`square` — implemented as documented
//!   distance metrics from the axis (Euclidean, angular, log-spiral,
//!   Chebyshev respectively) rather than measured pixel-for-pixel against
//!   the reference.
//!
//! **Close, not bit-exact**, except that the `linear` type with explicit
//! `c0`/`c1`/`x0`/`y0`/`x1`/`y1` and no animation exercises the one part of
//! this module (the piecewise-linear blend) that is a plain, checkable
//! formula. See `docs/filter/vaco-filter-source.md`.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use crate::rng::{SplitMix64, resolve_seed};
use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "gradients_type", base = "int")]
pub enum GradientType {
    #[opt_const(name = "linear", help = "set linear gradient")]
    #[default]
    Linear,
    #[opt_const(name = "radial", help = "set radial gradient")]
    Radial,
    #[opt_const(name = "circular", help = "set circular gradient")]
    Circular,
    #[opt_const(name = "spiral", help = "set spiral gradient")]
    Spiral,
    #[opt_const(name = "square", help = "set square gradient")]
    Square,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "gradients", help = "draw a gradients")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set frame size", default = (640, 480), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set frame rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "c0", help = "set 1st color", default = "random".to_owned(), flags(filtering))]
    pub c0: String,
    #[opt(name = "c1", help = "set 2nd color", default = "random".to_owned(), flags(filtering))]
    pub c1: String,
    #[opt(name = "c2", help = "set 3rd color", default = "random".to_owned(), flags(filtering))]
    pub c2: String,
    #[opt(name = "c3", help = "set 4th color", default = "random".to_owned(), flags(filtering))]
    pub c3: String,
    #[opt(name = "c4", help = "set 5th color", default = "random".to_owned(), flags(filtering))]
    pub c4: String,
    #[opt(name = "c5", help = "set 6th color", default = "random".to_owned(), flags(filtering))]
    pub c5: String,
    #[opt(name = "c6", help = "set 7th color", default = "random".to_owned(), flags(filtering))]
    pub c6: String,
    #[opt(name = "c7", help = "set 8th color", default = "random".to_owned(), flags(filtering))]
    pub c7: String,
    #[opt(name = "x0", help = "set gradient line source x0", default = -1, flags(filtering))]
    pub x0: i32,
    #[opt(name = "y0", help = "set gradient line source y0", default = -1, flags(filtering))]
    pub y0: i32,
    #[opt(name = "x1", help = "set gradient line destination x1", default = -1, flags(filtering))]
    pub x1: i32,
    #[opt(name = "y1", help = "set gradient line destination y1", default = -1, flags(filtering))]
    pub y1: i32,
    #[opt(name = "nb_colors", alias = "n", help = "set the number of colors", default = 2, range = 2..=8, flags(filtering))]
    pub nb_colors: i32,
    #[opt(name = "seed", help = "set the seed", default = -1_i64, flags(filtering))]
    pub seed: i64,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "type", help = "set gradient type", unit = "gradients_type", default = GradientType::Linear, default_repr = "linear", flags(filtering))]
    pub kind: GradientType,
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
    name: "gradients",
    description: "Draw a gradients",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

fn resolve_color(spec: &str, rng: &mut SplitMix64) -> [u8; 3] {
    if spec == "random" {
        [rng.next_byte(), rng.next_byte(), rng.next_byte()]
    } else {
        vaco_core::parse::color(spec).map_or([0, 0, 0], |c| [c.r, c.g, c.b])
    }
}

/// The piecewise-linear blend across `stops` at parameter `t in [0, 1]`.
pub(crate) fn blend(stops: &[[u8; 3]], t: f64) -> [u8; 3] {
    let Some(&first) = stops.first() else {
        return [0, 0, 0];
    };
    if stops.len() == 1 {
        return first;
    }
    let t = t.clamp(0.0, 1.0);
    let segs = stops.len() - 1;
    #[allow(clippy::cast_precision_loss, reason = "segs <= 7")]
    let scaled = t * segs as f64;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scaled in [0, segs]"
    )]
    let seg = (scaled.floor() as usize).min(segs - 1);
    let frac = scaled - seg as f64;
    let a = stops.get(seg).copied().unwrap_or(first);
    let b = stops.get(seg + 1).copied().unwrap_or(a);
    let mut out = [0u8; 3];
    for ((dst, &av), &bv) in out.iter_mut().zip(a.iter()).zip(b.iter()) {
        let v = f64::from(av) + (f64::from(bv) - f64::from(av)) * frac;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "v is a convex combination of two u8s"
        )]
        {
            *dst = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The gradient parameter `t` for `(x, y)`, per `kind`.
fn gradient_t(x: f64, y: f64, x0: f64, y0: f64, x1: f64, y1: f64, kind: GradientType) -> f64 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    match kind {
        GradientType::Linear => {
            if len2 <= 0.0 {
                0.0
            } else {
                (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0)
            }
        }
        GradientType::Radial | GradientType::Circular => {
            let r = len2.sqrt().max(1.0);
            let d = ((x - x0).powi(2) + (y - y0).powi(2)).sqrt();
            match kind {
                GradientType::Circular => (d / r).rem_euclid(1.0),
                _ => (d / r).clamp(0.0, 1.0),
            }
        }
        GradientType::Spiral => {
            let r = len2.sqrt().max(1.0);
            let d = ((x - x0).powi(2) + (y - y0).powi(2)).sqrt();
            let angle = (y - y0).atan2(x - x0) / std::f64::consts::TAU;
            ((d / r) + angle).rem_euclid(1.0)
        }
        GradientType::Square => {
            let half = len2.sqrt().max(1.0);
            let d = (x - x0).abs().max((y - y0).abs());
            (d / half).clamp(0.0, 1.0)
        }
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    stops: Vec<[u8; 3]>,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    kind: GradientType,
    frame_rate: Rational,
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
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
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
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(clippy::cast_precision_loss, reason = "row index stays small")]
                let y = row_idx as f64;
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        #[allow(clippy::cast_precision_loss, reason = "column index stays small")]
                        let xf = x as f64;
                        let t = gradient_t(xf, y, self.x0, self.y0, self.x1, self.y1, self.kind);
                        let rgb = blend(&self.stops, t);
                        if let [r, g, b, a] = px {
                            *r = rgb[0];
                            *g = rgb[1];
                            *b = rgb[2];
                            *a = 255;
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
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
    let total_frames = if opts.duration.as_micros() < 0 {
        None
    } else {
        Some(
            (opts.duration.as_secs_f64() * rate.to_f64())
                .round()
                .max(0.0) as u64,
        )
    };
    let mut rng = SplitMix64::new(resolve_seed(opts.seed, 0x6772_6164));
    let all = [
        &opts.c0, &opts.c1, &opts.c2, &opts.c3, &opts.c4, &opts.c5, &opts.c6, &opts.c7,
    ];
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "nb_colors is range-checked to 2..=8"
    )]
    let n = opts.nb_colors as usize;
    let stops: Vec<[u8; 3]> = all
        .iter()
        .take(n)
        .map(|c| resolve_color(c, &mut rng))
        .collect();
    let (fw, fh) = (f64::from(width), f64::from(height));
    let resolve = |v: i32, auto: f64| if v < 0 { auto } else { f64::from(v) };
    let x0 = resolve(opts.x0, fw / 2.0);
    let y0 = resolve(opts.y0, fh / 2.0);
    let x1 = resolve(opts.x1, fw - 1.0);
    let y1 = resolve(opts.y1, fh - 1.0);
    let source = Source {
        width,
        height,
        stops,
        x0,
        y0,
        x1,
        y1,
        kind: opts.kind,
        frame_rate: rate,
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
    fn blend_at_the_endpoints_is_exact() {
        let stops = vec![[0, 0, 0], [255, 255, 255]];
        assert_eq!(blend(&stops, 0.0), [0, 0, 0]);
        assert_eq!(blend(&stops, 1.0), [255, 255, 255]);
        assert_eq!(blend(&stops, 0.5), [128, 128, 128]);
    }

    #[test]
    fn blend_across_three_stops_hits_the_middle_stop_exactly() {
        let stops = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255]];
        assert_eq!(blend(&stops, 0.5), [0, 255, 0]);
    }

    #[test]
    fn linear_gradient_t_is_monotonic_along_its_axis() {
        let mut prev = -1.0;
        for x in 0..10 {
            let t = gradient_t(f64::from(x), 0.0, 0.0, 0.0, 9.0, 0.0, GradientType::Linear);
            assert!(t >= prev);
            prev = t;
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "gradients",
            instance: "gradients",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn explicit_colors_and_geometry_parse() {
        let req = Instantiate {
            name: "gradients",
            instance: "gradients",
            args: Some("c0=red:c1=blue:x0=0:y0=0:x1=63:y1=0:size=64x32"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
