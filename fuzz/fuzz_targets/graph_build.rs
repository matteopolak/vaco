//! Instantiation, link resolution and validation, over arbitrary descriptions.
//!
//! `graph_parse` covers the grammar; this covers everything after it, where the
//! hazards are different: pad indices derived from user-supplied counts, labels
//! that match nothing or match twice, chains that make a cycle, and the
//! bookkeeping that decides which pads are left open. All of it is arithmetic
//! on numbers a description chose.
//!
//! The invariant asserted is the one a caller relies on: a graph that builds
//! has every non-open pad connected exactly once, and no pad appears in both
//! the open list and a link.
//!
//! fuzz-crate: vaco-filter-graph
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_filter_graph::mock::MockRegistry;
use vaco_filter_graph::parse_and_build;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = core::str::from_utf8(data) else {
        return;
    };
    if src.len() > 8 * 1024 {
        return;
    }
    let registry = MockRegistry::new();
    let Ok(built) = parse_and_build(src, &registry) else {
        return;
    };

    let node_count = built.graph.node_count();
    assert_eq!(built.nodes.len(), node_count, "node bookkeeping diverged");

    // Every link names pads that exist, and no input pad is fed twice.
    let mut inputs: Vec<(u32, u32)> = Vec::new();
    let mut outputs: Vec<(u32, u32)> = Vec::new();
    for link in built.graph.links().iter() {
        let src_pad = (link.src().node.0, link.src().pad);
        let dst_pad = (link.dst().node.0, link.dst().pad);
        assert!((link.src().node.0 as usize) < node_count);
        assert!((link.dst().node.0 as usize) < node_count);
        assert!(!inputs.contains(&dst_pad), "input pad fed twice: {src:?}");
        assert!(!outputs.contains(&src_pad), "output pad read twice: {src:?}");
        inputs.push(dst_pad);
        outputs.push(src_pad);
    }

    // An open pad is genuinely open: it is not also on a link.
    for open in &built.open_inputs {
        assert!(
            !inputs.contains(&(open.node.0, open.pad)),
            "an open input is also connected: {src:?}"
        );
    }
    for open in &built.open_outputs {
        assert!(
            !outputs.contains(&(open.node.0, open.pad)),
            "an open output is also connected: {src:?}"
        );
    }

    // Introspection must be as total as the rest.
    let _ = built.to_dot();
    let _ = built.dump();
});
