//! `bass` — boost or cut lower frequencies. A low shelf; `ffmpeg -h
//! filter=bass` prints the shared class name `bass/lowshelf` (probed
//! 2026-08-23), confirming it is the *same* registered filter as
//! [`crate::lowshelf`] under a friendlier name, not a second implementation.
//!
//! Defaults: `frequency`/`f` 100 Hz, `width_type`/`t` `q`, `width`/`w` 0.5,
//! `gain`/`g` 0 dB, `poles`/`p` 2 (only 2-pole is implemented; see
//! `crate::engine::lowshelf`), `mix`/`m`, `channels`/`c`.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Biquad, ChannelSelect, Design};

pub const DESC: FilterDesc = FilterDesc {
    name: "bass",
    description: "boost or cut lower frequencies",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let design = Design::Lowshelf {
        f0: common::frequency_opt(req, 100.0),
        wt: common::width_type_opt(req),
        width: common::width_opt(req, 0.5),
        gain_db: common::gain_opt(req, 0.0),
    };
    let filter = Biquad::new(design, common::mix_opt(req), ChannelSelect::parse(req));
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
