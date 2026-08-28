//! `drawgrid` — draw a repeating colour grid over the input.
//!
//! `ffmpeg -h filter=drawgrid` (2026-08-28): `x`/`y` (offset expressions,
//! default `"0"`), `width`/`w`, `height`/`h` (grid cell size, default
//! `"0"`), `color`/`c` (default `"black"`), `thickness`/`t` (default
//! `"1"`), `replace` (bool, default `false`). Same expression/blend
//! mechanism as `drawbox` (see that module's doc for the full
//! measurement of both) — pure geometry, no text, so framecrc-exactness
//! is not foreclosed by a font ceiling.
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs, `-bitexact`)
//!
//! **Grid lines repeat in both directions from the `(x, y)` offset, by
//! `w`/`h`, not just forward from it.** `x=15:y=15:w=6:h=6` on a `20x20`
//! canvas lit columns `15` and rows `15`, but *also* row `3` entirely —
//! `15 - 6 - 6 = 3`, one whole period backward twice — confirming the
//! grid is `(coordinate - offset) mod period`, not "offset, then every
//! period going forward only". A forward-only probe (`x=5:y=5:w=6:h=6`)
//! separately confirmed the forward direction (`5, 11, 17`).

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
    name: "drawgrid",
    description: "Draw a colored grid on the input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const BASE_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub"];
const WH_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub", "t", "w", "h"];
const XY_VARS: &[&str] = &["iw", "ih", "dar", "sar", "hsub", "vsub", "t", "w", "h", "x", "y"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "drawgrid", help = "Draw a colored grid on the input video.")]
pub(crate) struct Opts {
    #[opt(name = "x", help = "set horizontal offset", default = "0".to_owned(), flags(video, filtering))]
    pub x: String,
    #[opt(name = "y", help = "set vertical offset", default = "0".to_owned(), flags(video, filtering))]
    pub y: String,
    #[opt(name = "width", alias = "w", help = "set width of grid cell", default = "0".to_owned(), flags(video, filtering))]
    pub w: String,
    #[opt(name = "height", alias = "h", help = "set height of grid cell", default = "0".to_owned(), flags(video, filtering))]
    pub h: String,
    #[opt(name = "color", alias = "c", help = "set color of the grid", default = "black".to_owned(), flags(video, filtering))]
    pub color: String,
    #[opt(name = "thickness", alias = "t", help = "set grid line thickness", default = "1".to_owned(), flags(video, filtering))]
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

fn base_values(iw: u32, ih: u32, sar: f64) -> [f64; 6] {
    let (iwf, ihf) = (f64::from(iw), f64::from(ih));
    let dar = if ih == 0 { 0.0 } else { iwf / ihf * sar };
    [iwf, ihf, dar, sar, 0.0, 0.0]
}

#[derive(Debug)]
pub(crate) struct Filter {
    w_expr: Expr,
    h_expr: Expr,
    x_expr: Expr,
    y_expr: Expr,
    thickness_expr: Expr,
    color: Rgba,
    replace: bool,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        Ok(Self {
            w_expr: Expr::parse(&opts.w, &Bindings::new(WH_VARS))
                .map_err(|e| format!("drawgrid: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &Bindings::new(WH_VARS))
                .map_err(|e| format!("drawgrid: bad `h` `{e}`"))?,
            x_expr: Expr::parse(&opts.x, &Bindings::new(XY_VARS))
                .map_err(|e| format!("drawgrid: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &Bindings::new(XY_VARS))
                .map_err(|e| format!("drawgrid: bad `y` `{e}`"))?,
            thickness_expr: Expr::parse(&opts.thickness, &Bindings::new(BASE_VARS))
                .map_err(|e| format!("drawgrid: bad `thickness` `{e}`"))?,
            color: color::parse_color(&opts.color).map_err(|e| format!("drawgrid: {e}"))?,
            replace: opts.replace,
        })
    }
}

/// Resolve `thickness`, `w`, `h`, `x`, `y` in that order — the same shape
/// and the same unconfirmed-order caveat as `drawbox::resolve`.
pub(crate) fn resolve(
    thickness_expr: &Expr,
    w_expr: &Expr,
    h_expr: &Expr,
    x_expr: &Expr,
    y_expr: &Expr,
    iw: u32,
    ih: u32,
    sar: f64,
) -> (i64, i64, i64, i64, i64) {
    let base = base_values(iw, ih, sar);
    let thickness = thickness_expr.eval(&base);

    let mut t_vals = [0.0; 7];
    t_vals[..6].copy_from_slice(&base);
    t_vals[6] = thickness;

    let w = w_expr.eval(&t_vals).max(1.0) as i64;
    let h = h_expr.eval(&t_vals).max(1.0) as i64;

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
    let thickness_px = thickness.max(0.0) as i64;
    (thickness_px, w, h, x, y)
}

/// Euclidean `(v - offset) mod period`, always in `[0, period)` even for
/// `v < offset` — the modular rule the backward-extension probe confirmed.
fn on_grid_line(v: i64, offset: i64, period: i64, thickness: i64) -> bool {
    if period <= 0 {
        return false;
    }
    (v - offset).rem_euclid(period) < thickness
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

        let (thickness, w, h, x, y) = resolve(
            &self.thickness_expr,
            &self.w_expr,
            &self.h_expr,
            &self.x_expr,
            &self.y_expr,
            width,
            height,
            1.0,
        );

        let mut out = input;
        for (plane, channel) in [(0usize, self.color.g), (1, self.color.b), (2, self.color.r)] {
            let Some(mut dst) = out.plane_mut(plane) else {
                continue;
            };
            let alpha = if self.replace { 1.0 } else { self.color.a };
            for (py, row) in dst.rows_mut().enumerate() {
                for (px, dst_px) in row.iter_mut().enumerate() {
                    let px_i = i64::try_from(px).unwrap_or(i64::MAX);
                    let py_i = i64::try_from(py).unwrap_or(i64::MAX);
                    let on_v = on_grid_line(px_i, x, w, thickness);
                    let on_h = on_grid_line(py_i, y, h, thickness);
                    if on_v || on_h {
                        *dst_px = color::blend_channel(*dst_px, channel, alpha);
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
            name: "drawgrid",
            instance: "drawgrid",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe: `x=5:w=6` on a `20`-wide canvas
    /// lights columns `5, 11, 17`.
    #[test]
    fn grid_repeats_forward_at_the_measured_period() {
        for col in 0..20 {
            let expected = matches!(col, 5 | 11 | 17);
            assert_eq!(on_grid_line(col, 5, 6, 1), expected, "column {col}");
        }
    }

    /// Pinned against the reference probe: `x=15:w=6` on a `20`-wide
    /// canvas also lights column `3` (`15 - 6 - 6`), confirming the grid
    /// extends *backward* from the offset too, not just forward.
    #[test]
    fn grid_repeats_backward_from_the_offset_too() {
        assert!(on_grid_line(3, 15, 6, 1));
        assert!(on_grid_line(9, 15, 6, 1));
        assert!(on_grid_line(15, 15, 6, 1));
    }

    #[test]
    fn thickness_widens_the_lit_band() {
        assert!(on_grid_line(5, 5, 6, 2));
        assert!(on_grid_line(6, 5, 6, 2));
        assert!(!on_grid_line(7, 5, 6, 2));
        assert!(!on_grid_line(4, 5, 6, 2));
    }
}
