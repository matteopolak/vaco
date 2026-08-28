//! `yadif` — motion-adaptive deinterlacer.
//!
//! `ffmpeg -h filter=yadif`: `mode` (`send_frame`=0 default, `send_field`=1,
//! `send_frame_nospatial`=2, `send_field_nospatial`=3), `parity`
//! (`tff`=0, `bff`=1, `auto`=-1 default), `deint` (`all`=0 default,
//! `interlaced`=1).
//!
//! # Not byte-exact — see `crate::mad`
//!
//! This crate does not reproduce the reference's published interpolation
//! kernel. [`crate::mad`]'s module doc explains why (no source this
//! project may read describes it precisely enough to transcribe honestly)
//! and what is implemented instead: an original motion-adaptive
//! interpolator satisfying the same structural invariant the row's brief
//! requires — exact reproduction on a genuinely static/progressive
//! sequence — without claiming to match the reference's output on general
//! interlaced content. `docs/filter/vaco-filter-deinterlace.md` states this
//! per filter.
//!
//! # What is simplified
//!
//! Only `send_frame`-shaped output (one frame per input frame) is
//! implemented, regardless of the parsed `mode`; `deint` is parsed but not
//! applied (every frame is deinterlaced). See [`crate::mad::Lookahead`]'s
//! doc for the full accounting.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::mad::Lookahead;
use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "yadif",
    description: "Deinterlace the input image.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `ffmpeg -h filter=yadif`'s own named constants for `mode`.
const YADIF_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "send_frame",
        help: "",
        unit: "yadif_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "send_field",
        help: "",
        unit: "yadif_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "send_frame_nospatial",
        help: "",
        unit: "yadif_mode",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "send_field_nospatial",
        help: "",
        unit: "yadif_mode",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "yadif", help = "Deinterlace the input image")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "interlacing mode", unit = "yadif_mode", consts = YADIF_MODE_CONSTS, default = 0, range = 0..=3, flags(video, filtering))]
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

pub(crate) fn parity_from_opt(v: i32) -> Option<bool> {
    match v {
        0 => Some(true),
        1 => Some(false),
        _ => None,
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
    /// (`ffmpeg -h filter=yadif`).
    #[test]
    fn named_option_values_parse() {
        for (name, expected) in [
            ("send_frame", 0),
            ("send_field", 1),
            ("send_frame_nospatial", 2),
            ("send_field_nospatial", 3),
        ] {
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
