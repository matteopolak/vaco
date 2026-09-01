//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-draw-vf`.
//!
//! Same pattern as `filter_blur_options`/`filter_video_geometry_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline (not
//! a hand-built `Instantiate`) so the fuzzer also exercises the
//! graph-string grammar's own escaping, not just each filter's
//! `set_from_string`.
//!
//! This crate's option parsers include the things worth fuzzing beyond the
//! usual `vaco-opts` integer/float/string paths: `drawbox`/`drawgrid`'s
//! `w`/`h`/`x`/`y`/`thickness` expressions (`vaco_expr::Expr::parse`, which
//! can produce huge or non-finite values from a short string) and both
//! filters' `color=`/`c=` parser in `color.rs`, which accepts `0xRRGGBB`,
//! `0xRRGGBBAA`, named colours and an `@alpha` suffix over arbitrary bytes.
//!
//! Property: for any byte string, for either of the two registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-draw-vf

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_draw_vf::registry::DrawVfRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &["drawbox", "drawgrid"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = DrawVfRegistry;
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
