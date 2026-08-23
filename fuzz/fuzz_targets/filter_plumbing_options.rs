//! Arbitrary filtergraph text against every T1 plumbing filter's option
//! parser.
//!
//! `concat`'s `n`/`v`/`a` combine multiplicatively into a pad count
//! (`Concat::create` guards this with `checked_mul` plus a `pads::MAX`
//! ceiling — this target is what such a guard is for, D6's untrusted-input
//! bar applies to option text just as much as to bitstreams); `trim`/`atrim`
//! parse up to eight independent bound options; `select`/`setpts`/`settb`
//! each hand a string straight to `vaco_expr::Expr::parse`. All of it runs on
//! `-filter_complex` text.
//!
//! Routed through the real `vaco_filter_graph::ast::parse`/`arguments()`
//! pipeline rather than a hand-built `Instantiate`, matching the pattern in
//! `filter_audio_options.rs` (see that target's doc for why going through
//! the actual graph-string grammar matters, not just each filter's own
//! parser in isolation).
//!
//! Property: for any byte string, for any of the twenty registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-plumbing

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_plumbing::registry::PlumbingRegistry;

const NAMES: &[&str] = &[
    "acopy",
    "anull",
    "anullsink",
    "anullsrc",
    "aselect",
    "asettb",
    "asetpts",
    "asplit",
    "atrim",
    "color",
    "concat",
    "copy",
    "null",
    "nullsink",
    "nullsrc",
    "select",
    "settb",
    "setpts",
    "split",
    "trim",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, exactly the
    // bound `filter_audio_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = PlumbingRegistry;
    for &name in NAMES {
        // Route through the real graph-string grammar so the filter's parser
        // sees text the way it actually would from `-filter_complex`,
        // escaping included, rather than a hand-assembled `Instantiate`.
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
        // The result is not inspected: a clean `Err` and a working `Instance`
        // are both fine outcomes. Only a panic or a hang is a finding.
        let _ = registry.create(&req);
    }
});
