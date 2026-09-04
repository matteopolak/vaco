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

use smallvec::SmallVec;
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
    Activity, Command, CommandFlags, CommandReply, Dual, DualFilter, Fanout, FanoutFilter, Filter,
    FilterContext, FilterDesc, FilterFlags, FrameFilter, FrameOut, Graph, GraphStatus, LinkFormat,
    LinkView, NodeId, NodeView, Pad, Paired, PairedFilter, Simple, Violation,
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
            Err(r) if matches!(r.error, Error::OutputPending) => break,
            Err(r) => return Err(r.error),
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

// -------------------------------------------------- fan-out backpressure

/// A two-output split that does **not** check for room before pushing.
///
/// The shipped `Split` in `vaco-filter-graph` guards itself with
/// `output_has_room`, so it never lost a frame. This one is deliberately naive,
/// because the question is whether the *scheduler* can put a filter into the
/// lossy state at all — not whether one careful filter avoids it.
#[derive(Debug)]
struct NaiveSplit;

impl Filter for NaiveSplit {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if let Some(frame) = ctx.take_input(0) {
            ctx.push_output(0, frame.clone())?;
            ctx.push_output(1, frame)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }
}

/// Readiness requires room on **every** open output, not on any one of them.
///
/// With `any`, this filter was runnable the moment the drained sink had room:
/// it took its input frame, delivered one copy, and dropped the copy bound for
/// the full pad — silently, reporting `Progressed`. The output ended up short
/// by exactly the frames the graph was busiest for.
#[test]
fn a_naive_split_cannot_be_scheduled_into_losing_a_frame() -> Result<()> {
    const TWO_OUT: &[Pad] = &[
        Pad {
            name: "out0",
            media_type: MediaType::Video,
        },
        Pad {
            name: "out1",
            media_type: MediaType::Video,
        },
    ];
    const DESC: FilterDesc = FilterDesc {
        name: "naivesplit",
        description: "two-output split with no self-guard",
        inputs: VIDEO_PAD,
        outputs: TWO_OUT,
        flags: FilterFlags::empty(),
    };

    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let split = graph.add(
        DESC,
        NodeFormats::passthrough(1, 2, MediaType::Video, "naivesplit"),
        Box::new(NaiveSplit),
    );
    let fast = graph.add_sink("fast", MediaType::Video, any_video_sink("fast"));
    let slow = graph.add_sink("slow", MediaType::Video, any_video_sink("slow"));
    graph.connect(src, 0, split, 0)?;
    graph.connect(split, 0, fast, 0)?;
    graph.connect(split, 1, slow, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    // Push more than one link can hold while draining **only** `fast`, so
    // `slow` backs up. That asymmetry is the whole experiment.
    let mut fast_seen = 0_usize;
    let mut slow_seen = 0_usize;
    let mut sent = 0_i64;
    for i in 0..64 {
        if graph.send(src, gray_frame(16, 16, i, 0)).is_err() {
            break;
        }
        sent += 1;
        graph.run()?;
        while graph.recv(fast).is_ok() {
            fast_seen += 1;
        }
    }
    graph.close_source(src, Timestamp::new(sent))?;

    // Now drain both to exhaustion.
    for _ in 0..1000 {
        graph.run()?;
        let mut moved = false;
        while graph.recv(fast).is_ok() {
            fast_seen += 1;
            moved = true;
        }
        while graph.recv(slow).is_ok() {
            slow_seen += 1;
            moved = true;
        }
        if !moved {
            break;
        }
    }
    assert!(fast_seen > 0, "nothing got through at all");
    assert_eq!(
        fast_seen,
        slow_seen,
        "the slow consumer is short by {} frames",
        fast_seen.saturating_sub(slow_seen)
    );
    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
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

// ------------------------------------------------------------ Paired

/// Sums the first byte of every input into a copy of input 0.
///
/// Proves the adapter delivers exactly `n` frames together, in pad order —
/// not just "a pair" — since the sum only comes out right if every input
/// contributed the frame from the same step.
#[derive(Debug)]
struct SumInputs {
    n: usize,
}

impl PairedFilter for SumInputs {
    fn input_count(&self) -> usize {
        self.n
    }

    fn filter_frames(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[vaco_frame::Frame; 4]>,
    ) -> Result<FrameOut> {
        let sum: u32 = inputs.iter().filter_map(first_byte).map(u32::from).sum();
        let byte = u8::try_from(sum & 0xff).unwrap_or(0);
        let mut iter = inputs.into_iter();
        let Some(mut main) = iter.next() else {
            return Ok(FrameOut::None);
        };
        if let Some(mut plane) = main.plane_mut(0) {
            plane.fill(byte);
        }
        Ok(FrameOut::One(main))
    }
}

const TWO_INPUT_PADS: &[Pad] = &[
    Pad {
        name: "a",
        media_type: MediaType::Video,
    },
    Pad {
        name: "b",
        media_type: MediaType::Video,
    },
];

const THREE_INPUT_PADS: &[Pad] = &[
    Pad {
        name: "a",
        media_type: MediaType::Video,
    },
    Pad {
        name: "b",
        media_type: MediaType::Video,
    },
    Pad {
        name: "c",
        media_type: MediaType::Video,
    },
];

/// Measured against `ffmpeg -h filter=framepack`/`=mergeplanes` (see
/// `Paired`'s own doc): unlike `vaco-filter-framesync`'s `overlay`/`blend`,
/// there is no `eof_action=repeat`. Feeding a 5-frame and a 3-frame input at
/// the same rate produces exactly 3 outputs, not 5 with the last of the
/// shorter input repeated.
#[test]
fn paired_stops_at_the_first_input_to_run_dry() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "sum2",
        description: "test: pairs two inputs and sums their pixel values",
        inputs: TWO_INPUT_PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src_a = graph.add_source(
        "a",
        MediaType::Video,
        video_source_formats("a", PixFmt::Gray8),
    );
    let src_b = graph.add_source(
        "b",
        MediaType::Video,
        video_source_formats("b", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(2, 1, MediaType::Video, "sum2"),
        Box::new(Paired::new(SumInputs { n: 2 })),
    );
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src_a, 0, node, 0)?;
    graph.connect(src_b, 0, node, 1)?;
    graph.connect(node, 0, sink, 0)?;
    graph.set_source_format(src_a, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.set_source_format(src_b, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    for i in 0..5u8 {
        graph.send(src_a, gray_frame(16, 16, i64::from(i), i))?;
    }
    graph.close_source(src_a, Timestamp::new(5))?;
    for i in 0..3u8 {
        graph.send(src_b, gray_frame(16, 16, i64::from(i), i * 10))?;
    }
    graph.close_source(src_b, Timestamp::new(3))?;

    let mut out = Vec::new();
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        loop {
            match graph.recv(sink) {
                Ok(f) => out.push(f),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
    }
    assert_eq!(
        out.len(),
        3,
        "stops the instant the shorter input is exhausted, discarding the rest of the longer one"
    );
    let values: Vec<u8> = out.iter().filter_map(first_byte).collect();
    assert_eq!(values, vec![0, 11, 22]);
    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
    Ok(())
}

/// `mergeplanes` needs more than two inputs (up to four, fixed at
/// construction). `PairedFilter::input_count` is what makes that the same
/// adapter rather than a second one: three inputs, still strict lockstep,
/// still stopping at the shortest.
#[test]
fn paired_generalises_beyond_two_inputs() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "sum3",
        description: "test: pairs three inputs and sums their pixel values",
        inputs: THREE_INPUT_PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src_a = graph.add_source(
        "a",
        MediaType::Video,
        video_source_formats("a", PixFmt::Gray8),
    );
    let src_b = graph.add_source(
        "b",
        MediaType::Video,
        video_source_formats("b", PixFmt::Gray8),
    );
    let src_c = graph.add_source(
        "c",
        MediaType::Video,
        video_source_formats("c", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(3, 1, MediaType::Video, "sum3"),
        Box::new(Paired::new(SumInputs { n: 3 })),
    );
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src_a, 0, node, 0)?;
    graph.connect(src_b, 0, node, 1)?;
    graph.connect(src_c, 0, node, 2)?;
    graph.connect(node, 0, sink, 0)?;
    graph.set_source_format(src_a, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.set_source_format(src_b, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.set_source_format(src_c, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    for i in 0..4u8 {
        graph.send(src_a, gray_frame(16, 16, i64::from(i), i))?;
    }
    graph.close_source(src_a, Timestamp::new(4))?;
    for i in 0..6u8 {
        graph.send(src_b, gray_frame(16, 16, i64::from(i), i * 10))?;
    }
    graph.close_source(src_b, Timestamp::new(6))?;
    for i in 0..2u8 {
        graph.send(src_c, gray_frame(16, 16, i64::from(i), i * 100))?;
    }
    graph.close_source(src_c, Timestamp::new(2))?;

    let mut out = Vec::new();
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        loop {
            match graph.recv(sink) {
                Ok(f) => out.push(f),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
    }
    assert_eq!(out.len(), 2, "stops at the shortest of the three: `c`");
    let values: Vec<u8> = out.iter().filter_map(first_byte).collect();
    // i=0: 0 + 0 + 0 = 0. i=1: 1 + 10 + 100 = 111.
    assert_eq!(values, vec![0, 111]);
    Ok(())
}

// ------------------------------------------------------------ Fanout

/// Splits one frame into two derived outputs: pad 0 unchanged, pad 1 the
/// value plus one. Proves each pad gets its *own* frame in pad order, which a
/// plain N-way clone (`split`) does not exercise.
#[derive(Debug)]
struct SplitPlusOne;

impl FanoutFilter for SplitPlusOne {
    fn output_count(&self) -> usize {
        2
    }

    fn split_frame(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        input: vaco_frame::Frame,
    ) -> Result<SmallVec<[vaco_frame::Frame; 4]>> {
        let value = first_byte(&input).unwrap_or(0);
        let mut second = input.clone();
        if let Some(mut plane) = second.plane_mut(0) {
            plane.fill(value.wrapping_add(1));
        }
        Ok(SmallVec::from_iter([input, second]))
    }
}

#[test]
fn fanout_delivers_one_frame_per_output_pad() -> Result<()> {
    const TWO_OUT: &[Pad] = &[
        Pad {
            name: "a",
            media_type: MediaType::Video,
        },
        Pad {
            name: "b",
            media_type: MediaType::Video,
        },
    ];
    const DESC: FilterDesc = FilterDesc {
        name: "split_plus_one",
        description: "test: fans one input into two derived outputs",
        inputs: VIDEO_PAD,
        outputs: TWO_OUT,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
    };
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(1, 2, MediaType::Video, "split_plus_one"),
        Box::new(Fanout::new(SplitPlusOne)),
    );
    let sink_a = graph.add_sink("a", MediaType::Video, any_video_sink("a"));
    let sink_b = graph.add_sink("b", MediaType::Video, any_video_sink("b"));
    graph.connect(src, 0, node, 0)?;
    graph.connect(node, 0, sink_a, 0)?;
    graph.connect(node, 1, sink_b, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    for i in 0..3u8 {
        graph.send(src, gray_frame(16, 16, i64::from(i), i * 5))?;
    }
    graph.close_source(src, Timestamp::new(3))?;

    let mut out_a = Vec::new();
    let mut out_b = Vec::new();
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        loop {
            match graph.recv(sink_a) {
                Ok(f) => out_a.push(f),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
        loop {
            match graph.recv(sink_b) {
                Ok(f) => out_b.push(f),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
    }
    let a: Vec<u8> = out_a.iter().filter_map(first_byte).collect();
    let b: Vec<u8> = out_b.iter().filter_map(first_byte).collect();
    assert_eq!(a, vec![0, 5, 10]);
    assert_eq!(b, vec![1, 6, 11]);
    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
    Ok(())
}

/// A fanout filter that lies about how many frames it produces is a defect
/// the adapter catches rather than silently under- or over-delivering pads.
#[derive(Debug)]
struct WrongCount;

impl FanoutFilter for WrongCount {
    fn output_count(&self) -> usize {
        2
    }

    fn split_frame(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        input: vaco_frame::Frame,
    ) -> Result<SmallVec<[vaco_frame::Frame; 4]>> {
        Ok(SmallVec::from_iter([input]))
    }
}

#[test]
fn fanout_catches_a_filter_that_produces_the_wrong_count() -> Result<()> {
    const TWO_OUT: &[Pad] = &[
        Pad {
            name: "a",
            media_type: MediaType::Video,
        },
        Pad {
            name: "b",
            media_type: MediaType::Video,
        },
    ];
    const DESC: FilterDesc = FilterDesc {
        name: "wrong_count",
        description: "test: claims two outputs, delivers one",
        inputs: VIDEO_PAD,
        outputs: TWO_OUT,
        flags: FilterFlags::DYNAMIC_OUTPUTS,
    };
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(1, 2, MediaType::Video, "wrong_count"),
        Box::new(Fanout::new(WrongCount)),
    );
    let sink_a = graph.add_sink("a", MediaType::Video, any_video_sink("a"));
    let sink_b = graph.add_sink("b", MediaType::Video, any_video_sink("b"));
    graph.connect(src, 0, node, 0)?;
    graph.connect(node, 0, sink_a, 0)?;
    graph.connect(node, 1, sink_b, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;
    graph.send(src, gray_frame(16, 16, 0, 0))?;
    let e = graph.run();
    assert!(
        matches!(e, Err(Error::InvalidData(_))),
        "a mismatched frame count must surface as an error, not a dropped pad: {e:?}"
    );
    Ok(())
}

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

// -------------------------------------------------------------- Dual

const TWO_OUTPUT_PADS: &[Pad] = &[
    Pad {
        name: "out",
        media_type: MediaType::Video,
    },
    Pad {
        name: "fb",
        media_type: MediaType::Video,
    },
];

/// Swaps its two inputs onto its two outputs: output pad `0` gets input
/// pad `1`'s frame, output pad `1` gets input pad `0`'s frame.
///
/// Proves the adapter routes each output to *its own* pad rather than, say,
/// pushing the same frame twice or leaving the two pending queues aliased —
/// a swap only comes out right if pad `0`'s queue and pad `1`'s queue never
/// cross. `Paired`'s own `SumInputs` test proves the input side ("every
/// input contributed together"); this is the output-side analogue gap 24
/// needed and no existing adapter test covers, since every earlier adapter
/// has at most one output.
#[derive(Debug)]
struct SwapInputs;

impl DualFilter for SwapInputs {
    fn filter_frames(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[vaco_frame::Frame; 4]>,
    ) -> Result<SmallVec<[vaco_frame::Frame; 4]>> {
        let mut iter = inputs.into_iter();
        let (Some(a), Some(b)) = (iter.next(), iter.next()) else {
            return Ok(SmallVec::new());
        };
        Ok(SmallVec::from_iter([b, a]))
    }
}

/// End-to-end: two sources in, two sinks out, through `Dual`. Deliberately
/// swaps so that a bug routing both outputs from the same pending queue (or
/// routing pad `1`'s frame to pad `0`) fails the value assertions rather
/// than merely failing to compile — the runtime half of the pair this gap's
/// own doc asks for; `dual_stops_at_the_first_input_to_run_dry` below is
/// the lockstep half.
#[test]
fn dual_routes_each_output_to_its_own_pad() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "swap2",
        description: "test: swaps two inputs onto two outputs",
        inputs: TWO_INPUT_PADS,
        outputs: TWO_OUTPUT_PADS,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src_a = graph.add_source(
        "a",
        MediaType::Video,
        video_source_formats("a", PixFmt::Gray8),
    );
    let src_b = graph.add_source(
        "b",
        MediaType::Video,
        video_source_formats("b", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(2, 2, MediaType::Video, "swap2"),
        Box::new(Dual::new(SwapInputs)),
    );
    let sink_out = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    let sink_fb = graph.add_sink("fb", MediaType::Video, any_video_sink("fb"));
    graph.connect(src_a, 0, node, 0)?;
    graph.connect(src_b, 0, node, 1)?;
    graph.connect(node, 0, sink_out, 0)?;
    graph.connect(node, 1, sink_fb, 0)?;
    graph.set_source_format(src_a, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.set_source_format(src_b, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    for i in 0..3u8 {
        graph.send(src_a, gray_frame(16, 16, i64::from(i), i))?;
    }
    graph.close_source(src_a, Timestamp::new(3))?;
    for i in 0..3u8 {
        graph.send(src_b, gray_frame(16, 16, i64::from(i), i * 10))?;
    }
    graph.close_source(src_b, Timestamp::new(3))?;

    let mut out_vals = Vec::new();
    let mut fb_vals = Vec::new();
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        loop {
            match graph.recv(sink_out) {
                Ok(f) => out_vals.push(first_byte(&f)),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
        loop {
            match graph.recv(sink_fb) {
                Ok(f) => fb_vals.push(first_byte(&f)),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
    }
    // Output pad 0 got input b's values (0, 10, 20); output pad 1 got input
    // a's values (0, 1, 2) -- the swap, not a duplicate of either input.
    assert_eq!(
        out_vals,
        vec![Some(0), Some(10), Some(20)],
        "output pad 0 must carry input b's frames, not input a's or a copy of both"
    );
    assert_eq!(
        fb_vals,
        vec![Some(0), Some(1), Some(2)],
        "output pad 1 must carry input a's frames, not input b's or a copy of both"
    );
    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
    Ok(())
}

/// Same lockstep contract `Paired` has: no independent timeline, no
/// `eof_action=repeat` — the first input to run dry ends the whole filter,
/// discarding whatever the longer input still had queued.
#[test]
fn dual_stops_at_the_first_input_to_run_dry() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "swap2b",
        description: "test: swaps two inputs onto two outputs",
        inputs: TWO_INPUT_PADS,
        outputs: TWO_OUTPUT_PADS,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src_a = graph.add_source(
        "a",
        MediaType::Video,
        video_source_formats("a", PixFmt::Gray8),
    );
    let src_b = graph.add_source(
        "b",
        MediaType::Video,
        video_source_formats("b", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(2, 2, MediaType::Video, "swap2b"),
        Box::new(Dual::new(SwapInputs)),
    );
    let sink_out = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    let sink_fb = graph.add_sink("fb", MediaType::Video, any_video_sink("fb"));
    graph.connect(src_a, 0, node, 0)?;
    graph.connect(src_b, 0, node, 1)?;
    graph.connect(node, 0, sink_out, 0)?;
    graph.connect(node, 1, sink_fb, 0)?;
    graph.set_source_format(src_a, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.set_source_format(src_b, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    for i in 0..5u8 {
        graph.send(src_a, gray_frame(16, 16, i64::from(i), i))?;
    }
    graph.close_source(src_a, Timestamp::new(5))?;
    for i in 0..2u8 {
        graph.send(src_b, gray_frame(16, 16, i64::from(i), i * 10))?;
    }
    graph.close_source(src_b, Timestamp::new(2))?;

    let mut out_vals = Vec::new();
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        loop {
            match graph.recv(sink_out) {
                Ok(f) => out_vals.push(first_byte(&f)),
                Err(Error::NeedMoreInput | Error::Eof) => break,
                Err(e) => return Err(e),
            }
        }
        while graph.recv(sink_fb).is_ok() {}
    }
    assert_eq!(
        out_vals.len(),
        2,
        "stops the instant the shorter input (b, 2 frames) is exhausted"
    );
    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
    Ok(())
}

/// `feedback`'s own reference usage loops one output back as the filter's
/// next-frame input (`[0][fb]feedback[out][fb]`) — a genuine cycle, not
/// just the two-output arity `Dual` supplies. Confirms directly, against
/// this crate's own scheduler rather than by inspection, that
/// `Graph::configure` refuses such a link before a single frame flows: the
/// adapter is necessary but not sufficient for `feedback` (see `Dual`'s own
/// doc and `planning/INTERFACE-GAPS.md`).
#[test]
fn a_link_back_into_the_same_node_is_rejected_as_a_cycle_at_configure() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "swap2c",
        description: "test: swaps two inputs onto two outputs",
        inputs: TWO_INPUT_PADS,
        outputs: TWO_OUTPUT_PADS,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src_a = graph.add_source(
        "a",
        MediaType::Video,
        video_source_formats("a", PixFmt::Gray8),
    );
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(2, 2, MediaType::Video, "swap2c"),
        Box::new(Dual::new(SwapInputs)),
    );
    let sink_out = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src_a, 0, node, 0)?;
    // The feedback wiring itself: output pad 1 feeds back into input pad 1
    // of the very same node.
    graph.connect(node, 1, node, 1)?;
    graph.connect(node, 0, sink_out, 0)?;
    graph.set_source_format(src_a, gray_link(16, 16, Rational::new(1, 25)))?;

    let err = graph
        .configure()
        .expect_err("a self-loop must not configure");
    let message = err.to_string();
    assert!(
        message.contains("cycle"),
        "expected a cycle-shaped error, got: {message}"
    );
    Ok(())
}

// ------------------------------------------------------- graph introspection

/// Captures `ctx.graph_nodes()`/`ctx.graph_links()` on its first call, then
/// passes every frame through unchanged. `Arc<Mutex<..>>` rather than
/// `Simple::into_inner` because the filter is stored as `Box<dyn Filter>` on
/// the graph — the concrete type, and any state inside it, is not
/// recoverable after `graph.add` without a handle kept on the side.
#[derive(Debug, Clone)]
struct GraphProbe {
    seen: std::sync::Arc<std::sync::Mutex<Option<(Vec<NodeView>, Vec<LinkView>)>>>,
}

impl FrameFilter for GraphProbe {
    fn filter_frame(
        &mut self,
        ctx: &mut FilterContext<'_>,
        input: vaco_frame::Frame,
    ) -> Result<FrameOut> {
        let mut seen = self.seen.lock().unwrap();
        if seen.is_none() {
            *seen = Some((ctx.graph_nodes().to_vec(), ctx.graph_links()));
        }
        Ok(FrameOut::One(input))
    }
}

/// The runtime half of gap 22's proof: a filter mid-graph can see *other*
/// nodes' labels and link state through a real `Graph::run`, not just
/// through a type that happens to compile. Three nodes besides the probe
/// itself (source, sink, and the probe's own node) must all be visible by
/// label, and the source->probe link's queue state must be readable even
/// though it is not one of the probe's own *output* pads.
#[test]
fn a_filter_can_read_every_nodes_label_and_every_links_state() -> Result<()> {
    const DESC: FilterDesc = FilterDesc {
        name: "probe",
        description: "test: records graph_nodes()/graph_links() on its first frame",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };
    let mut graph = Graph::new();
    let src = graph.add_source(
        "probe_source",
        MediaType::Video,
        video_source_formats("probe_source", PixFmt::Gray8),
    );
    let probe = std::sync::Arc::new(std::sync::Mutex::new(None));
    let node = graph.add(
        DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "probe_node"),
        Box::new(Simple::new(GraphProbe {
            seen: std::sync::Arc::clone(&probe),
        })),
    );
    let sink = graph.add_sink("probe_sink", MediaType::Video, any_video_sink("probe_sink"));
    graph.connect(src, 0, node, 0)?;
    graph.connect(node, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    graph.send(src, gray_frame(16, 16, 0, 7))?;
    graph.close_source(src, Timestamp::new(1))?;
    for _ in 0..1000 {
        match graph.run()? {
            GraphStatus::Eof => break,
            GraphStatus::HasOutput(_) => {}
            other => panic!("unexpected graph status: {other:?}"),
        }
        while graph.recv(sink).is_ok() {}
    }

    let guard = probe.lock().unwrap();
    let (nodes, links) = guard.as_ref().expect("the probe frame must have run");

    let labels: Vec<&str> = nodes.iter().map(|n| n.label.as_str()).collect();
    assert!(
        labels.contains(&"probe_source") && labels.contains(&"probe_sink"),
        "a filter must be able to name nodes that are not itself: {labels:?}"
    );
    assert!(
        nodes.iter().any(|n| n.id == node),
        "the probe's own node must also appear, by the same id `connect` used"
    );

    // The source -> probe link is the probe's own *input*, not something
    // `FilterContext::input_link` alone would call "another node's state" —
    // but a diagram needs every link, including this one, addressed by the
    // same `PadRef` the source/sink pair used to `connect`.
    let source_link = links
        .iter()
        .find(|l| l.src.node == src)
        .expect("the source's own outbound link must be visible");
    assert_eq!(source_link.dst.node, node);
    Ok(())
}

// ----------------------------------------------------------- filter commands

#[derive(Debug)]
struct CommandProbe {
    events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    fast: bool,
}

#[derive(Debug)]
struct ReplyProbe;

impl Filter for ReplyProbe {
    fn activate(&mut self, _ctx: &mut FilterContext<'_>) -> Result<Activity> {
        Ok(Activity::Blocked)
    }

    fn process_command(&mut self, command: &Command<'_>) -> Result<CommandReply> {
        Ok(CommandReply::Text(format!(
            "{}={}",
            command.name, command.arg
        )))
    }
}

impl Filter for CommandProbe {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if let Some(frame) = ctx.take_input(0) {
            self.events
                .lock()
                .unwrap()
                .push(format!("frame:{}", frame.pts));
            ctx.push_output(0, frame)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_output(0);
            return Ok(Activity::Eof);
        }
        ctx.request_input(0);
        Ok(Activity::NeedInput)
    }

    fn command_flags(&self, _name: &str) -> CommandFlags {
        if self.fast {
            CommandFlags::FAST
        } else {
            CommandFlags::empty()
        }
    }

    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        if name == "reject" {
            return Err(Error::Option {
                name: name.to_owned(),
                detail: "rejected by probe".to_owned(),
            });
        }
        self.events
            .lock()
            .unwrap()
            .push(format!("command:{name}={value}"));
        Ok(())
    }
}

const COMMAND_DESC: FilterDesc = FilterDesc {
    name: "volume",
    description: "test: records commands and frames",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

fn command_node(
    graph: &mut Graph,
    label: &str,
    events: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    fast: bool,
) -> NodeId {
    graph.add(
        COMMAND_DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, label),
        Box::new(CommandProbe {
            events: std::sync::Arc::clone(events),
            fast,
        }),
    )
}

#[test]
fn immediate_commands_match_filter_names_and_exact_instance_labels() -> Result<()> {
    let first = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let second = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    command_node(&mut graph, "volume@first", &first, true);
    command_node(&mut graph, "volume@second", &second, true);

    assert_eq!(
        graph
            .send_command("volume", "gain", "2", CommandFlags::empty())?
            .len(),
        2
    );
    assert_eq!(
        graph
            .send_command("volume@second", "gain", "3", CommandFlags::empty())?
            .len(),
        1
    );
    assert_eq!(
        graph
            .send_command("all", "gain", "4", CommandFlags::empty())?
            .len(),
        2
    );
    assert_eq!(
        first.lock().unwrap().as_slice(),
        ["command:gain=2", "command:gain=4"],
        "a filter-name target reaches every instance"
    );
    assert_eq!(
        second.lock().unwrap().as_slice(),
        ["command:gain=2", "command:gain=3", "command:gain=4"],
        "an instance target reaches only the exact label"
    );
    Ok(())
}

#[test]
fn immediate_dispatch_returns_a_filters_text_reply() -> Result<()> {
    let mut graph = Graph::new();
    graph.add(
        COMMAND_DESC,
        NodeFormats::passthrough(1, 1, MediaType::Video, "volume@query"),
        Box::new(ReplyProbe),
    );
    assert_eq!(
        graph.send_command("volume@query", "status", "now", CommandFlags::empty())?,
        [CommandReply::Text("status=now".to_owned())]
    );
    Ok(())
}

#[test]
fn one_and_fast_flags_are_enforced_by_dispatch() -> Result<()> {
    let first = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let second = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let slow = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    command_node(&mut graph, "volume@first", &first, true);
    command_node(&mut graph, "volume@second", &second, true);
    command_node(&mut graph, "volume@slow", &slow, false);

    assert_eq!(
        graph
            .send_command("volume", "gain", "4", CommandFlags::ONE)?
            .len(),
        1
    );
    assert_eq!(first.lock().unwrap().as_slice(), ["command:gain=4"]);
    assert!(second.lock().unwrap().is_empty());
    assert!(slow.lock().unwrap().is_empty());

    let err = graph
        .send_command("volume@slow", "gain", "5", CommandFlags::FAST)
        .expect_err("FAST must refuse a command the filter marks slow");
    assert!(matches!(err, Error::Unsupported(_)));
    assert!(slow.lock().unwrap().is_empty());
    Ok(())
}

#[test]
fn command_target_must_match_a_filter() {
    let mut graph = Graph::new();
    let err = graph
        .send_command("missing", "gain", "2", CommandFlags::empty())
        .expect_err("a typo must not disappear as a successful no-op");
    match err {
        Error::Option { name, detail } => {
            assert_eq!(name, "target");
            assert!(detail.contains("missing"));
        }
        other => panic!("expected target option error, got {other:?}"),
    }
}

#[test]
fn queued_commands_fire_before_the_first_frame_at_or_after_their_time() -> Result<()> {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let node = command_node(&mut graph, "volume@timed", &events, true);
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, node, 0)?;
    graph.connect(node, 0, sink, 0)?;
    let base = Rational::new(1, 25);
    graph.set_source_format(src, gray_link(16, 16, base))?;
    graph.configure()?;

    // Queue out of timestamp order. Dispatch must still be chronological, and
    // the two commands at tick 5 must retain insertion order.
    assert_eq!(
        graph.queue_command(
            Timestamp::new(7),
            base,
            "volume@timed",
            "gain",
            "7",
            CommandFlags::empty(),
        )?,
        1
    );
    graph.queue_command(
        Timestamp::new(5),
        base,
        "volume@timed",
        "gain",
        "5a",
        CommandFlags::empty(),
    )?;
    graph.queue_command(
        Timestamp::new(5),
        base,
        "volume@timed",
        "gain",
        "5b",
        CommandFlags::empty(),
    )?;

    for pts in [4, 5, 7] {
        graph.send(src, gray_frame(16, 16, pts, 0))?;
        graph.run()?;
        while graph.recv(sink).is_ok() {}
    }

    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            "frame:4",
            "command:gain=5a",
            "command:gain=5b",
            "frame:5",
            "command:gain=7",
            "frame:7",
        ]
    );
    Ok(())
}

#[test]
fn a_rejected_queued_command_is_reported_once_and_does_not_stall_the_frame() -> Result<()> {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let node = command_node(&mut graph, "volume@reject", &events, true);
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, node, 0)?;
    graph.connect(node, 0, sink, 0)?;
    let base = Rational::new(1, 25);
    graph.set_source_format(src, gray_link(16, 16, base))?;
    graph.configure()?;
    graph.queue_command(
        Timestamp::new(0),
        base,
        "volume@reject",
        "reject",
        "value",
        CommandFlags::empty(),
    )?;
    graph.send(src, gray_frame(16, 16, 0, 0))?;

    assert!(matches!(graph.run(), Err(Error::Option { .. })));
    assert_eq!(graph.queued_command_count(), 0, "a rejection is consumed");
    graph.run()?;
    assert_eq!(events.lock().unwrap().as_slice(), ["frame:0"]);
    assert!(
        graph.recv(sink).is_ok(),
        "the due frame still runs on retry"
    );
    Ok(())
}
