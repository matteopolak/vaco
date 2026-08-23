//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-geometry`.
//!
//! Same pattern as the sibling `vaco-filter-video-geometry` crate's
//! `filter_video_geometry_options.rs` (see that target's doc for why the
//! real `vaco_filter_graph::ast::parse`/`arguments()` pipeline is used
//! rather than a hand-built `Instantiate`): every registered name gets the
//! same arbitrary argument text, run through `vaco-expr` parsing
//! (`swaprect`, `perspective`), `vaco_core::parse::color` (`fillborders`),
//! integer/float range checks (`pixelize`, `field`, `il`, `shuffleplanes`),
//! and `tile`/`untile`'s `layout` string parser and
//! `shuffleframes`'s whitespace-separated `mapping` parser.
//!
//! Property: for any byte string, for any of this crate's seventeen
//! registered names, either a clean `Err` comes back at some stage or a
//! working `Instance`, never a panic and never an unbounded allocation.
//! Four names — `framepack`, `mergeplanes`, `alphamerge`, `extractplanes`
//! — were added when `vaco-filter-core::adapt::Paired`/`Fanout` closed
//! `planning/INTERFACE-GAPS.md` gap 10 and let this crate register them;
//! `mergeplanes`' dynamic input count (from its own `map<N>s` options) and
//! `extractplanes`' dynamic output count (from `planes`) both go through
//! the same `Instantiate` path as every other name here, so no separate
//! harness is needed for the dynamic-pad-count machinery itself.
//! fuzz-crate: vaco-filter-geometry

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_geometry::registry::T2GeometryRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "alphaextract",
    "alphamerge",
    "extractplanes",
    "field",
    "fillborders",
    "framepack",
    "il",
    "mergeplanes",
    "perspective",
    "pixelize",
    "scroll",
    "shuffleframes",
    "shuffleplanes",
    "swaprect",
    "swapuv",
    "tile",
    "untile",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = T2GeometryRegistry;
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
