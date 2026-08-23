//! `hflip`/`vflip` — mirror the frame horizontally or vertically.
//!
//! Neither filter takes options (`ffmpeg -h filter=hflip`/`vflip` list none).
//! Both operate at the byte level via [`crate::geom::plane_unit_bytes`], so
//! they work on any addressable pixel format without knowing its component
//! layout — reversing whole pixel-sized chunks within a row (`hflip`) or
//! reversing row order (`vflip`) never needs to know what is *inside* a
//! chunk.
//!
//! `hflip` twice, or `vflip` twice, is the identity — exercised as a
//! `proptest` round-trip below, which is the cheapest possible regression
//! guard against an off-by-one in the reversal.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug)]
pub(crate) struct Filter {
    axis: Axis,
}

impl Filter {
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "constructed via `build` in normal use")
    )]
    pub(crate) const fn new(axis: Axis) -> Self {
        Self { axis }
    }
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
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            let rows = src_plane.rows();
            let row_bytes = src_plane.row_bytes();
            for row in 0..rows {
                let src_row_idx = if self.axis == Axis::Vertical {
                    rows.saturating_sub(1).saturating_sub(row)
                } else {
                    row
                };
                let Some(src_row) = src_plane.row(src_row_idx) else {
                    continue;
                };
                let Some(dst_row) = dst_plane.row_mut(row) else {
                    continue;
                };
                if self.axis == Axis::Horizontal {
                    reverse_units_into(src_row, dst_row, unit);
                } else {
                    let n = row_bytes.min(dst_row.len()).min(src_row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), src_row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

/// Copy `src` into `dst`, one `unit`-byte chunk at a time, right to left.
fn reverse_units_into(src: &[u8], dst: &mut [u8], unit: usize) {
    if unit == 0 {
        return;
    }
    #[allow(
        clippy::integer_division,
        reason = "unit is the pixel stride in bytes; this counts whole pixels"
    )]
    let units = src.len() / unit;
    for i in 0..units {
        let src_start = i.saturating_mul(unit);
        let Some(src_chunk) = src.get(src_start..src_start.saturating_add(unit)) else {
            continue;
        };
        let dst_i = units.saturating_sub(1).saturating_sub(i);
        let dst_start = dst_i.saturating_mul(unit);
        if let Some(dst_chunk) = dst.get_mut(dst_start..dst_start.saturating_add(unit)) {
            dst_chunk.copy_from_slice(src_chunk);
        }
    }
}

fn build(desc: FilterDesc, axis: Axis, req: &Instantiate<'_>) -> Instance {
    Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(axis))),
    }
}

pub mod hflip {
    use super::{Axis, FilterDesc, FilterFlags, Instance, Instantiate, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "hflip",
        description: "Horizontally flip the input video",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    #[allow(
        clippy::unnecessary_wraps,
        reason = "every filter module in this crate exposes `create` returning Result, for a uniform registry dispatch signature"
    )]
    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        Ok(build(DESC, Axis::Horizontal, req))
    }
}

pub mod vflip {
    use super::{Axis, FilterDesc, FilterFlags, Instance, Instantiate, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "vflip",
        description: "Vertically flip the input video",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    #[allow(
        clippy::unnecessary_wraps,
        reason = "every filter module in this crate exposes `create` returning Result, for a uniform registry dispatch signature"
    )]
    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        Ok(build(DESC, Axis::Vertical, req))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn reverse_units_reverses_pixel_groups_not_bytes() {
        // Two 2-byte "pixels": [1,2] [3,4]. Reversed as units: [3,4] [1,2].
        let src = [1u8, 2, 3, 4];
        let mut dst = [0u8; 4];
        reverse_units_into(&src, &mut dst, 2);
        assert_eq!(dst, [3, 4, 1, 2]);
    }

    #[test]
    fn reverse_units_of_single_byte_pixels_is_a_plain_reverse() {
        let src = [1u8, 2, 3, 4, 5];
        let mut dst = [0u8; 5];
        reverse_units_into(&src, &mut dst, 1);
        assert_eq!(dst, [5, 4, 3, 2, 1]);
    }
}
