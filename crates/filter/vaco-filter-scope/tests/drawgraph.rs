//! End-to-end proof that `drawgraph` reads real frame metadata (gap 11's
//! `Frame::metadata_get`) through a real `Graph`, not just this crate's
//! own pure-function unit tests of `value_to_row`/`parse_fg_hex`.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code"
)]

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::mock;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, GraphStatus};
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_scope::ScopeRegistry;

fn drawgraph_instance(args: Option<&str>) -> vaco_filter_graph::registry::Instance {
    let registry = ScopeRegistry;
    let req = Instantiate {
        name: "drawgraph",
        instance: "dg",
        args,
        arguments: &[],
    };
    registry
        .create(&req)
        .unwrap_or_else(|e| panic!("drawgraph failed to instantiate: {e}"))
}

/// A real source -> `drawgraph` -> sink graph, fed a frame carrying real
/// metadata (`Frame::set_metadata`, gap 11's own dictionary) — proves the
/// filter reads it through the actual scheduler, not just that
/// `value_to_row` computes the right row in isolation.
#[test]
fn drawgraph_plots_a_real_metadata_value_from_a_real_frame() -> Result<()> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "the_source",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(vaco_pixfmt::PixFmt::Gray8)],
            label: "the_source".into(),
            ..NodeFormats::default()
        },
    );

    let instance = drawgraph_instance(Some("m1=test.value:min=0:max=255:s=20x20:rate=25"));
    let dg = graph.add(instance.desc, instance.formats, instance.filter);

    let sink = graph.add_sink(
        "the_sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "the_sink".into(),
            ..NodeFormats::default()
        },
    );

    graph.connect(src, 0, dg, 0)?;
    graph.connect(dg, 0, sink, 0)?;
    graph.set_source_format(src, mock::gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    let mut frame = mock::gray_frame(16, 16, 0, 0x20);
    frame.set_metadata("test.value", "255");
    graph.send(src, frame)?;
    graph.close_source(src, Timestamp::new(1))?;
    graph.run()?;

    let out = graph.recv(sink)?;
    let plane = out
        .plane(0)
        .expect("drawgraph always draws to plane 0 (G, in gbrp)");
    // `slide=frame` (the default) fills left-to-right from column 0, not
    // a scroll that always appends at the right edge (see the module
    // doc's 2026-08-28 correction) — so the single frame this test sends
    // lands its ink in column 0. value=255 with min=0:max=255 maps to
    // row 0 exactly (the unmargined formula's own top edge).
    let first_col_has_ink = plane
        .rows_iter()
        .any(|row| row.first().is_some_and(|&px| px < 250));
    assert!(
        first_col_has_ink,
        "expected drawgraph to have plotted the real metadata value onto its first column"
    );

    assert!(graph.violations().is_empty());
    assert_eq!(graph.run()?, GraphStatus::Eof);
    Ok(())
}
