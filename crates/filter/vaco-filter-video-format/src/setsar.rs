//! `setsar` — set the sample (pixel) aspect ratio.
//!
//! `ffmpeg -h filter=setsar` documents `sar`/`ratio`/`r` (default `"0"`,
//! meaning "leave it alone") and `max` (bound the numerator/denominator of
//! the reduced fraction). Implemented: `sar`. Not implemented: `max` — this
//! filter reduces with [`vaco_core::Rational::reduced`], which has no
//! configurable bound, rather than the reference's continued-fraction
//! search; the practical difference only shows up for a `sar` expression
//! that evaluates to an already-awkward fraction, which is rare enough not
//! to have been worth measuring here.
//!
//! # Measured: `setsar` always *overwrites* SAR — it never reads DAR
//!
//! ```text
//! ffmpeg -f lavfi -i color=red:s=100x50 -vf "setdar=1/1,setsar=2/1" -f null -
//! # -> SAR 2:1, DAR 4:1 — setsar ignored the DAR that setdar had just set
//! ```
//!
//! `setsar=<X>` sets the link's SAR to exactly `X`, full stop. DAR is never
//! an independently stored property in this framework (nor, observably, in
//! the reference): it is always `SAR * width / height`, computed for display
//! rather than carried. `setsar` immediately after `setdar` in a chain
//! clobbers whatever `setdar` computed, with no attempt to reconcile the two
//! — see `setdar.rs` for the mirror-image measurement.
//!
//! Filter-graph syntax note: aspect ratios must be written with `/`
//! (`setsar=16/9`), not `:` (`setsar=16:9`) — `:` is the filtergraph's own
//! argument separator (plan 13 §1b), so `16:9` would be parsed as two
//! positional arguments, not one ratio.

use vaco_core::{MediaType, Rational, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "setsar",
    description: "Set the pixel sample aspect ratio",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "setsar", help = "Set the pixel sample aspect ratio")]
pub(crate) struct Opts {
    #[opt(
        name = "sar",
        alias = "ratio",
        help = "set sample (pixel) aspect ratio",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub sar: String,
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

#[derive(Debug)]
pub(crate) struct Filter {
    /// `None` means "0", the reference's own "leave it alone" sentinel.
    sar: Option<Rational>,
}

impl Filter {
    fn new(text: &str) -> std::result::Result<Self, String> {
        let r = vaco_core::parse::rational(text)
            .ok_or_else(|| format!("setsar: bad `sar` `{text}`"))?;
        Ok(Self {
            sar: (r.num != 0).then_some(r.reduced()),
        })
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(target) = self.sar else {
            return Ok(());
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *sample_aspect_ratio = target;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let Some(target) = self.sar {
            input.sample_aspect_ratio = target;
        }
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts.sar)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_means_leave_it_alone() {
        let f = Filter::new("0").unwrap();
        assert_eq!(f.sar, None);
    }

    #[test]
    fn a_ratio_is_the_new_sar_verbatim() {
        let f = Filter::new("16/9").unwrap();
        assert_eq!(f.sar, Some(Rational::new(16, 9)));
    }

    #[test]
    fn bad_ratio_is_a_clean_error() {
        assert!(Filter::new("not-a-ratio").is_err());
    }
}
