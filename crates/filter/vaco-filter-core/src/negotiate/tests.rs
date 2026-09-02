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

use super::*;
use vaco_chlayout::ChannelLayout;
use vaco_pixfmt::PixFmt as P;
use vaco_sampfmt::SampleFmt as S;

// ------------------------------------------------------------ Constraint

#[test]
fn any_is_the_identity_of_intersection() {
    let a: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p, P::Rgb24]);
    assert_eq!(Constraint::Any.intersect(&a), Some(a.clone()));
    assert_eq!(a.intersect(&Constraint::Any), Some(a));
}

#[test]
fn a_singleton_intersection_normalises_to_exact() {
    let a: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p, P::Rgb24]);
    let b: Constraint<P> = Constraint::OneOf(vec![P::Rgb24, P::Gbrp]);
    assert_eq!(a.intersect(&b), Some(Constraint::Exact(P::Rgb24)));
}

#[test]
fn preference_order_follows_the_left_operand() {
    let a: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p, P::Rgb24, P::Gbrp]);
    let b: Constraint<P> = Constraint::OneOf(vec![P::Gbrp, P::Rgb24, P::Yuv420p]);
    assert_eq!(
        a.intersect(&b),
        Some(Constraint::OneOf(vec![P::Yuv420p, P::Rgb24, P::Gbrp]))
    );
    assert_eq!(
        b.intersect(&a),
        Some(Constraint::OneOf(vec![P::Gbrp, P::Rgb24, P::Yuv420p]))
    );
}

#[test]
fn disjoint_lists_have_no_intersection() {
    let a: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p]);
    let b: Constraint<P> = Constraint::OneOf(vec![P::Rgb24]);
    assert_eq!(a.intersect(&b), None);
}

#[test]
fn exact_against_a_list_keeps_the_exact() {
    let e: Constraint<P> = Constraint::Exact(P::Rgb24);
    let l: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p, P::Rgb24]);
    assert_eq!(e.intersect(&l), Some(Constraint::Exact(P::Rgb24)));
    assert_eq!(l.intersect(&e), Some(Constraint::Exact(P::Rgb24)));
    assert_eq!(
        Constraint::Exact(P::Gbrp).intersect(&l),
        None,
        "exact outside the list is a conflict"
    );
}

#[test]
fn intersection_is_idempotent() {
    let a: Constraint<P> = Constraint::OneOf(vec![P::Yuv420p, P::Rgb24]);
    assert_eq!(a.intersect(&a), Some(a.clone()));
}

// ------------------------------------------------------------ FormatSet

#[test]
fn an_absent_property_takes_the_other_sides_value() {
    let a = FormatSet::video_exact(P::Rgb24);
    let b = FormatSet::default();
    assert_eq!(a.intersect(&b), Some(a.clone()));
    assert_eq!(b.intersect(&a), Some(a));
}

#[test]
fn intersect_detailed_names_the_failing_property() {
    let a = FormatSet::audio_exact(S::S16, 48_000, ChannelLayout::STEREO);
    let b = FormatSet::audio_exact(S::S16, 44_100, ChannelLayout::STEREO);
    assert_eq!(a.intersect_detailed(&b), Err(Property::SampleRate));

    let c = FormatSet::audio_exact(S::F32, 48_000, ChannelLayout::MONO);
    // Properties are tested in Property::ALL order, so sample format is named
    // before channel layout even though both conflict.
    assert_eq!(a.intersect_detailed(&c), Err(Property::SampleFormat));
}

#[test]
fn the_frozen_intersect_reports_failure_as_none() {
    let a = FormatSet::video_exact(P::Rgb24);
    let b = FormatSet::video_exact(P::Yuv420p);
    assert_eq!(a.intersect(&b), None);
}

// ------------------------------------------------------------ the solver

fn plan_chain(sets: &[FormatSet], media: MediaType) -> NegotiationPlan {
    let mut plan = NegotiationPlan::new();
    for (i, set) in sets.iter().enumerate() {
        let node = if i == 0 {
            NodeFormats {
                inputs: Vec::new(),
                outputs: vec![set.clone()],
                ties: Vec::new(),
                label: format!("n{i}"),
            }
        } else if i == sets.len() - 1 {
            NodeFormats {
                inputs: vec![set.clone()],
                outputs: Vec::new(),
                ties: Vec::new(),
                label: format!("n{i}"),
            }
        } else {
            NodeFormats::uniform(1, 1, media, set, &format!("n{i}"))
        };
        plan.add_node(node);
    }
    for i in 0..sets.len().saturating_sub(1) {
        plan.connect(
            PadRef::output(NodeId(i as u32), 0),
            PadRef::input(NodeId(i as u32 + 1), 0),
            media,
        )
        .expect("valid pads");
    }
    plan
}

fn solve(plan: &mut NegotiationPlan) -> Result<Assignment> {
    let mut conflicts = Vec::new();
    negotiate(plan, &NoConversion, AutoConvert::None, &mut conflicts)
}

fn pix(assignment: &Assignment, link: u32) -> Option<P> {
    assignment
        .link(LinkId(link))?
        .pixel_formats
        .as_ref()?
        .resolved()
        .copied()
}

#[test]
fn a_passthrough_chain_takes_the_sources_format() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::default(),
            FormatSet::default(),
        ],
        MediaType::Video,
    );
    let a = solve(&mut plan).expect("negotiates");
    assert_eq!(pix(&a, 0), Some(P::Gray8));
    assert_eq!(pix(&a, 1), Some(P::Gray8));
    assert_eq!(a.rounds, 0);
    assert!(a.inserted.is_empty());
}

#[test]
fn a_narrowing_filter_propagates_backwards_through_a_tie() {
    // source accepts three, middle accepts two, sink accepts one. The whole
    // chain must land on the one, including the link *upstream* of the sink.
    let mut plan = plan_chain(
        &[
            FormatSet::video_list([P::Yuv420p, P::Rgb24, P::Gbrp]),
            FormatSet::video_list([P::Rgb24, P::Gbrp]),
            FormatSet::video_exact(P::Gbrp),
        ],
        MediaType::Video,
    );
    let a = solve(&mut plan).expect("negotiates");
    assert_eq!(pix(&a, 0), Some(P::Gbrp));
    assert_eq!(pix(&a, 1), Some(P::Gbrp));
}

#[test]
fn the_upstream_most_preference_wins_among_equals() {
    // Both sides accept both; the source's order decides, which is what makes
    // "already agreed, so do not convert" the outcome in a real graph.
    let mut plan = plan_chain(
        &[
            FormatSet::video_list([P::Yuv420p, P::Rgb24]),
            FormatSet::video_list([P::Rgb24, P::Yuv420p]),
        ],
        MediaType::Video,
    );
    let a = solve(&mut plan).expect("negotiates");
    assert_eq!(pix(&a, 0), Some(P::Yuv420p));
}

#[test]
fn no_common_format_and_no_converter_is_unsupported() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::video_exact(P::Rgb24),
        ],
        MediaType::Video,
    );
    let mut conflicts = Vec::new();
    let e = negotiate(&mut plan, &NoConversion, AutoConvert::None, &mut conflicts);
    assert!(matches!(e, Err(Error::Unsupported(_))));
    assert_eq!(conflicts.len(), 1);
    let c = conflicts.first().expect("one conflict");
    assert_eq!(c.link, LinkId(0));
    assert_eq!(c.property, Property::PixelFormat);
    assert!(c.auto_convert_disabled);
    assert_eq!(c.upstream.accepts, vec!["gray".to_owned()]);
    assert_eq!(c.downstream.accepts, vec!["rgb24".to_owned()]);
}

#[test]
fn the_diagnostic_names_the_narrowing_nodes_and_a_fix() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::default(),
            FormatSet::video_exact(P::Rgb24),
        ],
        MediaType::Video,
    );
    let mut conflicts = Vec::new();
    let _ = negotiate(&mut plan, &NoConversion, AutoConvert::None, &mut conflicts);
    let rendered = conflicts.first().expect("a conflict").render();
    // The narrowing node is n0, not the link's own upstream endpoint n1 — which
    // is exactly what the reference cannot tell you.
    assert!(rendered.contains("narrowed by   n0"), "{rendered}");
    assert!(rendered.contains("narrowed by   n2"), "{rendered}");
    assert!(rendered.contains("fix:"), "{rendered}");
    assert!(rendered.contains("pix_fmt"), "{rendered}");
}

#[test]
fn a_node_that_ties_incompatible_pads_of_its_own_is_invalid_data() {
    let mut plan = NegotiationPlan::new();
    plan.add_node(NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(P::Gray8)],
        ties: Vec::new(),
        label: "src".into(),
    });
    plan.add_node(NodeFormats {
        inputs: vec![FormatSet::video_exact(P::Gray8)],
        outputs: vec![FormatSet::video_exact(P::Rgb24)],
        // Contradictory: says the two pads must agree, but they cannot.
        ties: Tie::all_pads(1, 1, MediaType::Video),
        label: "bad".into(),
    });
    plan.connect(
        PadRef::output(NodeId(0), 0),
        PadRef::input(NodeId(1), 0),
        MediaType::Video,
    )
    .expect("valid");
    assert!(matches!(solve(&mut plan), Err(Error::InvalidData(_))));
}

#[test]
fn an_entirely_unconstrained_class_is_refused_rather_than_guessed() {
    let mut plan = plan_chain(
        &[FormatSet::default(), FormatSet::default()],
        MediaType::Video,
    );
    assert!(matches!(solve(&mut plan), Err(Error::Unsupported(_))));
}

// ------------------------------------------------------------ conversion

/// A factory that converts anything to the downstream's first choice.
struct ToDownstream;

impl ConverterFactory for ToDownstream {
    fn converter(
        &self,
        media: MediaType,
        _properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        let filter = if media == MediaType::Audio {
            "aresample"
        } else {
            "scale"
        };
        // The choice of *which* downstream format to produce is the policy that
        // `loss` scores. Here: cheapest, which is what the reference does.
        let out = if media == MediaType::Video {
            let from = upstream.pixel_formats.as_ref()?.resolved().copied()?;
            let candidates = downstream.pixel_formats.as_ref()?.candidates();
            FormatSet::video_exact(loss::best_video(from, candidates)?)
        } else {
            downstream.clone()
        };
        Some(ConverterSpec {
            filter,
            args: String::new(),
            formats: NodeFormats::converter(upstream.clone(), out, "auto"),
        })
    }
}

#[test]
fn a_converter_is_spliced_into_the_offending_link() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::video_exact(P::Rgb24),
        ],
        MediaType::Video,
    );
    let mut conflicts = Vec::new();
    let a =
        negotiate(&mut plan, &ToDownstream, AutoConvert::All, &mut conflicts).expect("converts");
    assert!(conflicts.is_empty());
    assert_eq!(a.inserted.len(), 1);
    let ins = a.inserted.first().expect("one insertion");
    // The reference names them `auto_scale_N` and scripts grep for it.
    assert_eq!(ins.name, "auto_scale_0");
    assert_eq!(ins.filter, "scale");
    assert_eq!(ins.properties, vec![Property::PixelFormat]);
    // Existing link ids stay valid: link 0 now ends at the converter, and the
    // new tail is appended.
    assert_eq!(plan.links().len(), 2);
    assert_eq!(pix(&a, 0), Some(P::Gray8));
    assert_eq!(pix(&a, 1), Some(P::Rgb24));
    assert_eq!(a.rounds, 1);
}

#[test]
fn one_converter_fixes_every_property_of_a_link_at_once() {
    // Sample format, rate and layout all conflict. Coalescing means one
    // `aresample`, not three stacked ones.
    let mut plan = plan_chain(
        &[
            FormatSet::audio_exact(S::S16, 44_100, ChannelLayout::MONO),
            FormatSet::audio_exact(S::F32, 48_000, ChannelLayout::STEREO),
        ],
        MediaType::Audio,
    );
    let mut conflicts = Vec::new();
    let a =
        negotiate(&mut plan, &ToDownstream, AutoConvert::All, &mut conflicts).expect("converts");
    assert_eq!(a.inserted.len(), 1, "one converter, not three");
    let ins = a.inserted.first().expect("one insertion");
    assert_eq!(ins.filter, "aresample");
    assert_eq!(ins.properties.len(), 3);
}

#[test]
fn a_factory_that_refuses_produces_the_conflict_rather_than_looping() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::video_exact(P::Rgb24),
        ],
        MediaType::Video,
    );
    let mut conflicts = Vec::new();
    let e = negotiate(&mut plan, &NoConversion, AutoConvert::All, &mut conflicts);
    assert!(matches!(e, Err(Error::Unsupported(_))));
    assert_eq!(conflicts.len(), 1);
    assert!(
        !conflicts[0].auto_convert_disabled,
        "auto-conversion was on; the factory simply had nothing to offer"
    );
}

/// A deliberately broken factory: it inserts a converter that does not fix the
/// conflict. The round bound has to catch it rather than looping forever.
struct Useless;

impl ConverterFactory for Useless {
    fn converter(
        &self,
        _media: MediaType,
        _properties: &[Property],
        upstream: &FormatSet,
        _downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        Some(ConverterSpec {
            filter: "scale",
            args: String::new(),
            // Output equals input: the downstream link still conflicts.
            formats: NodeFormats::converter(upstream.clone(), upstream.clone(), "useless"),
        })
    }
}

#[test]
fn a_converter_that_does_not_converge_is_a_bounded_error() {
    let mut plan = plan_chain(
        &[
            FormatSet::video_exact(P::Gray8),
            FormatSet::video_exact(P::Rgb24),
        ],
        MediaType::Video,
    );
    let mut conflicts = Vec::new();
    let e = negotiate(&mut plan, &Useless, AutoConvert::All, &mut conflicts);
    assert!(
        matches!(e, Err(Error::Unsupported(_))),
        "the round bound must fire rather than the loop running away"
    );
}

#[test]
fn negotiation_is_deterministic() {
    let build = || {
        plan_chain(
            &[
                FormatSet::video_list([P::Yuv420p, P::Rgb24, P::Gbrp]),
                FormatSet::video_list([P::Gbrp, P::Rgb24]),
                FormatSet::video_list([P::Rgb24, P::Gbrp]),
            ],
            MediaType::Video,
        )
    };
    let mut first = build();
    let mut second = build();
    let a = solve(&mut first).expect("negotiates");
    let b = solve(&mut second).expect("negotiates");
    assert_eq!(pix(&a, 0), pix(&b, 0));
    assert_eq!(pix(&a, 1), pix(&b, 1));
}

// ------------------------------------------------------------ plan shape

#[test]
fn connect_rejects_a_backwards_link() {
    let mut plan = NegotiationPlan::new();
    plan.add_node(NodeFormats::passthrough(1, 1, MediaType::Video, "a"));
    plan.add_node(NodeFormats::passthrough(1, 1, MediaType::Video, "b"));
    let e = plan.connect(
        PadRef::input(NodeId(0), 0),
        PadRef::input(NodeId(1), 0),
        MediaType::Video,
    );
    assert!(matches!(e, Err(Error::InvalidData(_))));
}

#[test]
fn connect_rejects_an_absent_pad() {
    let mut plan = NegotiationPlan::new();
    plan.add_node(NodeFormats::passthrough(1, 1, MediaType::Video, "a"));
    let e = plan.connect(
        PadRef::output(NodeId(0), 7),
        PadRef::input(NodeId(0), 0),
        MediaType::Video,
    );
    assert!(matches!(e, Err(Error::InvalidData(_))));
}

#[test]
fn splice_rejects_a_converter_with_the_wrong_pad_count() {
    let mut plan = plan_chain(
        &[FormatSet::default(), FormatSet::default()],
        MediaType::Video,
    );
    let e = plan.splice(
        LinkId(0),
        NodeFormats::passthrough(2, 1, MediaType::Video, "wide"),
    );
    assert!(matches!(e, Err(Error::InvalidData(_))));
}

#[test]
fn tie_all_pads_is_empty_for_a_single_pad() {
    assert!(Tie::all_pads(1, 0, MediaType::Video).is_empty());
    assert_eq!(Tie::all_pads(1, 1, MediaType::Video).len(), 1);
    assert_eq!(Tie::all_pads(1, 1, MediaType::Audio).len(), 3);
    assert!(Tie::all_pads(1, 1, MediaType::Data).is_empty());
}

#[test]
fn property_names_are_the_references_own_spellings() {
    assert_eq!(Property::PixelFormat.name(), "pix_fmt");
    assert_eq!(Property::SampleFormat.name(), "sample_fmt");
    assert_eq!(Property::SampleRate.name(), "sample_rate");
    assert_eq!(Property::ChannelLayout.name(), "channel_layout");
}

// ------------------------------------------------------------ tied, non-conflicting properties

/// A converter factory that records the `upstream`/`downstream` sets a
/// repair actually handed it, instead of computing anything from them.
struct Spy {
    seen: std::cell::RefCell<Option<(FormatSet, FormatSet)>>,
}

impl Spy {
    fn new() -> Self {
        Self {
            seen: std::cell::RefCell::new(None),
        }
    }
}

impl ConverterFactory for Spy {
    fn converter(
        &self,
        _media: MediaType,
        _properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        *self.seen.borrow_mut() = Some((upstream.clone(), downstream.clone()));
        // A converter that forces every property downstream leaves open —
        // exactly `aresample`'s own shape when the user asked for a rate
        // change and nothing else.
        let mut out = downstream.clone();
        if out
            .sample_formats
            .as_ref()
            .is_none_or(|c| matches!(c, Constraint::Any))
        {
            out.sample_formats = upstream.sample_formats.clone();
        }
        if out
            .channel_layouts
            .as_ref()
            .is_none_or(|c| matches!(c, Constraint::Any))
        {
            out.channel_layouts = upstream.channel_layouts.clone();
        }
        Some(ConverterSpec {
            filter: "aresample",
            args: String::new(),
            formats: NodeFormats::converter(upstream.clone(), out, "auto"),
        })
    }
}

/// A fully-tied passthrough node — `anull`'s own shape, and
/// `aresample`'s shape for whichever properties the user did not ask it to
/// change (see the tie fix in `vaco_filter_audio::aresample::target_formats`).
/// Both pads declare nothing on their own; every property is resolved only
/// through the tie, from whatever the source side turns out to be.
fn tied_passthrough_node(label: &str) -> NodeFormats {
    NodeFormats::passthrough(1, 1, MediaType::Audio, label)
}

/// The regression for a real defect: a link whose sample rate conflicts
/// (forcing a converter insertion) sat next to a channel layout that never
/// conflicted at all — resolved cleanly via the middle node's own tie — and
/// the repair loop handed the factory `None` for it anyway, because it only
/// overlaid the *conflicting* property's resolved value onto `upstream`/
/// `downstream`, not every property a tie had already settled. The
/// mis-declared converter that produced then had no channel-layout
/// constraint on its own output pad, and the *next* link (to an equally
/// unconstrained sink) failed negotiation outright with "format negotiation
/// left a property unconstrained" for a property nothing on either side ever
/// disputed.
#[test]
fn a_converters_upstream_carries_a_tied_propertys_resolved_value_not_none() {
    let mut plan = NegotiationPlan::new();
    let source = plan.add_node(NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::audio_exact(S::S16, 48_000, ChannelLayout::MONO)],
        ties: Vec::new(),
        label: "source".to_owned(),
    });
    let middle = plan.add_node(tied_passthrough_node("anull"));
    let sink = plan.add_node(NodeFormats {
        inputs: vec![FormatSet {
            sample_rates: Some(Constraint::Exact(44_100)),
            ..FormatSet::default()
        }],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: "sink".to_owned(),
    });
    plan.connect(
        PadRef::output(source, 0),
        PadRef::input(middle, 0),
        MediaType::Audio,
    )
    .expect("connect");
    plan.connect(
        PadRef::output(middle, 0),
        PadRef::input(sink, 0),
        MediaType::Audio,
    )
    .expect("connect");

    let spy = Spy::new();
    let mut conflicts = Vec::new();
    let assignment = negotiate(&mut plan, &spy, AutoConvert::All, &mut conflicts)
        .expect("a tied, non-conflicting property must not block negotiation");
    assert!(conflicts.is_empty());
    assert_eq!(assignment.inserted.len(), 1);

    let (upstream, _downstream) = spy.seen.into_inner().expect("the factory was called");
    // The channel layout never conflicted — it is what this test is
    // guarding — so the factory must have seen it resolved to `MONO`
    // (propagated through the middle node's tie), not `None`/`Any`.
    assert_eq!(
        upstream.channel_layouts,
        Some(Constraint::Exact(ChannelLayout::MONO)),
        "a non-conflicting, tied property must reach the converter factory resolved"
    );
    assert_eq!(upstream.sample_formats, Some(Constraint::Exact(S::S16)));
}
