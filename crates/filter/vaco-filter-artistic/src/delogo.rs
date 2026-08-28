//! `delogo` — replace a rectangular region with values interpolated from its
//! border, so a static logo/watermark blends into its surroundings.
//!
//! `ffmpeg -h filter=delogo` (2026-08-28): `x`, `y`, `w`, `h` (expressions,
//! default `"-1"` each), `show` (bool, default false). Timeline-capable.
//! The online `ffmpeg-filters.html` documentation additionally describes
//! `band`/`t` options controlling the interpolation band width and a fuzzy
//! edge — **neither exists in this reference build** (`ffmpeg -h
//! filter=delogo`, `ffmpeg 8.1`, checked directly rather than trusted from
//! the docs, per D17: the shipped binary is the fact, its documentation is
//! not always current with it). The band/edge behaviour below is therefore
//! this crate's own default, not a reproduction of a configurable value.
//!
//! # Measured: box replacement is real, not content-aware
//!
//! A flat `50` background with a `200`-valued box at the target rectangle
//! comes back **entirely `50`** — the box's own content plays no part in
//! the output, confirming this is border interpolation, not any kind of
//! detection or content-preserving fill
//! (`ffmpeg -bitexact -f lavfi -i "color=black:s=10x10,format=gray,geq=lum=..."
//! -vf delogo=x=3:y=3:w=4:h=4 -f rawvideo -pix_fmt gray -`).
//!
//! # Measured, and only partially resolved: the interpolation formula
//!
//! Feeding a step function (left region `0`, right region `100`, top/bottom
//! borders also `0`) through a `4x4` box at `x=3,y=3` and reading the
//! reference's raw output pinned this formula for **most** of the box:
//!
//! ```text
//! dist_left  = x - x0 + 1        dist_right  = (x0+w) - x
//! dist_top   = y - y0 + 1        dist_bottom = (y0+h) - y
//! interp_h   = (L*dist_right + R*dist_left)   / (dist_left+dist_right)
//! interp_v   = (T*dist_bottom + B*dist_top)   / (dist_top+dist_bottom)
//! weight_h   = dist_top * dist_bottom          // trust horizontal more, far from top/bottom
//! weight_v   = dist_left * dist_right          // trust vertical more, far from left/right
//! out(x,y)   = (interp_h*weight_h + interp_v*weight_v) / (weight_h+weight_v)
//! ```
//!
//! where `L`/`R` are the single pixels immediately left/right of the box in
//! row `y`, and `T`/`B` the single pixels immediately above/below in column
//! `x` (this crate assumed a one-pixel band; the probe's uniform regions
//! cannot distinguish a wider band from a one-pixel sample, since either
//! reads the same value there — an acknowledged gap, not a measurement).
//!
//! **This formula matched three of the box's four columns exactly** (`x=3,
//! 4, 5` at every row, to the byte) but diverged on the fourth (`x=6`, the
//! column nearest the `R` step) at every row tested — predicted `40`/`48`,
//! observed `57`/`61`. Several alternate conventions were tried (shifting
//! the border-distance origin by one, plain four-corner bilinear, an
//! unweighted 50/50 blend, squared and min-based weights) and none matched
//! both the working columns and the diverging one simultaneously, so this
//! is reported as a real, unresolved discrepancy rather than a border
//! effect this crate silently patched over.
//!
//! # Not framecrc-verified
//!
//! Given the discrepancy above is not confined to a corner or an edge case
//! but reproduces across an entire column of the one box tested, this
//! filter is **implemented, structurally faithful to the measured shape,
//! but not framecrc-verified** — the same bar `vaco-filter-blur::gblur`
//! documents for the same reason: a formula good enough to be clearly the
//! right shape, not yet good enough to claim byte-exactness.

use vaco_core::{MediaType, Result};
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
    name: "delogo",
    description: "Remove logo from input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

const XY_VARS: &[&str] = &["w", "h", "x", "y"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "delogo", help = "Remove logo from input video.")]
pub(crate) struct Opts {
    #[opt(name = "x", help = "set logo x position", default = "-1".to_owned(), flags(video, filtering))]
    pub x: String,
    #[opt(name = "y", help = "set logo y position", default = "-1".to_owned(), flags(video, filtering))]
    pub y: String,
    #[opt(name = "w", help = "set logo width", default = "-1".to_owned(), flags(video, filtering))]
    pub w: String,
    #[opt(name = "h", help = "set logo height", default = "-1".to_owned(), flags(video, filtering))]
    pub h: String,
    #[opt(name = "show", help = "show delogo area", default = false, flags(video, filtering))]
    pub show: bool,
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
    x_expr: Expr,
    y_expr: Expr,
    w_expr: Expr,
    h_expr: Expr,
    show: bool,
}

/// The box, resolved to pixel coordinates and clamped to the frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(XY_VARS);
        Ok(Self {
            x_expr: Expr::parse(&opts.x, &bindings).map_err(|e| format!("delogo: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &bindings).map_err(|e| format!("delogo: bad `y` `{e}`"))?,
            w_expr: Expr::parse(&opts.w, &bindings).map_err(|e| format!("delogo: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &bindings).map_err(|e| format!("delogo: bad `h` `{e}`"))?,
            show: opts.show,
        })
    }

    fn resolve_box(&self, width: i32, height: i32) -> Option<Rect> {
        let vars0 = [f64::from(width), f64::from(height), 0.0, 0.0];
        #[allow(
            clippy::cast_possible_truncation,
            reason = "frame dimensions are tiny relative to f64's exact range"
        )]
        let (x0, y0, w, h) = (
            self.x_expr.eval(&vars0) as i32,
            self.y_expr.eval(&vars0) as i32,
            self.w_expr.eval(&vars0) as i32,
            self.h_expr.eval(&vars0) as i32,
        );
        if w <= 0 || h <= 0 || x0 < 0 || y0 < 0 {
            return None;
        }
        let x1 = (x0 + w).min(width);
        let y1 = (y0 + h).min(height);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect {
            x0,
            y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// One plane's box replaced in place, `rows` addressed `[y][x]`.
pub(crate) fn fill_box(rows: &mut [Vec<u8>], b: Rect) {
    for y in b.y0..b.y0 + b.h {
        let Ok(uy) = usize::try_from(y) else { continue };
        let Some(l) = rows.get(uy).and_then(|r| {
            let lx = usize::try_from(b.x0 - 1).ok()?;
            r.get(lx).copied()
        }) else {
            continue;
        };
        let r_val = rows.get(uy).and_then(|r| {
            let rx = usize::try_from(b.x0 + b.w).ok()?;
            r.get(rx).copied()
        });
        let Some(r_val) = r_val else { continue };
        for x in b.x0..b.x0 + b.w {
            let Ok(ux) = usize::try_from(x) else { continue };
            let t_val = usize::try_from(b.y0 - 1)
                .ok()
                .and_then(|ty| rows.get(ty))
                .and_then(|row| row.get(ux))
                .copied();
            let b_val = usize::try_from(b.y0 + b.h)
                .ok()
                .and_then(|by| rows.get(by))
                .and_then(|row| row.get(ux))
                .copied();
            let (Some(t_val), Some(b_val)) = (t_val, b_val) else {
                continue;
            };
            let dist_left = f64::from(x - b.x0 + 1);
            let dist_right = f64::from(b.x0 + b.w - x);
            let dist_top = f64::from(y - b.y0 + 1);
            let dist_bottom = f64::from(b.y0 + b.h - y);
            let interp_h =
                (f64::from(l) * dist_right + f64::from(r_val) * dist_left) / (dist_left + dist_right);
            let interp_v =
                (f64::from(t_val) * dist_bottom + f64::from(b_val) * dist_top) / (dist_top + dist_bottom);
            let weight_h = dist_top * dist_bottom;
            let weight_v = dist_left * dist_right;
            let value = if weight_h + weight_v > 0.0 {
                (interp_h * weight_h + interp_v * weight_v) / (weight_h + weight_v)
            } else {
                f64::from(l)
            };
            if let Some(row) = rows.get_mut(uy)
                && let Some(px) = row.get_mut(ux)
            {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is a weighted average of u8 inputs, within 0..=255"
                )]
                {
                    *px = value.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// Outline the box in green-as-luma-zero (matching this crate's 8-bit-only
/// scope: `show=1` is only meaningfully verified against a `gray`/luma
/// probe, not a colour marker).
fn draw_outline(rows: &mut [Vec<u8>], b: Rect) {
    for x in b.x0..b.x0 + b.w {
        let Ok(ux) = usize::try_from(x) else { continue };
        for y in [b.y0, b.y0 + b.h - 1] {
            if let Ok(uy) = usize::try_from(y)
                && let Some(row) = rows.get_mut(uy)
                && let Some(px) = row.get_mut(ux)
            {
                *px = 0;
            }
        }
    }
    for y in b.y0..b.y0 + b.h {
        let Ok(uy) = usize::try_from(y) else { continue };
        for x in [b.x0, b.x0 + b.w - 1] {
            if let Ok(ux) = usize::try_from(x)
                && let Some(row) = rows.get_mut(uy)
                && let Some(px) = row.get_mut(ux)
            {
                *px = 0;
            }
        }
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
        let Some(b) = self.resolve_box(common::to_i32(width), common::to_i32(height)) else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let ph = common::to_i32(format.plane_height(height, p8)).max(0);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let mut rows: Vec<Vec<u8>> = (0..ph.max(0))
                .map(|y| {
                    usize::try_from(y)
                        .ok()
                        .and_then(|uy| src_plane.row(uy))
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default()
                })
                .collect();
            if p == 0 {
                fill_box(&mut rows, b);
                if self.show {
                    draw_outline(&mut rows, b);
                }
            }
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in rows.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
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

    fn rows_of(grid: &[[u8; 10]]) -> Vec<Vec<u8>> {
        grid.iter().map(|r| r.to_vec()).collect()
    }

    /// Pinned against the reference probe in this module's doc: a flat `50`
    /// background with a `200` box comes back entirely `50`.
    #[test]
    fn a_flat_surrounding_erases_the_box_content_entirely() {
        let mut grid = [[50u8; 10]; 10];
        for row in grid.iter_mut().take(7).skip(3) {
            for px in row.iter_mut().take(7).skip(3) {
                *px = 200;
            }
        }
        let mut rows = rows_of(&grid);
        fill_box(&mut rows, Rect { x0: 3, y0: 3, w: 4, h: 4 });
        for row in rows.iter().take(7).skip(3) {
            for &px in row.iter().take(7).skip(3) {
                assert_eq!(px, 50);
            }
        }
    }

    /// Pinned against the three matching columns of the step-function probe
    /// in this module's doc (the fourth column's documented discrepancy is
    /// not asserted here, since it is a known, unresolved gap, not this
    /// crate's own formula disagreeing with itself).
    #[test]
    fn matches_the_measured_step_probe_on_its_agreeing_columns() {
        let mut grid = [[0u8; 10]; 10];
        for row in &mut grid {
            for (x, px) in row.iter_mut().enumerate() {
                *px = if x >= 7 { 100 } else { 0 };
            }
        }
        let mut rows = rows_of(&grid);
        fill_box(&mut rows, Rect { x0: 3, y0: 3, w: 4, h: 4 });
        let expected = [
            (3, 3, 10),
            (4, 3, 16),
            (5, 3, 24),
            (3, 4, 12),
            (4, 4, 20),
            (5, 4, 30),
        ];
        for (x, y, v) in expected {
            assert_eq!(rows[y][x], v, "({x},{y})");
        }
    }

    #[test]
    fn a_degenerate_zero_size_box_is_a_clean_no_op() {
        let f = Filter::new(&Opts {
            x: "5".to_owned(),
            y: "5".to_owned(),
            w: "0".to_owned(),
            h: "5".to_owned(),
            show: false,
        })
        .unwrap();
        assert!(f.resolve_box(20, 20).is_none());
    }

    #[test]
    fn a_box_touching_the_frame_edge_is_clamped_not_rejected() {
        let f = Filter::new(&Opts {
            x: "5".to_owned(),
            y: "5".to_owned(),
            w: "1000".to_owned(),
            h: "1000".to_owned(),
            show: false,
        })
        .unwrap();
        let b = f.resolve_box(20, 20).unwrap();
        assert_eq!(b.x0 + b.w, 20);
        assert_eq!(b.y0 + b.h, 20);
    }
}
