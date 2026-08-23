//! Arbitrary filtergraph text against every `vaco-filter-ameasure` filter's
//! option parser.
//!
//! Mirrors `filter_audio_dynamics_options.rs`/`filter_audio_eq_options.rs`:
//! routed through the real `vaco_filter_graph::parse` pipeline. `apsnr`/
//! `asdr`/`asisdr` declare two input pads; `Instantiate` construction does
//! not care how many pads a filter declares (pad wiring happens later, in
//! graph connection, which this target never reaches), so the same
//! single-`Instantiate` shape covers them too.
//!
//! Property: for any byte string, for any of the eleven registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation. `aspectralstats`'
//! `win_size` option is the one most worth fuzzing here specifically — it
//! feeds a `vaco-tx` FFT plan length, and this target is what proves a
//! hostile `win_size=` string cannot turn into an unbounded or panicking
//! allocation despite the reference accepting up to `65536`.
//! fuzz-crate: vaco-filter-ameasure

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_ameasure::registry::AmeasureRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "aderivative",
    "aintegral",
    "aphasemeter",
    "apsnr",
    "asdr",
    "asisdr",
    "aspectralstats",
    "ashowinfo",
    "drmeter",
    "ebur128",
    "replaygain",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = AmeasureRegistry;
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
