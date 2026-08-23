//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-source`.
//!
//! Same shape as `vaco-filter-video-source`'s own fuzz target: this crate's
//! fifteen generators cover a wide range of option types (colours, image
//! sizes, enums, seeds, per-band coefficients), so this is the one target
//! that exercises `vaco_opts` parsing across all of them at once, against
//! this crate's `GeneratorRegistry`. `size=99999999x99999999` and similar
//! oversized requests are exactly the finding this target exists to catch:
//! `FramePool::acquire_video`'s own limits must refuse them, not attempt
//! them.
//!
//! Property: for any byte string, for any registered name, either a clean
//! `Err` comes back at some stage or a working `Instance`, never a panic
//! and never an unbounded allocation.
//! fuzz-crate: vaco-filter-source

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_source::registry::GeneratorRegistry;

const NAMES: &[&str] = &[
    "allrgb",
    "allyuv",
    "cellauto",
    "colorchart",
    "colorspectrum",
    "gradients",
    "life",
    "mandelbrot",
    "perlin",
    "rgbtestsrc",
    "sierpinski",
    "smptebars",
    "smptehdbars",
    "yuvtestsrc",
    "zoneplate",
];

fuzz_target!(|args: &str| {
    if args.len() > 8192 {
        return;
    }
    let registry = GeneratorRegistry;
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
