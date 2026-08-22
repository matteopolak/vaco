//! What the framework itself costs, separated from what filters cost.
//!
//! The number that matters is **per-frame scheduler overhead**: with ~560
//! filters and graphs routinely ten deep, anything here is paid once per node
//! per frame. `Drop::new(0)` never drops and never writes, so the only thing
//! being measured through it is the machinery.
//!
//! Negotiation is measured separately because it happens once per graph, not
//! once per frame — a millisecond there is invisible, a microsecond per frame is
//! not, and reporting them in the same table invites the wrong conclusion.

use divan::Bencher;
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_filter_core::mock::{
    Drop, Fps, Invert, any_video_sink, gray_frame, gray_link, video_source_formats,
};
use vaco_filter_core::negotiate::{
    AutoConvert, Constraint, FormatSet, NegotiationPlan, NoConversion, NodeFormats, negotiate,
};
use vaco_filter_core::{Graph, NodeId, PadRef};
use vaco_pixfmt::PixFmt;

fn main() {
    divan::main();
}

/// A source, `depth` passthrough filters and a sink, configured and ready.
fn chain(depth: usize, invert: bool) -> Option<(Graph, NodeId, NodeId)> {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let mut prev = src;
    for i in 0..depth {
        let label = format!("f{i}");
        let node = if invert {
            Invert::node(&mut graph, &label)
        } else {
            Drop::node(&mut graph, &label, 0)
        };
        graph.connect(prev, 0, node, 0).ok()?;
        prev = node;
    }
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(prev, 0, sink, 0).ok()?;
    graph
        .set_source_format(src, gray_link(64, 64, Rational::new(1, 25)))
        .ok()?;
    graph.configure().ok()?;
    Some((graph, src, sink))
}

/// Push one frame through and take it out again.
fn pump(graph: &mut Graph, src: NodeId, sink: NodeId, pts: i64) -> usize {
    if graph.send(src, gray_frame(64, 64, pts, 0)).is_err() {
        return 0;
    }
    if graph.run().is_err() {
        return 0;
    }
    let mut n = 0;
    while graph.recv(sink).is_ok() {
        n += 1;
    }
    n
}

/// Framework overhead per node per frame: no pixels touched anywhere.
#[divan::bench(args = [1, 2, 4, 8])]
fn passthrough_frame(bencher: Bencher<'_, '_>, depth: usize) {
    let Some((mut graph, src, sink)) = chain(depth, false) else {
        return;
    };
    let mut pts = 0i64;
    bencher.bench_local(move || {
        pts += 1;
        divan::black_box(pump(&mut graph, src, sink, pts))
    });
}

/// The same chain, with each stage actually rewriting 4 KiB. The difference
/// against `passthrough_frame` is what the framework costs as a fraction of
/// real work.
#[divan::bench(args = [1, 4])]
fn inverting_frame(bencher: Bencher<'_, '_>, depth: usize) {
    let Some((mut graph, src, sink)) = chain(depth, true) else {
        return;
    };
    let mut pts = 0i64;
    bencher.bench_local(move || {
        pts += 1;
        divan::black_box(pump(&mut graph, src, sink, pts))
    });
}

/// A rate change: one input can produce several outputs, so this also measures
/// the multi-output push path.
#[divan::bench]
fn rate_doubling_frame(bencher: Bencher<'_, '_>) {
    let mut graph = Graph::new();
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let fps = Fps::node(&mut graph, "fps", Rational::new(50, 1));
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    if graph.connect(src, 0, fps, 0).is_err() || graph.connect(fps, 0, sink, 0).is_err() {
        return;
    }
    if graph
        .set_source_format(src, gray_link(64, 64, Rational::new(1, 25)))
        .is_err()
        || graph.configure().is_err()
    {
        return;
    }
    let mut pts = 0i64;
    bencher.bench_local(move || {
        pts += 1;
        divan::black_box(pump(&mut graph, src, sink, pts))
    });
}

/// Building the graph and agreeing formats. Once per graph, not per frame.
#[divan::bench(args = [2, 8, 32])]
fn configure_graph(bencher: Bencher<'_, '_>, depth: usize) {
    bencher.bench_local(move || divan::black_box(chain(depth, false).is_some()));
}

/// The solver alone, over a chain whose pads each list several formats — the
/// case where the union-find and the fold actually do work.
#[divan::bench(args = [4, 16, 64])]
fn negotiate_chain(bencher: Bencher<'_, '_>, depth: usize) {
    let palette = [
        PixFmt::Gray8,
        PixFmt::Yuv420p,
        PixFmt::Yuv422p,
        PixFmt::Yuv444p,
        PixFmt::Rgb24,
    ];
    bencher
        .with_inputs(move || {
            let mut plan = NegotiationPlan::new();
            for i in 0..depth {
                let set = FormatSet {
                    pixel_formats: Some(if i == 0 {
                        Constraint::Exact(PixFmt::Yuv420p)
                    } else {
                        Constraint::OneOf(palette.to_vec())
                    }),
                    ..FormatSet::default()
                };
                let label = format!("n{i}");
                let node = if i == 0 {
                    NodeFormats {
                        inputs: Vec::new(),
                        outputs: vec![set],
                        ties: Vec::new(),
                        label,
                    }
                } else if i == depth - 1 {
                    NodeFormats {
                        inputs: vec![set],
                        outputs: Vec::new(),
                        ties: Vec::new(),
                        label,
                    }
                } else {
                    NodeFormats::uniform(1, 1, MediaType::Video, &set, &label)
                };
                plan.add_node(node);
            }
            for i in 0..depth.saturating_sub(1) {
                let _ = plan.connect(
                    PadRef::output(NodeId(i as u32), 0),
                    PadRef::input(NodeId(i as u32 + 1), 0),
                    MediaType::Video,
                );
            }
            plan
        })
        .bench_local_values(|mut plan| {
            let mut conflicts = Vec::new();
            divan::black_box(
                negotiate(&mut plan, &NoConversion, AutoConvert::None, &mut conflicts).is_ok(),
            )
        });
}

/// Pure intersection, the inner loop of the fold.
#[divan::bench]
fn intersect_two_lists(bencher: Bencher<'_, '_>) {
    let a: Constraint<PixFmt> = Constraint::OneOf(vec![
        PixFmt::Yuv420p,
        PixFmt::Yuv422p,
        PixFmt::Yuv444p,
        PixFmt::Rgb24,
        PixFmt::Gbrp,
        PixFmt::Gray8,
    ]);
    let b: Constraint<PixFmt> = Constraint::OneOf(vec![
        PixFmt::Rgb24,
        PixFmt::Gbrp,
        PixFmt::Yuv444p,
        PixFmt::Bgr24,
    ]);
    bencher.bench_local(|| divan::black_box(a.intersect(divan::black_box(&b))));
}

/// Closing a source with a timestamp, which has to rescale it into every
/// downstream link's base. Cheap, but it happens on every seek boundary.
#[divan::bench]
fn close_and_rescale(bencher: Bencher<'_, '_>) {
    let Some((mut graph, src, _sink)) = chain(4, false) else {
        return;
    };
    bencher.bench_local(move || {
        graph.flush();
        divan::black_box(graph.close_source(src, Timestamp::new(1000)).is_ok())
    });
}
