//! `colorspectrum` — a full hue wheel swept across the width, blended
//! vertically toward black, white, or both.
//!
//! `ffmpeg -h filter=colorspectrum` documents `size`/`s` (default
//! `"320x240"`), `rate`/`r`, `duration`/`d`, `sar` and `type` (0 = `black`,
//! 1 = `white`, 2 = `all`, default `black`). Output is `gbrpf32le` — the
//! only floating-point video source in this crate's row.
//!
//! # The formula (measured, not read)
//!
//! Probed at 16×8 for all three `type` values
//! (`ffmpeg -f lavfi -i colorspectrum=size=16x8:type=T -f rawvideo -pix_fmt
//! gbrpf32le -frames:v 1 -`).
//!
//! **The hue wheel** (row `y = 0` of `type=black`, row `y = h-1` of
//! `type=white`): for column `x` in a `w`-wide frame, let
//! `t = x / (w - 1)` and `h6 = t * 6` (six 60°-equivalent segments, standard
//! red→yellow→green→cyan→blue→magenta→red order). `seg = floor(h6)`
//! (clamped to `0..=5`), `frac = h6 - seg`. Every segment ramps one channel
//! by `smoothstep(frac) = 3·frac² - 2·frac³` — **not** linearly in `frac`,
//! confirmed by `smoothstep(0.4) = 0.352` and `smoothstep(0.8) = 0.896`
//! matching measured samples to three decimal places where a linear ramp
//! would have given `0.4` and `0.8`. `x = w - 1` reproduces `x = 0` exactly
//! (a closed loop), confirming the `w - 1` period rather than `w`.
//!
//! **The vertical blend**, `t_y = y / (h - 1)`:
//!
//! ```text
//! type=black: color = hue(x) * (1 - t_y)                          // -> black
//! type=white: color = lerp(white, hue(x), t_y)                    // <- white
//! type=all:   t_y < 0.5  => lerp(white, hue(x), 2*t_y)
//!             t_y >= 0.5 => hue(x) * (2 - 2*t_y)
//! ```
//!
//! `lerp` is plain linear RGB interpolation (not an HSV desaturation, which
//! gives a visibly different curve for non-primary hues) — confirmed by
//! `type=white` matching `white + (hue - white) * t_y` to three decimals at
//! a non-primary hue (`x = 1`, `y = 4`, `h = 8`: predicted `(1, 0.6297,
//! 0.4286)`, measured `(1.0, 0.63, 0.429)`).
//!
//! **Exact** for the hue wheel and all three blend modes, at the precision
//! this crate's tests check (3+ significant decimal digits against the
//! measured references above). Full bit-for-bit float32 equality against the
//! reference's own arithmetic order was not separately re-derived.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::{OptEnum, OptionsExt as _};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const UNLIMITED: VDuration = VDuration::from_micros(-1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "colorspectrum_type", base = "int")]
pub enum SpectrumType {
    #[opt_const(name = "black", help = "fade to black")]
    #[default]
    Black,
    #[opt_const(name = "white", help = "fade to white")]
    White,
    #[opt_const(name = "all", help = "white to black")]
    All,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "colorspectrum", help = "generate colors spectrum")]
pub(crate) struct Opts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
    #[opt(name = "type", help = "set the color spectrum type", unit = "colorspectrum_type", default = SpectrumType::Black, default_repr = "black", flags(filtering))]
    pub kind: SpectrumType,
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
    name: "colorspectrum",
    description: "Generate colors spectrum",
    inputs: &[],
    outputs: &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }],
    flags: FilterFlags::empty(),
};

fn smoothstep(w: f32) -> f32 {
    w * w * (3.0 - 2.0 * w)
}

/// The saturated hue wheel: `(R, G, B)` in `[0, 1]` at fractional position
/// `t` (`0..=1`) around the wheel.
fn hue(t: f32) -> (f32, f32, f32) {
    let h6 = (t * 6.0).clamp(0.0, 6.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "h6 in [0, 6], floor(h6) in 0..=6"
    )]
    let seg = (h6.floor() as u32).min(5);
    let frac = smoothstep(h6 - seg as f32);
    match seg {
        0 => (1.0, frac, 0.0),
        1 => (1.0 - frac, 1.0, 0.0),
        2 => (0.0, 1.0, frac),
        3 => (0.0, 1.0 - frac, 1.0),
        4 => (frac, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - frac),
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `(R, G, B)` for the pixel at `(x, y)` in a `w`×`h` frame, per the module
/// doc's formula.
fn pixel(x: u32, y: u32, w: u32, h: u32, kind: SpectrumType) -> (f32, f32, f32) {
    let tx = if w <= 1 {
        0.0
    } else {
        f64::from(x) / f64::from(w - 1)
    };
    #[allow(clippy::cast_possible_truncation, reason = "tx in [0, 1]")]
    let (r, g, b) = hue(tx as f32);
    let ty = if h <= 1 {
        0.0
    } else {
        f64::from(y) / f64::from(h - 1)
    };
    #[allow(clippy::cast_possible_truncation, reason = "ty in [0, 1]")]
    let ty = ty as f32;
    match kind {
        SpectrumType::Black => {
            let f = 1.0 - ty;
            (r * f, g * f, b * f)
        }
        SpectrumType::White => (lerp(1.0, r, ty), lerp(1.0, g, ty), lerp(1.0, b, ty)),
        SpectrumType::All => {
            if ty < 0.5 {
                let t2 = ty * 2.0;
                (lerp(1.0, r, t2), lerp(1.0, g, t2), lerp(1.0, b, t2))
            } else {
                let f = 2.0 - 2.0 * ty;
                (r * f, g * f, b * f)
            }
        }
    }
}

#[derive(Debug)]
struct Source {
    width: u32,
    height: u32,
    kind: SpectrumType,
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
            .acquire_video(PixFmt::Gbrpf32le, self.width, self.height)?;
        let (w, h) = (self.width, self.height);
        for plane_idx in 0..3usize {
            if let Some(mut plane) = frame.plane_mut(plane_idx) {
                for row_idx in 0..plane.rows() {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "plane.rows() == h, which fits in u32"
                    )]
                    let yy = row_idx as u32;
                    if let Some(row) = plane.row_mut(row_idx) {
                        for (x, px) in row.chunks_exact_mut(4).enumerate() {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "x < w, which fits in u32"
                            )]
                            let xx = x as u32;
                            let (r, g, b) = pixel(xx, yy, w, h, self.kind);
                            // Plane order is G, B, R (the `gbrp` family's own
                            // name spells the layout).
                            let v = match plane_idx {
                                0 => g,
                                1 => b,
                                _ => r,
                            };
                            if let [b0, b1, b2, b3] = px {
                                let bytes = v.to_le_bytes();
                                *b0 = bytes[0];
                                *b1 = bytes[1];
                                *b2 = bytes[2];
                                *b3 = bytes[3];
                            }
                        }
                    }
                }
            }
        }
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

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (width, height) = opts.size;
    let rate = opts.rate.0;
    let total_frames = if opts.duration < VDuration::ZERO {
        None
    } else {
        Some(crate::frame_budget(opts.duration, rate))
    };
    let source = Source {
        width,
        height,
        kind: opts.kind,
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
                pixel_formats: Some(Constraint::Exact(PixFmt::Gbrpf32le)),
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

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 0.001, "{a} != {b}");
    }

    #[test]
    fn hue_wheel_matches_measured_reference_points() {
        // w = 16, period = w - 1 = 15.
        let at = |x: u32| hue(f32::from(x as u16) / 15.0);
        let (r, g, b) = at(0);
        approx(r, 1.0);
        approx(g, 0.0);
        approx(b, 0.0);
        let (r, g, b) = at(2);
        approx(r, 1.0);
        approx(g, 0.896);
        approx(b, 0.0);
        let (r, g, b) = at(5);
        approx(r, 0.0);
        approx(g, 1.0);
        approx(b, 0.0);
        let (r, g, b) = at(6);
        approx(r, 0.0);
        approx(g, 1.0);
        approx(b, 0.352);
        let (r, g, b) = at(10);
        approx(r, 0.0);
        approx(g, 0.0);
        approx(b, 1.0);
        // The wheel closes: x = w - 1 reproduces x = 0 exactly.
        let (r, g, b) = at(15);
        approx(r, 1.0);
        approx(g, 0.0);
        approx(b, 0.0);
    }

    #[test]
    fn black_type_fades_the_hue_wheel_to_black() {
        let (r, g, b) = pixel(0, 4, 16, 8, SpectrumType::Black);
        approx(r, 3.0 / 7.0);
        approx(g, 0.0);
        approx(b, 0.0);
        let (r, g, b) = pixel(0, 7, 16, 8, SpectrumType::Black);
        approx(r, 0.0);
        approx(g, 0.0);
        approx(b, 0.0);
    }

    #[test]
    fn white_type_fades_from_white_at_a_non_primary_hue() {
        // x = 1, y = 4, h = 8: predicted (1, 0.6297, 0.4286) by linear RGB
        // lerp, which is the value this crate measured against the
        // reference — an HSV desaturation would give a different number.
        let (r, g, b) = pixel(1, 4, 16, 8, SpectrumType::White);
        approx(r, 1.0);
        approx(g, 0.63);
        approx(b, 0.429);
    }

    #[test]
    fn all_type_is_continuous_at_the_midpoint() {
        // Just below and just above t_y = 0.5 must agree in the limit.
        let below = pixel(3, 999, 16, 2000, SpectrumType::All);
        let above = pixel(3, 1000, 16, 2000, SpectrumType::All);
        approx(below.0, above.0);
        approx(below.1, above.1);
        approx(below.2, above.2);
    }

    #[test]
    fn creatable_with_no_arguments() {
        let req = Instantiate {
            name: "colorspectrum",
            instance: "colorspectrum",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn type_option_parses() {
        let req = Instantiate {
            name: "colorspectrum",
            instance: "colorspectrum",
            args: Some("type=all"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
