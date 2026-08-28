//! Arbitrary filtergraph text against every `vaco-filter-analysis` filter's
//! option parser.
//!
//! Mirrors `filter_temporal_options.rs` exactly: routed through the real
//! `vaco_filter_graph::parse` pipeline so this exercises the filtergraph's
//! own escaping ahead of each filter's `Instantiate::named` reads, not a
//! hand-built `Instantiate`.
//!
//! Property: for any byte string, for any of this crate's ten registered
//! names, `create` never panics and never allocates unboundedly. This
//! crate's option parsers are all `.named(...).and_then(|v| v.parse().ok())
//! .unwrap_or(default)` (`crate::video::{f64_opt, u8_opt}`), so a malformed
//! value falls back to a default rather than erroring — this target watches
//! for the class of bug that shape can still have: `bbox`'s `min_val` and
//! `blackframe`'s `threshold`/`thresh` parse as `u8` (so an out-of-range
//! numeral like `999` must fail `.parse()` and fall back rather than wrap or
//! panic) and `blackdetect`'s four aliased float options
//! (`picture_black_ratio_th`/`pic_th`, `pixel_black_th`/`pix_th`) must not
//! produce a threshold outside `[0,255]` when multiplied out in
//! `frame_is_black`, which is checked here by never constructing a frame at
//! all — only `create` is called, so this target cannot itself observe that
//! computation, but it does exercise every alias/parse path that feeds it.
//! fuzz-crate: vaco-filter-analysis

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_analysis::registry::AnalysisRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &[
    "bbox",
    "blackdetect",
    "blackframe",
    "cropdetect",
    "entropy",
    "identity",
    "msad",
    "psnr",
    "signalstats",
    "ssim",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = AnalysisRegistry;
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
