//! Arbitrary filtergraph text against every `vaco-filter-denoise` filter's
//! option parser.
//!
//! Mirrors `filter_audio_eq_options.rs` exactly: routed through the real
//! `vaco_filter_graph::parse` pipeline so this exercises the filtergraph's
//! own escaping ahead of each filter's `Instantiate::named`/`positional`
//! reads, not a hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of this crate's eight registered
//! names, `create` never panics and never allocates unboundedly — every
//! option parser here is `.named(...).and_then(|v| v.parse().ok())
//! .unwrap_or(default)` or a positional equivalent, so a malformed or
//! out-of-range value falls back to a default rather than erroring, and
//! `create()` for this crate cannot fail at all (its signature has no
//! `Result`). What this target actually watches for is exactly the class of
//! bug that shape can still have: an option value parsed but not clamped
//! before it reaches an array index, a block-size calculation, or a
//! division (`removegrain`'s mode indices, `dctdnoiz`/`fftdnoiz`'s block
//! sizes, `nlmeans`'s patch/research radii, `vaguedenoiser`'s `nsteps`).
//! fuzz-crate: vaco-filter-denoise

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_denoise::registry::DenoiseRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "atadenoise",
    "dctdnoiz",
    "fftdnoiz",
    "hqdn3d",
    "nlmeans",
    "owdenoise",
    "removegrain",
    "vaguedenoiser",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = DenoiseRegistry;
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
