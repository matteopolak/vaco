//! The adapter, driven to completion inside a real `vaco-filter-core` graph.
//!
//! `tests/semantics.rs` proves the event loop against the reference; this
//! proves that a filter written the ordinary way — `on_event` and nothing else
//! — gets that behaviour through the scheduler, with backpressure, end of
//! stream and seeks all going through `vaco-filter-core`'s own machinery rather
//! than a test harness.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::match_wildcard_for_single_variants,
    reason = "test code"
)]

use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{Graph, GraphStatus, NodeId};
use vaco_filter_framesync::mock::{Stamp, first_byte, gray_frame, gray_link};
use vaco_filter_framesync::{FrameSyncOpts, TsSyncMode};
use vaco_pixfmt::PixFmt;

struct Rig {
    graph: Graph,
    sources: [NodeId; 2],
    sink: NodeId,
}

fn source_formats(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

fn sink_formats(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

fn rig(rate_a: i32, rate_b: i32, opts: FrameSyncOpts) -> Result<Rig> {
    let mut graph = Graph::new();
    let a = graph.add_source("a", MediaType::Video, source_formats("a"));
    let b = graph.add_source("b", MediaType::Video, source_formats("b"));
    let stamp = graph.add(
        Stamp::DESC,
        Stamp::formats("stamp"),
        Stamp::new(opts).boxed(),
    );
    let sink = graph.add_sink("out", MediaType::Video, sink_formats("out"));
    graph.connect(a, 0, stamp, 0)?;
    graph.connect(b, 0, stamp, 1)?;
    graph.connect(stamp, 0, sink, 0)?;
    graph.set_source_format(a, gray_link(1, 1, Rational::new(1, rate_a)))?;
    graph.set_source_format(b, gray_link(1, 1, Rational::new(1, rate_b)))?;
    graph.configure()?;
    Ok(Rig {
        graph,
        sources: [a, b],
        sink,
    })
}

/// Feed both sources their scripted frames and drain the sink.
fn drive(rig: &mut Rig, rates: [i32; 2], counts: [i64; 2]) -> Result<Vec<(i64, u8)>> {
    let pool = rig.graph.pool().clone();
    let mut sent = [0i64; 2];
    let mut closed = [false; 2];
    let mut out = Vec::new();
    for _ in 0..10_000 {
        rig.graph.run()?;
        loop {
            match rig.graph.recv(rig.sink) {
                Ok(frame) => out.push((
                    frame.pts.ticks().unwrap_or(-1),
                    first_byte(&frame).unwrap_or(0),
                )),
                Err(Error::NeedMoreInput) => break,
                Err(Error::Eof) => return Ok(out),
                Err(e) => return Err(e),
            }
        }
        for i in 0..2 {
            if sent[i] < counts[i] {
                let tb = Rational::new(1, rates[i]);
                let value = u8::try_from(sent[i] + 1).unwrap_or(255);
                let Some(frame) = gray_frame(&pool, sent[i], tb, value) else {
                    continue;
                };
                match rig.graph.send(rig.sources[i], frame) {
                    Ok(()) => sent[i] += 1,
                    Err(Error::OutputPending) => {}
                    Err(e) => return Err(e),
                }
            } else if !closed[i] {
                rig.graph
                    .close_source(rig.sources[i], Timestamp::new(counts[i]))?;
                closed[i] = true;
            }
        }
    }
    panic!("graph did not finish: {:?}", rig.graph.classify());
}

#[test]
fn a_framesync_filter_runs_through_the_scheduler() -> Result<()> {
    let mut r = rig(10, 4, FrameSyncOpts::default())?;
    let out = drive(&mut r, [10, 4], [10, 4])?;
    assert_eq!(out.len(), 10);
    assert_eq!(
        out.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [1, 1, 1, 2, 2, 3, 3, 3, 4, 4]
    );
    // Event timestamps come out in the common time base, 1/20 here.
    assert_eq!(
        out.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
        [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]
    );
    assert!(r.graph.violations().is_empty());
    assert_eq!(r.graph.run()?, GraphStatus::Eof);
    Ok(())
}

#[test]
fn nearest_mode_reaches_the_filter_through_the_adapter() -> Result<()> {
    let mut r = rig(
        10,
        4,
        FrameSyncOpts {
            ts_sync: TsSyncMode::Nearest,
            ..FrameSyncOpts::default()
        },
    )?;
    let out = drive(&mut r, [10, 4], [10, 4])?;
    assert_eq!(
        out.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [1, 1, 2, 2, 3, 3, 3, 4, 4, 4]
    );
    Ok(())
}

#[test]
fn backpressure_holds_the_synchroniser_rather_than_dropping_frames() -> Result<()> {
    // Far more frames than a link's eight-deep queue, so the adapter has to
    // hold events back and resume.
    let mut r = rig(10, 10, FrameSyncOpts::default())?;
    let out = drive(&mut r, [10, 10], [64, 64])?;
    assert_eq!(out.len(), 64);
    assert!(r.graph.violations().is_empty());
    Ok(())
}

#[test]
fn an_empty_stream_finishes_without_a_violation() -> Result<()> {
    let mut r = rig(10, 10, FrameSyncOpts::default())?;
    let out = drive(&mut r, [10, 10], [0, 0])?;
    assert!(out.is_empty());
    assert!(r.graph.violations().is_empty());
    assert_eq!(r.graph.run()?, GraphStatus::Eof);
    Ok(())
}

#[test]
fn only_one_input_carrying_anything_still_terminates() -> Result<()> {
    let mut r = rig(10, 10, FrameSyncOpts::default())?;
    let out = drive(&mut r, [10, 10], [4, 0])?;
    // The secondary never starts, so it contributes nothing; the main runs.
    assert_eq!(out.len(), 4);
    assert!(out.iter().all(|(_, v)| *v == 0));
    assert!(r.graph.violations().is_empty());
    Ok(())
}

#[test]
fn a_seek_discards_what_the_synchroniser_was_holding() -> Result<()> {
    let mut r = rig(10, 10, FrameSyncOpts::default())?;
    let pool = r.graph.pool().clone();
    let tb = Rational::new(1, 10);
    for i in 0..2i64 {
        for (n, source) in r.sources.iter().enumerate() {
            let value = u8::try_from(i + 1).unwrap_or(0) + u8::try_from(n).unwrap_or(0);
            let Some(frame) = gray_frame(&pool, i, tb, value) else {
                continue;
            };
            r.graph.send(*source, frame)?;
        }
    }
    r.graph.run()?;
    r.graph.flush();

    // After the flush nothing survives, and a fresh stream comes out clean.
    let out = drive(&mut r, [10, 10], [3, 3])?;
    assert_eq!(out.len(), 3);
    assert_eq!(out.iter().map(|(_, v)| *v).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(r.graph.violations().is_empty());
    Ok(())
}

#[test]
fn the_output_link_takes_the_common_time_base() -> Result<()> {
    let r = rig(10, 25, FrameSyncOpts::default())?;
    let format = r.graph.sink_format(r.sink)?;
    match format {
        vaco_filter_core::LinkFormat::Video { time_base, .. } => {
            assert_eq!(time_base.reduced(), Rational::new(1, 50));
        }
        other => panic!("{other:?}"),
    }
    Ok(())
}
