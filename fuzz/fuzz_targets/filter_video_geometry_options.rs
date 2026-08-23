//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-video-geometry`.
//!
//! `crop`/`pad` hand `w`/`h`/`x`/`y` straight to `vaco_expr::Expr::parse`;
//! `scale` has its own hand-rolled `w`/`h`/`size` presence logic (this
//! crate's one deliberate divergence from the reference's own asymmetric
//! `w`-alone-errors behaviour, see `scale.rs`'s doc); `transpose` parses
//! `dir`/`passthrough` against a small fixed vocabulary. All of it runs on
//! `-filter_complex` text, matching the pattern in `vaco-filter-audio`'s
//! `filter_audio_options.rs` (see that target's doc for why the real
//! `vaco_filter_graph::ast::parse`/`arguments()` pipeline is used rather
//! than a hand-built `Instantiate`).
//!
//! Property: for any byte string, for any of the six registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-video-geometry

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_video_geometry::registry::GeometryRegistry;

const NAMES: &[&str] = &["crop", "hflip", "pad", "scale", "transpose", "vflip"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_audio_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = GeometryRegistry;
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
