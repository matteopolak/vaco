//! `setfield` — force a frame's interlaced field order metadata.
//!
//! `ffmpeg -h filter=setfield` documents `mode`: `auto` (default, "keep the
//! same input field"), `bff`, `tff`, `prog`. All four implemented — this is
//! metadata-only, three bits on [`vaco_frame::FrameFlags`], so there is
//! nothing left unimplemented.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameFlags};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "setfield",
    description: "Force field for the output video frame",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Auto,
    Bff,
    Tff,
    Prog,
}

impl Mode {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "-1" | "auto" => Ok(Self::Auto),
            "0" | "bff" => Ok(Self::Bff),
            "1" | "tff" => Ok(Self::Tff),
            "2" | "prog" => Ok(Self::Prog),
            other => Err(format!("setfield: bad `mode` `{other}`")),
        }
    }

    fn apply(self, flags: &mut FrameFlags) {
        match self {
            Self::Auto => {}
            Self::Bff => {
                flags.insert(FrameFlags::INTERLACED);
                flags.remove(FrameFlags::TOP_FIELD_FIRST);
            }
            Self::Tff => {
                flags.insert(FrameFlags::INTERLACED);
                flags.insert(FrameFlags::TOP_FIELD_FIRST);
            }
            Self::Prog => {
                flags.remove(FrameFlags::INTERLACED);
                flags.remove(FrameFlags::TOP_FIELD_FIRST);
            }
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "setfield", help = "Force field for the output video frame")]
pub(crate) struct Opts {
    #[opt(
        name = "mode",
        help = "select interlace mode",
        default = "auto".to_owned(),
        flags(video, filtering)
    )]
    pub mode: String,
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
        self.mode.apply(&mut input.flags);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let mode = Mode::parse(&opts.mode)?;
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
    fn tff_sets_both_bits() {
        let mut flags = FrameFlags::empty();
        Mode::parse("tff").unwrap().apply(&mut flags);
        assert!(flags.contains(FrameFlags::INTERLACED));
        assert!(flags.contains(FrameFlags::TOP_FIELD_FIRST));
    }

    #[test]
    fn bff_sets_interlaced_only() {
        let mut flags = FrameFlags::TOP_FIELD_FIRST;
        Mode::parse("bff").unwrap().apply(&mut flags);
        assert!(flags.contains(FrameFlags::INTERLACED));
        assert!(!flags.contains(FrameFlags::TOP_FIELD_FIRST));
    }

    #[test]
    fn prog_clears_both_bits() {
        let mut flags = FrameFlags::INTERLACED | FrameFlags::TOP_FIELD_FIRST;
        Mode::parse("prog").unwrap().apply(&mut flags);
        assert!(!flags.contains(FrameFlags::INTERLACED));
        assert!(!flags.contains(FrameFlags::TOP_FIELD_FIRST));
    }

    #[test]
    fn auto_leaves_flags_untouched() {
        let mut flags = FrameFlags::INTERLACED;
        Mode::parse("auto").unwrap().apply(&mut flags);
        assert_eq!(flags, FrameFlags::INTERLACED);
    }
}
