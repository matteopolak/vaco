//! The filtergraph description language, over arbitrary bytes.
//!
//! A graph description is untrusted: it comes from a command line, a
//! configuration file, or a web form in front of one. The hazards a
//! hand-written parser has here are all reachable from a short string —
//! unbalanced brackets, an unterminated quoted run, a trailing backslash, a
//! `name@` with nothing after it, and above all **depth**, which is the classic
//! way a recursive-descent parser turns 200 KB of `[` into a stack overflow.
//!
//! Three properties, in order of what they would cost if they broke:
//!
//! 1. `parse` never panics and always terminates.
//! 2. When it succeeds, printing and re-parsing gives the same tree — the round
//!    trip `-dumpgraph` and any future GUI depend on.
//! 3. When it succeeds, every chain holds at least one filter, so a caller can
//!    never index into an empty one.
//!
//! fuzz-crate: vaco-filter-graph
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::ast::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = core::str::from_utf8(data) else {
        return;
    };
    // Deep input is the point of this target, but libFuzzer should spend its
    // time on shapes rather than on one enormous string.
    if src.len() > 64 * 1024 {
        return;
    }
    let Ok(ast) = parse(src) else {
        return;
    };
    assert!(!ast.chains.is_empty(), "a parse that succeeded has no chains");
    for chain in &ast.chains {
        assert!(
            !chain.filters.is_empty(),
            "a parse that succeeded left an empty chain: {src:?}"
        );
        for filter in &chain.filters {
            assert!(
                !filter.name.is_empty(),
                "a parse that succeeded left an empty filter name: {src:?}"
            );
            // Argument splitting is the other half of the grammar and must be
            // just as total.
            let _ = filter.arguments();
        }
    }

    let printed = ast.to_string();
    let reparsed = match parse(&printed) {
        Ok(a) => a,
        Err(e) => panic!("printing {src:?} produced unparseable {printed:?}: {e}"),
    };
    assert_eq!(
        ast.without_spans(),
        reparsed.without_spans(),
        "round trip changed the tree: {src:?} -> {printed:?}"
    );
    assert_eq!(
        printed,
        reparsed.to_string(),
        "printing is not idempotent: {src:?}"
    );
});
