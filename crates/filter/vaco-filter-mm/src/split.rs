//! `split`/`asplit` — fan one input out to N identical outputs.
//!
//! `ffmpeg -h filter=(a)split` documents one option, `outputs`, default 2.
//! Cloning a [`Frame`] is a handful of `Arc` refcount bumps (one per plane),
//! never a pixel copy, so fan-out costs nothing per extra consumer — the
//! shape mock.rs's own `Split` proves for the framework.

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{
    Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad,
};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate, pads};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "split", help = "pass on the input to N outputs")]
pub(crate) struct Opts {
    #[opt(
        name = "outputs",
        help = "number of outputs",
        default = 2,
        range = 1..=4096,
        flags(filtering)
    )]
    pub outputs: i32,
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
    outputs: usize,
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if (0..self.outputs).any(|p| !ctx.output_has_room(p)) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        if let Some(frame) = ctx.take_input(0) {
            for pad in 0..self.outputs {
                ctx.push_output(pad, frame.clone())?;
            }
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }
}

fn build(
    media: MediaType,
    desc: FilterDesc,
    req: &Instantiate<'_>,
) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let outputs = usize::try_from(opts.outputs.max(1)).unwrap_or(1);
    let output_pads =
        pads::of(media, outputs).ok_or_else(|| "split: too many outputs".to_owned())?;
    Ok(Instance {
        desc: FilterDesc {
            outputs: output_pads,
            ..desc
        },
        formats: NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: vec![FormatSet::default(); outputs],
            ties: Tie::all_pads(1, outputs, media),
            label: req.instance.to_owned(),
        },
        filter: Box::new(Filter { outputs }),
    })
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "split",
        description: "Pass on the input to N video outputs",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Video, DESC, req)
    }
}

pub mod audio {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Pad, build};

    const AUDIO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "asplit",
        description: "Pass on the audio input to N audio outputs",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(MediaType::Audio, DESC, req)
    }
}
