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
//! here, so `filter` parses at the reference's own default (`complex`) and
//! any other value now refuses instead of silently picking a kernel that
//! is not actually there).
//!
//! `mode`'s own reference default (`field`, two output frames per input)
//! has the identical shape of gap [`crate::bwdif`]'s module doc explains
//! for the same option name: this crate implements only the `frame` shape
//! (one output frame per input), always, so `mode` parses at `frame` (0),
//! not the reference's `field`, and `field` now refuses. Both found by
//! `cargo xtask reachability-check`'s rule I.

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
    #[opt(name = "mode", help = "interlacing mode", unit = "w3fdif_mode", consts = W3FDIF_MODE_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
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
        if o.filter != 1 {
            return Err("w3fdif: `filter` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.mode != 0 {
            return Err("w3fdif: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.deint != 0 {
            return Err("w3fdif: `deint` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
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
        // `filter`'s own default (the reference's `complex`) still parses;
        // any other value now refuses. `mode` parses at `frame`, not the
        // reference's own `field` default -- see this file's top-of-module
        // doc -- and `field` now refuses (`cargo xtask
        // reachability-check`'s rule I).
        let opts = Opts::parse(Some("filter=complex")).unwrap();
        assert_eq!(opts.filter, 1, "filter=complex");
        let opts = Opts::parse(Some("mode=frame")).unwrap();
        assert_eq!(opts.mode, 0, "mode=frame");
        for (name, expected) in [("tff", 0), ("bff", 1), ("auto", -1)] {
            let opts = Opts::parse(Some(&format!("parity={name}"))).unwrap();
            assert_eq!(opts.parity, expected, "parity={name}");
        }
        // `deint`'s own default still parses; every other value now refuses
        // (`cargo xtask reachability-check`'s rule I).
        let opts = Opts::parse(Some("deint=all")).unwrap();
        assert_eq!(opts.deint, 0, "deint=all");
    }

    /// Regression for rule I: `filter`/`mode`/`deint` are parsed but this
    /// crate deinterlaces every frame with one fixed kernel regardless of
    /// any of them, so a non-default value must refuse rather than
    /// silently doing nothing.
    #[test]
    fn a_non_default_filter_mode_or_deint_is_refused() {
        assert!(Opts::parse(Some("filter=simple")).is_err());
        assert!(Opts::parse(Some("mode=field")).is_err());
        assert!(Opts::parse(Some("deint=interlaced")).is_err());
        assert!(Opts::parse(None).is_ok());
    }
}
