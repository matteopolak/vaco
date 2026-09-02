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
//! search, so `rslope`/`redge`/`ecost`/`mcost`/`dcost`/`interp`/`deint` do
//! not affect the interpolation — each parses at its own reference default
//! and a non-default value now refuses instead of being silently ignored
//! (`cargo xtask reachability-check`'s rule I).
//!
//! `mode` is the one field of the group with a genuinely honest side, the
//! same shape [`crate::bwdif`]'s module doc explains for its own `mode`:
//! this crate always produces one output frame per input (`frame`, not the
//! reference's own `field` default), so `mode` parses at `frame` (0) here,
//! not `field`, and `field` now refuses.

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

/// `ffmpeg -h filter=estdif`'s own named constants for `mode`/`interp`.
const ESTDIF_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "frame",
        help: "",
        unit: "estdif_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "field",
        help: "",
        unit: "estdif_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];
const ESTDIF_INTERP_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "2p",
        help: "",
        unit: "estdif_interp",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "4p",
        help: "",
        unit: "estdif_interp",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "6p",
        help: "",
        unit: "estdif_interp",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "estdif", help = "Apply Edge Slope Tracing deinterlace")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "interlacing mode", unit = "estdif_mode", consts = ESTDIF_MODE_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
    pub mode: i32,
    #[opt(name = "parity", help = "assumed picture field parity", unit = "parity", consts = crate::opt_consts::PARITY_CONSTS, default = -1, range = -1..=1, flags(video, filtering))]
    pub parity: i32,
    #[opt(name = "deint", help = "which frames to deinterlace", unit = "deint", consts = crate::opt_consts::DEINT_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
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
    #[opt(name = "interp", help = "type of interpolation", unit = "estdif_interp", consts = ESTDIF_INTERP_CONSTS, default = 1, range = 0..=2, flags(video, filtering))]
    pub interp: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.rslope != 1 {
            return Err("estdif: `rslope` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.redge != 2 {
            return Err("estdif: `redge` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.ecost != 2 {
            return Err("estdif: `ecost` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.mcost != 1 {
            return Err("estdif: `mcost` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.dcost != 1 {
            return Err("estdif: `dcost` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.interp != 1 {
            return Err("estdif: `interp` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.mode != 0 {
            return Err("estdif: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.deint != 0 {
            return Err("estdif: `deint` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
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
    /// (`ffmpeg -h filter=estdif`).
    #[test]
    fn named_option_values_parse() {
        // `mode` parses at `frame`, not the reference's own `field` default
        // -- see this file's top-of-module doc -- and `field` now refuses.
        let opts = Opts::parse(Some("mode=frame")).unwrap();
        assert_eq!(opts.mode, 0, "mode=frame");
        for (name, expected) in [("tff", 0), ("bff", 1), ("auto", -1)] {
            let opts = Opts::parse(Some(&format!("parity={name}"))).unwrap();
            assert_eq!(opts.parity, expected, "parity={name}");
        }
        // `deint`'s and `interp`'s own default values still parse -- see
        // `a_non_default_unimplemented_cost_parameter_is_refused` for why
        // every other named value now refuses instead.
        let opts = Opts::parse(Some("deint=all")).unwrap();
        assert_eq!(opts.deint, 0, "deint=all");
        let opts = Opts::parse(Some("interp=4p")).unwrap();
        assert_eq!(opts.interp, 1, "interp=4p");
    }

    /// `rslope`/`redge`/`ecost`/`mcost`/`dcost`/`interp`/`deint`/`mode` are
    /// parsed but this crate's EEDI2-derived interpolation never reads them
    /// -- refuse a non-default value rather than silently compute with the
    /// defaults. Regression for `cargo xtask reachability-check`'s rule I.
    #[test]
    fn a_non_default_unimplemented_cost_parameter_is_refused() {
        assert!(Opts::parse(Some("rslope=2")).is_err());
        assert!(Opts::parse(Some("redge=6")).is_err());
        assert!(Opts::parse(Some("ecost=3")).is_err());
        assert!(Opts::parse(Some("mcost=2")).is_err());
        assert!(Opts::parse(Some("dcost=2")).is_err());
        assert!(Opts::parse(Some("interp=2p")).is_err());
        assert!(Opts::parse(Some("interp=6p")).is_err());
        assert!(Opts::parse(Some("deint=interlaced")).is_err());
        assert!(Opts::parse(Some("mode=field")).is_err());
        assert!(Opts::parse(None).is_ok());
    }
}
