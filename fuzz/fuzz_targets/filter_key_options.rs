//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-key`.
//!
//! `premultiply`/`unpremultiply` parse a `planes` bitmask and an `inplace`
//! bool; `maskedmerge`/`maskedclamp`/`maskedmax`/`maskedmin`/
//! `maskedthreshold`/`threshold` parse `planes` plus a handful of small
//! integers; `colorkey`/`colorhold` parse a `color` spec (`vaco_core::
//! parse::color`, itself untrusted-input-bearing) plus two floats. Every
//! one of those goes through `vaco-opts`'s typed option parser or this
//! crate's own `eof_action`/`ts_sync_mode` name lookups (`premultiply`),
//! matching the pattern in
//! `vaco-filter-video-format`'s `filter_video_format_options.rs`.
//!
//! Property: for any byte string, for any of the ten registered names,
//! either a clean `Err` comes back or a working `Instance`, never a panic
//! and never an unbounded allocation.
//! fuzz-crate: vaco-filter-key

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_key::registry::KeyRegistry;

const NAMES: &[&str] = &[
    "colorhold",
    "colorkey",
    "maskedclamp",
    "maskedmax",
    "maskedmerge",
    "maskedmin",
    "maskedthreshold",
    "premultiply",
    "threshold",
    "unpremultiply",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = KeyRegistry;
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
