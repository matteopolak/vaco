//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-asource`.
//!
//! Same shape as `vaco-filter-source`'s own fuzz target: exercises
//! `vaco_opts` parsing across this crate's six generators (numeric ranges,
//! enums, expressions, channel layouts) against `AsourceRegistry`.
//!
//! Property: for any byte string, for any registered name, either a clean
//! `Err` comes back at some stage or a working `Instance`, never a panic
//! and never an unbounded allocation.
//! fuzz-crate: vaco-filter-asource

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_asource::registry::AsourceRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &["sine", "anoisesrc", "aevalsrc", "afdelaysrc", "sinc", "hilbert"];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = AsourceRegistry;
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
