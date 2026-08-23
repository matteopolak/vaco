//! `field` — extract one field (every other row) from the input.
//!
//! `ffmpeg -h filter=field` documents `type` (`top`=0 default, `bottom`=1).
//!
//! # Measured: row selection and output height
//!
//! Built a 1x6 `gray` column `[0,10,20,30,40,50]` (`geq`) and ran both
//! types:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=1x6,format=gray,geq=lum='Y*10'" \
//!   -vf field=type=top -f rawvideo -pix_fmt gray -
//! ```
//!
//! `type=top` gave `[0,20,40]` (the even rows, 0-indexed); `type=bottom`
//! gave `[10,30,50]` (the odd rows). Output height is exactly half the
//! input in both cases. An odd input height is not separately measured;
//! this implementation floors (`(height + 1) / 2` output rows for the top
//! field when height is odd, `height / 2` for the bottom field — the top
//! field always gets the possible extra row, since row 0 is always top).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::geom;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "field",
    description: "Extract a field from the input video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "field", help = "Extract a field from the input video")]
pub(crate) struct Opts {
    #[opt(name = "type", help = "set field type", default = 0, range = 0..=1, flags(video, filtering))]
    pub field_type: i32,
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

/// Output height for one field of an `in_h`-tall plane. The top field
/// (`bottom = false`) gets the extra row when `in_h` is odd.
#[allow(
    clippy::integer_division,
    reason = "field height is a whole-row count by definition, not a lossy approximation"
)]
const fn field_height(in_h: u32, bottom: bool) -> u32 {
    if bottom { in_h / 2 } else { in_h.div_ceil(2) }
}

#[derive(Debug)]
pub(crate) struct Filter {
    bottom: bool,
}

impl Filter {
    pub(crate) const fn new(opts: &Opts) -> Self {
        Self {
            bottom: opts.field_type == 1,
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video { height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(());
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video { height: h, .. } = &mut out {
                *h = field_height(height, self.bottom).max(1);
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

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
        let start_row = u32::from(self.bottom);
        let out_h = field_height(height, self.bottom).max(1);
        let mut out = ctx.pool().acquire_video(format, width, out_h)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let plane_start = format.plane_height(start_row, plane_idx);
            let plane_out_h = format.plane_height(out_h, plane_idx);
            let Some(src) = input.plane(p) else { continue };
            let Some(mut dst) = out.plane_mut(p) else {
                continue;
            };
            for oy in 0..plane_out_h {
                let sy = plane_start + oy * 2;
                let Some(src_row) = src.row(sy as usize) else {
                    continue;
                };
                if let Some(dst_row) = dst.row_mut(oy as usize) {
                    let n = dst_row.len().min(src_row.len());
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
    fn top_field_takes_even_rows() {
        // Measured: [0,10,20,30,40,50] -> top=[0,20,40].
        let in_rows = [0u8, 10, 20, 30, 40, 50];
        let out: Vec<u8> = (0..field_height(6, false))
            .map(|oy| in_rows[(oy * 2) as usize])
            .collect();
        assert_eq!(out, vec![0, 20, 40]);
    }

    #[test]
    fn bottom_field_takes_odd_rows() {
        let in_rows = [0u8, 10, 20, 30, 40, 50];
        let start = 1u32;
        let out: Vec<u8> = (0..field_height(6, true))
            .map(|oy| in_rows[(start + oy * 2) as usize])
            .collect();
        assert_eq!(out, vec![10, 30, 50]);
    }

    #[test]
    fn odd_height_gives_the_extra_row_to_top() {
        assert_eq!(field_height(7, false), 4);
        assert_eq!(field_height(7, true), 3);
    }
}
