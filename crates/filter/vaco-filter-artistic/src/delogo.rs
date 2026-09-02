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
//! observed `57`/`61`. That divergence turned out to be a **wrong weighting
//! model, not a border-sampling detail** — see the next section.
//!
//! # Corrected: it is four-point inverse-distance weighting, not a two-stage blend
//!
//! Re-measured with a *wider* box (`x=3:y=3:w=10:h=6` on a 20x20 frame, same
//! step-function source) specifically because the original `w=4` probe was
//! too narrow to distinguish the two-stage formula above from a simpler
//! one: with only four columns, several different formulas happen to agree
//! on three of them by coincidence. At `w=10` the two-stage formula's error
//! grows across nearly the *whole* box (mean absolute error `6.4`, max
//! `27`, over the 60-pixel box), not just one column — it was never the
//! right shape.
//!
//! The formula that **does** fit, to within rounding, over 54 of the box's
//! 60 pixels (mean absolute error `1.4`, max `17`, confined as described
//! below) is plain inverse-distance weighting from all four border samples
//! at once, no two-stage horizontal/vertical split:
//!
//! ```text
//! dist_left  = x - x0 + 1        dist_right  = (x0+w) - x
//! dist_top   = y - y0 + 1        dist_bottom = (y0+h) - y
//! out(x,y) = (L/dist_left + R/dist_right + T/dist_top + B/dist_bottom)
//!          / (1/dist_left + 1/dist_right + 1/dist_top + 1/dist_bottom)
//! ```
//!
//! Re-confirmed independently in the *vertical* direction (a box fed a
//! top/bottom step instead of left/right reproduces the identical sequence
//! of values transposed), so the four border directions share one formula,
//! not per-axis tuning.
//!
//! **The remaining residual is the entire column or row immediately
//! adjacent to a border whose value differs from the local background**
//! (`dist == 1` on the side facing the contrast) — plain IDW consistently
//! *underestimates* the true output there, by an amount that does not
//! depend on how far along that column/row the pixel is (`13`-`17` counts,
//! fairly flat, not growing toward the corners the way an error compounding
//! from two axes would). On the `w=10:h=6` probe this is the whole `x=12`
//! column (`dist_right == 1` for every row in the box, `6` of the box's
//! `60` pixels); on the original `w=4:h=4` probe it is the whole `x=6`
//! column (`4` of `16` pixels) — the same shape this module's very first
//! probe already reported (`3` of `4` columns exact, the fourth off at
//! every row), now confirmed at a second, larger size rather than
//! superseded by it. Notably, the *matching* axis's own `dist == 1` column
//! (`x=3`, adjacent to `L`) is **not** anomalous when `L` itself equals the
//! local background — the defect tracks which border carries the real
//! discontinuity, not raw geometric distance, which is why a
//! content-independent per-axis correction (average the border value with
//! an IDW estimate computed one step further in) was tried and rejected:
//! it reproduced the anomalous column but also *broke* the non-anomalous
//! one, so it is not the real rule and is not shipped.
//!
//! # Not framecrc-verified
//!
//! This filter is **implemented with a formula now verified as the right
//! *shape* — exact through rounding across the large majority of any given
//! box, including every case the original two-stage formula got right —
//! but not framecrc-verified**, because of the anomalous-column residual
//! above. Same
//! bar `vaco-filter-blur::gblur` documents for the same reason, now with a
//! much smaller and more precisely bounded gap than the formula this
//! replaced.

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
    #[opt(
        name = "show",
        help = "show delogo area",
        default = false,
        flags(video, filtering)
    )]
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
            x_expr: Expr::parse(&opts.x, &bindings)
                .map_err(|e| format!("delogo: bad `x` `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &bindings)
                .map_err(|e| format!("delogo: bad `y` `{e}`"))?,
            w_expr: Expr::parse(&opts.w, &bindings)
                .map_err(|e| format!("delogo: bad `w` `{e}`"))?,
            h_expr: Expr::parse(&opts.h, &bindings)
                .map_err(|e| format!("delogo: bad `h` `{e}`"))?,
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
            // Four-point inverse-distance weighting — see this module's doc
            // for the wide-box probe that replaced the original two-stage
            // horizontal/vertical blend with this formula, and the
            // corner residual it does not resolve. `dist_*` are always
            // `>= 1` by construction (`x`/`y` range over the box interior),
            // so none of these divisions can be by zero.
            let dist_left = f64::from(x - b.x0 + 1);
            let dist_right = f64::from(b.x0 + b.w - x);
            let dist_top = f64::from(y - b.y0 + 1);
            let dist_bottom = f64::from(b.y0 + b.h - y);
            let wl = 1.0 / dist_left;
            let wr = 1.0 / dist_right;
            let wt = 1.0 / dist_top;
            let wb = 1.0 / dist_bottom;
            let value = (f64::from(l) * wl
                + f64::from(r_val) * wr
                + f64::from(t_val) * wt
                + f64::from(b_val) * wb)
                / (wl + wr + wt + wb);
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
        fill_box(
            &mut rows,
            Rect {
                x0: 3,
                y0: 3,
                w: 4,
                h: 4,
            },
        );
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
        fill_box(
            &mut rows,
            Rect {
                x0: 3,
                y0: 3,
                w: 4,
                h: 4,
            },
        );
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

    /// Pinned against the wide-box probe in this module's doc
    /// (`ffmpeg -bitexact -vf delogo=x=3:y=3:w=10:h=6` over a 20x20 frame
    /// with a vertical step at `x=13`, `L=0`/`R=100`/`T=0`/`B=0`): every
    /// interior and non-corner-border pixel matches the reference to the
    /// byte under the four-point inverse-distance formula. The box's own
    /// four corners are excluded here — they carry the documented,
    /// unresolved `~15`-count residual, not asserted as if it were exact.
    #[test]
    fn matches_the_wide_box_probe_away_from_its_anomalous_column() {
        let mut rows: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                (0..20)
                    .map(|x: usize| if x >= 13 { 100 } else { 0 })
                    .collect()
            })
            .collect();
        let b = Rect {
            x0: 3,
            y0: 3,
            w: 10,
            h: 6,
        };
        fill_box(&mut rows, b);
        let expected_rows: [[u8; 10]; 6] = [
            [4, 6, 8, 9, 11, 13, 16, 21, 28, 61],
            [6, 8, 11, 13, 16, 19, 23, 29, 38, 69],
            [6, 9, 12, 15, 18, 21, 26, 32, 42, 71],
            [6, 9, 12, 15, 18, 21, 26, 32, 42, 71],
            [6, 8, 11, 13, 16, 19, 23, 29, 38, 69],
            [4, 6, 8, 9, 11, 13, 16, 21, 28, 61],
        ];
        // Every column except the last (`dist_right == 1`, adjacent to the
        // `R` step) matches to the byte — the same shape this module's doc
        // documents, now confirmed at a size where coincidental agreement
        // across a narrow box can't be the explanation.
        for (ry, row) in expected_rows.iter().enumerate() {
            let y = b.y0 as usize + ry;
            for (rx, &expected) in row.iter().enumerate().take(row.len() - 1) {
                let x = b.x0 as usize + rx;
                assert_eq!(rows[y][x], expected, "({x},{y})");
            }
        }
    }

    /// The box's whole `dist_right == 1` column is not exact (see this
    /// module's doc), but the residual is small and bounded, not unbounded
    /// drift — pinned so a future regression that makes it *worse* is
    /// caught.
    #[test]
    fn wide_box_anomalous_column_residual_stays_within_its_measured_bound() {
        let mut rows: Vec<Vec<u8>> = (0..20)
            .map(|_| {
                (0..20)
                    .map(|x: usize| if x >= 13 { 100 } else { 0 })
                    .collect()
            })
            .collect();
        fill_box(
            &mut rows,
            Rect {
                x0: 3,
                y0: 3,
                w: 10,
                h: 6,
            },
        );
        // Reference values at x=12 (dist_right == 1) for rows 3..=8 are
        // 61, 69, 71, 71, 69, 61.
        let reference = [61i32, 69, 71, 71, 69, 61];
        for (i, &expected) in reference.iter().enumerate() {
            let y = 3 + i;
            let diff = i32::from(rows[y][12]).abs_diff(expected);
            assert!(diff <= 20, "({},{y}) drifted to {}", 12, rows[y][12]);
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
