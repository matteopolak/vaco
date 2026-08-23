//! `untile` — split one grid frame into a sequence of sub-frames, the
//! inverse of [`crate::tile`].
//!
//! `ffmpeg -h filter=untile` documents only `layout` (`WxH`, default
//! `"6x5"`). No margin/padding/color options exist on this side — the
//! reference's own doc gives untile nothing to undo them with, so this
//! implementation assumes (not separately measured, but the only reading
//! that makes `tile:margin=0:padding=0` followed by `untile` round-trip)
//! that it slices the input into a plain `W`x`H` grid of equal-sized cells
//! with no margin or padding to skip, in the same row-major cell order
//! `tile` uses (verified for `tile` itself; see that module's doc).
//!
//! A width or height not evenly divisible by the grid drops the remainder
//! column/row rather than guessing how to distribute it.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
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
    name: "untile",
    description: "Untile a frame into a sequence of frames",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "untile", help = "Untile a frame into a sequence of frames")]
pub(crate) struct Opts {
    #[opt(
        name = "layout",
        help = "set grid size",
        default = "6x5".to_owned(),
        flags(video, filtering)
    )]
    pub layout: String,
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

fn parse_layout(s: &str) -> std::result::Result<(u32, u32), String> {
    let (w, h) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))
        .ok_or_else(|| format!("untile: bad `layout` `{s}`"))?;
    let w: u32 = w
        .parse()
        .map_err(|_| format!("untile: bad `layout` `{s}`"))?;
    let h: u32 = h
        .parse()
        .map_err(|_| format!("untile: bad `layout` `{s}`"))?;
    if w == 0 || h == 0 {
        return Err(format!("untile: bad `layout` `{s}`"));
    }
    Ok((w, h))
}

#[derive(Debug)]
pub(crate) struct Filter {
    cols: u32,
    rows: u32,
}

impl Filter {
    pub(crate) const fn new(cols: u32, rows: u32) -> Self {
        Self { cols, rows }
    }
}

/// Whole-cell size for a `w`x`h` frame split into a `cols`x`rows` grid;
/// a remainder row/column is dropped, per the module doc.
#[allow(
    clippy::integer_division,
    reason = "whole-cell grid split, not a lossy numeric approximation"
)]
const fn cell_size(w: u32, h: u32, cols: u32, rows: u32) -> (u32, u32) {
    (w / cols, h / rows)
}

fn extract_cell(
    src: &Frame,
    format: PixFmt,
    cell_w: u32,
    cell_h: u32,
    src_x: u32,
    src_y: u32,
    pool: &vaco_frame::FramePool,
) -> Result<Frame> {
    let mut out = pool.acquire_video(format, cell_w, cell_h)?;
    for p in 0..format.plane_count() {
        let plane_idx = p as u8;
        let unit = geom::plane_unit_bytes(format, plane_idx)?;
        let sx = format.plane_width(src_x, plane_idx) as usize;
        let sy = format.plane_height(src_y, plane_idx) as usize;
        let pw = format.plane_width(cell_w, plane_idx) as usize;
        let ph = format.plane_height(cell_h, plane_idx) as usize;
        let Some(src_plane) = src.plane(p) else {
            continue;
        };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        let row_bytes = pw.saturating_mul(unit);
        for row in 0..ph {
            let Some(src_row) = src_plane.row(sy.saturating_add(row)) else {
                continue;
            };
            let start = sx.saturating_mul(unit);
            let Some(src_slice) = src_row.get(start..start.saturating_add(row_bytes)) else {
                continue;
            };
            if let Some(dst_row) = dst_plane.row_mut(row) {
                let n = dst_row.len().min(src_slice.len());
                if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_slice.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    out.pts = src.pts;
    out.time_base = src.time_base;
    out.duration = src.duration;
    out.color = src.color;
    out.flags = src.flags;
    out.sample_aspect_ratio = src.sample_aspect_ratio;
    Ok(out)
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        geom::ensure_addressable(format)?;
        let (cell_w, cell_h) = cell_size(width, height, self.cols, self.rows);
        if cell_w == 0 || cell_h == 0 {
            return Ok(FrameOut::None);
        }
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        for row in 0..self.rows {
            for col in 0..self.cols {
                let frame = extract_cell(
                    &input,
                    format,
                    cell_w,
                    cell_h,
                    col * cell_w,
                    row * cell_h,
                    ctx.pool(),
                )?;
                out.push(frame);
            }
        }
        Ok(FrameOut::from_iter(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let (cols, rows) = parse_layout(&opts.layout)?;
    let filter = Filter::new(cols, rows);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn layout_parses_columns_then_rows() {
        assert_eq!(parse_layout("4x1").unwrap(), (4, 1));
        assert_eq!(parse_layout("1x4").unwrap(), (1, 4));
    }

    #[test]
    fn cell_size_is_frame_size_over_grid() {
        let (cols, rows) = (2u32, 2u32);
        let (w, h) = (8u32, 8u32);
        assert_eq!(cell_size(w, h, cols, rows), (4, 4));
    }
}
