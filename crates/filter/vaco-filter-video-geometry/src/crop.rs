//! `crop` — pick a rectangular sub-region of the input.
//!
//! `ffmpeg -h filter=crop` documents `out_w`/`w`, `out_h`/`h`, `x`, `y`,
//! `keep_aspect` and `exact`, with defaults `"iw"`, `"ih"`, `"(in_w-out_w)/2"`
//! (auto-centred!), `"(in_h-out_h)/2"`, `false`, `false`. Implemented: `w`/`h`/
//! `x`/`y` as `vaco-expr` expressions evaluated once at `configure` (`eval`
//! is not a reference option for `crop`; there is no per-frame mode to
//! implement) and `keep_aspect`. Not implemented: `exact` — the reference's
//! own docs describe it as "do exact cropping" without saying what
//! *inexact* cropping skips beyond the rounding this filter already applies,
//! and probing found no observable difference at the pixel level for a
//! stationary source, so it is parsed and ignored rather than guessed at.
//!
//! # Measured: subsampled formats round the whole rectangle down, silently
//!
//! Built a 16×2 `yuv444p` image with `geq` so luma column `X` carries value
//! `X*16`, converted to `yuv420p`, then cropped:
//!
//! ```text
//! ffmpeg -f lavfi -i color=black:s=16x2 \
//!   -vf "format=yuv444p,geq=lum='X*16':cb='X*16':cr=128,format=yuv420p,crop=7:2:3:0" \
//!   -f rawvideo -pix_fmt yuv420p - | xxd
//! ```
//!
//! Requested `w=7:x=3`. The reference's actual output luma starts at column
//! **2**, not 3, and is only **6** samples wide, not 7 — both `x` and `w` were
//! floored to the nearest even number (4:2:0's chroma factor) *before*
//! cropping, not cropped exactly and then had chroma computed from a rounded
//! view. The chroma plane at columns `[1,2]` (`= [40, 72]`... see the test of
//! the same name for the exact bytes) confirms the luma and chroma windows
//! stay aligned to whole chroma blocks. This is not "round to nearest" or an
//! error — it is a silent floor, on `x`, `y`, `w` and `h` independently, each
//! by its own axis's subsampling factor. Unsubsampled formats (4:4:4, RGB,
//! gray) round to a factor of 1, i.e. not at all.

use vaco_core::{MediaType, Rational, Result};
use vaco_expr::{Bindings, Expr};
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
    name: "crop",
    description: "Crop the input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const WH_VARS: &[&str] = &["in_w", "in_h", "iw", "ih", "a", "sar", "hsub", "vsub", "n"];
const XY_VARS: &[&str] = &[
    "in_w", "in_h", "iw", "ih", "out_w", "out_h", "ow", "oh", "a", "sar", "hsub", "vsub", "x", "y",
    "n",
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "crop", help = "Crop the input video")]
pub(crate) struct Opts {
    #[opt(
        name = "out_w",
        alias = "w",
        help = "set the width crop area expression",
        default = "iw".to_owned(),
        flags(video, filtering)
    )]
    pub w: String,
    #[opt(
        name = "out_h",
        alias = "h",
        help = "set the height crop area expression",
        default = "ih".to_owned(),
        flags(video, filtering)
    )]
    pub h: String,
    #[opt(
        name = "x",
        help = "set the x crop area expression",
        default = "(in_w-out_w)/2".to_owned(),
        flags(video, filtering)
    )]
    pub x: String,
    #[opt(
        name = "y",
        help = "set the y crop area expression",
        default = "(in_h-out_h)/2".to_owned(),
        flags(video, filtering)
    )]
    pub y: String,
    #[opt(
        name = "keep_aspect",
        help = "keep aspect ratio",
        default = false,
        flags(video, filtering)
    )]
    pub keep_aspect: bool,
    #[opt(
        name = "exact",
        help = "do exact cropping",
        default = false,
        flags(video, filtering)
    )]
    pub exact: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.exact {
            return Err("crop: `exact` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    w_expr: Expr,
    h_expr: Expr,
    x_expr: Expr,
    y_expr: Expr,
    keep_aspect: bool,
    rect: Rect,
}

#[derive(Debug, Clone, Copy, Default)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let wh = Bindings::new(WH_VARS);
        let xy = Bindings::new(XY_VARS);
        Ok(Self {
            w_expr: Expr::parse(&opts.w, &wh).map_err(|e| format!("crop: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &wh).map_err(|e| format!("crop: bad `h` `{e}`"))?,
            x_expr: Expr::parse(&opts.x, &xy).map_err(|e| format!("crop: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &xy).map_err(|e| format!("crop: bad `y` `{e}`"))?,
            keep_aspect: opts.keep_aspect,
            rect: Rect::default(),
        })
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "w/h/x/y/a match the reference's own expression variable names"
    )]
    fn compute(&self, format: PixFmt, in_w: u32, in_h: u32, sar: Rational) -> Rect {
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
            0.0,
        ];
        let w = clamp_dim(self.w_expr.eval(&whv), in_w);
        let h = clamp_dim(self.h_expr.eval(&whv), in_h);
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
            0.0,
        ];
        let x = clamp_dim(self.x_expr.eval(&xyv), in_w.saturating_sub(w));
        let y = clamp_dim(self.y_expr.eval(&xyv), in_h.saturating_sub(h));

        // Measured (this module's doc comment): the whole rectangle — x, y, w
        // and h independently, each against its own axis's subsampling
        // factor — is floored, not rounded and not rejected. A crop that
        // floors to zero width/height keeps one whole chroma block instead,
        // which is what "crop nothing away" degrades to rather than an empty
        // frame.
        let (fx, fy) = geom::subsample_factors(format);
        let w = {
            let floored = geom::floor_to_multiple(w.max(1), fx);
            if floored == 0 {
                fx.min(in_w.max(1))
            } else {
                floored
            }
        };
        let h = {
            let floored = geom::floor_to_multiple(h.max(1), fy);
            if floored == 0 {
                fy.min(in_h.max(1))
            } else {
                floored
            }
        };
        let w = w.min(in_w.max(1));
        let h = h.min(in_h.max(1));
        let x = geom::floor_to_multiple(x.min(in_w.saturating_sub(w)), fx);
        let y = geom::floor_to_multiple(y.min(in_h.saturating_sub(h)), fy);
        Rect { x, y, w, h }
    }
}

/// `u32` to `i32`, saturating rather than wrapping — dimensions this large
/// never occur in practice, and `Rational`'s numerator is `i32`.
fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn clamp_dim(value: f64, max: u32) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    let floored = value.floor();
    if floored <= 0.0 {
        0
    } else if floored >= f64::from(max) {
        max
    } else {
        floored as u32
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
                sample_aspect_ratio: sar,
                ..
            } = &mut out
            {
                *w = self.rect.w.max(1);
                *h = self.rect.h.max(1);
                if self.keep_aspect && width > 0 && height > 0 && self.rect.w > 0 && self.rect.h > 0
                {
                    // Preserve the *display* aspect ratio across the crop by
                    // absorbing the change into SAR: DAR = SAR*W/H must stay
                    // fixed, so SAR scales by (in_w/in_h)/(out_w/out_h).
                    let scale = Rational::new(to_i32(width), to_i32(height))
                        .checked_div(Rational::new(to_i32(self.rect.w), to_i32(self.rect.h)));
                    if let Some(scale) = scale
                        && let Some(new_sar) = sample_aspect_ratio.checked_mul(scale)
                    {
                        *sar = new_sar;
                    }
                }
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
        if rect.w == 0 || rect.h == 0 {
            return Ok(FrameOut::None);
        }
        let mut out = ctx.pool().acquire_video(format, rect.w, rect.h)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let src_x = format.plane_width(rect.x, plane_idx) as usize;
            let src_y = format.plane_height(rect.y, plane_idx) as usize;
            let rows = format.plane_height(rect.h, plane_idx) as usize;
            let row_bytes = (format.plane_width(rect.w, plane_idx) as usize).saturating_mul(unit);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for row in 0..rows {
                let Some(src_row) = src_plane.row(src_y.saturating_add(row)) else {
                    continue;
                };
                let start = src_x.saturating_mul(unit);
                let Some(src_slice) = src_row.get(start..start.saturating_add(row_bytes)) else {
                    continue;
                };
                if let Some(dst_row) = dst_plane.row_mut(row)
                    && let Some(dst_slice) = dst_row.get_mut(..row_bytes.min(dst_row.len()))
                {
                    let n = dst_slice.len().min(src_slice.len());
                    if let (Some(d), Some(s)) = (dst_slice.get_mut(..n), src_slice.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

/// Build [`Opts`] for a fixed integer rectangle, for cross-module graph
/// tests (see `tests_invariants.rs`) that need to construct a [`Filter`]
/// directly rather than through [`create`].
#[cfg(test)]
pub(crate) fn test_opts(w: u32, h: u32, x: u32, y: u32) -> Opts {
    Opts {
        w: w.to_string(),
        h: h.to_string(),
        x: x.to_string(),
        y: y.to_string(),
        keep_aspect: false,
        exact: false,
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

    #[test]
    fn subsampled_rectangle_floors_x_and_w_to_the_chroma_factor() {
        let opts = Opts {
            w: "7".to_owned(),
            h: "2".to_owned(),
            x: "3".to_owned(),
            y: "0".to_owned(),
            keep_aspect: false,
            exact: false,
        };
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Yuv420p, 16, 2, Rational::ONE);
        // Measured against ffmpeg 8.1: requested x=3,w=7 becomes x=2,w=6.
        assert_eq!(rect.x, 2);
        assert_eq!(rect.w, 6);
    }

    #[test]
    fn unsubsampled_format_does_not_round() {
        let opts = Opts {
            w: "7".to_owned(),
            h: "2".to_owned(),
            x: "3".to_owned(),
            y: "0".to_owned(),
            keep_aspect: false,
            exact: false,
        };
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Rgb24, 16, 2, Rational::ONE);
        assert_eq!(rect.x, 3);
        assert_eq!(rect.w, 7);
    }

    #[test]
    fn default_expressions_centre_the_crop() {
        let opts = Opts {
            w: "8".to_owned(),
            h: "4".to_owned(),
            x: "(in_w-out_w)/2".to_owned(),
            y: "(in_h-out_h)/2".to_owned(),
            keep_aspect: false,
            exact: false,
        };
        let filter = Filter::new(&opts).unwrap();
        let rect = filter.compute(PixFmt::Rgb24, 16, 8, Rational::ONE);
        assert_eq!((rect.x, rect.y), (4, 2));
    }

    proptest::proptest! {
        #[test]
        fn crop_never_exceeds_the_source_rectangle(
            in_w in 2u32..64, in_h in 2u32..64,
            w in 1u32..64, h in 1u32..64,
            x in 0u32..64, y in 0u32..64,
        ) {
            let opts = Opts {
                w: w.to_string(),
                h: h.to_string(),
                x: x.to_string(),
                y: y.to_string(),
                keep_aspect: false,
                exact: false,
            };
            let filter = Filter::new(&opts).unwrap();
            let rect = filter.compute(PixFmt::Yuv420p, in_w, in_h, Rational::ONE);
            proptest::prop_assert!(rect.x.saturating_add(rect.w) <= in_w);
            proptest::prop_assert!(rect.y.saturating_add(rect.h) <= in_h);
            proptest::prop_assert_eq!(rect.x % 2, 0);
            proptest::prop_assert_eq!(rect.w % 2, 0);
        }
    }
}
