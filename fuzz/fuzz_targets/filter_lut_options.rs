//! Two untrusted-input surfaces in `vaco-filter-lut`, fuzzed together.
//!
//! `lut3d`'s and `haldclut`'s option strings go through the registry, same
//! pattern as `vaco-filter-video-format`'s `filter_video_format_options.rs`
//! (a fuzzed `file=` path mostly just fails to open, harmlessly — the
//! filesystem call is read-only). The more interesting untrusted input is
//! **file content**: `lut3d`'s `.cube` parser
//! (`vaco_filter_lut::lut3d::Cube3d::parse`) runs directly on bytes that,
//! in a real deployment, come from a `.cube` file an attacker could
//! supply, so it is fuzzed directly on the same input too, not only
//! reachable through a file path the fuzzer would have to happen to hit.
//!
//! Property: for any byte string, interpreted both as filtergraph option
//! text for `haldclut`/`lut3d` and as raw `.cube` file content, either a
//! clean `Err` or a working value comes back — never a panic, never an
//! unbounded allocation.
//! fuzz-crate: vaco-filter-lut

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_lut::lut3d::Cube3d;
use vaco_filter_lut::registry::LutRegistry;

const NAMES: &[&str] = &["haldclut", "lut3d"];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let _ = Cube3d::parse(args);

    let registry = LutRegistry;
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
