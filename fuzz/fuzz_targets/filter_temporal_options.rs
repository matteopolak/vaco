//! Arbitrary filtergraph text against every `vaco-filter-temporal` filter's
//! option parser.
//!
//! Mirrors `filter_denoise_options.rs` exactly: routed through the real
//! `vaco_filter_graph::parse` pipeline so this exercises the filtergraph's
//! own escaping ahead of each filter's `Instantiate::named` reads, not a
//! hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of this crate's sixteen
//! registered names, `create` never panics and never allocates unboundedly.
//! Most of this crate's option parsers are `.named(...).and_then(|v|
//! v.parse().ok()).unwrap_or(default)`, so a malformed value falls back to a
//! default rather than erroring — this target watches for the class of bug
//! that shape can still have: an option parsed but not clamped before it
//! reaches a window size (`tmedian`/`tmidequalizer`'s `radius`, `tmix`'s
//! `frames`, `decimate`'s `cycle`), a block grid (`mpdecimate`'s implicit
//! 8x8 blocking over an unclamped frame size — not exercised here, since
//! this target never allocates a frame, only parses options), or an
//! expression (`tblend`/`tlut2`'s `cN_expr`/`cN`, parsed through
//! `vaco-expr`, which has its own fuzz target for the language itself but is
//! reached here with `vaco-filter-temporal`'s own variable bindings).
//! `fsync`'s `file` option is exercised too: a bogus path is expected to
//! produce a clean `Err`, never a panic, and never actually block on I/O
//! since the fuzzer only ever hands it paths it invented.
//! fuzz-crate: vaco-filter-temporal

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_temporal::registry::TemporalRegistry;

const NAMES: &[&str] = &[
    "decimate",
    "deflicker",
    "dejudder",
    "framestep",
    "freezedetect",
    "freezeframes",
    "fsync",
    "lagfun",
    "mpdecimate",
    "random",
    "tblend",
    "tlut2",
    "tmedian",
    "tmidequalizer",
    "tmix",
    "tpad",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = TemporalRegistry;
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
