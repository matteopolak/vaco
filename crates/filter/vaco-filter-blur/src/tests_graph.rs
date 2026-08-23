//! Graph-level check that [`crate::varblur`]'s `FrameSyncFilter` wiring
//! actually runs when driven through the real scheduler — not just when its
//! pure helper functions are called directly (as `varblur`'s own unit tests
//! do). Same pattern as `vaco-filter-video-composite::tests_invariants`.

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

fn gray_frame(pool: &FramePool, w: u32, h: u32, fill: u8, pts: i64) -> Frame {
    let mut frame = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
    if let Some(mut plane) = frame.plane_mut(0) {
        for row in plane.rows_mut() {
            for byte in row.iter_mut() {
                *byte = fill;
            }
        }
    }
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 1);
    frame
}

/// End-to-end: a constant main field driven through `varblur` with a
/// constant radius map stays that same constant (the average of a constant
/// is itself, independent of the per-pixel radius — see `varblur`'s own
/// `a_constant_main_field_is_always_a_fixed_point` unit test for the pure
/// engine; this drives the same property through the real `Graph`/`Synced`
/// scheduler instead).
#[test]
fn varblur_through_the_real_graph_leaves_a_constant_field_unchanged() -> Result<()> {
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
    let radius = graph.add_source(
        "radius",
        MediaType::Video,
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            ties: Vec::new(),
            label: "radius".into(),
        },
    );
    let req = Instantiate {
        name: "varblur",
        instance: "varblur",
        args: Some("min_r=0:max_r=8"),
        arguments: &[],
    };
    let inst = crate::varblur::create(&req).map_err(|e| {
        eprintln!("{e}");
        Error::InvalidData("varblur: create failed")
    })?;
    let varblur = graph.add(inst.desc, inst.formats, inst.filter);
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
    graph.connect(main, 0, varblur, 0)?;
    graph.connect(radius, 0, varblur, 1)?;
    graph.connect(varblur, 0, sink, 0)?;
    graph.set_source_format(main, video_link(6, 6))?;
    graph.set_source_format(radius, video_link(6, 6))?;
    graph.configure()?;

    let pool = graph.pool().clone();
    let mut sent_main = false;
    let mut sent_radius = false;
    let mut closed_main = false;
    let mut closed_radius = false;
    let mut received = 0;

    for _ in 0..10_000 {
        graph.run()?;
        loop {
            match graph.recv(sink) {
                Ok(frame) => {
                    let plane = frame.plane(0).expect("gray8 has one plane");
                    for y in 0..6usize {
                        let row = plane.row(y).expect("row present");
                        for &v in &row[..6] {
                            assert_eq!(
                                v, 60,
                                "constant field must stay constant through the real graph"
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
            match graph.send(main, gray_frame(&pool, 6, 6, 60, 0)) {
                Ok(()) => sent_main = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_main {
            graph.close_source(main, Timestamp::new(1))?;
            closed_main = true;
        }
        if !sent_radius {
            match graph.send(radius, gray_frame(&pool, 6, 6, 200, 0)) {
                Ok(()) => sent_radius = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_radius {
            graph.close_source(radius, Timestamp::new(1))?;
            closed_radius = true;
        }
    }
    panic!("graph did not finish: {:?}", graph.classify());
}
