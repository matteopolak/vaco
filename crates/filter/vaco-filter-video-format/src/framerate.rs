//! `framerate` — **structural only**.
//!
//! The reference's `framerate` converts frame rate by motion-compensated
//! blending between the two input frames straddling each output instant
//! (`interp_start`/`interp_end` control the blend window, `scene` gates it
//! off across a scene cut). That needs `vaco-filter-vdsp`'s `scene_sad` and a
//! motion-estimation/blend kernel — plan 16 §4.1 places both in
//! `vaco-filter-vdsp`/`vaco-filter-motion`, neither of which this crate
//! depends on or owns.
//!
//! What is registered here instead is [`crate::fps::Filter`]'s zero-order
//! hold (duplicate/drop) with `framerate`'s own option names accepted where
//! they overlap. This is **not** the reference's algorithm — there is no
//! blending at all — so treat `framerate` from this crate as "keeps the
//! stream at a constant rate the same way `fps` does", not as a fidelity
//! claim. Real motion-compensated interpolation is future work once
//! `vaco-filter-vdsp` exists.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Pad};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "framerate",
    description: "Upsamples or downsamples progressive source between specified frame rates",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "framerate",
    help = "Upsamples or downsamples progressive source between specified frame rates"
)]
pub(crate) struct Opts {
    #[opt(
        name = "fps",
        help = "required output frames per second rate",
        default = "50".to_owned(),
        flags(video, filtering)
    )]
    pub fps: String,
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

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let fps_opts = crate::fps::Opts {
        fps: opts.fps,
        start_time: f64::MAX,
        round: "near".to_owned(),
        eof_action: "round".to_owned(),
    };
    let filter = crate::fps::Filter::new(&fps_opts)?;
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
    fn registers_and_builds_a_running_filter() {
        let req = Instantiate {
            name: "framerate",
            instance: "framerate",
            args: Some("fps=30"),
            arguments: &[],
        };
        assert!(create(&req).is_ok());
    }
}
