//! `treble` — boost or cut upper frequencies. A high shelf; `ffmpeg -h
//! filter=treble` prints the shared class name `treble/high/tiltshelf`
//! (probed 2026-08-23) — `treble` and [`crate::highshelf`] are the same
//! registered algorithm under two names, while `tiltshelf` (also listed
//! there) shares the option *schema* but is a genuinely different transfer
//! function; see [`crate::tiltshelf`].
//!
//! Defaults: `frequency`/`f` 3000 Hz, `width_type`/`t` `q`, `width`/`w` 0.5,
//! `gain`/`g` 0 dB.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Biquad, ChannelSelect, Design};

pub const DESC: FilterDesc = FilterDesc {
    name: "treble",
    description: "boost or cut upper frequencies",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let design = Design::Highshelf {
        f0: common::frequency_opt(req, 3000.0),
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
