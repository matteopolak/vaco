//! `equalizer` — apply a two-pole peaking equalization (EQ) filter.
//!
//! `ffmpeg -h filter=equalizer` (2026-08-23): `frequency`/`f` (default
//! 0 Hz), `width_type`/`t` (default `q`), `width`/`w` (default 1),
//! `gain`/`g` (dB, default 0), `mix`/`m`, `channels`/`c`,
//! `normalize`/`n` (accepted, not applied — see `crate::common`'s doc on
//! options this crate does not implement). See `crate::engine::peaking`:
//! `gain=0` is verified to be the identity, and the gain at `frequency`
//! matches `gain` for several settings.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Biquad, ChannelSelect, Design};

pub const DESC: FilterDesc = FilterDesc {
    name: "equalizer",
    description: "apply two-pole peaking equalization (EQ) filter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let design = Design::Peaking {
        f0: common::frequency_opt(req, 0.0),
        wt: common::width_type_opt(req),
        width: common::width_opt(req, 1.0),
        gain_db: common::gain_opt(req, 0.0),
    };
    let filter = Biquad::new(design, common::mix_opt(req), ChannelSelect::parse(req));
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
