//! `drawbox` — draw a colour-filled or outlined rectangle on the input.
//!
//! `ffmpeg -h filter=drawbox` (2026-08-28): `x`/`y` (left/top edge
//! expressions, default `"0"`), `width`/`w`, `height`/`h` (default `"0"`),
//! `color`/`c` (default `"black"`), `thickness`/`t` (default `"3"`),
//! `replace` (bool, default `false`), `box_source` (read a rectangle from
//! side data). Pure geometry over one input — no text, so unlike this
//! project's scope-filter family this one is not foreclosed from
//! framecrc-exactness by a font ceiling.
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs, `-bitexact`)
//!
//! **Every geometry option is a `vaco-expr` expression, evaluated exactly
//! once** (there is no `eval=init/frame` choice the way `crop`/`pad` have
//! one, and no way to make it re-evaluate): a source varying over time or
//! frame count showed `x=10*n` rejected outright (`"Undefined constant
//! ... in 'n'"` — `n`, the frame counter, is not a bound name at all) and
//! `x=10*t` produced the *same* box position on every one of `5` output
//! frames rather than one that moved with real playback time. That second
//! probe also revealed what `t` actually is:
//!
//! **`t` is not time — it is this filter's own resolved `thickness`
//! value.** `x=t` alone produced `x=3` (`thickness`'s default) on a
//! zero-argument instance, and produced `x=9` once `thickness=9` was
//! passed explicitly — an exact match, not a coincidence of one probe.
//! This lets an `x`/`y`/`w`/`h` expression inset a box by its own stroke
//! width, e.g. `x=t/2`. Bound names confirmed valid: `iw`, `ih`, `dar`,
//! `sar`, `hsub`, `vsub`, `w`, `h`, `t`; confirmed invalid: `n`, `main_w`,
//! `main_h`. `x`'s own expression could not reference `x` itself
//! (`"Undefined constant"`), but could reference `y` — the reverse was
//! not independently checked, so this module resolves `w`, `h`, then `x`
//! (with `y` unresolved, fed as `0.0`), then `y` (with the now-resolved
//! `x`) — the same order the option list itself declares them in, not
//! independently confirmed as the reference's own evaluation order.
//!
//! **The colour blend is `floor(src*(1-a) + color*a)` per channel, not
//! `round`** — pinned at three different alpha values (`0.5`, `0.3`,
//! `0.33`) each landing on the floored result; see `crate::color`'s own
//! doc for the exact probes and for the hex layout (`0xRRGGBBAA`, alpha
//! **last**) `color=0x11223344` confirmed. `replace=true` skips blending
//! entirely and assigns the colour (and, on a format with alpha, the
//! alpha channel) directly — the reference's own documented meaning of
//! "replace", implemented as stated rather than re-derived, since a
//! direct assignment has no blend arithmetic left to probe.
//!
//! **`thickness` also accepts the literal string `"fill"`**, meaning "no
//! outline — fill the whole rectangle", confirmed by comparing a
//! `t=fill` render's lit-pixel count against `w*h` exactly.
//!
//! # Not implemented
//!
//! `box_source` (reading a rectangle from frame side data — no producer
//! of that side data exists in this tree yet, the same "declined rather
//! than guessed" reasoning `vaco-filter-scope`'s own crates use
//! elsewhere). This module only addresses planar RGB (`gbrp`-family,
//! plane order `G,B,R` per this project's established convention) 8-bit,
//! no-alpha formats — see `crate::color`'s own doc for why a full
//! reference-matching YUV colour conversion was not attempted this pass:
//! blending is exact and conversion-free only when the option's own
//! colour is already expressed in the same colour model as the frame.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::color::{self, Rgba};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "drawbox",
    description: "Draw a colored box on the input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `iw, ih, dar, sar, hsub, vsub` — every geometry expression's shared
/// prefix; `t` (this filter's own resolved thickness) and `w, h, x, y`
/// (cross-referenceable, in resolution order) are appended per use.
const BASE_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub"];
const WH_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub", "t", "w", "h"];
const XY_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub", "t", "w", "h", "x", "y"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "drawbox", help = "Draw a colored box on the input video.")]
pub(crate) struct Opts {
    #[opt(name = "x", help = "set horizontal position of the left box edge", default = "0".to_owned(), flags(video, filtering))]
    pub x: String,
    #[opt(name = "y", help = "set vertical position of the top box edge", default = "0".to_owned(), flags(video, filtering))]
    pub y: String,
    #[opt(name = "width", alias = "w", help = "set width of the box", default = "0".to_owned(), flags(video, filtering))]
    pub w: String,
    #[opt(name = "height", alias = "h", help = "set height of the box", default = "0".to_owned(), flags(video, filtering))]
    pub h: String,
    #[opt(name = "color", alias = "c", help = "set color of the box", default = "black".to_owned(), flags(video, filtering))]
    pub color: String,
    #[opt(name = "thickness", alias = "t", help = "set the box thickness", default = "3".to_owned(), flags(video, filtering))]
    pub thickness: String,
    #[opt(name = "replace", help = "replace color & alpha", default = false, flags(video, filtering))]
    pub replace: bool,
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

/// A resolved rectangle: pixel-space, already clamped is the caller's job.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

/// `thickness`'s own two forms: a resolved pixel width, or "fill the
/// whole rectangle" (`t=fill`, confirmed by an exact lit-pixel-count
/// match against `w*h`).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Thickness {
    Pixels(i64),
    Fill,
}

fn base_values(iw: u32, ih: u32, sar: f64) -> [f64; 6] {
    let (iwf, ihf) = (f64::from(iw), f64::from(ih));
    let dar = if ih == 0 { 0.0 } else { iwf / ihf * sar };
    [iwf, ihf, dar, sar, 0.0, 0.0]
}

/// Resolve `thickness`, `w`, `h`, `x`, `y` in that order — see the module
/// doc for why this order and what is (and is not) confirmed about it.
#[allow(clippy::too_many_arguments, reason = "one geometry-resolution pass")]
pub(crate) fn resolve(
    thickness_expr: Option<&Expr>,
    w_expr: &Expr,
    h_expr: &Expr,
    x_expr: &Expr,
    y_expr: &Expr,
    iw: u32,
    ih: u32,
    sar: f64,
) -> (Thickness, Rect) {
    let base = base_values(iw, ih, sar);
    let Some(thickness_expr) = thickness_expr else {
        // `t=fill`: no pixel thickness to resolve at all, and the module
        // doc's own measurement found this means "the whole rectangle",
        // not some numeric stand-in thickness.
        let mut t_vals = [0.0; 7];
        t_vals[..6].copy_from_slice(&base);
        let w = w_expr.eval(&t_vals).max(0.0) as i64;
        let h = h_expr.eval(&t_vals).max(0.0) as i64;
        let mut wh_vals = [0.0; 9];
        wh_vals[..7].copy_from_slice(&t_vals);
        wh_vals[7] = w as f64;
        wh_vals[8] = h as f64;
        let mut xy_vals = [0.0; 11];
        xy_vals[..9].copy_from_slice(&wh_vals);
        let x = x_expr.eval(&xy_vals) as i64;
        xy_vals[9] = x as f64;
        let y = y_expr.eval(&xy_vals) as i64;
        return (Thickness::Fill, Rect { x, y, w, h });
    };
    let thickness = thickness_expr.eval(&base);

    let mut t_vals = [0.0; 7];
    t_vals[..6].copy_from_slice(&base);
    t_vals[6] = thickness;

    let w = w_expr.eval(&t_vals).max(0.0) as i64;
    let h = h_expr.eval(&t_vals).max(0.0) as i64;

    let mut wh_vals = [0.0; 9];
    wh_vals[..7].copy_from_slice(&t_vals);
    wh_vals[7] = w as f64;
    wh_vals[8] = h as f64;

    let mut xy_vals = [0.0; 11];
    xy_vals[..9].copy_from_slice(&wh_vals);
    let x = x_expr.eval(&xy_vals) as i64;
    xy_vals[9] = x as f64;
    let y = y_expr.eval(&xy_vals) as i64;

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "thickness is a small positive pixel count by construction"
    )]
    let thick = Thickness::Pixels(thickness.max(0.0) as i64);
    (thick, Rect { x, y, w, h })
}

#[derive(Debug)]
pub(crate) struct Filter {
    w_expr: Expr,
    h_expr: Expr,
    x_expr: Expr,
    y_expr: Expr,
    thickness_expr: Option<Expr>,
    fill: bool,
    color: Rgba,
    replace: bool,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let (thickness_expr, fill) = if opts.thickness.trim() == "fill" {
            (None, true)
        } else {
            let e = Expr::parse(&opts.thickness, &Bindings::new(BASE_VARS))
                .map_err(|e| format!("drawbox: bad `thickness` `{e}`"))?;
            (Some(e), false)
        };
        Ok(Self {
            w_expr: Expr::parse(&opts.w, &Bindings::new(WH_VARS))
                .map_err(|e| format!("drawbox: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &Bindings::new(WH_VARS))
                .map_err(|e| format!("drawbox: bad `h` `{e}`"))?,
            x_expr: Expr::parse(&opts.x, &Bindings::new(XY_VARS))
                .map_err(|e| format!("drawbox: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &Bindings::new(XY_VARS))
                .map_err(|e| format!("drawbox: bad `y` `{e}`"))?,
            thickness_expr,
            fill,
            color: color::parse_color(&opts.color).map_err(|e| format!("drawbox: {e}"))?,
            replace: opts.replace,
        })
    }
}

/// True when `(px, py)` is inside the drawn stroke — the outline band
/// `thickness` pixels wide, or the whole rectangle when `fill`.
fn in_stroke(px: i64, py: i64, rect: Rect, thickness: i64, fill: bool) -> bool {
    if px < rect.x || py < rect.y || px >= rect.x + rect.w || py >= rect.y + rect.h {
        return false;
    }
    if fill || thickness <= 0 {
        return true;
    }
    let from_left = px - rect.x;
    let from_top = py - rect.y;
    let from_right = rect.x + rect.w - 1 - px;
    let from_bottom = rect.y + rect.h - 1 - py;
    from_left < thickness || from_top < thickness || from_right < thickness || from_bottom < thickness
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, width, height, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        if !format.is_rgb() || !format.is_planar() || format.plane_count() != 3 || format.has_alpha() || format.max_depth() != 8
        {
            return Ok(FrameOut::One(input));
        }

        let (thickness, rect) = resolve(
            self.thickness_expr.as_ref(),
            &self.w_expr,
            &self.h_expr,
            &self.x_expr,
            &self.y_expr,
            width,
            height,
            1.0,
        );
        let thickness_px = match thickness {
            Thickness::Fill => 0,
            Thickness::Pixels(t) => t,
        };
        let fill = self.fill || matches!(thickness, Thickness::Fill);

        let mut out = input;
        // Plane order for `gbrp`: `G, B, R` — see `crate::color`'s doc and
        // this project's own established convention.
        for (plane, channel) in [(0usize, self.color.g), (1, self.color.b), (2, self.color.r)] {
            let Some(mut dst) = out.plane_mut(plane) else {
                continue;
            };
            let alpha = if self.replace { 1.0 } else { self.color.a };
            for (y, row) in dst.rows_mut().enumerate() {
                for (x, px) in row.iter_mut().enumerate() {
                    if in_stroke(
                        i64::try_from(x).unwrap_or(i64::MAX),
                        i64::try_from(y).unwrap_or(i64::MAX),
                        rect,
                        thickness_px,
                        fill,
                    ) {
                        *px = color::blend_channel(*px, channel, alpha);
                    }
                }
            }
        }

        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::converter(
            FormatSet::video_exact(PixFmt::Gbrp),
            FormatSet::video_exact(PixFmt::Gbrp),
            req.instance,
        ),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "drawbox",
            instance: "drawbox",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_color_is_a_clean_error() {
        let req = Instantiate {
            name: "drawbox",
            instance: "drawbox",
            args: Some("color=not_a_color"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    /// Pinned against the reference probe: `x=t` with the default
    /// `thickness=3` resolves to `x=3`; with `thickness=9` it resolves to
    /// `x=9` — `t` is the filter's own resolved thickness, not time or a
    /// frame count.
    #[test]
    fn t_resolves_to_the_filters_own_thickness() {
        let x = Expr::parse("t", &Bindings::new(XY_VARS)).unwrap();
        let w = Expr::parse("4", &Bindings::new(WH_VARS)).unwrap();
        let h = Expr::parse("4", &Bindings::new(WH_VARS)).unwrap();
        let y = Expr::parse("0", &Bindings::new(XY_VARS)).unwrap();

        let default_t = Expr::parse("3", &Bindings::new(BASE_VARS)).unwrap();
        let (_, rect) = resolve(Some(&default_t), &w, &h, &x, &y, 64, 64, 1.0);
        assert_eq!(rect.x, 3);

        let custom_t = Expr::parse("9", &Bindings::new(BASE_VARS)).unwrap();
        let (_, rect) = resolve(Some(&custom_t), &w, &h, &x, &y, 64, 64, 1.0);
        assert_eq!(rect.x, 9);
    }

    #[test]
    fn fill_covers_the_whole_rectangle_not_just_an_outline() {
        let rect = Rect { x: 2, y: 2, w: 5, h: 5 };
        let mut count = 0;
        for py in 0..10 {
            for px in 0..10 {
                if in_stroke(px, py, rect, 0, true) {
                    count += 1;
                }
            }
        }
        assert_eq!(count, 25);
    }

    #[test]
    fn a_thin_outline_leaves_the_interior_untouched() {
        let rect = Rect { x: 0, y: 0, w: 10, h: 10 };
        assert!(in_stroke(0, 0, rect, 2, false));
        assert!(in_stroke(1, 5, rect, 2, false));
        assert!(!in_stroke(5, 5, rect, 2, false));
        assert!(in_stroke(9, 9, rect, 2, false));
    }
}
