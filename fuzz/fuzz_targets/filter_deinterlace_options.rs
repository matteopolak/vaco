//! Arbitrary filtergraph text against every `vaco-filter-deinterlace`
//! filter's option parser.
//!
//! Mirrors `filter_temporal_options.rs` exactly: routed through the real
//! `vaco_filter_graph::parse` pipeline so this exercises the filtergraph's
//! own escaping ahead of each filter's `Instantiate::named` reads, not a
//! hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of this crate's twenty
//! registered names, `create` never panics and never allocates unboundedly.
//! `fieldhint`'s `hint` option in particular exercises a real file-open
//! path (`std::fs::File::open`) with an arbitrary, fuzzer-controlled
//! string: a bogus or nonexistent path is expected to fail cleanly with an
//! `Err`, never a panic and never a hang, the same contract
//! `vaco-filter-temporal::fsync`'s own fuzz target documents for its `file`
//! option. `telecine`/`detelecine`'s `pattern` string is parsed digit by
//! digit (`crate::telecine::parse_pattern`) and always falls back to `"23"`
//! rather than erroring, so this also exercises that fallback against
//! arbitrary text.
//! fuzz-crate: vaco-filter-deinterlace

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_deinterlace::registry::DeinterlaceRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "bwdif",
    "detelecine",
    "doubleweave",
    "estdif",
    "fieldhint",
    "fieldmatch",
    "fieldorder",
    "idet",
    "interlace",
    "kerndeint",
    "phase",
    "pullup",
    "repeatfields",
    "separatefields",
    "telecine",
    "tinterlace",
    "vfrdet",
    "w3fdif",
    "weave",
    "yadif",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = DeinterlaceRegistry;
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
