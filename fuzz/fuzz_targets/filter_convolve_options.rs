//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-convolve`.
//!
//! Same pattern as `filter_blur_options`/`filter_video_geometry_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline (not
//! a hand-built `Instantiate`) so the fuzzer also exercises the
//! graph-string grammar's own escaping, not just each filter's
//! `set_from_string`.
//!
//! This crate's option parsers include the one thing worth fuzzing beyond
//! the usual `vaco-opts` integer/float/string paths: `convolution`'s
//! whitespace-separated matrix parser (`Kernel::parse`, arbitrary length,
//! `square`/`row`/`column` mode, and the `rdiv=0`-is-a-sentinel handling).
//!
//! Property: for any byte string, for any of the nine registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-convolve

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_convolve::registry::ConvolveRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "convolution",
    "dilation",
    "erosion",
    "kirsch",
    "median",
    "prewitt",
    "roberts",
    "scharr",
    "sobel",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = ConvolveRegistry;
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
