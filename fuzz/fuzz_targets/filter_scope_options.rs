//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-scope`.
//!
//! Same pattern as `filter_blur_options`/`filter_video_geometry_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline so
//! the fuzzer also exercises the graph-string grammar's own escaping, not
//! just each filter's `set_from_string`. All eleven names are exercised,
//! including the two that map to an audio-mode constructor
//! (`agraphmonitor`/`adrawgraph`) rather than the video one the bare name
//! would suggest.
//!
//! Worth fuzzing beyond the usual `vaco-opts` integer/float/string paths:
//! `datascope`/`pixscope` and `drawgraph`'s small fixed-vocabulary `mode`/
//! `format` string options, `graphmonitor`'s comma-separated `stats=`
//! keyword list (an unbounded-looking list from a short string), and
//! `waveform`/`vectorscope`'s bit-depth/mode combinations that size
//! internal histogram buffers from option values rather than frame data —
//! exactly the shape of input that should go through `vaco_limits::Budget`
//! rather than a bare allocation.
//!
//! Property: for any byte string, for any of the eleven registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-scope

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_scope::registry::ScopeRegistry;

const NAMES: &[&str] = &[
    "histogram",
    "waveform",
    "datascope",
    "thistogram",
    "graphmonitor",
    "agraphmonitor",
    "pixscope",
    "drawgraph",
    "adrawgraph",
    "vectorscope",
    "oscilloscope",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = ScopeRegistry;
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
