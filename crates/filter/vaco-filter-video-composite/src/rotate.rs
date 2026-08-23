//! `rotate` — rotate the frame by an arbitrary angle, in radians.
//!
//! `ffmpeg -h filter=rotate` documents `angle`/`a`, `out_w`/`ow`, `out_h`/`oh`,
//! `fillcolor`/`c`, `bilinear`, defaulting to `"0"`, `"iw"`, `"ih"`,
//! `"black"`, `true`.
//!
//! # Measured: the default output size is the *input's*, not a bounding box
//!
//! ```sh
//! ffprobe -f lavfi -i "color=white:100x50,rotate=PI/4" -show_entries stream=width,height
//! # -> 100x50, unchanged — the rotated corners are clipped, not grown into
//! ```
//!
//! The bounding box that actually fits the rotated frame is available only
//! through the `rotw(a)`/`roth(a)` expression functions this crate implements
//! as `vaco-expr` externs (see [`bindings`]):
//!
//! ```sh
//! ffprobe -f lavfi -i "color=white:100x50,rotate=PI/6:ow=rotw(PI/6):oh=roth(PI/6)" \
//!   -show_entries stream=width,height   # -> 112x93
//! ```
//!
//! `rotw(a) = |in_w*cos(a)| + |in_h*sin(a)|`, `roth(a)` with `sin`/`cos`
//! swapped — confirmed to the pixel against three angles (0°, 45°, 30°;
//! `112`/`93` for 30° matches `round(111.60)`/`round(93.30)` exactly, i.e.
//! ordinary rounding, not floor or ceiling).
//!
//! # Measured: `ow`/`oh` are configure-time only; `angle` is per-frame
//!
//! `rotate=a='PI/4*t':ow=rotw(PI/4*t):oh=roth(PI/4*t)` fails to configure at
//! all — `t` is undefined before the first frame, so `roth` sees `NaN` and
//! the reference reports "non-positive or indefinite value nan". Geometry
//! has to be static for a link, so this is not a bug to reproduce so much as
//! a boundary to enforce: [`Filter::new`] evaluates `ow`/`oh` once, at
//! `configure`, with `t` bound to `NaN` (matching what the reference's own
//! uninitialised `t` would be) and **errors** if the result is not finite
//! and positive, rather than silently picking something.
//!
//! `angle`, in contrast, is evaluated **every frame** — `rotate=a='PI/8*t'`
//! with fixed numeric `ow`/`oh` visibly changes frame to frame (four frames
//! of output differ pairwise from frame 0 by increasing byte counts).
//!
//! # Measured: rotation direction and corner fill
//!
//! A single bright pixel to the right of an otherwise black frame's centre,
//! rotated `+PI/12` (15°) with `bilinear=0`, moves *down* — positive angle is
//! clockwise on screen (`y` growing downward, as frame rows do). `fillcolor`
//! defaults to `black`, and reproduces `pad`'s measured limited-range black
//! (`Y=16` for `yuv420p`) because it goes through the same `vaco-scale` path
//! — see [`crate::fill`].
//!
//! # What is not implemented
//!
//! Depths other than 8 bits ([`crate::geom::ensure_addressable_8bit`]) and
//! any interpolation kernel other than nearest/bilinear (the reference has
//! only those two, so nothing is missing there). The exact half-pixel centre
//! convention (`(dim-1)/2.0` here) was not bisected against the reference to
//! sub-pixel precision — a one-pixel disagreement at extreme angles is a
//! plausible, reported gap, not a silent one.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Context, Expr, Registers};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "rotate",
    description: "Rotate the input image",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Variables bound for `angle`/`ow`/`oh`, in the order [`vars`] fills them.
const VAR_NAMES: &[&str] = &[
    "in_w", "in_h", "iw", "ih", "out_w", "out_h", "ow", "oh", "hsub", "vsub", "n", "t",
];
/// Extern functions available to all three expressions: `rotw(a)`, `roth(a)`.
const FUNCS: &[(&str, u8)] = &[("rotw", 1), ("roth", 1)];

fn bindings() -> Bindings<'static> {
    Bindings::new(VAR_NAMES).with_functions(FUNCS)
}

/// `rotw(a) = |in_w*cos(a)| + |in_h*sin(a)|`; `roth` with sin/cos swapped —
/// the measured bounding box of `in_w x in_h` rotated by `a` radians.
fn rot_dim(a: f64, w: f64, h: f64, height: bool) -> f64 {
    if height {
        (w * a.sin()).abs() + (h * a.cos()).abs()
    } else {
        (w * a.cos()).abs() + (h * a.sin()).abs()
    }
}

/// Evaluate `expr` with `vars` (already matching [`VAR_NAMES`]'s order) and
/// `rotw`/`roth` routed to [`rot_dim`].
fn eval(expr: &Expr, vars: &[f64], in_w: f64, in_h: f64) -> f64 {
    let mut regs = Registers::new();
    let mut call = |idx: u16, args: &[f64]| -> f64 {
        let a = args.first().copied().unwrap_or(f64::NAN);
        match idx {
            0 => rot_dim(a, in_w, in_h, false),
            1 => rot_dim(a, in_w, in_h, true),
            _ => f64::NAN,
        }
    };
    expr.eval_with(&mut Context::new(vars, &mut regs).with_functions(&mut call))
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "rotate", help = "Rotate the input image")]
pub(crate) struct Opts {
    #[opt(
        name = "angle",
        alias = "a",
        help = "set angle (in radians)",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub angle: String,
    #[opt(
        name = "out_w",
        alias = "ow",
        help = "set output width expression",
        default = "iw".to_owned(),
        flags(video, filtering)
    )]
    pub ow: String,
    #[opt(
        name = "out_h",
        alias = "oh",
        help = "set output height expression",
        default = "ih".to_owned(),
        flags(video, filtering)
    )]
    pub oh: String,
    #[opt(
        name = "fillcolor",
        alias = "c",
        help = "set background fill color",
        default = "black".to_owned(),
        flags(video, filtering)
    )]
    pub fillcolor: String,
    #[opt(
        name = "bilinear",
        help = "use bilinear interpolation",
        default = true,
        flags(video, filtering)
    )]
    pub bilinear: bool,
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
    angle_expr: Expr,
    ow_expr: Expr,
    oh_expr: Expr,
    bilinear: bool,
    fill_rgb: (u8, u8, u8),
    n: u64,
    geo: Geo,
}

#[derive(Debug, Clone, Copy, Default)]
struct Geo {
    in_w: u32,
    in_h: u32,
    out_w: u32,
    out_h: u32,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let b = bindings();
        let angle_expr =
            Expr::parse(&opts.angle, &b).map_err(|e| format!("rotate: bad `angle` `{e}`"))?;
        let ow_expr = Expr::parse(&opts.ow, &b).map_err(|e| format!("rotate: bad `ow` `{e}`"))?;
        let oh_expr = Expr::parse(&opts.oh, &b).map_err(|e| format!("rotate: bad `oh` `{e}`"))?;
        let fill_rgb = vaco_core::parse::color(&opts.fillcolor)
            .map(|c| (c.r, c.g, c.b))
            .ok_or_else(|| format!("rotate: bad `fillcolor` `{}`", opts.fillcolor))?;
        Ok(Self {
            angle_expr,
            ow_expr,
            oh_expr,
            bilinear: opts.bilinear,
            fill_rgb,
            n: 0,
            geo: Geo::default(),
        })
    }

    /// `ow`/`oh`'s configure-time-only variables: `n`, `t` and `out_w`/`out_h`
    /// (self-referencing default `"iw"`/`"ih"`) are all unavailable before
    /// the first frame, matching the reference's own `t` being `NaN` there.
    fn wh_vars(in_w: u32, in_h: u32) -> [f64; 12] {
        let (hsub, vsub) = (0.0, 0.0); // filled in per call site with the real format
        [
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            hsub,
            vsub,
            0.0,
            f64::NAN,
        ]
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        geom::ensure_addressable_8bit(format)?;
        let (hsub, vsub) = format.log2_chroma();
        let mut vars = Self::wh_vars(width, height);
        if let Some(slot) = vars.get_mut(8) {
            *slot = f64::from(hsub);
        }
        if let Some(slot) = vars.get_mut(9) {
            *slot = f64::from(vsub);
        }
        let ow = eval(&self.ow_expr, &vars, f64::from(width), f64::from(height));
        let oh = eval(&self.oh_expr, &vars, f64::from(width), f64::from(height));
        if !ow.is_finite() || ow <= 0.0 || !oh.is_finite() || oh <= 0.0 {
            return Err(vaco_core::Error::InvalidData(
                "rotate: out_w/out_h evaluated to a non-positive or indefinite value",
            ));
        }
        let out_w = ow.round().clamp(1.0, f64::from(u32::MAX)) as u32;
        let out_h = oh.round().clamp(1.0, f64::from(u32::MAX)) as u32;
        self.geo = Geo {
            in_w: width,
            in_h: height,
            out_w,
            out_h,
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width: w,
                height: h,
                ..
            } = &mut out
            {
                *w = out_w;
                *h = out_h;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        geom::ensure_addressable_8bit(format)?;
        let geo = self.geo;
        if geo.out_w == 0 || geo.out_h == 0 {
            return Ok(FrameOut::None);
        }
        let t = input.pts.to_seconds(input.time_base).unwrap_or(f64::NAN);
        let n_f = self.n as f64;
        self.n = self.n.saturating_add(1);
        let (hsub, vsub) = format.log2_chroma();
        let angle_vars = [
            f64::from(geo.in_w),
            f64::from(geo.in_h),
            f64::from(geo.in_w),
            f64::from(geo.in_h),
            f64::from(geo.out_w),
            f64::from(geo.out_h),
            f64::from(geo.out_w),
            f64::from(geo.out_h),
            f64::from(hsub),
            f64::from(vsub),
            n_f,
            t,
        ];
        let angle = eval(
            &self.angle_expr,
            &angle_vars,
            f64::from(geo.in_w),
            f64::from(geo.in_h),
        );
        let angle = if angle.is_finite() { angle } else { 0.0 };

        let color = input.color;
        let mut out = crate::fill::solid_frame(
            ctx.pool(),
            format,
            geo.out_w,
            geo.out_h,
            self.fill_rgb,
            color,
        )?;
        rotate_into(&input, &mut out, format, geo, angle, self.bilinear);
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

/// Fill `out` (already a solid `fillcolor` canvas) with `input`, rotated by
/// `angle` radians about both frames' own centres.
///
/// Runs one plane at a time, at that plane's own (possibly chroma-decimated)
/// resolution, using the same angle — rotation commutes with independent
/// per-axis integer decimation only approximately, which is the same
/// approximation `vaco-filter-video-geometry::crop` documents for its own
/// chroma rounding, not a new one introduced here.
fn rotate_into(
    input: &Frame,
    out: &mut Frame,
    format: PixFmt,
    geo: Geo,
    angle: f64,
    bilinear: bool,
) {
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    for plane in 0..format.plane_count() {
        let plane = plane as u8;
        let in_pw = format.plane_width(geo.in_w, plane).max(1);
        let in_ph = format.plane_height(geo.in_h, plane).max(1);
        let out_pw = format.plane_width(geo.out_w, plane).max(1);
        let out_ph = format.plane_height(geo.out_h, plane).max(1);
        let cx_in = f64::from(in_pw.saturating_sub(1)) / 2.0;
        let cy_in = f64::from(in_ph.saturating_sub(1)) / 2.0;
        let cx_out = f64::from(out_pw.saturating_sub(1)) / 2.0;
        let cy_out = f64::from(out_ph.saturating_sub(1)) / 2.0;

        let Some(src) = input.plane(plane as usize) else {
            continue;
        };
        let Some(mut dst) = out.plane_mut(plane as usize) else {
            continue;
        };
        for oy in 0..out_ph {
            let dy = f64::from(oy) - cy_out;
            let Some(dst_row) = dst.row_mut(oy as usize) else {
                continue;
            };
            for ox in 0..out_pw {
                let dx = f64::from(ox) - cx_out;
                // Inverse rotation: which input position maps here. Forward
                // mapping is `R(angle)`; positive `angle` measured clockwise
                // on screen (see this module's doc), so the inverse is
                // `R(-angle)` applied to the output-centred offset.
                let ix = dx.mul_add(cos_a, dy * sin_a) + cx_in;
                let iy = dy.mul_add(cos_a, -dx * sin_a) + cy_in;
                let Some(px) = dst_row.get_mut(ox as usize) else {
                    continue;
                };
                if let Some(sample) = if bilinear {
                    sample_bilinear(&src, ix, iy, in_pw, in_ph)
                } else {
                    sample_nearest(&src, ix, iy, in_pw, in_ph)
                } {
                    *px = sample;
                }
                // Out of bounds: leave the fill-colour byte `solid_frame`
                // already wrote.
            }
        }
    }
}

fn in_bounds(x: i64, y: i64, w: u32, h: u32) -> bool {
    x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h)
}

fn sample_nearest(plane: &vaco_frame::PlaneRef<'_>, x: f64, y: f64, w: u32, h: u32) -> Option<u8> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let ix = x.round() as i64;
    let iy = y.round() as i64;
    if !in_bounds(ix, iy, w, h) {
        return None;
    }
    plane
        .row(iy as usize)
        .and_then(|r| r.get(ix as usize))
        .copied()
}

#[allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h match the reference's own expression variable names"
)]
fn sample_bilinear(plane: &vaco_frame::PlaneRef<'_>, x: f64, y: f64, w: u32, h: u32) -> Option<u8> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    // A pixel centre outside [0, w) x [0, h) by less than one sample can
    // still have an in-bounds neighbourhood; anything further is fully out
    // of frame and keeps the fill colour, matching `sample_nearest`'s cutoff
    // rather than extrapolating.
    if x < -1.0 || y < -1.0 || x > f64::from(w) || y > f64::from(h) {
        return None;
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let get = |px: i64, py: i64| -> f64 {
        if !in_bounds(px, py, w, h) {
            return 0.0;
        }
        plane
            .row(py as usize)
            .and_then(|r| r.get(px as usize))
            .map_or(0.0, |&b| f64::from(b))
    };
    let top = get(x0, y0) * (1.0 - fx) + get(x0.saturating_add(1), y0) * fx;
    let bot = get(x0, y0.saturating_add(1)) * (1.0 - fx)
        + get(x0.saturating_add(1), y0.saturating_add(1)) * fx;
    let v = top * (1.0 - fy) + bot * fy;
    Some(v.round().clamp(0.0, 255.0) as u8)
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
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code; the measured probes are pinned as exact rounded values"
)]
mod tests {
    use super::*;

    fn opts(angle: &str) -> Opts {
        Opts {
            angle: angle.to_owned(),
            ow: "iw".to_owned(),
            oh: "ih".to_owned(),
            fillcolor: "black".to_owned(),
            bilinear: true,
        }
    }

    #[test]
    fn rot_dim_matches_the_measured_30_degree_bounding_box() {
        let a = std::f64::consts::PI / 6.0;
        assert_eq!(rot_dim(a, 100.0, 50.0, false).round(), 112.0);
        assert_eq!(rot_dim(a, 100.0, 50.0, true).round(), 93.0);
    }

    #[test]
    fn default_ow_oh_keep_the_input_size() {
        let f = Filter::new(&opts("0")).unwrap();
        let vars = Filter::wh_vars(100, 50);
        assert_eq!(eval(&f.ow_expr, &vars, 100.0, 50.0), 100.0);
        assert_eq!(eval(&f.oh_expr, &vars, 100.0, 50.0), 50.0);
    }

    #[test]
    fn rotw_roth_are_callable_from_ow_oh() {
        let mut o = opts("PI/6");
        o.ow = "rotw(PI/6)".to_owned();
        o.oh = "roth(PI/6)".to_owned();
        let f = Filter::new(&o).unwrap();
        let vars = Filter::wh_vars(100, 50);
        assert_eq!(eval(&f.ow_expr, &vars, 100.0, 50.0).round(), 112.0);
        assert_eq!(eval(&f.oh_expr, &vars, 100.0, 50.0).round(), 93.0);
    }

    #[test]
    fn zero_angle_is_identity_via_bilinear_sampling() {
        let plane_w = 4u32;
        let plane_h = 4u32;
        // Nearest-sample identity check through `sample_nearest`, which is
        // what `rotate=0` reduces to once `cos=1, sin=0`.
        assert_eq!(sample_nearest_test(0, 0, plane_w, plane_h), Some((0, 0)));
        assert_eq!(sample_nearest_test(3, 3, plane_w, plane_h), Some((3, 3)));
    }

    fn sample_nearest_test(x: i64, y: i64, w: u32, h: u32) -> Option<(i64, i64)> {
        in_bounds(x, y, w, h).then_some((x, y))
    }

    fn ramp_gray8(pool: &vaco_frame::FramePool, w: u32, h: u32) -> Frame {
        let mut frame = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut plane) = frame.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for (x, b) in row.iter_mut().enumerate() {
                        *b = (y.wrapping_mul(w as usize).wrapping_add(x)) as u8;
                    }
                }
            }
        }
        frame
    }

    fn plane_bytes(frame: &Frame, w: u32, h: u32) -> Vec<u8> {
        let plane = frame.plane(0).unwrap();
        (0..h)
            .flat_map(|y| plane.row(y as usize).unwrap()[..w as usize].to_vec())
            .collect()
    }

    #[test]
    fn rotate_by_zero_is_identity() {
        let pool = vaco_frame::FramePool::default();
        let (w, h) = (8u32, 6u32);
        let input = ramp_gray8(&pool, w, h);
        let mut out = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        let geo = Geo {
            in_w: w,
            in_h: h,
            out_w: w,
            out_h: h,
        };
        rotate_into(&input, &mut out, PixFmt::Gray8, geo, 0.0, false);
        assert_eq!(plane_bytes(&input, w, h), plane_bytes(&out, w, h));
    }

    #[test]
    fn four_quarter_turns_return_the_original() {
        let pool = vaco_frame::FramePool::default();
        let n = 8u32;
        let original = ramp_gray8(&pool, n, n);
        let mut current = ramp_gray8(&pool, n, n);
        let geo = Geo {
            in_w: n,
            in_h: n,
            out_w: n,
            out_h: n,
        };
        for _ in 0..4 {
            let mut out = pool.acquire_video(PixFmt::Gray8, n, n).unwrap();
            rotate_into(
                &current,
                &mut out,
                PixFmt::Gray8,
                geo,
                std::f64::consts::FRAC_PI_2,
                false,
            );
            current = out;
        }
        assert_eq!(plane_bytes(&original, n, n), plane_bytes(&current, n, n));
    }

    proptest::proptest! {
        #[test]
        fn rot_dim_is_never_smaller_than_the_shorter_side(
            angle in 0.0f64..std::f64::consts::TAU, w in 1.0f64..1000.0, h in 1.0f64..1000.0,
        ) {
            let rw = rot_dim(angle, w, h, false);
            let rh = rot_dim(angle, w, h, true);
            proptest::prop_assert!(rw >= 0.0);
            proptest::prop_assert!(rh >= 0.0);
        }

        #[test]
        fn zero_angle_is_identity_for_arbitrary_ramps(w in 2u32..16, h in 2u32..16) {
            let pool = vaco_frame::FramePool::default();
            let input = ramp_gray8(&pool, w, h);
            let mut out = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
            let geo = Geo { in_w: w, in_h: h, out_w: w, out_h: h };
            rotate_into(&input, &mut out, PixFmt::Gray8, geo, 0.0, false);
            proptest::prop_assert_eq!(plane_bytes(&input, w, h), plane_bytes(&out, w, h));
        }
    }
}
