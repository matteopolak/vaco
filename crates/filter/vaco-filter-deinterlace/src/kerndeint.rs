//! `kerndeint` — kernel deinterlacing (spatial, single-frame in the
//! reference).
//!
//! `ffmpeg -h filter=kerndeint`: `thresh` (`0..=255`, default `10`), `map`
//! (bool, default `false`), `order` (bool, default `false`), `sharp`
//! (bool, default `false`), `twoway` (bool, default `false`).
//!
//! # Simplification, stated plainly
//!
//! The reference's `kerndeint` is purely spatial — it never looks at
//! adjacent frames. This crate reuses [`crate::mad`]'s shared
//! motion-adaptive core (the same one `yadif`/`bwdif`/`w3fdif`/`estdif`
//! use) rather than writing a fifth, separate kernel, which means it *does*
//! use temporal information the reference does not. `thresh`/`map`/`sharp`/
//! `twoway` are parsed for option-table completeness and do not change
//! behaviour — a non-default value now refuses rather than being silently
//! ignored (`cargo xtask reachability-check` rule I). `order` is the
//! exception: it *is* read, to pick [`crate::mad::Lookahead`]'s field
//! parity — a corrected claim, since an earlier pass here lumped it in
//! with the other four as inert, which stopped being true once `order`
//! started feeding `parity` below.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::mad::Lookahead;
use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "kerndeint",
    description: "Apply kernel deinterlacing to the input.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "kerndeint", help = "Apply kernel deinterlacing to the input")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the reference's own four independent boolean options exactly"
)]
pub(crate) struct Opts {
    #[opt(name = "thresh", help = "set the threshold", default = 10, range = 0..=255, flags(video, filtering))]
    pub thresh: i32,
    #[opt(
        name = "map",
        help = "set the map",
        default = false,
        flags(video, filtering)
    )]
    pub map: bool,
    #[opt(
        name = "order",
        help = "set the order",
        default = false,
        flags(video, filtering)
    )]
    pub order: bool,
    #[opt(
        name = "sharp",
        help = "set sharpening",
        default = false,
        flags(video, filtering)
    )]
    pub sharp: bool,
    #[opt(
        name = "twoway",
        help = "set twoway",
        default = false,
        flags(video, filtering)
    )]
    pub twoway: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.map {
            return Err("kerndeint: `map` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.thresh != 10 {
            return Err("kerndeint: `thresh` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.sharp {
            return Err("kerndeint: `sharp` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        if o.twoway {
            return Err("kerndeint: `twoway` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let parity = Some(opts.order);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Lookahead::new(parity))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    /// `thresh`/`map`/`sharp`/`twoway` are parsed but this crate's
    /// motion-adaptive core never reads them (`order` is the one exception
    /// -- see this module's own doc). Regression for `cargo xtask
    /// reachability-check`'s rule I.
    #[test]
    fn a_non_default_unimplemented_option_is_refused() {
        assert!(Opts::parse(Some("thresh=20")).is_err());
        assert!(Opts::parse(Some("map=1")).is_err());
        assert!(Opts::parse(Some("sharp=1")).is_err());
        assert!(Opts::parse(Some("twoway=1")).is_err());
        assert!(Opts::parse(None).is_ok());
        assert!(
            Opts::parse(Some("order=1")).is_ok(),
            "order is genuinely read"
        );
    }
}
