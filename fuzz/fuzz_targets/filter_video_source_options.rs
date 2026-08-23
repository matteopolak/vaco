//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-video-source`.
//!
//! `pal100bars`/`pal75bars` share `vaco-filter-plumbing::color`'s option
//! shape (`size`, `rate`, `duration`, `sar`), so this exercises the same
//! `vaco_opts` machinery that crate's own fuzz target does, against this
//! crate's own registry. `size=99999999x99999999` is exactly the
//! `vaco-limits`-shaped finding this target exists to catch — an oversized
//! allocation request must be refused by `FramePool::acquire_video`
//! (`vaco-frame`'s own limits), not attempted.
//!
//! Property: for any byte string, for either of the two registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-video-source

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_video_source::registry::SourceRegistry;

const NAMES: &[&str] = &["pal100bars", "pal75bars"];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = SourceRegistry;
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
