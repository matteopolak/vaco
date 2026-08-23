//! Arbitrary filtergraph text against `overlay` and `rotate`'s option
//! parsers in `vaco-filter-video-composite`.
//!
//! `overlay`'s `x`/`y` and `rotate`'s `angle`/`out_w`/`out_h` all go through
//! `vaco_expr::Expr::parse` with the extra `rotw`/`roth` externs `rotate`
//! declares; `eof_action`/`eval`/`format`/`alpha`/`ts_sync_mode` parse
//! against small fixed vocabularies. All of it runs on `-filter_complex`
//! text through the real `vaco_filter_graph::parse`/`arguments()` pipeline —
//! not a hand-built `Instantiate` — matching the pattern in
//! `vaco-filter-video-geometry`'s `filter_video_geometry_options` target
//! (see that target's doc for why: two agents on this project once probed a
//! parser *through* a filtergraph and mistook the filtergraph's own
//! unescaping for the parser's behaviour, which is a trap in the opposite
//! direction from this one — here the filtergraph front door is the
//! deliberate, most-direct entry point available for `overlay`/`rotate`
//! specifically, because their options are never reachable except through a
//! `name=args` fragment, unlike a bare expression parser that has its own
//! direct `Expr::parse` fuzz target in `vaco-expr`).
//!
//! Property: for any byte string, for either registered name, either a
//! clean `Err` comes back at some stage or a working `Instance`, never a
//! panic and never an unbounded allocation. An `x`/`angle` expression that
//! evaluates to a huge or non-finite number is exactly the shape of input
//! this target can produce, and is the reason `blend::to_pixel`/`clip` and
//! `rotate`'s `ow`/`oh` finiteness check exist rather than a bare cast.
//!
//! fuzz-crate: vaco-filter-video-composite

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_video_composite::registry::CompositeRegistry;

const NAMES: &[&str] = &["overlay", "rotate"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_audio_options`/`filter_video_geometry_options` use
    // for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = CompositeRegistry;
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
