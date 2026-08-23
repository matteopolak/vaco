//! `nullsink`/`anullsink` — discard the input. No options.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};

use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if ctx.take_input(0).is_some() {
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            return Ok(Activity::Eof);
        }
        ctx.request_input(0);
        Ok(Activity::NeedInput)
    }
}

macro_rules! sink_filter {
    ($mod_name:ident, $desc_name:literal, $description:literal, $media:expr) => {
        pub mod $mod_name {
            use super::{Filter, FilterDesc, FilterFlags, FormatSet, Instance, Instantiate, MediaType, NodeFormats, Pad};

            pub const DESC: FilterDesc = FilterDesc {
                name: $desc_name,
                description: $description,
                inputs: &[Pad {
                    name: "default",
                    media_type: $media,
                }],
                outputs: &[],
                flags: FilterFlags::empty(),
            };

            #[allow(
                clippy::unnecessary_wraps,
                reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> \
                          signature every filter in this crate's registry.rs dispatches through"
            )]
            pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
                Ok(Instance {
                    desc: DESC,
                    formats: NodeFormats {
                        inputs: vec![FormatSet::default()],
                        outputs: Vec::new(),
                        ties: Vec::new(),
                        label: req.instance.to_owned(),
                    },
                    filter: Box::new(Filter),
                })
            }
        }
    };
}

sink_filter!(
    video,
    "nullsink",
    "Do absolutely nothing with the input video",
    MediaType::Video
);
sink_filter!(
    audio,
    "anullsink",
    "Do absolutely nothing with the input audio",
    MediaType::Audio
);
