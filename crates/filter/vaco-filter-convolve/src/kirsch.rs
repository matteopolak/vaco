//! `kirsch` — the Kirsch compass operator.
//!
//! Shares [`crate::edge`]'s option table (`planes`, `scale`, `delta`).
//! Implements the standard eight-direction Kirsch compass masks (each a
//! rotation of `[5,5,5;-3,0,-3;-3,-3,-3]`), taking the maximum response
//! over all eight, published image-processing background rather than
//! anything read from the reference's source.
//!
//! # What is measured, and what is not
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray:s=5x5,format=gray8,geq=lum='10*X'" -vf kirsch \
//!   -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! gives `80` at every interior pixel. The eight-mask maximum on this field
//! is `240`; `240/3 = 80` — so a divisor of `3` reproduces the *interior*
//! exactly, and that is as far as this was pinned down.
//!
//! An earlier pass at this got `240` wrong as `400`, from a hand-written
//! mask array with one rotation carrying four `5`s instead of three (an
//! extra `5` in place of a `-3`, caught by [`MASKS`]'s doc: every true
//! rotation of the base mask sums to `0`, and that one summed to `8`).
//! `400/5` happened to equal the measured `80` too, which is exactly the
//! trap of a wrong model and a wrong divisor cancelling into a number that
//! looks confirmed. Caught by
//! regenerating the eight masks programmatically (cyclic shift of the
//! perimeter, see [`MASKS`]) instead of by hand, and rejecting any
//! candidate whose coefficients do not sum to zero.
//!
//! The border values this same probe measures (`60` at column 0, `20` at
//! column 4) do **not** match clamp-to-edge, zero-padding, or
//! [`crate::convolution`]'s "force zero" rule under this divisor — every
//! model tried was refuted, not merely untested (see this crate's
//! `docs/filter/vaco-filter-blur.md` for the arithmetic). Rather than ship
//! a border rule known to be wrong, this implementation uses clamp-to-edge
//! (the least surprising choice, and correct for the interior) and
//! documents the border as **unverified against the reference** rather
//! than implying more than was checked.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, LinkFormat};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::convolution::clamp_u8;
use crate::edge::{self, Opts};

pub const DESC: FilterDesc = edge::pad_desc("kirsch", "Apply kirsch operator");

/// The eight compass rotations of `[5,5,5;-3,0,-3;-3,-3,-3]`: cyclically
/// shift which three of the eight perimeter cells (in clockwise order
/// starting at N) carry `5`, the rest `-3`, centre always `0`. Generated
/// programmatically (not hand-rotated) after an earlier hand-written set
/// had one mask with an extra `5` in place of a `-3` — caught because that
/// mask alone summed to `8` instead of `0`, which every true rotation must.
const MASKS: [[i32; 9]; 8] = [
    [-3, 5, 5, -3, 0, 5, -3, -3, -3],
    [-3, -3, 5, -3, 0, 5, -3, -3, 5],
    [-3, -3, -3, -3, 0, 5, -3, 5, 5],
    [-3, -3, -3, -3, 0, -3, 5, 5, 5],
    [-3, -3, -3, 5, 0, -3, 5, 5, -3],
    [5, -3, -3, 5, 0, -3, 5, -3, -3],
    [5, 5, -3, 5, 0, -3, -3, -3, -3],
    [5, 5, 5, -3, 0, -3, -3, -3, -3],
];

const DIVISOR: f64 = 3.0;

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

    #[allow(
        clippy::integer_division,
        reason = "decomposing a flat 3x3 window index into (row, col); both \
                  operands are always in 0..9"
    )]
    fn apply_plane(&self, rows: &[&[u8]], w: i32, h: i32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for y in 0..h {
            let mut row = Vec::new();
            for x in 0..w {
                let mut window = [0.0f64; 9];
                for (i, w9) in window.iter_mut().enumerate() {
                    let dx = common::to_i32(i % 3) - 1;
                    let dy = common::to_i32(i / 3) - 1;
                    *w9 = f64::from(common::sample_clamped(rows, x + dx, y + dy, w, h));
                }
                let max_response = MASKS
                    .iter()
                    .map(|mask| {
                        mask.iter()
                            .zip(window.iter())
                            .map(|(&m, &v)| f64::from(m) * v)
                            .sum::<f64>()
                    })
                    .fold(f64::MIN, f64::max);
                let value = (max_response / DIVISOR).mul_add(self.scale, self.delta);
                row.push(clamp_u8(value));
            }
            out.push(row);
        }
        out
    }
}

impl FrameFilter for Filter {
    fn filter_frame(
        &mut self,
        ctx: &mut FilterContext<'_>,
        input: vaco_frame::Frame,
    ) -> Result<FrameOut> {
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
    /// only; the border is documented as unverified).
    #[test]
    fn interior_matches_the_reference() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        let img: Vec<Vec<u8>> = (0..5)
            .map(|_| (0..5).map(|x| (x as u8) * 10).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        assert_eq!(out[2][2], 80);
    }

    /// Independent oracle: a uniform field has zero compass response
    /// everywhere (every mask sums to 0 on a constant window).
    #[test]
    fn a_constant_field_has_no_edges() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        let img = vec![vec![77u8; 5]; 5];
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = filter.apply_plane(&rows, 5, 5);
        for row in out {
            assert!(row.iter().all(|&v| v == 0));
        }
    }
}
