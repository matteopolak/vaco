//! Arbitrary filtergraph text against every `vaco-filter-achannel` filter's
//! option parser.
//!
//! Mirrors `filter_audio_eq_options.rs` and `filter_audio_dynamics_options.rs`
//! exactly: routed through the real `vaco_filter_graph::parse` pipeline so
//! this exercises the filtergraph's own escaping ahead of each filter's
//! `Instantiate::named` reads, not a hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of the seven registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation. `axcorrelate`'s `size`
//! option is the specific worry this target is aimed at — it is clamped to
//! `2..=131_072` in `axcorrelate::create` specifically so an attacker-sized
//! `size=99999999999999999999` (which does not fit `usize` at all, let alone
//! a sane window) cannot turn into an unbounded `VecDeque` allocation.
//! fuzz-crate: vaco-filter-achannel

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_achannel::registry::AchannelRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "axcorrelate",
    "crossfeed",
    "earwax",
    "extrastereo",
    "haas",
    "stereotools",
    "stereowiden",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = AchannelRegistry;
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
