//! `roberts` — the Roberts cross operator.
//!
//! Shares [`crate::edge`]'s option table (`planes`, `scale`, `delta`) but
//! not its engine: the 2x2 cross does not fit
//! [`crate::convolution::Kernel`]'s odd-square assumption, and — measured
//! below — its border behaviour differs from the three operators that do
//! share that engine.
//!
//! # Measured formula
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" -vf roberts \
//!   -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! gives `14` at *every* pixel, interior and border alike. The standard
//! Roberts cross `Gx=P(x,y)-P(x+1,y+1)`, `Gy=P(x+1,y)-P(x,y+1)` on a field
//! that varies only in `X` gives `Gx=Gy=-10` (or `10`, depending on
//! anchor), `sqrt(10^2+10^2)=14.14 -> 14` — matching at every interior
//! position tried.
//!
//! # A measured anomaly this implementation does not resolve
//!
//! The value stays `14` even at the last row/column, where a `(x+1,y+1)`
//! read must fall outside the frame. Every boundary model tried (clamp,
//! zero-pad, treat as if that neighbour does not exist) predicts something
//! other than `14` at the corner for at least one of the two anchor
//! conventions, and [`crate::convolution`]'s "force zero" rule does not fit
//! either (the value is not `0`). This implementation uses clamp-to-edge,
//! matches the reference in the interior, and is **not proven bit-exact
//! for the border row/column** — see `docs/filter/vaco-filter-blur.md`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, LinkFormat};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::convolution::clamp_u8;
use crate::edge::{self, Opts};

pub const DESC: FilterDesc = edge::pad_desc("roberts", "Apply roberts cross operator");

#[derive(Debug)]
pub(crate) struct Filter {
    planes: i64,
    scale: f64,
    delta: f64,
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        Self {
            planes: opts.planes,
            scale: opts.scale,
            delta: opts.delta,
        }
    }

    fn apply_plane(&self, rows: &[&[u8]], w: i32, h: i32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for y in 0..h {
            let mut row = Vec::new();
            for x in 0..w {
                let p00 = f64::from(common::sample_clamped(rows, x, y, w, h));
                let p10 = f64::from(common::sample_clamped(rows, x + 1, y, w, h));
                let p01 = f64::from(common::sample_clamped(rows, x, y + 1, w, h));
                let p11 = f64::from(common::sample_clamped(rows, x + 1, y + 1, w, h));
                let gx = p00 - p11;
                let gy = p10 - p01;
                row.push(clamp_u8(gx.hypot(gy).mul_add(self.scale, self.delta)));
            }
            out.push(row);
        }
        out
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: vaco_frame::Frame) -> Result<FrameOut> {
        let vaco_frame::FrameData::Video { format, .. } = input.data else {
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

    /// Pinned against the reference probe in this module's doc (interior
    /// only; see the doc for the border anomaly this does not resolve).
    #[test]
    fn interior_matches_the_reference() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        let img: Vec<Vec<u8>> = (0..5).map(|_| (0..5).map(|x| (x as u8) * 10).collect()).collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        assert_eq!(out[2][2], 14);
    }

    /// Independent oracle: a uniform field has zero cross-difference
    /// everywhere.
    #[test]
    fn a_constant_field_has_no_edges() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        let img = vec![vec![90u8; 5]; 5];
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        for row in out {
            assert!(row.iter().all(|&v| v == 0));
        }
    }
}
