//! `format` — restrict the link to one of a list of pixel formats.
//!
//! `ffmpeg -h filter=format` documents `pix_fmts`, `color_spaces`,
//! `color_ranges` and `alpha_modes`, each a `|`-separated list. Implemented:
//! `pix_fmts`. Not implemented: the other three — `vaco-filter-core`'s
//! negotiation model (§1.6 of plan 16) carries pixel format as the only
//! negotiated video property; colour space/range/alpha are link metadata
//! copied through, not negotiated (see `vaco-filter-core`'s own docs, "the
//! signature gaps" section), so there is nowhere in the current framework to
//! attach those three constraints. `noformat` (the reference's "anything but
//! this list" sibling) is not registered — it is a straightforward negation
//! of the same option and can be added the same way once wanted.
//!
//! # This filter does no conversion itself
//!
//! `format` declares a constraint; it does not run `vaco-scale`. Concretely:
//! its input and output pads share one [`Constraint::OneOf`], so the
//! negotiation engine either finds the link already in that set or — if the
//! graph builder has auto-conversion enabled — splices a converter upstream.
//! The filter's own [`Filter::filter_frame`] is a pure passthrough: by the
//! time it runs, negotiation has already guaranteed the frame is in one of
//! the requested formats.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "format",
    description: "Convert the input video to one of the specified pixel formats",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "format",
    help = "Convert the input video to one of the specified pixel formats"
)]
pub(crate) struct Opts {
    #[opt(
        name = "pix_fmts",
        help = "A '|'-separated list of pixel formats",
        default = String::new(),
        flags(video, filtering)
    )]
    pub pix_fmts: String,
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

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(input))
    }
}

fn parse_list(text: &str) -> std::result::Result<Vec<PixFmt>, String> {
    text.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| PixFmt::from_name(s).map_err(|e| format!("format: {e}")))
        .collect()
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let text = if opts.pix_fmts.is_empty() {
        // The bare positional form, `format=yuv420p|rgb24`.
        req.positional(0).unwrap_or_default()
    } else {
        opts.pix_fmts.clone()
    };
    let formats = parse_list(&text)?;
    if formats.is_empty() {
        return Err("format: `pix_fmts` names no known pixel format".to_owned());
    }
    let set = FormatSet {
        pixel_formats: Some(Constraint::OneOf(formats)),
        ..FormatSet::default()
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_a_pipe_separated_list_in_order() {
        let list = parse_list("yuv420p|rgb24|gray8").unwrap();
        assert_eq!(list, vec![PixFmt::Yuv420p, PixFmt::Rgb24, PixFmt::Gray8]);
    }

    #[test]
    fn unknown_format_is_a_clean_error() {
        assert!(parse_list("not-a-real-format").is_err());
    }

    #[test]
    fn empty_list_is_rejected_by_create() {
        let req = Instantiate {
            name: "format",
            instance: "format",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }
}
