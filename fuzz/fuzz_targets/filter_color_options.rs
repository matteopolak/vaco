//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-color`.
//!
//! `colorchannelmixer` parses sixteen `f64` gains plus `pc`/`pa`;
//! `lut`/`lutrgb`/`lutyuv` parse four `vaco-expr` expressions each
//! (`c0..c3`, aliased `y`/`u`/`v`/`r`/`g`/`b`/`a`); `lut2` parses four more
//! plus an integer `d`; `pseudocolor` parses four expressions, an index
//! and a preset; `colorlevels` parses sixteen `f64` range endpoints plus
//! `preserve`. Every one of those goes through `vaco-expr`'s own parser
//! or `vaco-opts`'s typed option parser, neither of which this crate
//! trusts blindly — matching the pattern in
//! `vaco-filter-video-format`'s `filter_video_format_options.rs`.
//!
//! Property: for any byte string, for any of the seven registered names,
//! either a clean `Err` comes back or a working `Instance`, never a panic
//! and never an unbounded allocation.
//! fuzz-crate: vaco-filter-color

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_color::registry::ColorRegistry;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};

const NAMES: &[&str] =
    &["colorchannelmixer", "colorlevels", "lut", "lut2", "lutrgb", "lutyuv", "pseudocolor"];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = ColorRegistry;
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
