//! Arbitrary filtergraph text against every `vaco-filter-aeq` filter's
//! option parser.
//!
//! Mirrors `filter_audio_options.rs` (the T1 audio crate's fuzz target)
//! exactly: routed through the real `vaco_filter_graph::parse` pipeline so
//! this exercises the filtergraph's own escaping ahead of each filter's
//! `Instantiate::named` reads, not a hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of the seventeen registered names
//! (fifteen from FT-4.8a, plus `aemphasis`/`atilt` from FT-4.13e, GitHub
//! #485), either a clean `Err` comes back at some stage or a working
//! `Instance`,
//! never a panic and never an unbounded allocation. This is also where the
//! brief's specific worry — a cutoff of 0 Hz or above Nyquist driving a
//! biquad's coefficients non-finite, and `NaN` propagating silently through
//! a whole stream — is exercised: `vaco_filter_adsp::biquad::Coeffs::normalise` is supposed to
//! catch every such case and this target is what would eventually surface a
//! gap in that guard if creation itself doesn't panic (a frame would still
//! be needed to observe `NaN` in the *output*, which this target does not
//! push through the graph — see `docs/filter/vaco-filter-aeq.md`).
//! fuzz-crate: vaco-filter-aeq

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_aeq::registry::EqRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "aemphasis",
    "allpass",
    "anequalizer",
    "atilt",
    "bandpass",
    "bandreject",
    "bass",
    "biquad",
    "equalizer",
    "firequalizer",
    "highpass",
    "highshelf",
    "lowpass",
    "lowshelf",
    "superequalizer",
    "tiltshelf",
    "treble",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = EqRegistry;
    for &name in NAMES {
        let text = format!("{name}={args}");
        let Ok(ast) = vaco_filter_graph::parse(&text) else {
            continue;
        };
        let Some(spec) = ast.chains.first().and_then(|c| c.filters.first()) else {
            continue;
        };
        let Ok(arguments) = spec.arguments() else {
            continue;
        };
        let req = Instantiate {
            name: &spec.name,
            instance: spec.instance.as_deref().unwrap_or(&spec.name),
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        let _ = registry.create(&req);
    }
});
