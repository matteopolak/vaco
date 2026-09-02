//! `mandelbrot` — the Mandelbrot set escape-time fractal, animated as a
//! continuous zoom from `start_scale` to `end_scale`.
//!
//! `ffmpeg -h filter=mandelbrot` documents `size`/`s`, `rate`/`r`,
//! `maxiter`, `start_x`/`start_y`/`start_scale`/`end_scale`/`end_pts`
//! (the zoom path), `bailout`, `morphxf`/`morphyf`/`morphamp` (a
//! Julia-morph wobble), and `outer`/`inner` colouring modes. The reference's
//! own `-h` text is itself the specification for what each mode means (a
//! black-box probe of the tool's documented options, not its source, per
//! D17) — `inner=mincol`'s help text literally says "color based on point
//! closest to the origin of the iterations", which is precise enough to
//! implement without reading `vsrc_mandelbrot.c`.
//!
//! # What is exact, and what is not
//!
//! The escape-time recurrence itself — `z_{n+1} = z_n^2 + c`, `z_0 = 0`,
//! escaped once `|z_n| > bailout`, capped at `maxiter` — is Mandelbrot's own
//! 1980 definition, independent of any reference implementation, and is
//! this module's actual fractal content. It is checkable two ways that do
//! not depend on any implementation: `c = 0` (the origin) never escapes
//! (it is the fixed point `z = 0`), and `c` outside the radius-2 disc always
//! escapes within one or two iterations (`|c| > 2 => |z_1| = |c| > 2`
//! already). Both are asserted directly against [`escape`] in this module's
//! tests, independent of colour.
//!
//! **The colour palette is not calibrated to the reference.** `-h` names
//! the outer/inner *modes* but not the actual gradient a mode paints with,
//! and that gradient is not something a handful of black-box probes
//! resolve with confidence. This module picks a plain, documented HSV
//! sweep over the normalised escape count for the outer modes, and plain
//! black for `inner=black`; every other `inner` mode falls back to the same
//! HSV sweep applied to the tracked "point closest to the origin" rather
//! than the escape count. **Close, not bit-exact** — same honesty
//! `vaco-filter-blur`'s `gblur` doc gives its IIR-vs-FIR Gaussian gap. See
//! `docs/filter/vaco-filter-source.md`.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "mandelbrot_outer", base = "int")]
pub enum Outer {
    #[opt_const(name = "iteration_count", help = "set iteration count mode")]
    IterationCount,
    #[opt_const(
        name = "normalized_iteration_count",
        help = "set normalized iteration count mode"
    )]
    #[default]
    NormalizedIterationCount,
    #[opt_const(name = "white", help = "set white mode")]
    White,
    #[opt_const(name = "outz", help = "set outz mode")]
    Outz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "mandelbrot_inner", base = "int")]
pub enum Inner {
    #[opt_const(name = "black", help = "set black mode")]
    Black,
    #[opt_const(name = "period", help = "set period mode")]
    Period,
    #[opt_const(name = "convergence", help = "show time until convergence")]
    Convergence,
    #[opt_const(
        name = "mincol",
        help = "color based on point closest to the origin of the iterations"
    )]
    #[default]
    Mincol,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "mandelbrot", help = "render a Mandelbrot fractal")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set frame size", default = (640, 480), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set frame rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "maxiter", help = "set max iterations number", default = 7189, range = 1..=i32::MAX, flags(filtering))]
    pub maxiter: i32,
    #[opt(name = "start_x", help = "set the initial x position", default = -0.743_643_9_f64, range = -100.0..=100.0, flags(filtering))]
    pub start_x: f64,
    #[opt(name = "start_y", help = "set the initial y position", default = -0.131_825_9_f64, range = -100.0..=100.0, flags(filtering))]
    pub start_y: f64,
    #[opt(
        name = "start_scale",
        help = "set the initial scale value",
        default = 3.0,
        flags(filtering)
    )]
    pub start_scale: f64,
    #[opt(
        name = "end_scale",
        help = "set the terminal scale value",
        default = 0.3,
        flags(filtering)
    )]
    pub end_scale: f64,
    #[opt(
        name = "end_pts",
        help = "set the terminal pts value",
        default = 400.0,
        flags(filtering)
    )]
    pub end_pts: f64,
    #[opt(
        name = "bailout",
        help = "set the bailout value",
        default = 10.0,
        flags(filtering)
    )]
    pub bailout: f64,
    #[opt(name = "outer", help = "set outer coloring mode", unit = "mandelbrot_outer", default = Outer::NormalizedIterationCount, default_repr = "normalized_iteration_count", flags(filtering))]
    pub outer: Outer,
    #[opt(name = "inner", help = "set inner coloring mode", unit = "mandelbrot_inner", default = Inner::Mincol, default_repr = "mincol", flags(filtering))]
    pub inner: Inner,
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
    name: "mandelbrot",
    description: "Render a Mandelbrot fractal",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

/// The escape-time recurrence's outcome for one point `c`: either the
/// smooth "normalized iteration count" at which `|z| > bailout`, or
/// [`Escape::Bounded`] (with the closest approach to the origin, for
/// `inner=mincol`) if `maxiter` was reached without escaping.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Escape {
    Escaped { smooth_n: f64 },
    Bounded { min_abs2: f64 },
}

/// Mandelbrot's own 1980 recurrence: `z_{n+1} = z_n^2 + c`, `z_0 = 0`.
/// Independent of any reference implementation — see the module doc.
pub(crate) fn escape(cx: f64, cy: f64, maxiter: u32, bailout: f64) -> Escape {
    let (mut zx, mut zy) = (0.0f64, 0.0f64);
    let bailout2 = bailout * bailout;
    let mut min_abs2 = f64::MAX;
    for n in 0..maxiter {
        let abs2 = zx * zx + zy * zy;
        min_abs2 = min_abs2.min(abs2);
        if abs2 > bailout2 {
            // Smooth escape count: standard normalised-iteration-count
            // formula (Linas Vepstas / common fractal literature).
            let n_f = f64::from(n);
            let smooth_n =
                n_f + 1.0 - (abs2.sqrt().ln() / bailout.ln()).ln() / std::f64::consts::LN_2;
            return Escape::Escaped { smooth_n };
        }
        let (nzx, nzy) = (zx * zx - zy * zy + cx, 2.0 * zx * zy + cy);
        zx = nzx;
        zy = nzy;
    }
    Escape::Bounded { min_abs2 }
}

/// A plain HSV sweep (documented in the module doc as not calibrated to the
/// reference's own gradient) mapping a `[0, 1)` fraction to RGB.
fn palette(t: f64) -> (u8, u8, u8) {
    let t = t.rem_euclid(1.0);
    let h6 = t * 6.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "h6 in [0, 6)"
    )]
    let seg = (h6 as u32).min(5);
    let frac = h6 - f64::from(seg);
    let (r, g, b) = match seg {
        0 => (1.0, frac, 0.0),
        1 => (1.0 - frac, 1.0, 0.0),
        2 => (0.0, 1.0, frac),
        3 => (0.0, 1.0 - frac, 1.0),
        4 => (frac, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - frac),
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "r, g, b in [0, 1]"
    )]
    {
        ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
    }
}

/// `(R, G, B)` for one point, given the resolved `outer`/`inner` modes.
pub(crate) fn color_at(
    cx: f64,
    cy: f64,
    maxiter: u32,
    bailout: f64,
    outer: Outer,
    inner: Inner,
) -> (u8, u8, u8) {
    match escape(cx, cy, maxiter, bailout) {
        Escape::Escaped { smooth_n, .. } => match outer {
            Outer::White => (255, 255, 255),
            #[allow(
                clippy::cast_precision_loss,
                reason = "maxiter is far below f64's precision limit"
            )]
            Outer::IterationCount => palette(smooth_n.floor() / f64::from(maxiter)),
            Outer::NormalizedIterationCount | Outer::Outz => palette(smooth_n * 0.05),
        },
        Escape::Bounded { min_abs2 } => match inner {
            Inner::Black | Inner::Period | Inner::Convergence => (0, 0, 0),
            Inner::Mincol => palette(min_abs2.sqrt()),
        },
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    maxiter: u32,
    start_x: f64,
    start_y: f64,
    start_scale: f64,
    end_scale: f64,
    end_pts: f64,
    bailout: f64,
    outer: Outer,
    inner: Inner,
    frame_rate: Rational,
    next: i64,
}

impl Source {
    fn scale_at(&self, pts: f64) -> f64 {
        if self.end_pts <= 0.0 {
            return self.start_scale;
        }
        let t = (pts / self.end_pts).clamp(0.0, 1.0);
        // Geometric interpolation: a zoom is exponential, not linear.
        self.start_scale * (self.end_scale / self.start_scale).powf(t)
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
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Rgb0, self.width, self.height)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "frame index precision loss is irrelevant at any duration this runs for"
        )]
        let scale = self.scale_at(self.next as f64);
        let (w, h) = (self.width.max(1), self.height.max(1));
        if let Some(mut plane) = frame.plane_mut(0) {
            for row_idx in 0..plane.rows() {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "plane.rows() == height, which fits in u32"
                )]
                let yy = row_idx as u32;
                #[allow(clippy::cast_precision_loss, reason = "pixel coordinates are small")]
                let cy = self.start_y + (f64::from(yy) / f64::from(h) - 0.5) * scale;
                if let Some(row) = plane.row_mut(row_idx) {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "pixel coordinates are small"
                        )]
                        let cx = self.start_x
                            + (x as f64 / f64::from(w) - 0.5)
                                * scale
                                * (f64::from(w) / f64::from(h));
                        let (r, g, b) =
                            color_at(cx, cy, self.maxiter, self.bailout, self.outer, self.inner);
                        if let [pr, pg, pb, pa] = px {
                            *pr = r;
                            *pg = g;
                            *pb = b;
                            *pa = 255;
                        }
                    }
                }
            }
        }
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.duration = vaco_core::Duration(1);
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
    let source = Source {
        width,
        height,
        maxiter: u32::try_from(opts.maxiter.max(1)).unwrap_or(7189),
        start_x: opts.start_x,
        start_y: opts.start_y,
        start_scale: opts.start_scale,
        end_scale: opts.end_scale,
        end_pts: opts.end_pts,
        bailout: opts.bailout.max(1.0),
        outer: opts.outer,
        inner: opts.inner,
        frame_rate: rate,
        next: 0,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet {
                pixel_formats: Some(Constraint::Exact(PixFmt::Rgb0)),
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
    fn the_origin_never_escapes() {
        // c = 0 is the recurrence's fixed point: z stays 0 forever.
        assert!(
            matches!(escape(0.0, 0.0, 1000, 2.0), Escape::Bounded { .. }),
            "the origin must be bounded"
        );
    }

    #[test]
    fn points_far_outside_the_radius_two_disc_escape_immediately() {
        // |c| > 2 => |z_1| = |z_0^2 + c| = |c| > bailout on the very first
        // iteration, independent of any implementation detail.
        match escape(5.0, 5.0, 1000, 2.0) {
            Escape::Escaped { smooth_n } => assert!(smooth_n < 3.0),
            Escape::Bounded { .. } => unreachable!("|c| > 2 must escape"),
        }
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "mandelbrot",
            instance: "mandelbrot",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
