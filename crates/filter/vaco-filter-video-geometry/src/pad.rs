//! `pad` — place the input on a larger, colour-filled canvas.
//!
//! `ffmpeg -h filter=pad` documents `width`/`w`, `height`/`h`, `x`, `y`,
//! `color`, `eval` and `aspect`, defaulting to `"iw"`, `"ih"`, `"0"`, `"0"`,
//! `"black"`, `init`, `0/1`. Implemented: `w`/`h`/`x`/`y` as `vaco-expr`
//! expressions evaluated once at `configure`, and `color`. Not implemented:
//! `eval=frame` (geometry is fixed for the stream, matching this crate's
//! `crop` and `scale`) and `aspect` (pad-to-a-ratio; not measured).
//!
//! # Measured: the default fill is *limited-range* black, not zero
//!
//! See [`crate::fill`]'s doc for the probe. `pad`'s own contribution is
//! routing its `color` option through that helper rather than writing `0`
//! into the luma plane directly, which is what a first pass over this filter
//! would do and get wrong for every YUV destination.
//!
//! # Placement rounding
//!
//! Not separately measured against the reference; by symmetry with `crop`'s
//! measured behaviour (this module's sibling), the input's placement offset
//! is floored to the destination format's chroma factor on each axis, so
//! that both the border fill and the copied input stay aligned to whole
//! chroma blocks. If the reference does something else for `pad` specifically
//! this is the divergence to look at first.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::{fill, geom};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "pad",
    description: "Pad the input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const WH_VARS: &[&str] = &["in_w", "in_h", "iw", "ih", "a", "sar", "hsub", "vsub"];
const XY_VARS: &[&str] = &[
    "in_w", "in_h", "iw", "ih", "out_w", "out_h", "ow", "oh", "a", "sar", "hsub", "vsub", "x", "y",
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pad", help = "Pad the input video")]
pub(crate) struct Opts {
    #[opt(
        name = "width",
        alias = "w",
        help = "set the pad area width expression",
        default = "iw".to_owned(),
        flags(video, filtering)
    )]
    pub w: String,
    #[opt(
        name = "height",
        alias = "h",
        help = "set the pad area height expression",
        default = "ih".to_owned(),
        flags(video, filtering)
    )]
    pub h: String,
    #[opt(
        name = "x",
        help = "set the x offset for the input image position",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub x: String,
    #[opt(
        name = "y",
        help = "set the y offset for the input image position",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub y: String,
    #[opt(
        name = "color",
        help = "set the color of the padded area border",
        default = "black".to_owned(),
        flags(video, filtering)
    )]
    pub color: String,
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

#[derive(Debug, Clone, Copy, Default)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Debug)]
pub(crate) struct Filter {
    w_expr: Expr,
    h_expr: Expr,
    x_expr: Expr,
    y_expr: Expr,
    rgb: (u8, u8, u8),
    rect: Rect,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let wh = Bindings::new(WH_VARS);
        let xy = Bindings::new(XY_VARS);
        let rgba = vaco_core::parse::color(&opts.color)
            .ok_or_else(|| format!("pad: bad `color` `{}`", opts.color))?;
        Ok(Self {
            w_expr: Expr::parse(&opts.w, &wh).map_err(|e| format!("pad: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &wh).map_err(|e| format!("pad: bad `h` `{e}`"))?,
            x_expr: Expr::parse(&opts.x, &xy).map_err(|e| format!("pad: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &xy).map_err(|e| format!("pad: bad `y` `{e}`"))?,
            rgb: (rgba.r, rgba.g, rgba.b),
            rect: Rect::default(),
        })
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "w/h/x/y/a match the reference's own expression variable names"
    )]
    fn compute(&self, format: PixFmt, in_w: u32, in_h: u32, sar: vaco_core::Rational) -> Rect {
        let a = if in_h == 0 {
            0.0
        } else {
            f64::from(in_w) / f64::from(in_h)
        };
        let (hsub, vsub) = format.log2_chroma();
        let whv = [
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            a,
            sar.to_f64(),
            f64::from(hsub),
            f64::from(vsub),
        ];
        let w = round_at_least(self.w_expr.eval(&whv), in_w);
        let h = round_at_least(self.h_expr.eval(&whv), in_h);
        let xyv = [
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            f64::from(w),
            f64::from(h),
            f64::from(w),
            f64::from(h),
            a,
            sar.to_f64(),
            f64::from(hsub),
            f64::from(vsub),
            0.0,
            0.0,
        ];
        let x = clamp_nonneg(self.x_expr.eval(&xyv), w.saturating_sub(in_w));
        let y = clamp_nonneg(self.y_expr.eval(&xyv), h.saturating_sub(in_h));

        let (fx, fy) = geom::subsample_factors(format);
        let x = geom::floor_to_multiple(x, fx);
        let y = geom::floor_to_multiple(y, fy);
        Rect { x, y, w, h }
    }
}

fn round_at_least(value: f64, min: u32) -> u32 {
    if !value.is_finite() || value <= f64::from(min) {
        return min.max(1);
    }
    value.floor() as u32
}

fn clamp_nonneg(value: f64, max: u32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value.floor() as u32).min(max)
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            sample_aspect_ratio,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        geom::ensure_addressable(format)?;
        self.rect = self.compute(format, width, height, sample_aspect_ratio);
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width: w,
                height: h,
                ..
            } = &mut out
            {
                *w = self.rect.w;
                *h = self.rect.h;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let rect = self.rect;
        let mut out = fill::solid_frame(ctx.pool(), format, rect.w, rect.h, self.rgb, input.color)?;

        let in_w = match &input.data {
            FrameData::Video { width, .. } => *width,
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => 0,
        };
        let in_h = match &input.data {
            FrameData::Video { height, .. } => *height,
            FrameData::Audio { .. } | FrameData::Subtitle { .. } => 0,
        };
        let copy_w = in_w.min(rect.w.saturating_sub(rect.x));
        let copy_h = in_h.min(rect.h.saturating_sub(rect.y));

        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let dst_x = format.plane_width(rect.x, plane_idx) as usize;
            let dst_y = format.plane_height(rect.y, plane_idx) as usize;
            let rows = format.plane_height(copy_h, plane_idx) as usize;
            let row_bytes = (format.plane_width(copy_w, plane_idx) as usize).saturating_mul(unit);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for row in 0..rows {
                let Some(src_row) = src_plane.row(row) else {
                    continue;
                };
                let Some(src_slice) = src_row.get(..row_bytes.min(src_row.len())) else {
                    continue;
                };
                if let Some(dst_row) = dst_plane.row_mut(dst_y.saturating_add(row)) {
                    let start = dst_x.saturating_mul(unit);
                    if let Some(dst_slice) = dst_row.get_mut(start..) {
                        let n = dst_slice.len().min(src_slice.len());
                        if let (Some(d), Some(s)) = (dst_slice.get_mut(..n), src_slice.get(..n)) {
                            d.copy_from_slice(s);
                        }
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.flags = input.flags;
        Ok(FrameOut::One(out))
    }
}

/// Build [`Opts`] for a fixed integer canvas, for cross-module graph tests
/// (see `tests_invariants.rs`).
#[cfg(test)]
pub(crate) fn test_opts(w: u32, h: u32, x: u32, y: u32, color: &str) -> Opts {
    Opts {
        w: w.to_string(),
        h: h.to_string(),
        x: x.to_string(),
        y: y.to_string(),
        color: color.to_owned(),
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
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_a_noop_sized_canvas() {
        let opts = Opts::default();
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Yuv420p, 16, 8, vaco_core::Rational::ONE);
        assert_eq!((rect.w, rect.h, rect.x, rect.y), (16, 8, 0, 0));
    }

    #[test]
    fn explicit_canvas_centres_nothing_by_default() {
        let opts = Opts {
            w: "32".to_owned(),
            h: "16".to_owned(),
            x: "0".to_owned(),
            y: "0".to_owned(),
            color: "black".to_owned(),
        };
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Yuv420p, 16, 8, vaco_core::Rational::ONE);
        assert_eq!((rect.w, rect.h), (32, 16));
    }

    #[test]
    fn offset_is_floored_to_the_chroma_factor() {
        let opts = Opts {
            w: "32".to_owned(),
            h: "16".to_owned(),
            x: "3".to_owned(),
            y: "1".to_owned(),
            color: "black".to_owned(),
        };
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Yuv420p, 16, 8, vaco_core::Rational::ONE);
        assert_eq!((rect.x, rect.y), (2, 0));
    }

    proptest::proptest! {
        #[test]
        fn canvas_is_never_smaller_than_the_input(
            in_w in 2u32..64, in_h in 2u32..64,
            w in 0u32..128, h in 0u32..128,
        ) {
            let opts = Opts {
                w: w.to_string(),
                h: h.to_string(),
                x: "0".to_owned(),
                y: "0".to_owned(),
                color: "black".to_owned(),
            };
            let filter = Filter::new(&opts).unwrap();
            let rect = filter.compute(PixFmt::Yuv420p, in_w, in_h, vaco_core::Rational::ONE);
            proptest::prop_assert!(rect.w >= in_w);
            proptest::prop_assert!(rect.h >= in_h);
        }
    }
}
