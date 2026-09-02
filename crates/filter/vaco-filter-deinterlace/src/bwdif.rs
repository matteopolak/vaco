//! `bwdif` — "Bob Weaver Deinterlacing Filter", motion-adaptive.
//!
//! `ffmpeg -h filter=bwdif`: `mode` (`send_frame`=0, `send_field`=1
//! default), `parity` (`tff`=0, `bff`=1, `auto`=-1 default), `deint`
//! (`all`=0 default, `interlaced`=1).
//!
//! See [`crate::mad`] and [`crate::yadif`]'s module docs: this shares the
//! same original motion-adaptive core as `yadif` and is **not** byte-exact
//! against the reference. The reference's own default mode for `bwdif` is
//! `send_field` (two output frames per input), unlike `yadif`'s
//! `send_frame` default; this crate implements only the `send_frame` shape
//! for every mode value.
//!
//! **Measured default divergence, stated plainly** (same treatment as
//! `vaco-filter-artistic::vignette`'s `dither`): declaring this field's own
//! default as the reference's real `send_field` would describe a value
//! this crate cannot actually honour even when the user asks for nothing
//! at all — every user gets `send_frame`'s single-output-frame shape
//! regardless, so `mode`'s default here is `send_frame` (0), matching what
//! the code unconditionally does, and requesting the reference's real
//! default (`send_field`) now refuses instead of silently returning
//! `send_frame`'s frame count while claiming to have honoured
//! `send_field`. Found by `cargo xtask reachability-check`'s rule I.

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
    name: "bwdif",
    description: "Deinterlace the input image.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `ffmpeg -h filter=bwdif`'s own named constants for `mode`.
const BWDIF_MODE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "send_frame",
        help: "",
        unit: "bwdif_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "send_field",
        help: "",
        unit: "bwdif_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "bwdif", help = "Deinterlace the input image")]
pub(crate) struct Opts {
    #[opt(name = "mode", help = "interlacing mode", unit = "bwdif_mode", consts = BWDIF_MODE_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
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
            return Err("bwdif: `mode` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.deint != 0 {
            return Err("bwdif: `deint` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
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
    /// (`ffmpeg -h filter=bwdif`): every named constant on every
    /// enumerated option must parse, not just the bare integer.
    #[test]
    fn named_option_values_parse() {
        // `mode`'s default here is `send_frame`, not the reference's own
        // `send_field` default -- see this file's top-of-module doc.
        // `send_field` now refuses (`cargo xtask reachability-check`'s
        // rule I).
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
    /// always deinterlaces every frame in the `send_frame` shape regardless
    /// of either, so a non-default value must refuse rather than silently
    /// doing nothing.
    #[test]
    fn a_non_default_mode_or_deint_is_refused() {
        assert!(Opts::parse(Some("mode=send_field")).is_err());
        assert!(Opts::parse(Some("deint=interlaced")).is_err());
        assert!(Opts::parse(None).is_ok());
    }
}
