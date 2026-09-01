//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-artistic`.
//!
//! `removelogo_pgm_parse` already covers this crate's one genuinely
//! untrusted *file-content* surface (`removelogo`'s PGM mask parser) by
//! calling `parse_pgm` directly; it does not exercise this crate's option
//! *strings*, which is a different front door (`-filter_complex` text) and
//! a different parser (`vaco-opts`' `set_from_string` plus, for
//! `delogo`/`vignette`, `vaco_expr::Expr::parse`). Same pattern as
//! `filter_blur_options`/`filter_video_geometry_options`: run the real
//! `vaco_filter_graph::ast::parse`/`arguments()` pipeline so the fuzzer
//! also exercises the graph-string grammar's own escaping.
//!
//! Worth fuzzing beyond the usual `vaco-opts` integer/float/string paths:
//! `delogo`'s and `vignette`'s `x`/`y`/`w`/`h`/`angle`/`x0`/`y0` expressions
//! (which can evaluate to huge or non-finite numbers from a short string,
//! the same class `filter_video_composite_options` documents for
//! `overlay`/`rotate`), and `noise`'s per-plane `strength`/`flags` list
//! parser (`Opts::parse` at `noise.rs:81`, a second, nested parser fed
//! from one option value).
//!
//! Property: for any byte string, for any of the six registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-artistic

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_artistic::registry::ArtisticRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] = &["amplify", "delogo", "epx", "noise", "removelogo", "vignette"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = ArtisticRegistry;
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
