//! End-to-end proof that `vectorscope` builds its per-frame histogram
//! from real pixel data through the actual scheduler, not just that its
//! pure per-cell formula computes the right byte in isolation.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, GraphStatus};
use vaco_filter_graph::registry::{FilterRegistry, Instantiate};
use vaco_filter_scope::ScopeRegistry;
use vaco_frame::FramePool;
use vaco_pixfmt::PixFmt;

fn vectorscope_instance(args: Option<&str>) -> vaco_filter_graph::registry::Instance {
    let registry = ScopeRegistry;
    let req = Instantiate {
        name: "vectorscope",
        instance: "vs",
        args,
        arguments: &[],
    };
    registry
        .create(&req)
        .unwrap_or_else(|e| panic!("vectorscope failed to instantiate: {e}"))
}

/// A real source -> `vectorscope` -> sink graph. The source frame is
/// `yuv444p`, `10x10`, with a single pixel at `(0, 0)` carrying `cb=200,
/// cr=90` and every other pixel at the neutral `cb=cr=128`. The measured
/// rule (module doc) says that single hit should land at canvas
/// `(col=200, row=255-90=165)` with `Y = 1*floor(255*0.004) = 1` and
/// `chroma = 127`, while an untouched cell like `(128, 127)` (the
/// neutral background's own cell) stays at `chroma = 128`.
#[test]
fn vectorscope_builds_its_histogram_from_real_frame_pixels() -> Result<()> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "the_source",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(PixFmt::Yuv444p)],
            label: "the_source".into(),
            ..NodeFormats::default()
        },
    );

    let instance = vectorscope_instance(None);
    let vs = graph.add(instance.desc, instance.formats, instance.filter);

    let sink = graph.add_sink(
        "the_sink",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "the_sink".into(),
            ..NodeFormats::default()
        },
    );

    graph.connect(src, 0, vs, 0)?;
    graph.connect(vs, 0, sink, 0)?;

    let width = 10u32;
    let height = 10u32;
    let time_base = Rational::new(1, 25);
    let source_format = vaco_filter_core::LinkFormat::Video {
        format: PixFmt::Yuv444p,
        width,
        height,
        time_base,
        frame_rate: time_base.inverse(),
        sample_aspect_ratio: Rational::ONE,
        color: vaco_color::ColorInfo::default(),
    };
    graph.set_source_format(src, source_format)?;
    graph.configure()?;

    let pool = FramePool::default();
    let mut frame = pool.acquire_video(PixFmt::Yuv444p, width, height)?;
    if let Some(mut y) = frame.plane_mut(0) {
        y.fill(16);
    }
    if let Some(mut cb) = frame.plane_mut(1) {
        cb.fill(128);
        if let Some(row0) = cb.rows_mut().next()
            && let Some(px) = row0.first_mut()
        {
            *px = 200;
        }
    }
    if let Some(mut cr) = frame.plane_mut(2) {
        cr.fill(128);
        if let Some(row0) = cr.rows_mut().next()
            && let Some(px) = row0.first_mut()
        {
            *px = 90;
        }
    }
    frame.pts = Timestamp::new(0);
    frame.time_base = time_base;
    frame.duration = vaco_core::Duration(1);

    graph.send(src, frame)?;
    graph.close_source(src, Timestamp::new(1))?;
    graph.run()?;

    let out = graph.recv(sink)?;
    let y_plane = out.plane(0).expect("vectorscope always draws Y (plane 0)");
    let cb_plane = out.plane(1).expect("vectorscope always draws Cb (plane 1)");
    let cr_plane = out.plane(2).expect("vectorscope always draws Cr (plane 2)");

    let y_rows: Vec<&[u8]> = y_plane.rows_iter().collect();
    let cb_rows: Vec<&[u8]> = cb_plane.rows_iter().collect();
    let cr_rows: Vec<&[u8]> = cr_plane.rows_iter().collect();

    let hit_row = 255 - 90;
    let hit_col = 200;
    assert_eq!(y_rows[hit_row][hit_col], 1, "single hit at intensity=0.004 should paint Y=1");
    assert_eq!(cb_rows[hit_row][hit_col], 127, "a touched cell's chroma should read 127");
    assert_eq!(cr_rows[hit_row][hit_col], 127, "a touched cell's chroma should read 127");

    // The neutral background (99 of the 100 source pixels) all map to
    // cell (128, 255-128=127) with count=99, well above the single hit.
    let bg_row = 255 - 128;
    let bg_col = 128;
    assert_eq!(
        y_rows[bg_row][bg_col],
        99u8,
        "the neutral background's own cell should show its own hit count"
    );
    assert_eq!(cb_rows[bg_row][bg_col], 127, "the background cell was touched too, so it also reads 127");

    // An entirely untouched cell stays at the neutral chroma marker.
    assert_eq!(cb_rows[0][0], 128, "an untouched cell must not be marked as touched");
    assert_eq!(y_rows[0][0], 0, "an untouched cell must stay at Y=0");

    assert!(graph.violations().is_empty());
    assert_eq!(graph.run()?, GraphStatus::Eof);
    Ok(())
}
