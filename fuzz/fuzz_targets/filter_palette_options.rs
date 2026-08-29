//! Arbitrary filtergraph option text against every filter `vaco-filter-palette`
//! registers (`palettegen`, `paletteuse`, `elbg`) — same shape as
//! `filter_mm_options.rs`.
//!
//! Property: for any byte string, for any of this crate's registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-palette

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_palette::PaletteRegistry;

const NAMES: &[&str] = &["palettegen", "paletteuse", "elbg"];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = PaletteRegistry;
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
