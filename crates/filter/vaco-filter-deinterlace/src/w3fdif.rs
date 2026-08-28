//! `w3fdif` — Martin Weston three-field deinterlace.
//!
//! `ffmpeg -h filter=w3fdif`: `filter` (`simple`=0, `complex`=1 default),
//! `mode` (`frame`=0, `field`=1 default), `parity` (`tff`=0, `bff`=1,
//! `auto`=-1 default), `deint` (`all`=0 default, `interlaced`=1).
//!
//! See [`crate::mad`]'s module doc: shares the same original
//! motion-adaptive core and is **not** byte-exact against the reference's
//! three-field weighted-FIR kernel (`simple`/`complex` select between two
//! different published tap sets in the reference; neither is reproduced
//! here). `mode` and `filter` are parsed but do not change behaviour.

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
    name: "w3fdif",
    description: "Apply Martin Weston three field deinterlace.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `ffmpeg -h filter=w3fdif`'s own named constants for `filter`/`mode`.
const W3FDIF_FILTER_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "simple",
        help: "",
        unit: "w3fdif_filter",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "complex",
        help: "",
        unit: "w3fdif_filter",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];
const W3FDIF_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "frame",
        help: "",
        unit: "w3fdif_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "field",
        help: "",
        unit: "w3fdif_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "w3fdif", help = "Apply Martin Weston three field deinterlace")]
pub(crate) struct Opts {
    #[opt(name = "filter", help = "filter to use", unit = "w3fdif_filter", consts = W3FDIF_FILTER_CONSTS, default = 1, range = 0..=1, flags(video, filtering))]
    pub filter: i32,
    #[opt(name = "mode", help = "interlacing mode", unit = "w3fdif_mode", consts = W3FDIF_MODE_CONSTS, default = 1, range = 0..=1, flags(video, filtering))]
    pub mode: i32,
    #[opt(name = "parity", help = "assumed picture field parity", unit = "parity", consts = crate::opt_consts::PARITY_CONSTS, default = -1, range = -1..=1, flags(video, filtering))]
    pub parity: i32,
    #[opt(name = "deint", help = "which frames to deinterlace", unit = "deint", consts = crate::opt_consts::DEINT_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
    pub deint: i32,
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

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Lookahead::new(parity_from_opt(opts.parity)))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=w3fdif`).
    #[test]
    fn named_option_values_parse() {
        for (name, expected) in [("simple", 0), ("complex", 1)] {
            let opts = Opts::parse(Some(&format!("filter={name}"))).unwrap();
            assert_eq!(opts.filter, expected, "filter={name}");
        }
        for (name, expected) in [("frame", 0), ("field", 1)] {
            let opts = Opts::parse(Some(&format!("mode={name}"))).unwrap();
            assert_eq!(opts.mode, expected, "mode={name}");
        }
        for (name, expected) in [("tff", 0), ("bff", 1), ("auto", -1)] {
            let opts = Opts::parse(Some(&format!("parity={name}"))).unwrap();
            assert_eq!(opts.parity, expected, "parity={name}");
        }
        for (name, expected) in [("all", 0), ("interlaced", 1)] {
            let opts = Opts::parse(Some(&format!("deint={name}"))).unwrap();
            assert_eq!(opts.deint, expected, "deint={name}");
        }
    }
}
