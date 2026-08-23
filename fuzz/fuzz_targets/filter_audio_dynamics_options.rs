//! Arbitrary filtergraph text against every `vaco-filter-audio-dynamics`
//! filter's option parser.
//!
//! Mirrors `filter_audio_options.rs`/`filter_audio_eq_options.rs`: routed
//! through the real `vaco_filter_graph::parse` pipeline. `sidechaincompress`/
//! `sidechaingate` declare two input pads; `Instantiate` construction does
//! not care how many pads a filter declares (pad wiring happens later, in
//! graph connection, which this target never reaches), so the same
//! single-`Instantiate` shape covers them too.
//!
//! Property: for any byte string, for any of the fourteen registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-audio-dynamics

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_audio_dynamics::registry::DynamicsRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "acompressor",
    "agate",
    "alimiter",
    "astats",
    "compand",
    "dynaudnorm",
    "loudnorm",
    "mcompand",
    "sidechaincompress",
    "sidechaingate",
    "silencedetect",
    "silenceremove",
    "speechnorm",
    "volumedetect",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = DynamicsRegistry;
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
