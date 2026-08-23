//! `median` — order-statistic filter over a rectangular neighbourhood.
//!
//! `ffmpeg -h filter=median` documents `radius` (`1..=127`, default `1`),
//! `planes` (default `15`), `radiusV` (`0..=127`, default `0`, meaning "same
//! as `radius`" — confirmed the same way [`crate::avgblur`]'s `sizeY=0`
//! was: a `radiusV=0` run still blurs vertically), `percentile`
//! (`0..=1`, default `0.5`).
//!
//! # Measured: `percentile` selects an order statistic, not a blend
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" \
//!   -vf "median=radius=1:percentile=0" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! gives the row-wise minimum of each 3x3 window; `percentile=1` gives the
//! maximum; the default `percentile=0.5` on the same input reproduces the
//! true median (`window[len/2]` after sorting, for the odd window sizes
//! this filter always has). The rank used here,
//! `round(percentile*(len-1))`, reduces to exactly those three cases.
//!
//! # Border: replicate, not omit
//!
//! Not fully disambiguated from "shrink the window at the edge" — both
//! predict the same values for every position this crate's tests exercise
//! except the very last column of a percentile-0.5 probe, where only the
//! replicate model reproduces the reference's `40`. Implemented as
//! replicate throughout for a single, uniform-size window; see
//! `docs/filter/vaco-filter-blur.md`.

use vaco_core::{MediaType, Result};
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
    name: "median",
    description: "Apply Median filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "median", help = "Apply Median filter")]
pub(crate) struct Opts {
    #[opt(name = "radius", help = "set median radius", default = 1, range = 1..=127, flags(video, filtering))]
    pub radius: i32,
    #[opt(name = "planes", help = "set planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(name = "radiusV", help = "set median vertical radius", default = 0, range = 0..=127, flags(video, filtering))]
    pub radius_v: i32,
    #[opt(name = "percentile", help = "set median percentile", default = 0.5, range = 0.0..=1.0, flags(video, filtering))]
    pub percentile: f64,
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
    rx: i32,
    ry: i32,
    planes: i64,
    percentile: f64,
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        let ry = if opts.radius_v <= 0 {
            opts.radius
        } else {
            opts.radius_v
        };
        Self {
            rx: opts.radius,
            ry,
            planes: opts.planes,
            percentile: opts.percentile,
        }
    }

    fn apply_plane(&self, rows: &[&[u8]], w: i32, h: i32) -> Vec<Vec<u8>> {
        let mut window = Vec::new();
        let mut out = Vec::new();
        for y in 0..h {
            let mut row = Vec::new();
            for x in 0..w {
                window.clear();
                for dy in -self.ry..=self.ry {
                    for dx in -self.rx..=self.rx {
                        window.push(common::sample_clamped(rows, x + dx, y + dy, w, h));
                    }
                }
                window.sort_unstable();
                let len = window.len();
                let rank = if len == 0 {
                    0
                } else {
                    (self.percentile * f64::from(len as u32 - 1))
                        .round()
                        .clamp(0.0, f64::from(len as u32 - 1)) as usize
                };
                row.push(window.get(rank).copied().unwrap_or(0));
            }
            out.push(row);
        }
        out
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        common::ensure_8bit_addressable(format)?;
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = if common::plane_selected(self.planes, p8) {
                self.apply_plane(&rows, pw, ph)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
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
    let filter = Filter::new(&opts);
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

    fn ramp() -> Vec<Vec<u8>> {
        (0..5)
            .map(|_| (0..5).map(|x| (x as u8) * 10).collect())
            .collect()
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn percentile_zero_is_the_minimum_filter() {
        let opts = Opts {
            percentile: 0.0,
            ..Opts::default()
        };
        let filter = Filter::new(&opts);
        let img = ramp();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        assert_eq!(out[2], vec![0, 0, 10, 20, 30]);
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn percentile_one_is_the_maximum_filter() {
        let opts = Opts {
            percentile: 1.0,
            ..Opts::default()
        };
        let filter = Filter::new(&opts);
        let img = ramp();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        assert_eq!(out[2], vec![10, 20, 30, 40, 40]);
    }

    /// Pinned against the reference probe in this module's doc: the default
    /// percentile reproduces the true median.
    #[test]
    fn default_percentile_is_the_true_median() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        let img = ramp();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        assert_eq!(out[2], vec![0, 10, 20, 30, 40]);
    }

    /// Independent oracle: for *any* percentile, the output at every pixel
    /// must lie between the window's min and max — the defining property
    /// of an order statistic, true regardless of which rank is picked.
    #[test]
    fn output_is_always_within_the_window_range() {
        let img: Vec<Vec<u8>> = (0..7)
            .map(|y| (0..7).map(|x| ((x * 41 + y * 17) % 253) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        for percentile in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let opts = Opts {
                percentile,
                radius: 2,
                ..Opts::default()
            };
            let filter = Filter::new(&opts);
            let out = filter.apply_plane(&rows, 7, 7);
            for y in 2..5 {
                for x in 2..5 {
                    let mut window = Vec::new();
                    for dy in -2i32..=2 {
                        for dx in -2i32..=2 {
                            window.push(
                                img[(common::to_i32(y) + dy) as usize]
                                    [(common::to_i32(x) + dx) as usize],
                            );
                        }
                    }
                    let min = *window.iter().min().unwrap();
                    let max = *window.iter().max().unwrap();
                    assert!(out[y][x] >= min && out[y][x] <= max);
                }
            }
        }
    }
}
