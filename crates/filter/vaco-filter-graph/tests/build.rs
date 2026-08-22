//! Instantiation, link resolution, validation and auto-conversion, driven to
//! completion against `vaco-filter-core`'s scheduler.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use vaco_core::MediaType;
use vaco_filter_core::negotiate::{AutoConvert, FormatSet, NodeFormats};
use vaco_filter_core::{GraphStatus, LinkFormat};
use vaco_filter_graph::error::ErrorKind;
use vaco_filter_graph::mock::MockRegistry;
use vaco_filter_graph::{BuiltGraph, parse_and_build};

fn build(src: &str) -> BuiltGraph {
    parse_and_build(src, &MockRegistry::new())
        .unwrap_or_else(|e| panic!("{src:?} should build:\n{}", e.render(src)))
}

fn build_err(src: &str) -> ErrorKind {
    parse_and_build(src, &MockRegistry::new())
        .err()
        .unwrap_or_else(|| panic!("{src:?} should not build"))
        .kind
}

fn any_sink(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

fn gray_source_formats(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(vaco_pixfmt::PixFmt::Gray8)],
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

fn gray_link() -> LinkFormat {
    let mut format = LinkFormat::unconfigured(MediaType::Video);
    if let LinkFormat::Video {
        format: f,
        width,
        height,
        time_base,
        frame_rate,
        sample_aspect_ratio,
        ..
    } = &mut format
    {
        *f = vaco_pixfmt::PixFmt::Gray8;
        *width = 16;
        *height = 16;
        *time_base = vaco_core::Rational::new(1, 25);
        *frame_rate = vaco_core::Rational::new(25, 1);
        *sample_aspect_ratio = vaco_core::Rational::ONE;
    }
    format
}

/// Attach a buffer source to every open input and a sink to every open output,
/// configure, feed `frames` frames, and drain.
///
/// Buffer sources rather than the `counter` generator, deliberately: a
/// generator at the head of a chain currently stalls — see
/// `a_generator_behind_a_filter_does_not_yet_start`, which pins the reason.
fn run(src: &str, mode: AutoConvert, frames: usize) -> (BuiltGraph, Vec<usize>) {
    let mut built = build(src);
    let mut sources = Vec::new();
    while !built.open_inputs.is_empty() {
        sources.push(
            built
                .attach_source(0, gray_source_formats("in"), gray_link())
                .unwrap(),
        );
    }
    let mut sinks = Vec::new();
    while !built.open_outputs.is_empty() {
        sinks.push(built.attach_sink(0, any_sink("out")).unwrap());
    }
    built
        .configure(&MockRegistry::new(), mode)
        .unwrap_or_else(|e| panic!("{src:?}: {e}"));

    // Feed lazily: a link holds eight frames, so anything longer needs sending
    // and running to interleave — which is also what a real caller does and
    // what makes backpressure part of the test rather than an obstacle to it.
    let pool = built.graph.pool().clone();
    let mut sent = vec![0usize; sources.len()];
    let mut closed = vec![false; sources.len()];
    let mut counts = vec![0usize; sinks.len()];
    let mut done = vec![false; sinks.len()];
    for _ in 0..10_000 {
        built.graph.run().unwrap();
        for (i, &sink) in sinks.iter().enumerate() {
            loop {
                match built.graph.recv(sink) {
                    Ok(_) => counts[i] = counts[i].saturating_add(1),
                    Err(vaco_core::Error::NeedMoreInput) => break,
                    Err(vaco_core::Error::Eof) => {
                        done[i] = true;
                        break;
                    }
                    Err(e) => panic!("{src:?}: {e}"),
                }
            }
        }
        for (i, &source) in sources.iter().enumerate() {
            if sent[i] < frames {
                let mut frame = pool
                    .acquire_video(vaco_pixfmt::PixFmt::Gray8, 16, 16)
                    .unwrap();
                frame.pts = vaco_core::Timestamp::new(sent[i] as i64);
                frame.time_base = vaco_core::Rational::new(1, 25);
                match built.graph.send(source, frame) {
                    Ok(()) => sent[i] = sent[i].saturating_add(1),
                    Err(vaco_core::Error::OutputPending) => {}
                    Err(e) => panic!("{src:?}: {e}"),
                }
            } else if !closed[i] {
                built
                    .graph
                    .close_source(source, vaco_core::Timestamp::new(frames as i64))
                    .unwrap();
                closed[i] = true;
            }
        }
        if done.iter().all(|d| *d) {
            assert!(built.graph.violations().is_empty(), "{src:?}");
            return (built, counts);
        }
    }
    panic!("{src:?} did not finish: {:?}", built.graph.classify());
}

#[test]
fn a_chain_wires_itself_left_to_right() {
    let built = build("counter=n=3,null,null");
    assert_eq!(built.nodes.len(), 3);
    assert_eq!(built.graph.links().len(), 2);
    assert!(built.open_inputs.is_empty());
    assert_eq!(built.open_outputs.len(), 1);
    assert_eq!(built.open_outputs[0].label, None);
}

#[test]
fn instance_names_follow_the_reference() {
    let built = build("counter=n=1,null,invert@x,null");
    let got: Vec<&str> = built.nodes.iter().map(|n| n.instance.as_str()).collect();
    assert_eq!(
        got,
        [
            "Parsed_counter_0",
            "Parsed_null_1",
            "invert@x",
            "Parsed_null_3"
        ]
    );
}

#[test]
fn a_duplicate_explicit_instance_tag_is_an_error() {
    assert!(matches!(
        build_err("counter=n=1,null@a,null@a"),
        ErrorKind::DuplicateInstanceName(_)
    ));
}

#[test]
fn labels_join_chains_and_forward_references_work() {
    // ffmpeg -i in.mp4 -filter_complex "[a]hflip[out];[0:v]null[a]" -map "[out]"
    //   -> accepted, so a label may be used before it is defined.
    let built = build("[a]null[out];counter=n=1,null[a]");
    assert_eq!(built.graph.links().len(), 2);
    assert_eq!(
        built
            .open_outputs
            .iter()
            .map(|p| p.label.clone())
            .collect::<Vec<_>>(),
        [Some("out".to_owned())]
    );
    assert!(built.open_inputs.is_empty());
}

#[test]
fn a_labelled_output_leaves_the_next_filter_input_open() {
    // ffmpeg -f lavfi -i "testsrc2=…,split[a][b],hflip"
    //   -> Open inputs in the filtergraph are not acceptable
    let built = build("counter=n=1,split[a][b],null");
    assert_eq!(built.open_inputs.len(), 1);
    assert_eq!(built.open_inputs[0].label, None);
    // `[a]`, `[b]`, and the unlabelled output of the trailing `null`.
    assert_eq!(built.open_outputs.len(), 3);
}

#[test]
fn labelled_inputs_take_the_first_pads_and_the_carried_stream_fills_the_rest() {
    // Measured, because the ordering is not obvious:
    //   "color=c=red:s=64x64[x];color=c=blue:s=8x8,[x]overlay"
    //   -> the output is 64x64, so `[x]` took overlay's *main* (pad 0).
    let built = build("counter=n=1[x];counter=n=1,[x]merge=inputs=2");
    let merge = built.nodes.last().unwrap().id;
    let mut from = Vec::new();
    for link in built.graph.links().iter() {
        if link.dst().node == merge {
            from.push((link.dst().pad, link.src().node));
        }
    }
    from.sort_unstable();
    assert_eq!(from.len(), 2);
    // pad 0 <- the labelled `[x]` chain (node 0), pad 1 <- the carried one.
    assert_eq!(from[0].1, built.nodes[0].id);
    assert_eq!(from[1].1, built.nodes[1].id);
}

#[test]
fn a_second_definition_of_an_output_label_is_an_error() {
    assert!(matches!(
        build_err("counter=n=1[v];counter=n=1[v];[v]null"),
        ErrorKind::DuplicateOutputLabel { .. }
    ));
}

#[test]
fn a_label_consumed_twice_stays_open_as_the_reference_leaves_it() {
    // ffmpeg -filter_complex "[0:v]null[a];[a]hflip[out];[a]null[out2]"
    //   -> Stream specifier 'a' … matches no streams
    // i.e. the graph parser does not reject it; the second `[a]` is left as an
    // unresolved *input* for the caller to bind or complain about.
    let built = build("counter=n=1[a];[a]null[x];[a]null[y]");
    let open: Vec<Option<String>> = built.open_inputs.iter().map(|p| p.label.clone()).collect();
    assert_eq!(open, [Some("a".to_owned())]);
}

#[test]
fn too_many_labels_for_the_pads_a_filter_has() {
    // ffmpeg -f lavfi -i "color=…[a][a2]"
    //   -> More output link labels specified for filter 'null' than it has outputs: 2 > 1
    assert!(matches!(
        build_err("counter=n=1,null[a][b];[a]null;[b]null"),
        ErrorKind::TooManyOutputLabels {
            given: 2,
            has: 1,
            ..
        }
    ));
    assert!(matches!(
        build_err("counter=n=1[a];[a][a]null"),
        ErrorKind::TooManyInputLabels {
            given: 2,
            has: 1,
            ..
        }
    ));
}

#[test]
fn dynamic_pad_counts_come_from_the_arguments() {
    let built = build("counter=n=1,split=outputs=4[a][b][c][d];[a]null;[b]null;[c]null;[d]null");
    assert_eq!(built.graph.links().len(), 5);
    assert!(matches!(
        build_err("counter=n=1,split=outputs=0"),
        ErrorKind::Filter { .. }
    ));
    assert!(matches!(
        build_err("counter=n=1,split=outputs=nine"),
        ErrorKind::Filter { .. }
    ));
}

#[test]
fn an_unknown_filter_is_named_and_a_near_miss_suggested() {
    match build_err("counter=n=1,spilt") {
        ErrorKind::UnknownFilter { name, suggestion } => {
            assert_eq!(name, "spilt");
            assert_eq!(suggestion.as_deref(), Some("split"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_media_type_mismatch_is_diagnosed_when_the_link_is_made() {
    // ffmpeg -f lavfi -i "sine=d=0.04,hflip"
    //   -> Media type mismatch between the 'Parsed_sine_0' filter output pad 0
    //      (video) and the 'Parsed_hflip_1' filter input pad 0 (audio)
    assert!(matches!(
        build_err("counter=n=1,anull"),
        ErrorKind::MediaMismatch { .. }
    ));
}

#[test]
fn a_label_that_feeds_itself_is_a_cycle() {
    assert!(matches!(build_err("[a]null[a]"), ErrorKind::Cycle(_)));
}

#[test]
fn a_graph_runs_to_completion_and_conserves_frames() {
    for n in [0usize, 1, 3, 17] {
        let (_, counts) = run("null,invert,null", AutoConvert::None, n);
        assert_eq!(counts, [n], "n={n}");
    }
}

#[test]
fn split_feeds_every_branch() {
    let (_, counts) = run("split[a][b];[a]null;[b]invert", AutoConvert::None, 5);
    assert_eq!(counts, [5, 5]);
}

#[test]
fn a_generator_behind_a_filter_does_not_yet_start() {
    // A limitation of `vaco-filter-core`'s scheduler, pinned here so that
    // fixing it fails this test rather than passing silently (the D17.1 rule-3
    // pattern). `Graph::score` gives a filter no priority while its inputs are
    // empty, and `request_inputs` runs only when a filter *activates*, so a
    // request never travels back through an idle filter. Every graph in that
    // crate's tests is headed by a buffer source, where frames push demand
    // forward, so nothing exercised it. Reproduced with its own mocks:
    //
    //   Counter -> Invert -> sink   ->   Deadlock, zero frames
    //
    // Reported, not worked around: the fix belongs in `sched.rs`.
    let mut built = build("counter=n=3,null");
    built.attach_sink(0, any_sink("out")).unwrap();
    built
        .configure(&MockRegistry::new(), AutoConvert::None)
        .unwrap();
    let _ = built.graph.run().unwrap();
    assert!(matches!(
        built.graph.classify(),
        vaco_filter_core::GraphStatus::Deadlock(_)
    ));
}

#[test]
fn a_generator_wired_straight_to_a_sink_does_run() {
    let mut built = build("counter=n=3");
    let sink = built.attach_sink(0, any_sink("out")).unwrap();
    built
        .configure(&MockRegistry::new(), AutoConvert::None)
        .unwrap();
    let mut n = 0;
    for _ in 0..50 {
        built.graph.run().unwrap();
        match built.graph.recv(sink) {
            Ok(_) => n += 1,
            Err(vaco_core::Error::Eof) => break,
            _ => {}
        }
    }
    assert_eq!(n, 3);
}

#[test]
fn auto_conversion_repairs_a_link_the_two_sides_cannot_agree_on() {
    // The buffer source declares gray8 and `format=pix_fmts=rgb24` accepts only
    // rgb24, so the graph cannot negotiate as written.
    let src = "format=pix_fmts=rgb24,null";
    let mut built = build(src);
    built
        .attach_source(0, gray_source_formats("in"), gray_link())
        .unwrap();
    built.attach_sink(0, any_sink("out")).unwrap();
    assert!(
        built
            .configure(&MockRegistry::new(), AutoConvert::None)
            .is_err(),
        "-noauto_conversion_filters should refuse this graph"
    );

    let (built, counts) = run(src, AutoConvert::All, 3);
    assert_eq!(counts, [3]);
    // The converter is named the way the reference names it, because scripts
    // grep for it: `auto_scale_0`, `auto_aresample_0`.
    let names: Vec<&str> = (0..built.graph.node_count())
        .map(|i| built.graph.label(vaco_filter_core::NodeId(i as u32)))
        .collect();
    assert!(names.contains(&"auto_scale_0"), "{names:?}");
}

#[test]
fn the_sws_prefix_reaches_the_auto_inserted_scale() {
    let built = build("sws_flags=bicubic+accurate_rnd;format=pix_fmts=rgb24");
    assert_eq!(built.sws_opts, "bicubic+accurate_rnd");
}

#[test]
fn attaching_a_source_closes_an_open_input() {
    let mut built = build("[in]null[out]");
    assert_eq!(built.open_inputs.len(), 1);
    let formats = NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(vaco_pixfmt::PixFmt::Gray8)],
        ties: Vec::new(),
        label: "in".into(),
    };
    let mut format = LinkFormat::unconfigured(MediaType::Video);
    if let LinkFormat::Video {
        width,
        height,
        time_base,
        format: f,
        ..
    } = &mut format
    {
        *width = 16;
        *height = 16;
        *time_base = vaco_core::Rational::new(1, 25);
        *f = vaco_pixfmt::PixFmt::Gray8;
    }
    let src = built.attach_source(0, formats, format).unwrap();
    built.attach_sink(0, any_sink("out")).unwrap();
    built
        .configure(&MockRegistry::new(), AutoConvert::All)
        .unwrap();
    assert!(built.open_inputs.is_empty());
    built
        .graph
        .close_source(src, vaco_core::Timestamp::ZERO)
        .unwrap();
    assert_eq!(built.graph.run().unwrap(), GraphStatus::Eof);
}

#[test]
fn introspection_renders_without_panicking() {
    let built = build("split[a][b];[a]null;[b]invert");
    assert!(built.to_dot().starts_with("digraph"));
    assert!(built.dump().contains("Parsed_split_0"));
}
