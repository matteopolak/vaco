//! `avgblur` — one-pass box average, independently sized per axis.
//!
//! `ffmpeg -h filter=avgblur` documents `sizeX` (default `1`), `planes`
//! (default `15`, all planes) and `sizeY` (default `0`). Measured (corner
//! impulse, same probe as [`crate::boxblur`]'s doc): `sizeY=0` behaves as
//! "same as `sizeX`", not "no vertical blur" — a plain horizontal-only box
//! would leave row 1 at zero for a row-0 impulse, and it does not.
//!
//! # Measured: truncating division, not round-to-nearest
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,0)*eq(Y,0),255,0)'" \
//!   -vf "avgblur=sizeX=1" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! Position `(0,1)`: sum `510`, `510/9 = 56.67`. `avgblur` gives `56`
//! (truncated); [`crate::boxblur`]'s identical probe gives `57` (rounded).
//! Same box-average engine ([`crate::common::box_pass`]), different
//! [`crate::common::Rounding`] — a real divergence between two filters that
//! look interchangeable from their names, not a copy-paste slip.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Rounding};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "avgblur",
    description: "Apply Average Blur filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "avgblur", help = "Apply Average Blur filter")]
pub(crate) struct Opts {
    #[opt(
        name = "sizeX",
        help = "set horizontal size",
        default = 1,
        range = 1..=1024,
        flags(video, filtering)
    )]
    pub size_x: i32,
    #[opt(
        name = "planes",
        help = "set planes to filter",
        default = 15,
        range = 0..=15,
        flags(video, filtering)
    )]
    pub planes: i64,
    #[opt(
        name = "sizeY",
        help = "set vertical size",
        default = 0,
        range = 0..=1024,
        flags(video, filtering)
    )]
    pub size_y: i32,
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
}

impl Filter {
    const fn new(opts: &Opts) -> Self {
        let ry = if opts.size_y <= 0 {
            opts.size_x
        } else {
            opts.size_y
        };
        Self {
            rx: opts.size_x,
            ry,
            planes: opts.planes,
        }
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
            let blurred = if common::plane_selected(self.planes, p8) {
                common::box_pass(&rows, pw, ph, self.rx, self.ry, Rounding::Trunc)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in blurred.iter().enumerate() {
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

    #[test]
    fn size_y_zero_inherits_size_x() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        assert_eq!(filter.rx, 1);
        assert_eq!(filter.ry, 1);
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn corner_impulse_matches_the_reference_with_truncation() {
        let mut img = vec![vec![0u8; 5]; 5];
        if let Some(row) = img.first_mut()
            && let Some(px) = row.first_mut()
        {
            *px = 255;
        }
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = common::box_pass(&rows, 5, 5, 1, 1, Rounding::Trunc);
        assert_eq!(out[0][0], 113);
        assert_eq!(out[0][1], 56);
    }
}
