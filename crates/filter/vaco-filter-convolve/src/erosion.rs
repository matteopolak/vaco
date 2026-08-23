//! `erosion` — greyscale morphological erosion, the dual of
//! [`crate::dilation`]. See [`crate::morph`] for the shared engine and the
//! measurements pinning down `coordinates` and `threshold`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::morph::{self, MorphParams, Op};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "erosion",
    description: "Apply erosion effect",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "erosion", help = "Apply erosion effect")]
pub(crate) struct Opts {
    #[opt(name = "coordinates", help = "set coordinates", default = 255, range = 0..=255, flags(video, filtering))]
    pub coordinates: i32,
    #[opt(name = "threshold0", help = "set threshold for 1st plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold0: i32,
    #[opt(name = "threshold1", help = "set threshold for 2nd plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold1: i32,
    #[opt(name = "threshold2", help = "set threshold for 3rd plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold2: i32,
    #[opt(name = "threshold3", help = "set threshold for 4th plane", default = 65535, range = 0..=65535, flags(video, filtering))]
    pub threshold3: i32,
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

    fn threshold(&self, plane: u8) -> i32 {
        match plane {
            0 => self.threshold0,
            1 => self.threshold1,
            2 => self.threshold2,
            _ => self.threshold3,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Opts,
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
            let params = MorphParams {
                coordinates: self.opts.coordinates,
                threshold: self.opts.threshold(p8),
            };
            let filtered = morph::apply_plane(&rows, pw, ph, Op::Erode, params);
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
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter { opts })),
    })
}
