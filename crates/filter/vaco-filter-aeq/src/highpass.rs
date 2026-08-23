//! `highpass` — apply a high-pass filter with a 3 dB point frequency.
//!
//! `ffmpeg -h filter=highpass` (2026-08-23): `frequency`/`f` (default
//! 3000 Hz), `width_type`/`t` (default `q`), `width`/`w` (default 0.707),
//! `poles`/`p`, `mix`/`m`, `channels`/`c`. See `crate::engine::highpass`.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Biquad, ChannelSelect, Design};

pub const DESC: FilterDesc = FilterDesc {
    name: "highpass",
    description: "apply a high-pass filter with 3dB point frequency",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let design = Design::Highpass {
        f0: common::frequency_opt(req, 3000.0),
        wt: common::width_type_opt(req),
        width: common::width_opt(req, 0.707),
        poles: common::poles_opt(req),
    };
    let filter = Biquad::new(design, common::mix_opt(req), ChannelSelect::parse(req));
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
