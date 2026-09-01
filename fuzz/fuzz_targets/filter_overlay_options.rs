//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-overlay` — the two-input `blend`/`multiply`/`mix`/
//! `xmedian`/`xfade`/`displace`/`remap` family (not `vaco-filter-video-
//! composite`'s single-input `overlay`, a different crate with a name that
//! collides only in English, never in the registry namespace).
//!
//! Same pattern as `filter_video_composite_options`/`filter_blur_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline so
//! the fuzzer also exercises the graph-string grammar's own escaping, not
//! just each filter's `set_from_string`. Every filter here only needs
//! option-string parsing to fail or succeed — none of the `create` paths
//! need a second frame to construct an `Instance`.
//!
//! Worth fuzzing beyond the usual `vaco-opts` integer/float/string paths:
//! `blend`'s per-plane `all_mode`/`Xexpr` mini-language (mode name plus an
//! optional `vaco_expr::Expr` for `blend`/`grainmerge`-family custom
//! blends), `xfade`'s `transition` name-to-enum lookup plus `duration`/
//! `offset` as fixed-point time strings, and `xmedian`/`mix`'s bounded
//! `nb_inputs`/`weights` list parsers (a huge `nb_inputs` here is exactly
//! the shape of input `vaco_limits::Budget` exists to bound before any
//! per-input `Vec` is sized against it).
//!
//! Property: for any byte string, for any of the seven registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-overlay

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_overlay::registry::OverlayRegistry;

const NAMES: &[&str] = &[
    "blend", "multiply", "mix", "xmedian", "xfade", "displace", "remap",
];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = OverlayRegistry;
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
