//! Graph-level check that [`crate::morpho`]'s `FrameSyncFilter` wiring
//! actually produces the measured result when driven through the real
//! scheduler — not just when its pure helper functions are called directly
//! (as `morpho`'s own unit tests do). Same pattern as
//! `vaco-filter-video-composite::tests_invariants`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, LinkFormat};
use vaco_filter_graph::registry::Instantiate;
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;

fn video_link(w: u32, h: u32) -> LinkFormat {
    LinkFormat::Video {
        format: PixFmt::Gray8,
        width: w,
        height: h,
        time_base: Rational::new(1, 1),
        frame_rate: Rational::new(1, 1),
        sample_aspect_ratio: Rational::ONE,
        color: vaco_color::ColorInfo::default(),
    }
}

fn gray_frame(
    pool: &FramePool,
    w: u32,
    h: u32,
    fill: u8,
    impulse: Option<(u32, u32, u8)>,
    pts: i64,
) -> Frame {
    let mut frame = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
    if let Some(mut plane) = frame.plane_mut(0) {
        for row in plane.rows_mut() {
            for byte in row.iter_mut() {
                *byte = fill;
            }
        }
        if let Some((ix, iy, v)) = impulse
            && let Some(row) = plane.row_mut(iy as usize)
            && let Some(px) = row.get_mut(ix as usize)
        {
            *px = v;
        }
    }
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 1);
    frame
}

/// End-to-end: an all-`255` 3x3 structure dilates a `100` impulse into its
/// full 3x3 neighbourhood — the same result `morpho::tests::
/// all_ones_structure_matches_the_fixed_mask_dilation` checks against the
/// pure engine, driven here through the real `Graph`/`Synced` scheduler.
#[test]
fn morpho_dilate_through_the_real_graph_matches_the_pure_engine() -> Result<()> {
    let mut graph = Graph::new();
    let main = graph.add_source(
        "main",
        MediaType::Video,
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            ties: Vec::new(),
            label: "main".into(),
        },
    );
    let structure = graph.add_source(
        "structure",
        MediaType::Video,
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            ties: Vec::new(),
            label: "structure".into(),
        },
    );
    let req = Instantiate {
        name: "morpho",
        instance: "morpho",
        args: Some("mode=dilate"),
        arguments: &[],
    };
    let inst = crate::morpho::create(&req).map_err(|e| {
        eprintln!("{e}");
        Error::InvalidData("morpho: create failed")
    })?;
    let morpho = graph.add(inst.desc, inst.formats, inst.filter);
    let sink = graph.add_sink(
        "out",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            outputs: Vec::new(),
            ties: Vec::new(),
            label: "out".into(),
        },
    );
    graph.connect(main, 0, morpho, 0)?;
    graph.connect(structure, 0, morpho, 1)?;
    graph.connect(morpho, 0, sink, 0)?;
    graph.set_source_format(main, video_link(5, 5))?;
    graph.set_source_format(structure, video_link(3, 3))?;
    graph.configure()?;

    let pool = graph.pool().clone();
    let mut sent_main = false;
    let mut sent_structure = false;
    let mut closed_main = false;
    let mut closed_structure = false;
    let mut received = 0;

    for _ in 0..10_000 {
        graph.run()?;
        loop {
            match graph.recv(sink) {
                Ok(frame) => {
                    let plane = frame.plane(0).expect("gray8 has one plane");
                    for y in 0..5usize {
                        for x in 0..5usize {
                            let v = plane.row(y).and_then(|r| r.get(x)).copied().unwrap_or(0);
                            let expect_hit = (1..=3).contains(&y) && (1..=3).contains(&x);
                            assert_eq!(
                                v,
                                if expect_hit { 100 } else { 0 },
                                "({x},{y}) via the real graph"
                            );
                        }
                    }
                    received += 1;
                }
                Err(Error::NeedMoreInput) => break,
                Err(Error::Eof) => {
                    assert!(received > 0, "the graph produced no frames at all");
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        if !sent_main {
            match graph.send(main, gray_frame(&pool, 5, 5, 0, Some((2, 2, 100)), 0)) {
                Ok(()) => sent_main = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_main {
            graph.close_source(main, Timestamp::new(1))?;
            closed_main = true;
        }
        if !sent_structure {
            match graph.send(structure, gray_frame(&pool, 3, 3, 255, None, 0)) {
                Ok(()) => sent_structure = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_structure {
            graph.close_source(structure, Timestamp::new(1))?;
            closed_structure = true;
        }
    }
    panic!("graph did not finish: {:?}", graph.classify());
}
