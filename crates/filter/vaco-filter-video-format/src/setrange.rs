//! `setrange` — force a frame's colour range metadata.
//!
//! `ffmpeg -h filter=setrange` documents `range`: `auto` (default, "keep the
//! same colour range"), `unspecified`/`unknown`, `limited`/`tv`/`mpeg`,
//! `full`/`pc`/`jpeg`. All aliases implemented, matching the reference's own
//! grouping exactly (`tv` and `mpeg` both mean limited; `pc` and `jpeg` both
//! mean full) — this is metadata-only, one enum on
//! [`vaco_color::ColorInfo::range`], so no pixel is ever touched.

use vaco_color::ColorRange;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "setrange",
    description: "Force color range for the output video frame",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Mode {
    /// Leave whatever the frame already carries alone.
    Auto,
    Set(ColorRange),
}

impl Mode {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "-1" | "auto" => Ok(Self::Auto),
            "0" | "unspecified" | "unknown" => Ok(Self::Set(ColorRange::Unspecified)),
            "1" | "limited" | "tv" | "mpeg" => Ok(Self::Set(ColorRange::Limited)),
            "2" | "full" | "pc" | "jpeg" => Ok(Self::Set(ColorRange::Full)),
            other => Err(format!("setrange: bad `range` `{other}`")),
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "setrange",
    help = "Force color range for the output video frame"
)]
pub(crate) struct Opts {
    #[opt(
        name = "range",
        help = "select color range",
        default = "auto".to_owned(),
        flags(video, filtering)
    )]
    pub range: String,
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
    mode: Mode,
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let Mode::Set(range) = self.mode {
            input.color.range = range;
        }
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let mode = Mode::parse(&opts.range)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter { mode })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn tv_and_mpeg_both_mean_limited() {
        assert!(matches!(
            Mode::parse("tv").unwrap(),
            Mode::Set(ColorRange::Limited)
        ));
        assert!(matches!(
            Mode::parse("mpeg").unwrap(),
            Mode::Set(ColorRange::Limited)
        ));
    }

    #[test]
    fn pc_and_jpeg_both_mean_full() {
        assert!(matches!(
            Mode::parse("pc").unwrap(),
            Mode::Set(ColorRange::Full)
        ));
        assert!(matches!(
            Mode::parse("jpeg").unwrap(),
            Mode::Set(ColorRange::Full)
        ));
    }

    #[test]
    fn bad_mode_is_a_clean_error() {
        assert!(Mode::parse("bogus").is_err());
    }
}
