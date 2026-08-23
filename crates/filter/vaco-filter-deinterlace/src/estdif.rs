//! `estdif` — Edge Slope Tracing deinterlace.
//!
//! `ffmpeg -h filter=estdif`: `mode` (`frame`=0, `field`=1 default),
//! `parity` (`tff`=0, `bff`=1, `auto`=-1 default), `deint` (`all`=0
//! default, `interlaced`=1), `rslope` (`1..=15`, default `1`), `redge`
//! (`0..=15`, default `2`), `ecost`/`mcost`/`dcost` (`0..=50`, defaults
//! `2`/`1`/`1`), `interp` (`2p`=0, `4p`=1 default, `6p`=2).
//!
//! See [`crate::mad`]'s module doc: shares the same original
//! motion-adaptive core rather than the reference's edge-slope-tracing
//! search (`rslope`/`redge`/`ecost`/`mcost`/`dcost`/`interp` are parsed —
//! for option-table completeness and so a filtergraph string using them
//! does not fail to parse — but do not affect the interpolation).

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::mad::Lookahead;
use crate::video::VIDEO_PAD;
use crate::yadif::parity_from_opt;

pub const DESC: FilterDesc = FilterDesc {
    name: "estdif",
    description: "Apply Edge Slope Tracing deinterlace.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "estdif", help = "Apply Edge Slope Tracing deinterlace")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "interlacing mode", default = 1, range = 0..=1, flags(video, filtering))]
    pub mode: i32,
    #[opt(name = "parity", help = "assumed picture field parity", default = -1, range = -1..=1, flags(video, filtering))]
    pub parity: i32,
    #[opt(name = "deint", help = "which frames to deinterlace", default = 0, range = 0..=1, flags(video, filtering))]
    pub deint: i32,
    #[opt(name = "rslope", help = "search radius for edge slope tracing", default = 1, range = 1..=15, flags(video, filtering))]
    pub rslope: i32,
    #[opt(name = "redge", help = "search radius for best edge matching", default = 2, range = 0..=15, flags(video, filtering))]
    pub redge: i32,
    #[opt(name = "ecost", help = "edge cost for edge matching", default = 2, range = 0..=50, flags(video, filtering))]
    pub ecost: i32,
    #[opt(name = "mcost", help = "middle cost for edge matching", default = 1, range = 0..=50, flags(video, filtering))]
    pub mcost: i32,
    #[opt(name = "dcost", help = "distance cost for edge matching", default = 1, range = 0..=50, flags(video, filtering))]
    pub dcost: i32,
    #[opt(name = "interp", help = "type of interpolation", default = 1, range = 0..=2, flags(video, filtering))]
    pub interp: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Lookahead::new(parity_from_opt(opts.parity)))),
    })
}
