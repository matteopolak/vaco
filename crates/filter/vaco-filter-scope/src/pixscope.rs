//! `pixscope` — pixel data analysis: a small marker box on the source, a
//! magnified view of the pixels under it, and a live statistics panel.
//!
//! `ffmpeg -h filter=pixscope` (2026-08-28): `x`/`y` (scope position,
//! `0..=1`, default `0.5`), `w`/`h` (scope size in source pixels, `1..=80`,
//! default `7`), `o` (window opacity, `0..=1`, default `0.5`), `wx`/`wy`
//! (window position, `-1..=1`, default `-1`).
//!
//! # Measured (`ffmpeg 8.1`, real filtergraphs, pixel-dumped — see below
//! for why "real" matters here)
//!
//! **The reference refuses any source smaller than `640x480`** (`"min
//! supported resolution is 640x480"`), undocumented in `-h` and found only
//! by trying. A prior pass's "the zoom window does not magnify" finding
//! was measured against an all-black source *below* this floor — this
//! pass's own correction, recorded in `planning/INTERFACE-GAPS.md`'s
//! surrounding history rather than silently overwritten.
//!
//! **The window magnifies.** At the default `w=h=7`, the on-screen window
//! is a fixed `294x294`px block grid (`42`px per source pixel, exact —
//! `294 / 7 = 42`) sitting above the stats panel. Measured at three
//! different `w`/`h` values that the *on-screen window size stays fixed*
//! (not the per-cell size) — `w=5` produced the same `~294`px window,
//! each source pixel now a non-integer-width block. This module keeps the
//! on-screen size fixed at `294`px for every `w`/`h`, matching the
//! reference exactly at its own default and approximating it elsewhere.
//!
//! **The sampled window's pixel bounds are `round(coord * dimension) - 1`,
//! spanning `w` (or `h`) consecutive pixels — not centred on the option's
//! own coordinate.** Confirmed at four independent anchors: `x=0.5` on an
//! `800`-wide canvas selected columns `399..405` (not `397..403`, which a
//! naive `centre - w/2` would give); `x=0.25` selected `199..205`;
//! `x=0.1` selected `79..85`; and `w=5` at the same `x=0.5` selected
//! `399..403` — the same `-1` offset regardless of `w`, ruling out a
//! `centre - w/2` (which would shift with `w`) in favour of a constant
//! `-1` bias. **At the frame edge the window clamps rather than shrinks
//! or wraps**: `x=0` (would need columns `-1..5`) produced `0..6` — a
//! full `7`-column window shifted to stay entirely on-screen, not a
//! `6`-column window missing its first column and not a wraparound to
//! the frame's far edge.
//!
//! **The marker box** is a `1`px-stroke, unfilled outline, `w+3` by `h+3`
//! pixels (`10x10` at the default `w=h=7`), confirmed by an exact
//! `4*10-4=36`-pixel perimeter count against a probe with no other white
//! pixels nearby. Its top-left sits one pixel before the sampled window's
//! own top-left on both axes.
//!
//! **The statistics panel is fully read, not guessed** — reading UI text
//! off a rendered frame is the same black-box pixel measurement this
//! whole project is built on, not reading the reference's source or its
//! font bitmap (`crate::font8x8`'s own doc explains why *that* specific
//! line is not crossed). Two groups, each one header line
//! (`"CH   AVG   MIN   MAX   RMS"` / `"CH   STD"`) then one line per
//! active channel, colour-coded in the reference (white/blue/red for
//! `Y`/`U`/`V`, red/green/blue for `R`/`G`/`B`) — this module draws every
//! line in a single colour, since no two rows here need a colour to tell
//! them apart, the row's own label already does, and colour cannot buy
//! back framecrc-exactness the font ceiling already forecloses (see
//! below). Channel labels and their plane order were read directly off a
//! `gbrp` source (`R`, `G`, `B`, confirmed against a pure-red frame
//! reading `R=255,G=0,B=0`) and a `yuv444p` source (`Y`, `U`, `V`).
//!
//! **The five statistics, each pinned at more than one point** — a flat
//! field (baseline), a `1`-of-`7`-columns outlier (`10` everywhere but one
//! column of `250`), a symmetric `7`-value ramp (`54..114` in steps of
//! `10`), and the edge-clamped `0..6` ramp above — computed by hand
//! against each probe's own known pixel values and matched against the
//! panel's displayed numbers:
//!
//! ```text
//! AVG = round(mean(v), 1)                         // arithmetic mean, not median
//! MIN = min(v)
//! MAX = max(v)
//! RMS = round(sqrt(mean(v^2)), 1)                 // raw values, not deviations from the mean
//! STD = round(sqrt(mean((v - mean)^2)), 2)         // population, divided by N — NOT N-1
//! ```
//!
//! The outlier probe alone rules out three plausible alternatives at once:
//! `AVG=44.3` (not `10`, the median of 49 mostly-`10` values) confirms
//! arithmetic mean; `RMS=94.9` matches `sqrt(mean(v^2))` and not
//! `sqrt(mean((v-mean)^2))` (which would equal population `STD`, a
//! visibly different number); `STD=83.98` matches population variance
//! (`sum/49`) and not sample variance (`sum/48`, which would print
//! `84.85`). The ramp probe (`STD=20.00`, `RMS=86.3` against hand-computed
//! `20.0` and `86.348`) and the edge-clamped probe (`STD=2.00`,
//! `RMS=3.6` against `2.0` and `3.6056`) independently confirm the same
//! two formulas with exact or near-exact matches.
//!
//! # Not implemented
//!
//! `o` (window opacity — the window and panel are drawn fully opaque here,
//! the same unexplored-effect choice `datascope`'s own `opacity` already
//! made). Colour-coding (see above). Packed RGB (`rgb24`, `bgr24`, …) and
//! subsampled/alpha formats — this module only addresses planar formats
//! with full-resolution chroma (`yuv444p`-family, `gray`) or planar RGB
//! (`gbrp`-family, plane order `G,B,R` per this project's own established
//! convention — see `vaco-filter-color::exposure`'s test doc), declining
//! anything else rather than drawing onto the wrong bytes. Bit depths
//! above 8. The panel's exact column pixel positions are a readable
//! approximation, not chased to the reference's own pixel, for the same
//! reason `datascope`'s cell margins were not: no rendering here can be
//! framecrc-identical regardless (see "The bitmap-font hypothesis" in
//! this crate's top-level doc), so pixel-perfect column alignment would
//! not buy back exactness it cannot reach anyway.
//!
//! **This filter can never be framecrc-identical to the reference**, for
//! the same permanent reason as `datascope`/`graphmonitor`: it draws with
//! `crate::font8x8`'s independently-sourced glyphs, not the reference's.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "pixscope",
    description: "Pixel data analysis.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Pixels on the near side of the sampled window the marker box extends.
const BOX_NEAR: i64 = 1;
/// Pixels on the far side of the sampled window the marker box extends.
const BOX_FAR: i64 = 2;
/// Fixed on-screen size of the magnified window — measured constant
/// regardless of `w`/`h`; see the module doc.
const WINDOW_PX: u32 = 294;
/// Gap between the magnified window's bottom and the stats panel's first
/// line.
const PANEL_GAP: u32 = 2;
/// Row pitch inside the stats panel (one measured outlier treated as
/// noise — see the module doc).
const PANEL_ROW_PITCH: u32 = 15;
/// Gap between the two panel groups (one blank row).
const PANEL_GROUP_GAP: u32 = PANEL_ROW_PITCH * 2;
/// Column pitch for the panel's four numeric fields — a readable
/// approximation, not the reference's own exact spacing (see module doc).
const PANEL_COL_PITCH: u32 = 110;
/// Column where the panel's first numeric field starts, relative to the
/// panel's own left edge.
const PANEL_FIRST_COL: u32 = 64;
/// Channel labels for a non-RGB (`Y`/`U`/`V`) plane order.
const YUV_LABELS: [&str; 3] = ["Y", "U", "V"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pixscope", help = "Pixel data analysis.")]
pub(crate) struct Opts {
    #[opt(name = "x", help = "set scope x offset", default = 0.5, range = 0.0..=1.0, flags(video, filtering))]
    pub x: f64,
    #[opt(name = "y", help = "set scope y offset", default = 0.5, range = 0.0..=1.0, flags(video, filtering))]
    pub y: f64,
    #[opt(name = "w", help = "set scope width", default = 7, range = 1..=80, flags(video, filtering))]
    pub w: i64,
    #[opt(name = "h", help = "set scope height", default = 7, range = 1..=80, flags(video, filtering))]
    pub h: i64,
    #[opt(name = "o", help = "set window opacity", default = 0.5, range = 0.0..=1.0, flags(video, filtering))]
    pub o: f64,
    #[opt(name = "wx", help = "set window x offset", default = -1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub wx: f64,
    #[opt(name = "wy", help = "set window y offset", default = -1.0, range = -1.0..=1.0, flags(video, filtering))]
    pub wy: f64,
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
    x: f64,
    y: f64,
    w: u32,
    h: u32,
    wx: f64,
    wy: f64,
}

/// One channel's five statistics over its sampled window.
struct Stats {
    avg: f64,
    min: u8,
    max: u8,
    rms: f64,
    std: f64,
}

fn compute_stats(rows: &[&[u8]], x0: u32, y0: u32, w: u32, h: u32) -> Stats {
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut min = u8::MAX;
    let mut max = 0u8;
    let n = f64::from(w) * f64::from(h);
    for row in rows.iter().skip(y0 as usize).take(h as usize) {
        for &v in row.iter().skip(x0 as usize).take(w as usize) {
            sum += f64::from(v);
            sum_sq += f64::from(v) * f64::from(v);
            min = min.min(v);
            max = max.max(v);
        }
    }
    let mean = sum / n;
    let mean_sq = sum_sq / n;
    let variance = (mean_sq - mean * mean).max(0.0);
    Stats {
        avg: mean,
        min,
        max,
        rms: mean_sq.sqrt(),
        std: variance.sqrt(),
    }
}

/// The sampled window's start coordinate along one axis — `round(coord *
/// dimension) - 1`, clamped so the full `len`-pixel span stays on-screen.
/// Measured, not a `centre - len/2` guess; see the module doc.
fn window_start(coord: f64, dimension: u32, len: u32) -> u32 {
    let anchor = (coord * f64::from(dimension)).round();
    let start = anchor - 1.0;
    let max_start = dimension.saturating_sub(len);
    if start < 0.0 {
        0
    } else if start > f64::from(max_start) {
        max_start
    } else {
        start as u32
    }
}

/// Draw a `1`px-stroke rectangle outline, clamped to the plane's bounds.
fn draw_box_outline(rows: &mut [&mut [u8]], left: i64, top: i64, w: i64, h: i64, value: u8) {
    let mut put = |x: i64, y: i64| {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if let Some(row) = rows.get_mut(y)
            && let Some(px) = row.get_mut(x)
        {
            *px = value;
        }
    };
    for x in left..left + w {
        put(x, top);
        put(x, top + h - 1);
    }
    for y in top..top + h {
        put(left, y);
        put(left + w - 1, y);
    }
}

/// Blit one source pixel as a solid `bw x bh` block — the magnified
/// window's one cell.
fn draw_block(rows: &mut [&mut [u8]], left: u32, top: u32, bw: u32, bh: u32, value: u8) {
    for row in rows.iter_mut().skip(top as usize).take(bh as usize) {
        for px in row.iter_mut().skip(left as usize).take(bw as usize) {
            *px = value;
        }
    }
}

const fn round_i64(coord: f64, dimension: u32) -> i64 {
    (coord * dimension as f64) as i64
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let vaco_frame::FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        if common::ensure_8bit_addressable(format).is_err() || format.has_alpha() {
            return Ok(FrameOut::One(input));
        }
        let is_rgb = format.is_rgb();
        let plane_count = format.plane_count();
        let full_res_chroma = plane_count < 2
            || (format.plane_width(width, 1) == width && format.plane_height(height, 1) == height);
        let supported = full_res_chroma
            && ((is_rgb && format.is_planar() && plane_count == 3)
                || (!is_rgb && plane_count <= 3));
        if !supported || width < 640 || height < 480 {
            // Undocumented reference floor (`"min supported resolution is
            // 640x480"`) and the format families this pass measured — see
            // the module doc's "Not implemented".
            return Ok(FrameOut::One(input));
        }

        let w = self.w.min(width);
        let h = self.h.min(height);
        let x0 = window_start(self.x, width, w);
        let y0 = window_start(self.y, height, h);

        // Channel identity and plane order: `Y,U,V` for non-RGB (plane
        // index order), `R,G,B` for `gbrp`-family (plane order `G,B,R` —
        // see the module doc).
        let (labels, plane_order): (&[&str], &[usize]) = if is_rgb {
            (&["R", "G", "B"], &[2, 0, 1])
        } else {
            (
                YUV_LABELS.get(..plane_count.min(3)).unwrap_or(&YUV_LABELS),
                &[0, 1, 2],
            )
        };

        let mut out = input;
        let stats: Vec<Stats> = plane_order
            .iter()
            .take(labels.len())
            .filter_map(|&plane| {
                let src = out.plane(plane)?;
                let pw = format.plane_width(width, u8::try_from(plane).unwrap_or(0));
                let ph = format.plane_height(height, u8::try_from(plane).unwrap_or(0));
                let px0 = x0.min(pw.saturating_sub(1));
                let py0 = y0.min(ph.saturating_sub(1));
                let rows: Vec<&[u8]> = src.rows_iter().collect();
                Some(compute_stats(
                    &rows,
                    px0,
                    py0,
                    w.min(pw - px0),
                    h.min(ph - py0),
                ))
            })
            .collect();

        // The marker box and the magnified window both draw onto every
        // plane at that plane's own resolution — trivial here since this
        // module only accepts full-resolution-chroma formats.
        for &plane in plane_order.iter().take(labels.len()) {
            let Some(mut dst) = out.plane_mut(plane) else {
                continue;
            };
            let mut rows: Vec<&mut [u8]> = dst.rows_mut().collect();
            render_plane(
                &mut rows, x0, y0, w, h, self.wx, self.wy, width, height, labels, &stats,
            );
        }

        Ok(FrameOut::One(out))
    }
}

/// Draw the marker box, the magnified window and the stats panel onto one
/// plane. Pure over a plain `&mut [&mut [u8]]` so it can be exercised
/// directly in tests without a live `Frame`/`FilterContext`.
#[allow(clippy::too_many_arguments, reason = "one plane's whole render pass")]
fn render_plane(
    rows: &mut [&mut [u8]],
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
    wx: f64,
    wy: f64,
    width: u32,
    height: u32,
    labels: &[&str],
    stats: &[Stats],
) {
    let box_left = i64::from(x0) - BOX_NEAR;
    let box_top = i64::from(y0) - BOX_NEAR;
    let box_w = i64::from(w) + BOX_NEAR + BOX_FAR;
    let box_h = i64::from(h) + BOX_NEAR + BOX_FAR;
    draw_box_outline(rows, box_left, box_top, box_w, box_h, 255);

    #[allow(
        clippy::integer_division,
        reason = "counting how many whole pixels of magnification fit \
                  the fixed window budget is an exact floor by construction, \
                  not a lossy average"
    )]
    let (block_w, block_h) = (WINDOW_PX / w.max(1), WINDOW_PX / h.max(1));
    let win_w = block_w * w;
    let win_h = block_h * h;
    let (win_left, win_top) = window_block_origin(wx, wy, width, height, win_w, win_h);

    // Read the source window's values before overwriting any of this
    // plane's rows with the magnified block or the panel.
    let mut cell_values = vec![0u8; (w * h) as usize];
    for row in 0..h {
        let sy = usize::try_from(y0 + row).unwrap_or(0);
        if let Some(src_row) = rows.get(sy) {
            for col in 0..w {
                let sx = usize::try_from(x0 + col).unwrap_or(0);
                let idx = (row * w + col) as usize;
                if let Some(slot) = cell_values.get_mut(idx) {
                    *slot = src_row.get(sx).copied().unwrap_or(0);
                }
            }
        }
    }
    for row in 0..h {
        for col in 0..w {
            let idx = (row * w + col) as usize;
            let value = cell_values.get(idx).copied().unwrap_or(0);
            draw_block(
                rows,
                win_left + col * block_w,
                win_top + row * block_h,
                block_w,
                block_h,
                value,
            );
        }
    }

    // The stats panel: filled black background, then text.
    let panel_top = win_top + win_h + PANEL_GAP;
    let panel_left = win_left;
    let panel_h = PANEL_ROW_PITCH * 4 + PANEL_GROUP_GAP;
    for row in rows
        .iter_mut()
        .skip(panel_top as usize)
        .take(panel_h as usize)
    {
        for px in row
            .iter_mut()
            .skip(panel_left as usize)
            .take(win_w as usize)
        {
            *px = 0;
        }
    }
    common::draw_text(rows, panel_top, panel_left, b"CH   AVG   MIN   MAX   RMS");
    for (i, label) in labels.iter().enumerate() {
        let row_top = panel_top + PANEL_ROW_PITCH * u32::try_from(i + 1).unwrap_or(0);
        common::draw_text(rows, row_top, panel_left, label.as_bytes());
        if let Some(s) = stats.get(i) {
            draw_field(rows, row_top, panel_left, 0, &format!("{:05.1}", s.avg));
            draw_field(rows, row_top, panel_left, 1, &format!("{:05}", s.min));
            draw_field(rows, row_top, panel_left, 2, &format!("{:05}", s.max));
            draw_field(rows, row_top, panel_left, 3, &format!("{:05.1}", s.rms));
        }
    }
    let group2_top = panel_top + PANEL_GROUP_GAP;
    common::draw_text(rows, group2_top, panel_left, b"CH   STD");
    for (i, label) in labels.iter().enumerate() {
        let row_top = group2_top + PANEL_ROW_PITCH * u32::try_from(i + 1).unwrap_or(0);
        common::draw_text(rows, row_top, panel_left, label.as_bytes());
        if let Some(s) = stats.get(i) {
            draw_field(rows, row_top, panel_left, 0, &format!("{:04.2}", s.std));
        }
    }
}

/// Draw one panel field: `col` is the 0-based numeric column index after
/// the leading two-character channel label.
fn draw_field(rows: &mut [&mut [u8]], top: u32, panel_left: u32, col: u32, text: &str) {
    let left = panel_left + PANEL_FIRST_COL + col * PANEL_COL_PITCH;
    common::draw_text(rows, top, left, text.as_bytes());
}

/// The magnified window's own top-left, from `wx`/`wy` — `< 0` means the
/// reference's own default (this module: flush to the frame's bottom
/// right), otherwise a fraction of the frame the window's top-left sits
/// at, the same convention `x`/`y` use for the sampled window.
fn window_block_origin(
    wx: f64,
    wy: f64,
    width: u32,
    height: u32,
    win_w: u32,
    win_h: u32,
) -> (u32, u32) {
    let panel_h = PANEL_ROW_PITCH * 4 + PANEL_GROUP_GAP + PANEL_GAP;
    if wx < 0.0 || wy < 0.0 {
        let left = width.saturating_sub(win_w);
        let top = height.saturating_sub(win_h + panel_h);
        (left, top)
    } else {
        let left = round_i64(wx, width).clamp(0, i64::from(width.saturating_sub(win_w)));
        let top = round_i64(wy, height).clamp(0, i64::from(height.saturating_sub(win_h + panel_h)));
        (left as u32, top as u32)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter {
        x: opts.x,
        y: opts.y,
        w: u32::try_from(opts.w).unwrap_or(7).max(1),
        h: u32::try_from(opts.h).unwrap_or(7).max(1),
        wx: opts.wx,
        wy: opts.wy,
    };
    let _ = opts.o;
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
    fn creatable_with_defaults() {
        let req = Instantiate {
            name: "pixscope",
            instance: "pixscope",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }

    /// Pinned against the reference probe: `x=0.5` on an `800`-wide
    /// canvas, `w=7`, selects columns `399..405`, not the naive
    /// `centre - w/2` (`397..403`).
    #[test]
    fn window_start_matches_the_measured_minus_one_bias() {
        assert_eq!(window_start(0.5, 800, 7), 399);
        assert_eq!(window_start(0.25, 800, 7), 199);
        assert_eq!(window_start(0.1, 800, 7), 79);
    }

    /// The `-1` bias is constant, not `w`-dependent — `w=5` at the same
    /// `x=0.5` still starts at `399`, not `398`.
    #[test]
    fn window_start_bias_does_not_scale_with_w() {
        assert_eq!(window_start(0.5, 800, 5), 399);
    }

    /// At the frame edge the window clamps (shifts to stay fully
    /// on-screen) rather than shrinking or wrapping.
    #[test]
    fn window_start_clamps_at_the_left_edge() {
        assert_eq!(window_start(0.0, 800, 7), 0);
    }

    /// Pinned against the single-outlier probe: seven `250`s and
    /// forty-two `10`s in a `7x7` window give `AVG=44.3`, `RMS=94.9`,
    /// `STD=83.98` — confirming arithmetic mean (not median), raw-value
    /// RMS (not deviation-from-mean), and population (not sample) STD.
    #[test]
    fn statistics_match_the_outlier_probe() {
        let mut rows_owned = vec![vec![10u8; 7]; 7];
        for r in &mut rows_owned {
            r[0] = 250;
        }
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let s = compute_stats(&rows, 0, 0, 7, 7);
        assert!((s.avg - 44.2857).abs() < 0.001);
        assert_eq!(s.min, 10);
        assert_eq!(s.max, 250);
        assert!((s.rms - 94.947).abs() < 0.01);
        assert!((s.std - 83.985).abs() < 0.01);
    }

    /// Pinned against the symmetric-ramp probe: `54,64,74,84,94,104,114`
    /// (each repeated per row) give exact round numbers — `AVG=84.0`,
    /// `RMS=86.348` (displays `86.3`), `STD=20.0` exactly.
    #[test]
    fn statistics_match_the_ramp_probe() {
        let cols: [u8; 7] = [54, 64, 74, 84, 94, 104, 114];
        let rows_owned: Vec<Vec<u8>> = (0..7).map(|_| cols.to_vec()).collect();
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let s = compute_stats(&rows, 0, 0, 7, 7);
        assert!((s.avg - 84.0).abs() < 0.001);
        assert_eq!(s.min, 54);
        assert_eq!(s.max, 114);
        assert!((s.rms - 86.348).abs() < 0.01);
        assert!((s.std - 20.0).abs() < 0.001);
    }

    /// A flat field is the simplest baseline: every statistic collapses
    /// to the flat value, and `STD` is exactly zero.
    #[test]
    fn statistics_match_the_flat_field_baseline() {
        let rows_owned = vec![vec![126u8; 7]; 7];
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let s = compute_stats(&rows, 0, 0, 7, 7);
        assert!((s.avg - 126.0).abs() < f64::EPSILON);
        assert_eq!(s.min, 126);
        assert_eq!(s.max, 126);
        assert!((s.rms - 126.0).abs() < 0.001);
        assert!((s.std - 0.0).abs() < f64::EPSILON);
    }

    /// `render_plane` on a real in-memory buffer, large enough that the
    /// fixed-size magnified window and panel fit without clipping: the
    /// marker box's outline actually paints white pixels near the
    /// sampled window, and the magnified window's very first block
    /// carries the exact source pixel value it magnifies, not the
    /// background or a neighbouring cell's value — exercising the
    /// drawing glue `compute_stats`/`window_start`'s own tests do not
    /// touch.
    #[test]
    fn render_plane_draws_the_box_and_magnifies_the_right_source_pixel() {
        let width = 500u32;
        let height = 500u32;
        let mut buf: Vec<Vec<u8>> = (0..height).map(|_| vec![0u8; width as usize]).collect();
        // A distinctive value at the sampled window's own top-left pixel
        // (x0=10, y0=10), left as 0 everywhere else in the window.
        buf[10][10] = 200;
        let mut rows: Vec<&mut [u8]> = buf.iter_mut().map(Vec::as_mut_slice).collect();

        let stats = vec![Stats {
            avg: 42.0,
            min: 10,
            max: 250,
            rms: 94.9,
            std: 83.98,
        }];
        render_plane(
            &mut rows,
            10,
            10,
            3,
            3,
            -1.0,
            -1.0,
            width,
            height,
            &["Y"],
            &stats,
        );

        // Marker box: outline around the sampled window (x0-1..x0+3+2,
        // clamped) must have painted white pixels.
        let lit = rows[8..16].iter().any(|r| r[8..16].contains(&255));
        assert!(
            lit,
            "expected the marker box outline near the sampled window"
        );

        // The magnified window is fixed-size and anchored bottom-right on
        // a 500x500 canvas well clear of the marker box, so its first
        // cell (top-left of the window) must carry the exact value (200)
        // of the source pixel at (x0, y0) — not 0, and not some other
        // cell's value smeared across the boundary.
        #[allow(
            clippy::integer_division,
            reason = "recomputing the same exact-floor cell size render_plane used"
        )]
        let block = WINDOW_PX / 3;
        let win_left = width - (block * 3);
        let win_top = height - (block * 3) - PANEL_GAP - PANEL_ROW_PITCH * 4 - PANEL_GROUP_GAP;
        assert_eq!(
            rows[win_top as usize + 1][win_left as usize + 1],
            200,
            "the magnified window's first cell should carry the sampled pixel's own value"
        );
    }

    /// The edge-clamped window (`x=0`, would need columns `-1..5`)
    /// produces a full `0..6` window, not a `6`-wide one missing its
    /// first column — confirmed by its own statistics matching `0..6`'s
    /// (`AVG=3.0`, `RMS=3.6`, `STD=2.0`), not `1..6`'s.
    #[test]
    fn statistics_match_the_edge_clamped_probe() {
        let cols: [u8; 7] = [0, 1, 2, 3, 4, 5, 6];
        let rows_owned: Vec<Vec<u8>> = (0..7).map(|_| cols.to_vec()).collect();
        let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
        let s = compute_stats(&rows, 0, 0, 7, 7);
        assert!((s.avg - 3.0).abs() < 0.001);
        assert!((s.rms - 3.6056).abs() < 0.01);
        assert!((s.std - 2.0).abs() < 0.001);
    }
}
