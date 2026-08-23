//! `biquad` — apply a biquad IIR filter with directly-supplied coefficients.
//!
//! `ffmpeg -h filter=biquad` (2026-08-23): `a0`/`a1`/`a2`/`b0`/`b1`/`b2`
//! (defaults `a0=1`, everything else `0` — the identity section), `mix`/`m`,
//! `channels`/`c`. Unlike every other filter in this crate, coefficients here
//! are not derived from a design frequency at all — they *are* the option
//! values, normalised by `a0` exactly as [`vaco_filter_adsp::biquad::Coeffs`] normalises
//! every cookbook formula's output.

use vaco_core::MediaType;
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterDesc, FilterFlags, Timeline};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, Biquad, ChannelSelect, Design};
use vaco_filter_adsp::biquad::Coeffs;

pub const DESC: FilterDesc = FilterDesc {
    name: "biquad",
    description: "apply a biquad IIR filter with the given coefficients",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let a0 = common::f64_opt(req, &["a0"], 1.0);
    let a1 = common::f64_opt(req, &["a1"], 0.0);
    let a2 = common::f64_opt(req, &["a2"], 0.0);
    let b0 = common::f64_opt(req, &["b0"], 0.0);
    let b1 = common::f64_opt(req, &["b1"], 0.0);
    let b2 = common::f64_opt(req, &["b2"], 0.0);
    // `Coeffs::normalise` falls back to the identity section for a zero,
    // non-finite, or overflowing `a0` rather than letting `NaN`/`inf` reach a
    // sample — the failure mode the fuzz target for this crate targets.
    let coeffs = Coeffs::normalise(b0, b1, b2, a0, a1, a2);
    let filter = Biquad::new(
        Design::Raw(coeffs),
        common::mix_opt(req),
        ChannelSelect::parse(req),
    );
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
