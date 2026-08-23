//! Graph-level invariants that only show up when a filter runs inside the
//! real scheduler, not just against a hand-called `filter_frame`.
//!
//! Two invariants, per this crate's brief: `hflip` twice is the identity, and
//! `crop` then `pad` back to the original size restores the retained region
//! exactly (everything outside it is necessarily lost — that is what makes
//! `crop` a crop — so the honest version of the invariant is "the kept pixels
//! round-trip and the rest is the fill colour").

#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_core::{MediaType, Rational, Timestamp};
use vaco_filter_core::adapt::Simple;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, GraphStatus, LinkFormat};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_pixfmt::PixFmt;

fn gray_link(width: u32, height: u32) -> LinkFormat {
    LinkFormat::Video {
        format: PixFmt::Gray8,
        width,
        height,
        time_base: Rational::new(1, 25),
        frame_rate: Rational::new(25, 1),
        sample_aspect_ratio: Rational::ONE,
        color: vaco_color::ColorInfo::default(),
    }
}

/// A gray8 frame filled row-major with `0..width*height`, truncated to `u8`.
fn ramp_frame(width: u32, height: u32) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool.acquire_video(PixFmt::Gray8, width, height).unwrap();
    if let Some(mut plane) = frame.plane_mut(0) {
        for y in 0..plane.rows() {
            if let Some(row) = plane.row_mut(y) {
                for (x, b) in row.iter_mut().enumerate() {
                    *b = (y.wrapping_mul(width as usize).wrapping_add(x)) as u8;
                }
            }
        }
    }
    frame.pts = Timestamp::new(0);
    frame
}

fn read_pixel(frame: &Frame, x: usize, y: usize) -> Option<u8> {
    let FrameData::Video { .. } = &frame.data else {
        return None;
    };
    frame.plane(0)?.row(y)?.get(x).copied()
}

#[test]
fn hflip_twice_through_the_real_graph_is_identity() {
    let (w, h) = (8u32, 6u32);
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            label: "in".into(),
            ..NodeFormats::default()
        },
    );
    let f1 = graph.add(
        crate::flip::hflip::DESC,
        NodeFormats::uniform(
            1,
            1,
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Gray8),
            "f1",
        ),
        Box::new(Simple::new(crate::flip::Filter::new(
            crate::flip::Axis::Horizontal,
        ))),
    );
    let f2 = graph.add(
        crate::flip::hflip::DESC,
        NodeFormats::uniform(
            1,
            1,
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Gray8),
            "f2",
        ),
        Box::new(Simple::new(crate::flip::Filter::new(
            crate::flip::Axis::Horizontal,
        ))),
    );
    let sink = graph.add_sink(
        "out",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "out".into(),
            ..NodeFormats::default()
        },
    );
    graph.connect(src, 0, f1, 0).unwrap();
    graph.connect(f1, 0, f2, 0).unwrap();
    graph.connect(f2, 0, sink, 0).unwrap();
    graph.set_source_format(src, gray_link(w, h)).unwrap();
    graph.configure().unwrap();

    let input = ramp_frame(w, h);
    graph.send(src, input.clone()).unwrap();
    graph.close_source(src, Timestamp::new(1)).unwrap();
    graph.run().unwrap();
    let output = graph.recv(sink).unwrap();

    for y in 0..h as usize {
        for x in 0..w as usize {
            assert_eq!(
                read_pixel(&output, x, y),
                read_pixel(&input, x, y),
                "pixel ({x},{y}) survived a double hflip"
            );
        }
    }
    assert_eq!(graph.run().unwrap(), GraphStatus::Eof);
}

#[test]
fn crop_then_pad_to_original_size_restores_the_retained_region() {
    let (w, h) = (8u32, 8u32);
    let (cw, ch, cx, cy) = (4u32, 4u32, 2u32, 2u32);
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        NodeFormats {
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            label: "in".into(),
            ..NodeFormats::default()
        },
    );
    let crop_opts = crate::crop::test_opts(cw, ch, cx, cy);
    let crop_node = graph.add(
        crate::crop::DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "crop"),
        Box::new(Simple::new(crate::crop::Filter::new(&crop_opts).unwrap())),
    );
    let pad_opts = crate::pad::test_opts(w, h, cx, cy, "black");
    let pad_node = graph.add(
        crate::pad::DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "pad"),
        Box::new(Simple::new(crate::pad::Filter::new(&pad_opts).unwrap())),
    );
    let sink = graph.add_sink(
        "out",
        MediaType::Video,
        NodeFormats {
            inputs: vec![FormatSet::default()],
            label: "out".into(),
            ..NodeFormats::default()
        },
    );
    graph.connect(src, 0, crop_node, 0).unwrap();
    graph.connect(crop_node, 0, pad_node, 0).unwrap();
    graph.connect(pad_node, 0, sink, 0).unwrap();
    graph.set_source_format(src, gray_link(w, h)).unwrap();
    graph.configure().unwrap();

    let input = ramp_frame(w, h);
    graph.send(src, input.clone()).unwrap();
    graph.close_source(src, Timestamp::new(1)).unwrap();
    graph.run().unwrap();
    let output = graph.recv(sink).unwrap();

    for y in 0..h as usize {
        for x in 0..w as usize {
            let inside = x >= cx as usize
                && x < (cx + cw) as usize
                && y >= cy as usize
                && y < (cy + ch) as usize;
            let got = read_pixel(&output, x, y);
            if inside {
                assert_eq!(got, read_pixel(&input, x, y), "retained pixel ({x},{y})");
            } else {
                assert_eq!(got, Some(0), "border pixel ({x},{y}) is gray8 black");
            }
        }
    }
}
