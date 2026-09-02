//! `vignette` — darken (or, in `mode=backward`, brighten) the frame radially
//! from its centre, following the classic optical `cos^4` falloff law.
//!
//! `ffmpeg -h filter=vignette` (2026-08-28): `angle`/`a` (expression, default
//! `"PI/5"`), `x0` (expression, default `"w/2"`), `y0` (expression, default
//! `"h/2"`), `mode` (`0` forward / `1` backward, default forward), `eval`
//! (`0` init / `1` frame, default init), `dither` (bool, default **true**
//! in the reference; this crate's own default is `false` — see `Opts::dither`'s
//! own doc for why), `aspect` (rational, default `1/1`). Timeline-capable
//! (`This filter has support for timeline through the 'enable' option`).
//!
//! # Measured: the exact formula (`ffmpeg 8.1`, tiny `gray`/`yuv420p`
//! sources through `-f rawvideo`, `dither=0` to remove the jitter below)
//!
//! ```text
//! ffmpeg -bitexact -f lavfi -i "color=gray:s=8x8:d=1:r=1" \
//!   -vf "format=gray,vignette=dither=0" -f rawvideo -pix_fmt gray -
//! ```
//!
//! An 8x8 uniform-128 frame produced `[54,66,76,82,85,82,76,66; ...]`
//! (symmetric bowl peaking at 128 in the centre). A parameter sweep over
//! pixel-centre convention (`x`/`y` integer vs `+0.5`), the distance
//! normalisation, and the rounding mode found exactly one combination with
//! zero error over the whole 8x8 grid: integer pixel coordinates (no
//! half-pixel offset), `max_dist = sqrt(x0^2 + y0^2)` (the *unscaled*
//! distance from centre to the frame's own corner), and C-style truncation
//! (`as i32`, not `round`):
//!
//! ```text
//! dx = x - x0;  dy = y - y0
//! dist = sqrt(dx*dx + dy*dy)
//! theta = angle * dist / max_dist
//! factor = theta < PI/2 ? cos(theta)^4 : 0
//! out = trunc(in * factor)                      // mode=forward
//! ```
//!
//! Re-confirmed on a 16x8 frame (non-square, so `x0 != y0`) with zero error.
//!
//! # Measured: `mode=backward` divides instead of multiplying
//!
//! Same 16x8 probe with `mode=backward`: `out = clamp(in / factor, 0, 255)`,
//! with `factor == 0` (i.e. `theta >= PI/2`) reading as "divide by zero,
//! clip to 255" rather than a special case — zero error over the whole grid
//! with that one rule, matching the forward path's `factor == 0 -> 0` by
//! the same "the clip absorbs it" logic.
//!
//! # Measured: `aspect` scales the *vertical* distance, exactly, in the interior
//!
//! `aspect=2` on the same 16x8 probe: `dy` in the formula above becomes
//! `(y - y0) * aspect` (`max_dist` itself is **not** rescaled — still
//! `sqrt(x0^2 + y0^2)` from the `aspect=1` case). This reproduced every
//! interior pixel exactly. It did **not** reproduce the reference's harder
//! clipping right at the frame's extreme corners (a handful of pixels near
//! `y=0`/`y=h-1` that the reference zeroes and this formula does not) — a
//! parameter search over plausible variants (scaling `dx` instead, rescaling
//! `max_dist` by `aspect` in either direction, both scaled) did not find a
//! single rule matching both the interior *and* the corners, and this pass
//! stopped chasing it rather than ship a second guess. Recorded as a gap:
//! `aspect != 1` is framecrc-exact away from the frame's extreme corners
//! only.
//!
//! # Not measured: chroma planes and `dither=1`
//!
//! A `yuv420p` probe confirms chroma is *not* multiplied directly — it is
//! scaled around the neutral point `128` (`out = 128 + (in-128)*factor`),
//! which is what this module implements — but computing the chroma plane's
//! own `x0`/`y0`/`max_dist` from its own (subsampled) dimensions left a
//! residual of about 1 count against the reference in the one probe run, and
//! this pass did not track down the exact sampling grid the reference uses
//! for chroma before its time budget ran out. `dither=1` (**the default**)
//! reproducibly (same output across repeated runs, despite `vignette` having
//! no `seed` option at all) perturbs the truncated output by up to ±1 per
//! pixel; this module does not attempt to reproduce that generator, matching
//! this crate's `noise` and `vaco-filter-temporal::random`'s "algorithmically
//! faithful, not bit-exact" precedent for exactly this shape of problem.
//! `dither=0` is unaffected and is this filter's framecrc-verified path.

use std::f64::consts::PI;

use vaco_core::Result;
use vaco_core::{MediaType, Rational};
use vaco_expr::{Bindings, Expr};
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
    name: "vignette",
    description: "Make or reverse a vignette effect",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

const WH_VARS: &[&str] = &["w", "h"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Forward,
    Backward,
}

impl Mode {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "forward" | "0" => Some(Self::Forward),
            "backward" | "1" => Some(Self::Backward),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eval {
    Init,
    Frame,
}

impl Eval {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "init" | "0" => Some(Self::Init),
            "frame" | "1" => Some(Self::Frame),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "vignette", help = "Make or reverse a vignette effect")]
pub(crate) struct Opts {
    #[opt(
        name = "angle",
        alias = "a",
        help = "set lens angle",
        default = "PI/5".to_owned(),
        flags(video, filtering)
    )]
    pub angle: String,
    #[opt(
        name = "x0",
        help = "set circle center position on x-axis",
        default = "w/2".to_owned(),
        flags(video, filtering)
    )]
    pub x0: String,
    #[opt(
        name = "y0",
        help = "set circle center position on y-axis",
        default = "h/2".to_owned(),
        flags(video, filtering)
    )]
    pub y0: String,
    #[opt(
        name = "mode",
        help = "set forward/backward mode",
        default = "forward".to_owned(),
        flags(video, filtering)
    )]
    pub mode: String,
    #[opt(
        name = "eval",
        help = "specify when to evaluate expressions",
        default = "init".to_owned(),
        flags(video, filtering)
    )]
    pub eval: String,
    // Measured default divergence, stated plainly: the reference's own
    // default is `true`, but this crate's overlap-add path always produces
    // `dither=0`'s output regardless of what this field holds -- see the
    // module doc's "Not measured: chroma planes and `dither=1`" section.
    // Declaring the default as `false` describes what the code actually,
    // unconditionally does; requesting the reference's real default
    // (`dither=1`) now refuses instead of silently returning `dither=0`'s
    // pixels while claiming to have honoured `dither=1`.
    #[opt(
        name = "dither",
        help = "set dithering",
        default = false,
        flags(video, filtering)
    )]
    pub dither: bool,
    #[opt(
        name = "aspect",
        help = "set aspect ratio",
        default = "1/1".to_owned(),
        flags(video, filtering)
    )]
    pub aspect: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.dither {
            return Err("vignette: `dither` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug, Clone, Copy)]
struct Params {
    x0: f64,
    y0: f64,
    max_dist: f64,
    angle: f64,
}

fn eval_params(
    angle_expr: &Expr,
    x0_expr: &Expr,
    y0_expr: &Expr,
    width: u32,
    height: u32,
) -> Params {
    let vars = [f64::from(width), f64::from(height)];
    let x0 = x0_expr.eval(&vars);
    let y0 = y0_expr.eval(&vars);
    Params {
        x0,
        y0,
        max_dist: (x0 * x0 + y0 * y0).sqrt(),
        angle: angle_expr.eval(&vars),
    }
}

/// Darkening factor at one pixel: `cos(theta)^4`, clipped to `0` once
/// `theta` reaches a right angle. See this module's doc for the probe that
/// pinned down every term, including `max_dist` staying the *unscaled*
/// corner distance even when `aspect != 1`.
fn factor_at(params: Params, x: i32, y: i32, aspect: f64) -> f64 {
    let dx = f64::from(x) - params.x0;
    let dy = (f64::from(y) - params.y0) * aspect;
    let dist = dx.hypot(dy);
    if params.max_dist <= 0.0 {
        return 1.0;
    }
    let theta = params.angle * dist / params.max_dist;
    if theta < PI / 2.0 {
        theta.cos().powi(4)
    } else {
        0.0
    }
}

/// `out = baseline + (in-baseline)*factor` (forward) or
/// `baseline + (in-baseline)/factor`, clamped, (backward). `baseline` is
/// `128.0` for a chroma plane, `0.0` for luma/RGB/alpha.
fn apply_forward(value: u8, factor: f64, baseline: f64) -> u8 {
    let raw = baseline + (f64::from(value) - baseline) * factor;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "raw is finite and within 0..255 by construction of factor"
    )]
    let trunc = raw as i32;
    trunc.clamp(0, 255) as u8
}

fn apply_backward(value: u8, factor: f64, baseline: f64) -> u8 {
    if factor <= 1e-9 {
        let v = f64::from(value);
        return if v >= baseline { 255 } else { 0 };
    }
    let raw = baseline + (f64::from(value) - baseline) / factor;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "raw is finite; clamp below bounds it to 0..255"
    )]
    let trunc = raw as i32;
    trunc.clamp(0, 255) as u8
}

#[derive(Debug)]
pub(crate) struct Filter {
    angle_expr: Expr,
    x0_expr: Expr,
    y0_expr: Expr,
    mode: Mode,
    eval: Eval,
    aspect: f64,
    params: Option<Params>,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(WH_VARS);
        let angle_expr = Expr::parse(&opts.angle, &bindings)
            .map_err(|e| format!("vignette: bad `angle` `{e}`"))?;
        let x0_expr =
            Expr::parse(&opts.x0, &bindings).map_err(|e| format!("vignette: bad `x0` `{e}`"))?;
        let y0_expr =
            Expr::parse(&opts.y0, &bindings).map_err(|e| format!("vignette: bad `y0` `{e}`"))?;
        let mode = Mode::from_name(&opts.mode)
            .ok_or_else(|| format!("vignette: bad `mode` `{}`", opts.mode))?;
        let eval = Eval::from_name(&opts.eval)
            .ok_or_else(|| format!("vignette: bad `eval` `{}`", opts.eval))?;
        let aspect_ratio: Rational = vaco_core::parse::rational(&opts.aspect)
            .ok_or_else(|| format!("vignette: bad `aspect` `{}`", opts.aspect))?;
        if aspect_ratio.den == 0 {
            return Err("vignette: `aspect` denominator is zero".to_owned());
        }
        Ok(Self {
            angle_expr,
            x0_expr,
            y0_expr,
            mode,
            eval,
            aspect: f64::from(aspect_ratio.num) / f64::from(aspect_ratio.den),
            params: None,
        })
    }

    fn params_for(&mut self, width: u32, height: u32) -> Params {
        if self.eval == Eval::Frame || self.params.is_none() {
            self.params = Some(eval_params(
                &self.angle_expr,
                &self.x0_expr,
                &self.y0_expr,
                width,
                height,
            ));
        }
        #[allow(
            clippy::unwrap_used,
            reason = "just assigned above whenever it was None"
        )]
        self.params.unwrap()
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() {
            return Ok(FrameOut::One(input));
        }
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let params = self.params_for(width, height);
        let is_rgb = format.is_rgb();
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let baseline = if p == 0 || is_rgb || p >= 3 {
                0.0
            } else {
                128.0
            };
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            let plane_x0 = params.x0 * f64::from(pw) / f64::from(common::to_i32(width).max(1));
            let plane_y0 = params.y0 * f64::from(ph) / f64::from(common::to_i32(height).max(1));
            let plane_max_dist = if p == 0 {
                params.max_dist
            } else {
                (plane_x0 * plane_x0 + plane_y0 * plane_y0).sqrt()
            };
            let plane_params = Params {
                x0: plane_x0,
                y0: plane_y0,
                max_dist: plane_max_dist,
                angle: params.angle,
            };
            for y in 0..ph {
                let Ok(uy) = usize::try_from(y) else { continue };
                let Some(src_row) = src_plane.row(uy) else {
                    continue;
                };
                let Some(dst_row) = dst_plane.row_mut(uy) else {
                    continue;
                };
                let n = dst_row.len().min(src_row.len());
                for x in 0..n {
                    let xi = common::to_i32(x);
                    if xi >= pw {
                        break;
                    }
                    let factor = factor_at(plane_params, xi, y, self.aspect);
                    let Some(src) = src_row.get(x) else { continue };
                    let Some(dst) = dst_row.get_mut(x) else {
                        continue;
                    };
                    *dst = match self.mode {
                        Mode::Forward => apply_forward(*src, factor, baseline),
                        Mode::Backward => apply_backward(*src, factor, baseline),
                    };
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
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

    /// Pinned against the reference probe in this module's doc: an 8x8
    /// uniform-128 gray frame, default angle/x0/y0, `dither=0`.
    #[test]
    fn matches_the_measured_8x8_forward_grid() {
        let bindings = Bindings::new(WH_VARS);
        let angle = Expr::parse("PI/5", &bindings).unwrap();
        let x0e = Expr::parse("w/2", &bindings).unwrap();
        let y0e = Expr::parse("h/2", &bindings).unwrap();
        let params = eval_params(&angle, &x0e, &y0e, 8, 8);
        let expected = [
            [54, 66, 76, 82, 85, 82, 76, 66],
            [66, 80, 92, 99, 102, 99, 92, 80],
            [76, 92, 104, 112, 115, 112, 104, 92],
            [82, 99, 112, 121, 124, 121, 112, 99],
            [85, 102, 115, 124, 128, 124, 115, 102],
            [82, 99, 112, 121, 124, 121, 112, 99],
            [76, 92, 104, 112, 115, 112, 104, 92],
            [66, 80, 92, 99, 102, 99, 92, 80],
        ];
        for y in 0..8i32 {
            for x in 0..8i32 {
                let f = factor_at(params, x, y, 1.0);
                let out = apply_forward(128, f, 0.0);
                assert_eq!(out, expected[y as usize][x as usize], "({x},{y})");
            }
        }
    }

    /// Pinned against the reference's `mode=backward` probe on the 16x8 grid.
    #[test]
    fn matches_the_measured_16x8_backward_grid() {
        let bindings = Bindings::new(WH_VARS);
        let angle = Expr::parse("PI/5", &bindings).unwrap();
        let x0e = Expr::parse("w/2", &bindings).unwrap();
        let y0e = Expr::parse("h/2", &bindings).unwrap();
        let params = eval_params(&angle, &x0e, &y0e, 16, 8);
        let expected_row0 = [
            255, 252, 218, 194, 177, 164, 156, 151, 150, 151, 156, 164, 177, 194, 218, 252,
        ];
        for x in 0..16i32 {
            let f = factor_at(params, x, 0, 1.0);
            let out = apply_backward(128, f, 0.0);
            assert_eq!(out, expected_row0[x as usize], "x={x}");
        }
    }

    #[test]
    fn center_pixel_is_always_unattenuated() {
        let bindings = Bindings::new(WH_VARS);
        let angle = Expr::parse("PI/5", &bindings).unwrap();
        let x0e = Expr::parse("w/2", &bindings).unwrap();
        let y0e = Expr::parse("h/2", &bindings).unwrap();
        let params = eval_params(&angle, &x0e, &y0e, 8, 8);
        let f = factor_at(params, 4, 4, 1.0);
        assert!((f - 1.0).abs() < 1e-9);
    }

    #[test]
    fn bad_mode_is_a_clean_error() {
        let opts = Opts {
            mode: "sideways".to_owned(),
            ..Opts::default()
        };
        assert!(Filter::new(&opts).is_err());
    }

    #[test]
    fn zero_aspect_denominator_is_a_clean_error() {
        let opts = Opts {
            aspect: "1/0".to_owned(),
            ..Opts::default()
        };
        assert!(Filter::new(&opts).is_err());
    }

    proptest::proptest! {
        /// Invariant, not a re-derivation of `apply_forward`: `factor` is
        /// always in `0..=1` by construction (a clipped `cos^4`), so
        /// forward mode can only darken (or leave unchanged) and never
        /// brighten past the original sample.
        #[test]
        fn forward_never_brightens(value in 0u8..=255, factor in 0.0f64..=1.0) {
            proptest::prop_assert!(apply_forward(value, factor, 0.0) <= value);
        }

        /// Mirror invariant: backward mode divides by a factor in `0..=1`,
        /// so it can only brighten (or leave unchanged), never darken.
        #[test]
        fn backward_never_darkens(value in 0u8..=255, factor in 0.01f64..=1.0) {
            proptest::prop_assert!(apply_backward(value, factor, 0.0) >= value);
        }

        /// Whatever `angle`/`x0`/`y0` evaluate to, a pixel exactly at the
        /// computed centre is always at `theta = 0`, hence `factor = 1` —
        /// true for any centre placement, not just the default `w/2,h/2`.
        #[test]
        fn factor_at_the_center_is_always_one(x0 in 1i32..500, y0 in 1i32..500) {
            let params = Params {
                x0: f64::from(x0),
                y0: f64::from(y0),
                max_dist: (f64::from(x0 * x0) + f64::from(y0 * y0)).sqrt(),
                angle: PI / 5.0,
            };
            let f = factor_at(params, x0, y0, 1.0);
            proptest::prop_assert!((f - 1.0).abs() < 1e-6);
        }
    }
}
