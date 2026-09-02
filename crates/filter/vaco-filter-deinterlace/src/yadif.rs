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
//! implemented, regardless of the parsed `mode` (a non-default `mode` is
//! not currently refused — see the note in [`crate::mad::Lookahead`]'s doc
//! for the full accounting); `deint` parses at its own default only, and a
//! non-default value now refuses instead of being silently ignored
//! (`cargo xtask reachability-check`'s rule I), since this crate
//! deinterlaces every frame regardless of what it names.

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
        if o.mode != 0 {
            return Err("yadif: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.deint != 0 {
            return Err("yadif: `deint` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
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
        // `mode`'s own default (`send_frame`) still parses; every other
        // value now refuses (`cargo xtask reachability-check`'s rule I).
        let opts = Opts::parse(Some("mode=send_frame")).unwrap();
        assert_eq!(opts.mode, 0, "mode=send_frame");
        for (name, expected) in [("tff", 0), ("bff", 1), ("auto", -1)] {
            let opts = Opts::parse(Some(&format!("parity={name}"))).unwrap();
            assert_eq!(opts.parity, expected, "parity={name}");
        }
        // `deint`'s own default still parses; every other value now refuses
        // (`cargo xtask reachability-check`'s rule I).
        let opts = Opts::parse(Some("deint=all")).unwrap();
        assert_eq!(opts.deint, 0, "deint=all");
    }

    /// Regression for rule I: `mode`/`deint` are parsed but this crate
    /// always produces `send_frame`-shaped output and deinterlaces every
    /// frame regardless of either, so a non-default value must refuse
    /// rather than silently doing nothing.
    #[test]
    fn a_non_default_mode_or_deint_is_refused() {
        assert!(Opts::parse(Some("mode=send_field")).is_err());
        assert!(Opts::parse(Some("mode=send_frame_nospatial")).is_err());
        assert!(Opts::parse(Some("mode=send_field_nospatial")).is_err());
        assert!(Opts::parse(Some("deint=interlaced")).is_err());
        assert!(Opts::parse(None).is_ok());
    }
}
