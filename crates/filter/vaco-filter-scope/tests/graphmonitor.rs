//! End-to-end proof that `graphmonitor` really uses
//! `FilterContext::graph_nodes`/`graph_links` when driven by a real
//! `Graph`, not just this crate's own unit-tested `render()` function
//! against hand-built `NodeView`/`LinkView` values.
//!
//! Gap 22 (`planning/INTERFACE-GAPS.md`) is only *proven* closed by a real
//! consumer wired through the actual scheduler — this is that proof.

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

/// Build `graphmonitor` (or `agraphmonitor`) the same way a real filtergraph
/// description would: through the crate's registered [`FilterRegistry`],
/// never the private `create`/`create_audio` functions.
fn graphmonitor_instance(name: &str, args: Option<&str>) -> vaco_filter_graph::registry::Instance {
    let registry = ScopeRegistry;
    let req = Instantiate {
        name,
        instance: "gm",
        args,
        arguments: &[],
    };
    registry
        .create(&req)
        .unwrap_or_else(|e| panic!("{name} failed to instantiate: {e}"))
}

/// One real source node feeding one real `graphmonitor` node feeding one
/// real sink, run through the actual scheduler, with one frame.
#[test]
fn graphmonitor_draws_the_real_upstream_node_it_is_connected_to() -> Result<()> {
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

    let instance = graphmonitor_instance("graphmonitor", Some("s=96x32:rate=25"));
    let gm = graph.add(instance.desc, instance.formats, instance.filter);

    let sink = graph.add_sink(
        "the_sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "the_sink".into(),
            ..NodeFormats::default()
        },
    );

    graph.connect(src, 0, gm, 0)?;
    graph.connect(gm, 0, sink, 0)?;
    graph.set_source_format(src, mock::gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    graph.send(src, mock::gray_frame(16, 16, 0, 0x20))?;
    graph.close_source(src, Timestamp::new(1))?;
    graph.run()?;

    let out = graph.recv(sink)?;
    let plane = out.plane(0).expect("graphmonitor always draws to plane 0");
    let rows: Vec<&[u8]> = plane.rows_iter().collect();

    // If `ctx.graph_nodes()`/`ctx.graph_links()` came back empty (the bug
    // this test exists to catch — e.g. `node_labels` not threaded through
    // the scheduler, or `graph_links` returning nothing), `render()` would
    // still draw its own node's bare header line: one contiguous band of
    // lit rows. What only the *real* graph_nodes()/graph_links() data can
    // produce is more than one such band, separated by the measured
    // inter-line gaps — this graph has three nodes (`the_source`,
    // `graphmonitor` itself, `the_sink`) each contributing a header plus
    // at least one pad line, seven lines/bands in total.
    let ink_row_bands = count_ink_row_bands(&rows);
    assert!(
        ink_row_bands >= 2,
        "expected more than one line of drawn text (got {ink_row_bands} \
         band(s) of lit rows) if graphmonitor could see the real node it is \
         wired to, rather than drawing only its own bare header"
    );

    assert!(graph.violations().is_empty());
    assert_eq!(graph.run()?, GraphStatus::Eof);
    Ok(())
}

/// How many separate runs of "this row has at least one lit pixel" the
/// plane contains — one text line's glyphs make one run, and this crate's
/// own measured inter-line gaps (`>= 2`px, see `graphmonitor`'s module
/// doc) always leave at least one all-zero row between two lines.
fn count_ink_row_bands(rows: &[&[u8]]) -> usize {
    let mut bands = 0usize;
    let mut in_band = false;
    for row in rows {
        let lit = row.iter().any(|&px| px != 0);
        if lit && !in_band {
            bands += 1;
        }
        in_band = lit;
    }
    bands
}

/// The same wiring, but for `agraphmonitor`: an audio source into the
/// monitor into a video sink — the one place this crate's `FrameFilter`
/// must read `input.pts`/`.time_base` off an *audio* frame rather than a
/// video one, and still reach the same `graph_nodes`/`graph_links`.
#[test]
fn agraphmonitor_runs_end_to_end_from_a_real_audio_source() -> Result<()> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "the_audio_source",
        MediaType::Audio,
        NodeFormats {
            outputs: vec![FormatSet::audio_exact(
                vaco_sampfmt::SampleFmt::S16,
                48000,
                vaco_chlayout::ChannelLayout::STEREO,
            )],
            label: "the_audio_source".into(),
            ..NodeFormats::default()
        },
    );

    let instance = graphmonitor_instance("agraphmonitor", Some("s=96x32:rate=25"));
    let gm = graph.add(instance.desc, instance.formats, instance.filter);

    let sink = graph.add_sink(
        "the_sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "the_sink".into(),
            ..NodeFormats::default()
        },
    );

    graph.connect(src, 0, gm, 0)?;
    graph.connect(gm, 0, sink, 0)?;
    graph.set_source_format(
        src,
        vaco_filter_core::LinkFormat::Audio {
            format: vaco_sampfmt::SampleFmt::S16,
            sample_rate: 48000,
            layout: vaco_chlayout::ChannelLayout::STEREO,
            time_base: Rational::new(1, 48000),
        },
    )?;
    graph.configure()?;

    graph.send(src, mock::audio_frame(48000, 1024, 0))?;
    graph.close_source(src, Timestamp::new(1024))?;
    graph.run()?;

    let out = graph.recv(sink)?;
    assert!(matches!(
        out.data,
        vaco_frame::FrameData::Video { format: vaco_pixfmt::PixFmt::Gray8, .. }
    ));
    Ok(())
}
