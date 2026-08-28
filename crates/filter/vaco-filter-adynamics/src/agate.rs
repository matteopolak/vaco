//! `agate` — audio gate.
//!
//! `ffmpeg -h filter=agate` (2026-08-23): `level_in`, `mode`
//! (`downward`/`upward`, default `downward`), `range` (linear, default
//! `0.06125` — the maximum attenuation), `threshold` (linear, default
//! 0.125), `ratio` (default 2), `attack`/`release` (ms, default 20/250),
//! `makeup`, `knee`, `detection`, `link`, `level_sc`. No `mix` option
//! (unlike `acompressor`). Shares its class name
//! (`agate/sidechaingate`) with [`crate::sidechaingate`] — same processor,
//! different sidechain source.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Dynamics};
use crate::engine::Curve;

pub const DESC: FilterDesc = FilterDesc {
    name: "agate",
    description: "audio gate",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn build(req: &Instantiate<'_>) -> Dynamics {
    let threshold = common::f64_opt(req, &["threshold"], 0.125);
    let ratio = common::f64_opt(req, &["ratio"], 2.0);
    let knee = common::f64_opt(req, &["knee"], 2.828_43);
    let curve = Curve {
        threshold_db: common::db(threshold),
        ratio,
        knee_db: common::db(knee.max(1.0)),
        mode: common::mode_opt(req),
    };
    Dynamics::new(
        common::f64_opt(req, &["level_in"], 1.0),
        curve,
        common::f64_opt(req, &["attack"], 20.0),
        common::f64_opt(req, &["release"], 250.0),
        common::f64_opt(req, &["makeup"], 1.0),
        common::f64_opt(req, &["range"], 0.061_25),
        common::link_opt(req),
        common::detection_opt(req),
        common::f64_opt(req, &["level_sc"], 1.0),
        1.0,
    )
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = build(req);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
