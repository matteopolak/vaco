//! End-to-end cases: real graphs, driven to completion, one rule pinned each.
//!
//! The point of this file is that the trait layer is *proven* rather than
//! asserted. `vaco-format-core` did the same thing with a worked container and
//! it caught real design errors; three of the cases below caught one here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::match_wildcard_for_single_variants,
    clippy::items_after_statements,
    clippy::single_match_else,
    clippy::option_if_let_else,
    clippy::too_many_lines,
    reason = "test code"
)]

use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::mock::{
    Counter, Drop, Fps, Gain, Invert, any_audio_sink, any_video_sink, audio_frame, audio_link,
    audio_source_formats, gray_frame, gray_link, video_source_formats,
};
use vaco_filter_core::negotiate::{
    ConverterFactory, ConverterSpec, FormatSet, NodeFormats, Property, loss,
};
use vaco_filter_core::{
    Activity, Filter, FilterContext, FilterDesc, FilterFlags, Graph, GraphStatus, LinkFormat,
    NodeId, Pad, Violation,
};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// `source -> [filters] -> sink`, all `gray8` at 25 fps.
struct Chain {
    graph: Graph,
    src: NodeId,
    sink: NodeId,
}

fn chain(build: impl FnOnce(&mut Graph) -> Vec<NodeId>) -> Result<Chain> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let mids = build(&mut graph);
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    let mut prev = (src, 0u32);
    for m in &mids {
        graph.connect(prev.0, prev.1, *m, 0)?;
        prev = (*m, 0);
    }
    graph.connect(prev.0, prev.1, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;
    Ok(Chain { graph, src, sink })
}

/// Feed `frames`, close, run to exhaustion, and collect everything the sink
/// yields. Interleaves running and draining, which is what a real caller does
/// and what makes backpressure observable.
fn drive(c: &mut Chain, frames: Vec<vaco_frame::Frame>) -> Result<Vec<vaco_frame::Frame>> {
    let mut out = Vec::new();
    let mut queue = frames.into_iter();
    let mut closed = false;
    for _ in 0..10_000 {
        c.graph.run()?;
        loop {
            match c.graph.recv(c.sink) {
                Ok(f) => out.push(f),
                Err(Error::NeedMoreInput) => break,
                Err(Error::Eof) => return Ok(out),
                Err(e) => return Err(e),
            }
        }
        if !closed {
            match queue.next() {
                Some(f) => c.graph.send(c.src, f)?,
                None => {
                    c.graph.close_source(c.src, Timestamp::new(0))?;
                    closed = true;
                }
            }
        }
    }
    panic!("graph did not finish");
}

fn grays(n: i64) -> Vec<vaco_frame::Frame> {
    (0..n)
        .map(|i| gray_frame(16, 16, i, u8::try_from(i & 0xff).unwrap_or(0)))
        .collect()
}

fn first_byte(f: &vaco_frame::Frame) -> Option<u8> {
    f.plane(0)?.row(0)?.first().copied()
}

// ------------------------------------------------------------- video 1:1

#[test]
fn a_one_in_one_out_video_filter_runs_end_to_end() -> Result<()> {
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    let out = drive(&mut c, grays(5))?;
    assert_eq!(out.len(), 5);
    for (i, f) in out.iter().enumerate() {
        assert_eq!(first_byte(f), Some(!(i as u8)), "frame {i}");
        assert_eq!(f.pts, Timestamp::new(i as i64));
    }
    assert!(
        c.graph.violations().is_empty(),
        "{:?}",
        c.graph.violations()
    );
    Ok(())
}

#[test]
fn nothing_is_lost_at_end_of_stream() -> Result<()> {
    // The bug that F2 exists to prevent: a filter that observes end of stream
    // before the queue has drained silently truncates every stream it touches.
    for n in [0i64, 1, 2, 7, 33] {
        let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
        let out = drive(&mut c, grays(n))?;
        assert_eq!(out.len() as i64, n, "{n} frames in, {} out", out.len());
    }
    Ok(())
}

#[test]
fn two_filters_chain() -> Result<()> {
    let mut c = chain(|g| vec![Invert::node(g, "a"), Invert::node(g, "b")])?;
    let out = drive(&mut c, grays(4))?;
    assert_eq!(out.len(), 4);
    // Inverted twice is the identity.
    assert_eq!(first_byte(&out[0]), Some(0));
    assert_eq!(first_byte(&out[3]), Some(3));
    Ok(())
}

#[test]
fn a_filter_that_drops_produces_fewer_frames_than_it_consumes() -> Result<()> {
    let mut c = chain(|g| vec![Drop::node(g, "drop", 3)])?;
    let out = drive(&mut c, grays(9))?;
    assert_eq!(out.len(), 6, "every third of nine is dropped");
    assert_eq!(first_byte(&out[0]), Some(0));
    assert_eq!(first_byte(&out[1]), Some(1));
    assert_eq!(first_byte(&out[2]), Some(3), "frame 2 was dropped");
    Ok(())
}

// ------------------------------------------------------------ N:M and time

#[test]
fn downsampling_the_frame_rate_is_n_to_m_and_exact() -> Result<()> {
    // 25 fps in, 10 fps out. One second of input must yield ten frames whose
    // timestamps are 0..9 in the *output* time base of 1/10 — not 0, 2.5, 5 in
    // the input's, and not anything that went through a float.
    let mut c = chain(|g| vec![Fps::node(g, "fps", Rational::new(10, 1))])?;
    let out = drive(&mut c, grays(25))?;
    assert_eq!(out.len(), 10);
    for (i, f) in out.iter().enumerate() {
        assert_eq!(f.time_base, Rational::new(1, 10), "frame {i}");
        assert_eq!(f.pts, Timestamp::new(i as i64), "frame {i}");
    }
    // Sample-and-hold: each output slot carries the *last* input whose own time
    // fell inside it. Slot 0 spans inputs 0, 1 and 2, so it carries input 2;
    // slot 1 spans inputs 3 and 4, so it carries input 4.
    assert_eq!(first_byte(&out[0]), Some(2));
    assert_eq!(first_byte(&out[1]), Some(4));
    Ok(())
}

#[test]
fn upsampling_the_frame_rate_produces_more_frames_than_it_consumes() -> Result<()> {
    // 25 fps in, 50 fps out: every input frame covers two output slots. This is
    // the case a `filter(frame) -> Frame` signature cannot express at all, and
    // the reason `activate` returns an `Activity` instead.
    let mut c = chain(|g| vec![Fps::node(g, "fps", Rational::new(50, 1))])?;
    let out = drive(&mut c, grays(10))?;
    assert_eq!(out.len(), 20);
    for (i, f) in out.iter().enumerate() {
        assert_eq!(f.time_base, Rational::new(1, 50));
        assert_eq!(f.pts, Timestamp::new(i as i64));
    }
    Ok(())
}

#[test]
fn a_rate_change_sets_the_output_links_time_base_and_frame_rate() -> Result<()> {
    let c = chain(|g| vec![Fps::node(g, "fps", Rational::new(10, 1))])?;
    let format = c.graph.sink_format(c.sink)?;
    match format {
        LinkFormat::Video {
            time_base,
            frame_rate,
            format,
            ..
        } => {
            assert_eq!(*time_base, Rational::new(1, 10));
            assert_eq!(*frame_rate, Rational::new(10, 1));
            assert_eq!(*format, PixFmt::Gray8, "the format still negotiated");
        }
        other => panic!("expected video, got {other:?}"),
    }
    Ok(())
}

#[test]
fn timestamps_rescale_exactly_across_a_link_whose_base_differs() -> Result<()> {
    // 1/25 into 1/90000: 1 tick becomes exactly 3600, with no float in the path.
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let inv = Invert::node(&mut graph, "invert");
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, inv, 0)?;
    graph.connect(inv, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 90_000)))?;
    graph.configure()?;

    let mut f = gray_frame(16, 16, 0, 0);
    f.time_base = Rational::new(1, 25);
    f.pts = Timestamp::new(7);
    graph.send(src, f)?;
    graph.close_source(src, Timestamp::NONE)?;
    graph.run()?;
    let out = graph.recv(sink)?;
    assert_eq!(out.time_base, Rational::new(1, 90_000));
    assert_eq!(out.pts, Timestamp::new(7 * 3600));
    Ok(())
}

// ------------------------------------------------------------------ audio

#[test]
fn a_one_in_one_out_audio_filter_runs_end_to_end() -> Result<()> {
    let rate = 48_000u32;
    let mut graph = Graph::new();
    let src = graph.add_source("in", MediaType::Audio, audio_source_formats("in", rate));
    let gain = Gain::node(&mut graph, "gain", rate, 1, 1024);
    let sink = graph.add_sink("out", MediaType::Audio, any_audio_sink("out"));
    graph.connect(src, 0, gain, 0)?;
    graph.connect(gain, 0, sink, 0)?;
    graph.set_source_format(src, audio_link(rate))?;
    graph.configure()?;

    match graph.sink_format(sink)? {
        LinkFormat::Audio {
            format,
            sample_rate,
            layout,
            ..
        } => {
            assert_eq!(*format, SampleFmt::S16);
            assert_eq!(*sample_rate, rate);
            assert_eq!(*layout, ChannelLayout::STEREO);
        }
        other => panic!("expected audio, got {other:?}"),
    }

    let mut out = 0usize;
    for i in 0..4 {
        graph.send(src, audio_frame(rate, 1024, i * 1024))?;
        graph.run()?;
        while let Ok(f) = graph.recv(sink) {
            assert!(matches!(
                f.data,
                vaco_frame::FrameData::Audio { samples: 1024, .. }
            ));
            out += 1;
        }
    }
    graph.close_source(src, Timestamp::new(4096))?;
    graph.run()?;
    while let Ok(_f) = graph.recv(sink) {
        out += 1;
    }
    assert_eq!(out, 4);
    assert!(graph.violations().is_empty());
    Ok(())
}

// ------------------------------------------------------------ negotiation

#[test]
fn negotiation_propagates_the_sources_format_to_the_sink() -> Result<()> {
    let c = chain(|g| vec![Fps::node(g, "fps", Rational::new(25, 1))])?;
    // `fps` declares Passthrough, so the format comes from the source and lands
    // on the sink without either being told directly.
    match c.graph.sink_format(c.sink)? {
        LinkFormat::Video { format, .. } => assert_eq!(*format, PixFmt::Gray8),
        other => panic!("expected video, got {other:?}"),
    }
    Ok(())
}

#[test]
fn an_impossible_link_fails_configuration_with_a_readable_diagnostic() {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Rgb24),
    );
    // `Invert` accepts only gray8; the source produces only rgb24.
    let inv = Invert::node(&mut graph, "invert");
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, inv, 0).expect("connects");
    graph.connect(inv, 0, sink, 0).expect("connects");
    let e = graph.configure();
    assert!(matches!(e, Err(Error::Unsupported(_))), "{e:?}");
    let rendered = graph.last_conflict().expect("a conflict").render();
    assert!(rendered.contains("rgb24"), "{rendered}");
    assert!(rendered.contains("gray"), "{rendered}");
    assert!(rendered.contains("narrowed by   in"), "{rendered}");
    assert!(rendered.contains("narrowed by   invert"), "{rendered}");
}

/// A converter that really converts: it rewrites the pixel data.
#[derive(Debug)]
struct Convert {
    to: PixFmt,
    from: PixFmt,
}

impl Filter for Convert {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if !ctx.output_has_room(0) {
            return Ok(Activity::Blocked);
        }
        if let Some(frame) = ctx.take_input(0) {
            assert_eq!(
                frame.pixel_format(),
                Some(self.from),
                "negotiation promised this converter its input format"
            );
            let Some((w, h)) = frame.dimensions() else {
                return Ok(Activity::Progressed);
            };
            let mut out = ctx.pool().acquire_video(self.to, w, h)?;
            out.pts = frame.pts;
            out.time_base = frame.time_base;
            out.duration = frame.duration;
            if let Some(mut plane) = out.plane_mut(0) {
                plane.fill(0x5a);
            }
            ctx.push_output(0, out)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        Ok(Activity::NeedInput)
    }
}

struct Scale;

impl ConverterFactory for Scale {
    fn converter(
        &self,
        media: MediaType,
        properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        assert_eq!(media, MediaType::Video);
        assert_eq!(properties, [Property::PixelFormat]);
        let from = *upstream.pixel_formats.as_ref()?.resolved()?;
        let to = loss::best_video(from, downstream.pixel_formats.as_ref()?.candidates())?;
        Some(ConverterSpec {
            filter: "scale",
            args: String::new(),
            formats: NodeFormats::converter(
                FormatSet::video_exact(from),
                FormatSet::video_exact(to),
                "auto",
            ),
        })
    }
}

#[test]
fn auto_conversion_splices_a_converter_and_the_graph_then_runs() -> Result<()> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Rgb24),
    );
    let inv = Invert::node(&mut graph, "invert");
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, inv, 0)?;
    graph.connect(inv, 0, sink, 0)?;
    graph.set_source_format(
        src,
        LinkFormat::Video {
            format: PixFmt::Rgb24,
            width: 16,
            height: 16,
            time_base: Rational::new(1, 25),
            frame_rate: Rational::new(25, 1),
            sample_aspect_ratio: Rational::ONE,
            color: vaco_color::ColorInfo::default(),
        },
    )?;

    let conflicts = graph.configure_converting(&Scale, |spec| {
        let from = *spec.formats.inputs[0]
            .pixel_formats
            .as_ref()
            .and_then(|c| c.resolved())
            .expect("concrete input format");
        let to = *spec.formats.outputs[0]
            .pixel_formats
            .as_ref()
            .and_then(|c| c.resolved())
            .expect("concrete output format");
        Ok(Box::new(Convert { to, from }) as Box<dyn Filter>)
    })?;
    assert!(conflicts.is_empty());
    // The converter now sits between the source and `invert`, and the whole
    // graph is configured and runnable.
    assert_eq!(graph.node_count(), 4);
    match graph.sink_format(sink)? {
        LinkFormat::Video { format, .. } => assert_eq!(*format, PixFmt::Gray8),
        other => panic!("expected video, got {other:?}"),
    }

    let mut frame = graph
        .pool()
        .acquire_video(PixFmt::Rgb24, 16, 16)
        .expect("allocates");
    frame.pts = Timestamp::ZERO;
    frame.time_base = Rational::new(1, 25);
    graph.send(src, frame)?;
    graph.close_source(src, Timestamp::new(1))?;
    graph.run()?;
    let out = graph.recv(sink)?;
    assert_eq!(first_byte(&out), Some(!0x5a));
    assert!(graph.violations().is_empty());
    Ok(())
}

// ---------------------------------------------------------- graph shapes

#[test]
fn a_media_type_mismatch_is_caught_when_the_link_is_made() {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let sink = graph.add_sink("out", MediaType::Audio, any_audio_sink("out"));
    let e = graph.connect(src, 0, sink, 0);
    assert!(matches!(e, Err(Error::InvalidData(_))), "{e:?}");
}

#[test]
fn implicit_fan_out_is_refused() {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let a = graph.add_sink("a", MediaType::Video, any_video_sink("a"));
    let b = graph.add_sink("b", MediaType::Video, any_video_sink("b"));
    graph.connect(src, 0, a, 0).expect("first connects");
    let e = graph.connect(src, 0, b, 0);
    assert!(matches!(e, Err(Error::InvalidData(_))), "{e:?}");
}

#[test]
fn an_unconnected_pad_fails_configuration() {
    let mut graph = Graph::new();
    let _src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    assert!(matches!(graph.configure(), Err(Error::InvalidData(_))));
}

#[test]
fn a_cycle_is_detected() -> Result<()> {
    let mut graph = Graph::new();
    let a = Invert::node(&mut graph, "a");
    let b = Invert::node(&mut graph, "b");
    graph.connect(a, 0, b, 0)?;
    graph.connect(b, 0, a, 0)?;
    assert!(matches!(
        graph.topological_order(),
        Err(Error::InvalidData(_))
    ));
    Ok(())
}

// ----------------------------------------------------------- source shape

#[test]
fn a_generator_source_produces_only_on_demand() -> Result<()> {
    let mut graph = Graph::new();
    let counter = Counter::node(&mut graph, "counter", 16, 16, 3);
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(counter, 0, sink, 0)?;
    graph.configure()?;

    let mut out = Vec::new();
    for _ in 0..20 {
        graph.run()?;
        match graph.recv(sink) {
            Ok(f) => out.push(f),
            Err(Error::Eof) => break,
            Err(Error::NeedMoreInput) => {}
            Err(e) => return Err(e),
        }
    }
    assert_eq!(out.len(), 3);
    assert_eq!(first_byte(&out[2]), Some(2));
    assert_eq!(graph.run()?, GraphStatus::Eof);
    assert!(graph.violations().is_empty());
    Ok(())
}

// ------------------------------------------------------------ backpressure

#[test]
fn a_source_stops_being_wanted_once_the_graph_is_full() -> Result<()> {
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    // Never drain the sink. Queues are bounded, so `send` must eventually
    // refuse rather than growing without limit.
    let mut sent = 0usize;
    for i in 0..1000 {
        c.graph.run()?;
        match c.graph.send(c.src, gray_frame(16, 16, i, 0)) {
            Ok(()) => sent += 1,
            Err(Error::OutputPending) => break,
            Err(e) => return Err(e),
        }
    }
    assert!(sent > 0, "nothing got in at all");
    assert!(
        sent < 100,
        "backpressure never engaged: {sent} frames buffered"
    );
    assert!(c.graph.violations().is_empty());
    Ok(())
}

// ------------------------------------------------------------- violations

/// Claims to have made progress while doing nothing. Without the check this
/// spins forever, which is worse than hanging: it looks like work.
struct Liar;

impl Filter for Liar {
    fn activate(&mut self, _ctx: &mut FilterContext<'_>) -> Result<Activity> {
        Ok(Activity::Progressed)
    }
}

#[test]
fn a_filter_that_claims_progress_without_making_any_is_caught() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "liar",
        description: "claims progress it did not make",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new().with_step_budget(64);
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let liar = graph.add(
        DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "liar"),
        Box::new(Liar),
    );
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, liar, 0)?;
    graph.connect(liar, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;
    graph.send(src, gray_frame(16, 16, 0, 0))?;
    graph.close_source(src, Timestamp::ZERO)?;
    let status = graph.run()?;
    assert!(
        graph
            .violations()
            .contains(&Violation::ProgressWithoutChange),
        "{:?}",
        graph.violations()
    );
    // And it terminated rather than spinning.
    assert!(
        matches!(
            status,
            GraphStatus::Deadlock(_) | GraphStatus::BudgetExhausted
        ),
        "{status:?}"
    );
    Ok(())
}

/// Waits for input that will never come.
struct Waiter;

impl Filter for Waiter {
    fn activate(&mut self, _ctx: &mut FilterContext<'_>) -> Result<Activity> {
        Ok(Activity::NeedInput)
    }
}

#[test]
fn waiting_for_input_after_end_of_stream_is_caught() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "waiter",
        description: "waits forever",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new().with_step_budget(64);
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let waiter = graph.add(
        DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "waiter"),
        Box::new(Waiter),
    );
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, waiter, 0)?;
    graph.connect(waiter, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;
    graph.close_source(src, Timestamp::ZERO)?;
    let status = graph.run()?;
    assert!(
        graph.violations().contains(&Violation::NeedInputAtEof),
        "{:?}",
        graph.violations()
    );
    match status {
        GraphStatus::Deadlock(stalls) => {
            let stall = stalls.iter().find(|s| s.label == "waiter").expect("named");
            assert!(stall.closed, "the diagnostic says the link is closed");
            assert_eq!(stall.queue_depth, 0);
        }
        other => panic!("expected a deadlock diagnosis, got {other:?}"),
    }
    Ok(())
}

/// Fails on its first frame.
struct Broken;

impl Filter for Broken {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if ctx.take_input(0).is_some() {
            return Err(Error::InvalidData("deliberate"));
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        Ok(Activity::NeedInput)
    }
}

#[test]
fn a_filter_failure_surfaces_at_the_caller_and_closes_the_graph() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "broken",
        description: "fails",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let broken = graph.add(
        DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "broken"),
        Box::new(Broken),
    );
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, broken, 0)?;
    graph.connect(broken, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;
    graph.send(src, gray_frame(16, 16, 0, 0))?;
    let e = graph.run();
    assert!(matches!(e, Err(Error::InvalidData("deliberate"))), "{e:?}");
    // The failure reached the sink rather than being swallowed mid-graph.
    let e = graph.recv(sink);
    assert!(matches!(e, Err(Error::InvalidData(_))), "{e:?}");
    Ok(())
}

// ------------------------------------------------------------- zero copy

#[test]
fn a_filter_that_only_reads_shares_the_planes_it_was_given() -> Result<()> {
    // `Drop` never writes, so a frame that survives it comes out sharing the
    // same buffer it went in with. That is the whole "zero copy" claim, checked
    // rather than asserted.
    let mut c = chain(|g| vec![Drop::node(g, "drop", 0)])?;
    let frame = gray_frame(16, 16, 0, 0x11);
    let kept = frame.clone();
    assert!(!frame.is_writable(), "two holders");
    c.graph.send(c.src, frame)?;
    c.graph.close_source(c.src, Timestamp::new(1))?;
    c.graph.run()?;
    let out = c.graph.recv(c.sink)?;
    // Still shared with `kept`: nothing on the path copied a plane.
    assert!(!out.is_writable());
    core::mem::drop(kept);
    assert!(out.is_writable(), "and now uniquely ours");
    Ok(())
}

#[test]
fn a_filter_that_writes_copies_once_and_leaves_the_original_alone() -> Result<()> {
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    let frame = gray_frame(16, 16, 0, 0x11);
    let kept = frame.clone();
    c.graph.send(c.src, frame)?;
    c.graph.close_source(c.src, Timestamp::new(1))?;
    c.graph.run()?;
    let out = c.graph.recv(c.sink)?;
    assert_eq!(first_byte(&out), Some(0xee));
    assert_eq!(first_byte(&kept), Some(0x11), "the original is untouched");
    Ok(())
}

// ------------------------------------------------------------------ flush

#[test]
fn flush_returns_the_graph_to_a_runnable_state() -> Result<()> {
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    c.graph.send(c.src, gray_frame(16, 16, 0, 0))?;
    c.graph.close_source(c.src, Timestamp::new(1))?;
    c.graph.run()?;
    let _ = c.graph.recv(c.sink);
    c.graph.flush();
    // Formats survive a seek; queues and statuses do not.
    match c.graph.sink_format(c.sink)? {
        LinkFormat::Video { format, .. } => assert_eq!(*format, PixFmt::Gray8),
        other => panic!("expected video, got {other:?}"),
    }
    c.graph.send(c.src, gray_frame(16, 16, 9, 9))?;
    c.graph.run()?;
    let out = c.graph.recv(c.sink)?;
    assert_eq!(first_byte(&out), Some(!9));
    Ok(())
}

#[test]
fn a_seek_immediately_followed_by_end_of_stream_still_closes_the_outputs() -> Result<()> {
    // Found by `filter_graph_schedule`, first run, at exec 26. The shrunk
    // sequence was: send, send, close, run, flush.
    //
    // After the flush the link's sticky end of stream is cleared *and* the
    // output pad is re-opened, but the adapter's own "I have finished" flag is
    // not — `Filter` has no hook that a seek could reach. It recovered by
    // watching for the input to stop being at EOF, and closing the source again
    // before the filter next ran meant that never happened: the adapter
    // returned `Activity::Eof` over an output pad the flush had re-opened, and
    // downstream would have waited forever.
    //
    // This is a finding about the trait, not only about the adapter. See the
    // signature gaps in `docs/filter/vaco-filter-core.md`.
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    c.graph.send(c.src, gray_frame(16, 16, 0, 0))?;
    c.graph.send(c.src, gray_frame(16, 16, 1, 1))?;
    c.graph.close_source(c.src, Timestamp::new(2))?;
    c.graph.run()?;
    while c.graph.recv(c.sink).is_ok() {}

    c.graph.flush();
    // Close again *before* the filter gets a chance to run: the exact ordering
    // the fuzzer found.
    c.graph.close_source(c.src, Timestamp::new(0))?;
    let status = c.graph.run()?;
    assert!(
        c.graph.violations().is_empty(),
        "{:?}",
        c.graph.violations()
    );
    assert_eq!(status, GraphStatus::Eof, "the sink must see end of stream");
    assert!(matches!(c.graph.recv(c.sink), Err(Error::Eof)));
    Ok(())
}

#[test]
fn a_seek_that_brings_new_data_restarts_the_filter() -> Result<()> {
    // The other half of the same recovery: when the flush *is* followed by new
    // frames, the adapter must un-finish rather than re-close.
    let mut c = chain(|g| vec![Invert::node(g, "invert")])?;
    c.graph.send(c.src, gray_frame(16, 16, 0, 0))?;
    c.graph.close_source(c.src, Timestamp::new(1))?;
    c.graph.run()?;
    while c.graph.recv(c.sink).is_ok() {}

    c.graph.flush();
    c.graph.send(c.src, gray_frame(16, 16, 5, 5))?;
    c.graph.close_source(c.src, Timestamp::new(6))?;
    c.graph.run()?;
    let out = c.graph.recv(c.sink)?;
    assert_eq!(first_byte(&out), Some(!5));
    assert!(c.graph.violations().is_empty());
    Ok(())
}

#[test]
fn a_seek_reaches_the_filter_and_drops_what_it_was_holding() -> Result<()> {
    // `Fps` holds the previous input so it can fill an output slot that falls
    // between two inputs. Across a seek that frame is from the wrong place in
    // the stream, and before `Filter::flush` existed there was no way to tell it
    // so — it would have been spliced onto the new position.
    let mut c = chain(|g| vec![Fps::node(g, "fps", Rational::new(10, 1))])?;
    for i in 0..5 {
        c.graph.send(c.src, gray_frame(16, 16, i, 0xaa))?;
        c.graph.run()?;
        while c.graph.recv(c.sink).is_ok() {}
    }

    c.graph.flush();

    // A single, clearly distinguishable frame after the seek.
    c.graph.send(c.src, gray_frame(16, 16, 0, 0x11))?;
    c.graph.close_source(c.src, Timestamp::new(1))?;
    c.graph.run()?;
    let mut out = Vec::new();
    while let Ok(f) = c.graph.recv(c.sink) {
        out.push(f);
    }
    assert!(!out.is_empty(), "the post-seek frame must come through");
    for (i, f) in out.iter().enumerate() {
        assert_eq!(
            first_byte(f),
            Some(0x11),
            "frame {i} came from before the seek"
        );
        assert_eq!(f.pts, Timestamp::new(i as i64), "timestamps restart");
    }
    assert!(c.graph.violations().is_empty());
    Ok(())
}

// -------------------------------------------------------------- descriptors

#[test]
fn a_generic_timeline_filter_must_be_one_in_one_out() {
    assert!(Invert::DESC.is_consistent());
    const BAD: FilterDesc = FilterDesc {
        name: "bad",
        description: "generic timeline with two inputs",
        inputs: &[
            Pad {
                name: "a",
                media_type: MediaType::Video,
            },
            Pad {
                name: "b",
                media_type: MediaType::Video,
            },
        ],
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };
    assert!(
        !BAD.is_consistent(),
        "\"forward the input unchanged\" is not well defined for two inputs"
    );
}
