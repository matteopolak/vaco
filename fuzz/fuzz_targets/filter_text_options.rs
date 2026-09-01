//! Two untrusted-input surfaces in `vaco-filter-text`, fuzzed together.
//!
//! `drawtext`'s option string goes through the registry, same pattern as
//! `filter_blur_options`/`filter_video_geometry_options`: run the real
//! `vaco_filter_graph::ast::parse`/`arguments()` pipeline so the fuzzer also
//! exercises the graph-string grammar's own escaping — worth doing here
//! specifically because `fontsize`/`alpha`/`x`/`y` are all
//! `vaco_expr::Expr::parse` expressions that can evaluate to huge or
//! non-finite numbers from a short string, the same class
//! `filter_video_composite_options` documents for `overlay`/`rotate`.
//!
//! The second, more interesting surface is `expand::expand` — the
//! `%{...}` directive language this crate's own module doc says runs
//! **once per frame** on the `text=` option's content. That content is not
//! just a filtergraph option string parsed once at graph-build time: a
//! real pipeline can source it from `textfile=` (an attacker-supplied
//! subtitle burn-in file re-read every frame), so it is fuzzed directly on
//! arbitrary bytes, not only reachable through an option string the
//! fuzzer would have to happen to construct correctly first.
//!
//! Property: for any byte string, both as `drawtext=text=<bytes>` through
//! the registry and as raw content handed straight to `expand`, either a
//! clean `Err`/best-effort passthrough comes back or a working value —
//! never a panic, never an unbounded allocation. `expand`'s own doc names
//! unrecognised `%{...}` directives as intentionally passed through
//! verbatim rather than rejected, so there is no error path to check there
//! — only totality.
//! fuzz-crate: vaco-filter-text

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_text::expand::{ExpandContext, expand};
use vaco_filter_text::registry::TextRegistry;

const NAMES: &[&str] = &["drawtext"];

fuzz_target!(|args: &str| {
    // 8 KiB is far past any real `-filter_complex` argument; beyond it the
    // fuzzer only measures how fast the parser scans whitespace, matching
    // the bound `filter_video_geometry_options` uses for the same reason.
    if args.len() > 8192 {
        return;
    }

    // Fixed, small metadata values: a `%{metadata:title}` directive must
    // substitute a bounded value regardless of how many times the input
    // repeats it, so growth stays linear in the number of directives, not
    // quadratic in the input length (which a value sized off `args` itself
    // would produce, and which would be an artifact of this harness, not
    // of `expand`).
    let metadata = [
        ("title".to_string(), "T".repeat(16)),
        ("comment".to_string(), "C".repeat(16)),
    ];
    let ctx = ExpandContext {
        pts_seconds: Some(1.5),
        frame_num: 42,
        metadata: &metadata,
    };
    let expanded = expand(args, &ctx);
    // `expand` must never blow the input up unboundedly: every directive it
    // actually substitutes is a fixed-size value (a timestamp, a frame
    // number, or one of the two 16-byte metadata values above), so the
    // output can only grow by a bounded amount per directive, never run
    // away on a crafted input.
    assert!(
        expanded.len() <= args.len().saturating_mul(4).saturating_add(256),
        "expand grew {} bytes into {} — a directive must be unbounded",
        args.len(),
        expanded.len()
    );

    let registry = TextRegistry;
    for &name in NAMES {
        let text = format!("{name}=text={args}");
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
