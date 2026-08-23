//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-video-format`.
//!
//! `format` splits `pix_fmts` on `|` and resolves each name through
//! `vaco_pixfmt::PixFmt::from_name`; `noformat` mirrors that but builds the
//! complement over `PixFmt::all()`; `setsar`/`setdar` hand their ratio
//! straight to `vaco_core::parse::rational`, which itself falls back to a
//! full `vaco-expr` parse for anything that is not a bare `int:int` or
//! integer; `fps`/`framerate` parse a rational plus three small fixed
//! vocabularies (`round`, `eof_action`); `setparams` fuses
//! `setfield`+`setrange` plus four `vaco_color::from_name` lookups. All of
//! it runs on `-filter_complex` text, matching the pattern in
//! `vaco-filter-audio`'s `filter_audio_options.rs`.
//!
//! Property: for any byte string, for any of the nine registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-video-format

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_video_format::registry::FormatRegistry;

const NAMES: &[&str] = &[
    "format",
    "fps",
    "framerate",
    "noformat",
    "setdar",
    "setfield",
    "setparams",
    "setrange",
    "setsar",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = FormatRegistry;
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
