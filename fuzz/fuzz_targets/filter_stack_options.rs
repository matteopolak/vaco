//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-stack`.
//!
//! Same pattern as `filter_blur_options`/`filter_video_geometry_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline so
//! the fuzzer also exercises the graph-string grammar's own escaping, not
//! just each filter's `set_from_string`.
//!
//! The interesting surface here is `xstack`'s `inputs=`/`grid=` pair —
//! `inputs` is a plain integer with no upper bound enforced by
//! `vaco-opts` itself, and `grid=WxH` is parsed and then, per
//! `xstack.rs`'s own doc comment, used to size a per-input position table.
//! A huge `inputs=` or `grid=` here is exactly the shape of input
//! `vaco_limits::Budget` exists to bound before any such table is
//! allocated; `xstack.rs` also documents that `layout=` (a free-form
//! per-input `x_y` expression string) is intentionally not implemented and
//! must fail cleanly rather than partially parse. `hstack`/`vstack` share
//! the same `inputs=` integer surface with no `grid=`/`layout=` at all.
//!
//! Property: for any byte string, for any of the three registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-stack

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_stack::registry::StackRegistry;

const NAMES: &[&str] = &["hstack", "vstack", "xstack"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = StackRegistry;
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
