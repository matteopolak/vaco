//! Arbitrary filtergraph text against every filter's option parser in
//! `vaco-filter-subtitle`.
//!
//! Same pattern as `filter_blur_options`/`filter_video_geometry_options`:
//! run the real `vaco_filter_graph::ast::parse`/`arguments()` pipeline so
//! the fuzzer also exercises the graph-string grammar's own escaping, not
//! just each filter's `set_from_string`. Both `ass` and `subtitles` accept
//! a `filename=`/positional path plus a handful of style-override options
//! (`force_style`, `original_size`, ...); the path itself is never opened
//! by this target (matching `filter_lut_options`'s note that a fuzzed
//! `file=` path mostly just fails to open, harmlessly), but the option
//! *parsing* around it — including `set_from_string`'s escaping — is
//! exercised in full.
//!
//! `subtitles.rs`'s own `.srt` content parser (`parse_srt`) is a second,
//! genuinely untrusted-file-content surface — an `.srt` a `subtitles=`
//! filter reads at open time — but it is not `pub`, so it is reachable
//! only through this crate's own unit tests today; a future agent wiring
//! this crate up to fuzz that content directly (the way `filter_lut_options`
//! fuzzes `Cube3d::parse`) needs to make it `pub(crate)` -> `pub` first.
//!
//! Property: for any byte string, for either of the two registered names,
//! either a clean `Err` comes back at some stage or a working `Instance`,
//! never a panic and never an unbounded allocation.
//! fuzz-crate: vaco-filter-subtitle

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_subtitle::registry::SubtitleRegistry;

const NAMES: &[&str] = &["ass", "subtitles"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }
    let registry = SubtitleRegistry;
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
