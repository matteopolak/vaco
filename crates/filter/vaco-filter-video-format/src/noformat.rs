//! `noformat` — restrict the link to anything *but* a list of pixel formats.
//!
//! `ffmpeg -h filter=noformat` documents `pix_fmts`, the same `|`-separated
//! list shape as `format`. This filter's `NodeFormats` builds the
//! complement: every [`PixFmt`] the table knows about, minus the named
//! ones, as a [`Constraint::OneOf`]. `vaco_filter_core::negotiate::Constraint`
//! has no "none of" variant, so the complement is enumerated once at
//! `create` time rather than represented symbolically — mechanical, and
//! correct for exactly the same reason `format`'s `OneOf` is: negotiation
//! only ever asks "is X a member of this set".

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
    name: "noformat",
    description: "Force libavfilter not to use any of the specified pixel formats for the input to the next filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "noformat",
    help = "Force libavfilter not to use any of the specified pixel formats for the input to the next filter"
)]
pub(crate) struct Opts {
    #[opt(
        name = "pix_fmts",
        help = "A '|'-separated list of forbidden pixel formats",
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

fn parse_excluded(text: &str) -> std::result::Result<Vec<PixFmt>, String> {
    text.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| PixFmt::from_name(s).map_err(|e| format!("noformat: {e}")))
        .collect()
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let text = if opts.pix_fmts.is_empty() {
        req.positional(0).unwrap_or_default()
    } else {
        opts.pix_fmts.clone()
    };
    let excluded = parse_excluded(&text)?;
    let allowed: Vec<PixFmt> = PixFmt::all()
        .iter()
        .copied()
        .filter(|f| !excluded.contains(f))
        .collect();
    if allowed.is_empty() {
        return Err("noformat: `pix_fmts` excludes every known pixel format".to_owned());
    }
    let set = FormatSet {
        pixel_formats: Some(Constraint::OneOf(allowed)),
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
    fn excludes_named_formats_only() {
        let allowed = parse_excluded("yuv420p|rgb24").unwrap();
        assert!(allowed.contains(&PixFmt::Yuv420p));
        assert!(allowed.contains(&PixFmt::Rgb24));
    }

    #[test]
    fn creating_with_no_exclusions_allows_everything() {
        let req = Instantiate {
            name: "noformat",
            instance: "noformat",
            args: None,
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
