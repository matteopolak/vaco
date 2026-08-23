//! `asetrate` — change the sample rate tag without touching the data.
//!
//! Sample bytes pass through completely unchanged; only the frame's declared
//! sample rate (and, to keep duration arithmetic consistent, the output
//! link's time base) changes. Downstream, the same tick count now spans a
//! different amount of wall-clock time, which is what produces the
//! pitch/speed change this filter is for — it is metadata surgery, not a
//! resample.
//!
//! `ffmpeg -h filter=asetrate` documents one option, `sample_rate`/`r`,
//! default 44100. Implemented.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "asetrate",
    description: "change the sample rate without altering the data",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
pub(crate) struct Filter {
    rate: u32,
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                sample_rate,
                time_base,
                ..
            } = &mut out
            {
                *sample_rate = self.rate;
                *time_base =
                    vaco_core::Rational::new(1, i32::try_from(self.rate.max(1)).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let FrameData::Audio { sample_rate, .. } = &mut input.data {
            *sample_rate = self.rate;
        }
        input.time_base = vaco_core::Rational::new(1, i32::try_from(self.rate.max(1)).unwrap_or(1));
        Ok(FrameOut::One(input))
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> signature every filter in this crate's registry.rs dispatches through, even though this particular filter never fails today"
)]
pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let rate = req
        .named("sample_rate")
        .or_else(|| req.named("r"))
        .or_else(|| req.positional(0))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(44100)
        .max(1);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Filter { rate })),
    })
}
