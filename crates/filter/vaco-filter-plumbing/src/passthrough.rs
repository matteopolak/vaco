//! `null`, `anull`, `copy`, `acopy` — pass the input through unchanged.
//!
//! All four are the same one-line filter body under four names: `null`/`copy`
//! for video, `anull`/`acopy` for audio. The reference keeps them as separate
//! registered names (`null`: "Pass the source unchanged to the output",
//! `copy`: "Copy the input video unchanged to the output" — different words,
//! identical behaviour, `ffmpeg -h filter=null`/`copy` both list zero
//! options), so this keeps them separate too rather than aliasing one to the
//! other, in case a future differential check on `-filters` output cares
//! about the distinct descriptions.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(input))
    }
}

macro_rules! passthrough_filter {
    ($mod_name:ident, $desc_name:literal, $description:literal, $pads:expr, $media:expr) => {
        pub mod $mod_name {
            // Each invocation names only one of `AUDIO_PAD`/`VIDEO_PAD` and
            // never `Pad` itself (only through the constant's own type), so
            // the other is unused for that instantiation of the macro.
            #[allow(unused_imports)]
            use super::{
                AUDIO_PAD, Filter, FilterDesc, FilterFlags, Instance, Instantiate, MediaType,
                NodeFormats, Pad, Simple, VIDEO_PAD,
            };

            pub const DESC: FilterDesc = FilterDesc {
                name: $desc_name,
                description: $description,
                inputs: $pads,
                outputs: $pads,
                flags: FilterFlags::empty(),
            };

            #[allow(
                clippy::unnecessary_wraps,
                reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> \
                          signature every filter in this crate's registry.rs dispatches through"
            )]
            pub(crate) fn create(
                req: &Instantiate<'_>,
            ) -> std::result::Result<Instance, String> {
                Ok(Instance {
                    desc: DESC,
                    formats: NodeFormats::passthrough(1, 1, $media, req.instance),
                    filter: Box::new(Simple::new(Filter)),
                })
            }
        }
    };
}

passthrough_filter!(
    null,
    "null",
    "Pass the source unchanged to the output",
    VIDEO_PAD,
    MediaType::Video
);
passthrough_filter!(
    anull,
    "anull",
    "Pass the source unchanged to the output",
    AUDIO_PAD,
    MediaType::Audio
);
passthrough_filter!(
    copy,
    "copy",
    "Copy the input video unchanged to the output",
    VIDEO_PAD,
    MediaType::Video
);
passthrough_filter!(
    acopy,
    "acopy",
    "Copy the input audio unchanged to the output",
    AUDIO_PAD,
    MediaType::Audio
);
