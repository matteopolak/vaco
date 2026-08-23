//! Graph-level invariants that only show up when a filter runs inside the
//! real scheduler, not just against a hand-called `on_event`/`filter_frame`.
//!
//! One invariant per this crate's brief: an overlay placed fully outside the
//! main frame must leave the main frame's pixels unchanged. (`rotate`'s
//! "angle 0 is identity" and "four quarter turns return the original" are
//! tested directly against [`crate::rotate::rotate_into`] in `rotate.rs`'s
//! own tests, where they do not need a graph — that free function is already
//! pure.)

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
        format: PixFmt::Rgb24,
        width: w,
        height: h,
        time_base: Rational::new(1, 1),
        frame_rate: Rational::new(1, 1),
        sample_aspect_ratio: Rational::ONE,
        color: vaco_color::ColorInfo::default(),
    }
}

fn solid_rgb24(pool: &FramePool, w: u32, h: u32, rgb: [u8; 3], pts: i64) -> Frame {
    let mut frame = pool.acquire_video(PixFmt::Rgb24, w, h).unwrap();
    if let Some(mut plane) = frame.plane_mut(0) {
        for row in plane.rows_mut() {
            for px in row.chunks_exact_mut(3) {
                px.copy_from_slice(&rgb);
            }
        }
    }
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 1);
    frame
}

fn every_pixel_is(frame: &Frame, h: u32, rgb: [u8; 3]) -> bool {
    let Some(plane) = frame.plane(0) else {
        return false;
    };
    (0..h).all(|y| {
        plane
            .row(y as usize)
            .is_some_and(|row| row.chunks_exact(3).all(|px| px == rgb))
    })
}

#[test]
fn overlay_fully_outside_the_main_frame_leaves_it_unchanged() -> Result<()> {
    let mut graph = Graph::new();
    let main = graph.add_source(
        "main",
        MediaType::Video,
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Rgb24)],
            ties: Vec::new(),
            label: "main".into(),
        },
    );
    let second = graph.add_source(
        "second",
        MediaType::Video,
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Rgb24)],
            ties: Vec::new(),
            label: "second".into(),
        },
    );
    // `x=main_w` places the overlay one whole main-width to the right: fully
    // outside, on every frame, regardless of the overlay's own size.
    let req = Instantiate {
        name: "overlay",
        instance: "overlay",
        args: Some("x=main_w:y=0:format=rgb"),
        arguments: &[],
    };
    let inst = crate::overlay::create(&req).map_err(|e| {
        eprintln!("{e}");
        Error::InvalidData("overlay: create failed")
    })?;
    let overlay = graph.add(inst.desc, inst.formats, inst.filter);
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
    graph.connect(main, 0, overlay, 0)?;
    graph.connect(second, 0, overlay, 1)?;
    graph.connect(overlay, 0, sink, 0)?;
    graph.set_source_format(main, video_link(20, 20))?;
    graph.set_source_format(second, video_link(4, 4))?;
    graph.configure()?;

    let pool = graph.pool().clone();
    let white = [253, 253, 253];
    let red = [200, 0, 0];
    let mut sent_main = false;
    let mut sent_second = false;
    let mut closed_main = false;
    let mut closed_second = false;
    let mut received = 0;

    for _ in 0..10_000 {
        graph.run()?;
        loop {
            match graph.recv(sink) {
                Ok(frame) => {
                    assert!(
                        every_pixel_is(&frame, 20, white),
                        "overlay placed fully outside main must leave main untouched"
                    );
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
            match graph.send(main, solid_rgb24(&pool, 20, 20, white, 0)) {
                Ok(()) => sent_main = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_main {
            graph.close_source(main, Timestamp::new(1))?;
            closed_main = true;
        }
        if !sent_second {
            match graph.send(second, solid_rgb24(&pool, 4, 4, red, 0)) {
                Ok(()) => sent_second = true,
                Err(r) if matches!(r.error, Error::OutputPending) => {}
                Err(r) => return Err(r.error),
            }
        } else if !closed_second {
            graph.close_source(second, Timestamp::new(1))?;
            closed_second = true;
        }
    }
    panic!("graph did not finish: {:?}", graph.classify());
}
