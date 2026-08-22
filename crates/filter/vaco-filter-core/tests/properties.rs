//! Properties, not examples.
//!
//! Each of these states an invariant the framework has to hold for *every*
//! input, not for the handful a named test happens to pick. The negotiation
//! ones matter most: plan 13 §3.2 asks specifically that "format negotiation
//! either succeeds or reports an incompatibility (never loops)", and that is
//! not something a fixed case can establish.

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

use proptest::prelude::*;
use vaco_core::{Error, MediaType, Rational, Timestamp};
use vaco_filter_core::link::{Direction, Link, PadRef, Status};
use vaco_filter_core::mock::{
    Fps, Invert, any_video_sink, gray_frame, gray_link, video_source_formats,
};
use vaco_filter_core::negotiate::{
    AutoConvert, Constraint, ConverterFactory, ConverterSpec, FormatSet, NegotiationPlan,
    NoConversion, NodeFormats, Property, loss, negotiate,
};
use vaco_filter_core::{Graph, LinkId, NodeId};
use vaco_pixfmt::PixFmt;

/// A small, fixed palette. Negotiation cares about set structure, not about
/// which 268 formats exist, and a small alphabet makes collisions — the
/// interesting cases — actually happen.
const PALETTE: [PixFmt; 6] = [
    PixFmt::Gray8,
    PixFmt::Yuv420p,
    PixFmt::Yuv422p,
    PixFmt::Yuv444p,
    PixFmt::Rgb24,
    PixFmt::Gbrp,
];

fn constraint() -> impl Strategy<Value = Constraint<PixFmt>> {
    prop_oneof![
        1 => Just(Constraint::Any),
        2 => (0usize..PALETTE.len()).prop_map(|i| Constraint::Exact(PALETTE[i])),
        4 => proptest::collection::vec(0usize..PALETTE.len(), 1..=6).prop_map(|idx| {
            let mut v: Vec<PixFmt> = Vec::new();
            for i in idx {
                if !v.contains(&PALETTE[i]) {
                    v.push(PALETTE[i]);
                }
            }
            Constraint::OneOf(v).normalised()
        }),
    ]
}

fn members(c: &Constraint<PixFmt>) -> Vec<PixFmt> {
    match c {
        Constraint::Any => PALETTE.to_vec(),
        other => other.candidates().to_vec(),
    }
}

proptest! {
    /// Membership is set intersection. Order is not — that is `self`'s — but
    /// *which* formats survive must not depend on which side you start from, or
    /// the fold order in the solver would change the answer rather than merely
    /// the preference.
    #[test]
    fn intersection_membership_is_commutative(a in constraint(), b in constraint()) {
        let ab = a.intersect(&b);
        let ba = b.intersect(&a);
        match (ab, ba) {
            (Some(x), Some(y)) => {
                let mut mx = members(&x);
                let mut my = members(&y);
                mx.sort_unstable();
                my.sort_unstable();
                prop_assert_eq!(mx, my);
            }
            (None, None) => {}
            (x, y) => prop_assert!(false, "asymmetric: {:?} vs {:?}", x, y),
        }
    }

    #[test]
    fn intersection_is_idempotent(a in constraint()) {
        let once = a.intersect(&a).expect("a set always meets itself");
        let mut m = members(&once);
        let mut n = members(&a);
        m.sort_unstable();
        n.sort_unstable();
        prop_assert_eq!(m, n);
    }

    /// Every surviving format was accepted by both sides, and nothing that both
    /// sides accepted was dropped.
    #[test]
    fn intersection_is_exactly_the_shared_members(a in constraint(), b in constraint()) {
        let ma = members(&a);
        let mb = members(&b);
        let shared: Vec<PixFmt> = ma.iter().copied().filter(|f| mb.contains(f)).collect();
        match a.intersect(&b) {
            Some(c) => {
                let mut got = members(&c);
                let mut want = shared;
                got.sort_unstable();
                want.sort_unstable();
                prop_assert_eq!(got, want);
            }
            None => prop_assert!(shared.is_empty()),
        }
    }

    /// `Any` is the identity, in both positions.
    #[test]
    fn any_is_the_identity(a in constraint()) {
        prop_assert_eq!(Constraint::Any.intersect(&a), Some(a.clone()));
        prop_assert_eq!(a.intersect(&Constraint::Any), Some(a));
    }
}

/// Build a passthrough chain of `n` nodes whose pads carry `sets`.
fn build_chain(sets: &[Constraint<PixFmt>]) -> NegotiationPlan {
    let mut plan = NegotiationPlan::new();
    for (i, c) in sets.iter().enumerate() {
        let set = FormatSet {
            pixel_formats: Some(c.clone()),
            ..FormatSet::default()
        };
        let node = if i == 0 {
            NodeFormats {
                inputs: Vec::new(),
                outputs: vec![set],
                ties: Vec::new(),
                label: format!("n{i}"),
            }
        } else if i == sets.len() - 1 {
            NodeFormats {
                inputs: vec![set],
                outputs: Vec::new(),
                ties: Vec::new(),
                label: format!("n{i}"),
            }
        } else {
            NodeFormats::uniform(1, 1, MediaType::Video, &set, &format!("n{i}"))
        };
        plan.add_node(node);
    }
    for i in 0..sets.len().saturating_sub(1) {
        plan.connect(
            PadRef::output(NodeId(i as u32), 0),
            PadRef::input(NodeId(i as u32 + 1), 0),
            MediaType::Video,
        )
        .expect("valid pads");
    }
    plan
}

/// Converts to the cheapest format the downstream accepts.
struct Cheapest;

impl ConverterFactory for Cheapest {
    fn converter(
        &self,
        _media: MediaType,
        _properties: &[Property],
        upstream: &FormatSet,
        downstream: &FormatSet,
    ) -> Option<ConverterSpec> {
        let from = *upstream.pixel_formats.as_ref()?.resolved()?;
        let candidates = downstream.pixel_formats.as_ref()?.candidates();
        let to = loss::best_video(from, candidates)?;
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// The headline property: negotiation terminates, on every graph, with
    /// either a complete assignment or a reported incompatibility. Never a loop,
    /// never a panic, never a partial answer.
    #[test]
    fn negotiation_terminates_and_is_total(
        sets in proptest::collection::vec(constraint(), 2..=6)
    ) {
        let mut plan = build_chain(&sets);
        let links = plan.links().len();
        let mut conflicts = Vec::new();
        match negotiate(&mut plan, &NoConversion, AutoConvert::None, &mut conflicts) {
            Ok(a) => {
                prop_assert_eq!(a.links.len(), links);
                for set in &a.links {
                    let c = set.pixel_formats.as_ref();
                    prop_assert!(
                        c.and_then(Constraint::resolved).is_some(),
                        "a link was left unresolved: {:?}", set
                    );
                }
            }
            Err(Error::Unsupported(_)) => {
                // Either a real conflict, reported, or the graph never said what
                // it carries. Both are answers.
                prop_assert!(conflicts.len() <= links);
            }
            Err(e) => prop_assert!(false, "unexpected error {:?}", e),
        }
    }

    /// The value chosen for every link satisfies every pad in that link's class.
    #[test]
    fn every_assignment_satisfies_every_constraint_on_its_link(
        sets in proptest::collection::vec(constraint(), 2..=6)
    ) {
        let mut plan = build_chain(&sets);
        let mut conflicts = Vec::new();
        let Ok(a) = negotiate(&mut plan, &NoConversion, AutoConvert::None, &mut conflicts) else {
            return Ok(());
        };
        for (i, ends) in plan.links().iter().enumerate() {
            let chosen = *a
                .link(LinkId(i as u32))
                .and_then(|s| s.pixel_formats.as_ref())
                .and_then(Constraint::resolved)
                .expect("resolved");
            for pad in [ends.src, ends.dst] {
                let node = &plan.nodes()[pad.node.0 as usize];
                let set = match pad.direction {
                    Direction::Input => &node.inputs[pad.pad as usize],
                    Direction::Output => &node.outputs[pad.pad as usize],
                };
                if let Some(c) = set.pixel_formats.as_ref() {
                    prop_assert!(
                        c.allows(&chosen),
                        "{:?} rejects the chosen {:?}", c, chosen
                    );
                }
            }
        }
    }

    /// With conversion enabled, a chain of *concrete* formats always negotiates:
    /// every link is either already agreed or repaired by exactly one converter.
    #[test]
    fn concrete_chains_always_negotiate_with_conversion(
        idx in proptest::collection::vec(0usize..PALETTE.len(), 2..=6)
    ) {
        let sets: Vec<Constraint<PixFmt>> =
            idx.iter().map(|&i| Constraint::Exact(PALETTE[i])).collect();
        let mut plan = build_chain(&sets);
        let before = plan.links().len();
        let mut conflicts = Vec::new();
        let a = negotiate(&mut plan, &Cheapest, AutoConvert::All, &mut conflicts)
            .expect("concrete formats are always convertible");
        prop_assert!(conflicts.is_empty());
        // One converter per genuinely differing adjacent pair, and no more.
        let differing = idx.windows(2).filter(|w| w[0] != w[1]).count();
        prop_assert_eq!(a.inserted.len(), differing);
        prop_assert_eq!(plan.links().len(), before + differing);
    }

    /// Negotiation is a pure function of the plan. Two identical inputs must
    /// give byte-identical answers, or D6's differential harness is meaningless
    /// for filtergraphs.
    #[test]
    fn negotiation_is_deterministic(
        sets in proptest::collection::vec(constraint(), 2..=5)
    ) {
        let mut a = build_chain(&sets);
        let mut b = build_chain(&sets);
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let ra = negotiate(&mut a, &Cheapest, AutoConvert::All, &mut ca);
        let rb = negotiate(&mut b, &Cheapest, AutoConvert::All, &mut cb);
        match (ra, rb) {
            (Ok(x), Ok(y)) => {
                prop_assert_eq!(x.links, y.links);
                prop_assert_eq!(x.inserted, y.inserted);
                prop_assert_eq!(x.rounds, y.rounds);
            }
            (Err(_), Err(_)) => {
                prop_assert_eq!(ca.len(), cb.len());
            }
            _ => prop_assert!(false, "one run succeeded and the other did not"),
        }
    }
}

// ------------------------------------------------------------------- links

#[derive(Debug, Clone, Copy)]
enum Op {
    Push,
    Pop,
    Close,
    PopStatus,
    CheckEof,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Push),
        Just(Op::Pop),
        Just(Op::Close),
        Just(Op::PopStatus),
        Just(Op::CheckEof),
    ]
}

proptest! {
    /// End of stream is sticky and ordered behind the queue, for every call
    /// sequence — not just the ones a named test thought of. This is F2, the
    /// rule `vaco-format-core` had to learn twice.
    #[test]
    fn end_of_stream_is_sticky_and_ordered(
        ops in proptest::collection::vec(op(), 1..64),
        capacity in 1usize..8,
    ) {
        let mut link = Link::new(
            PadRef::output(NodeId(0), 0),
            PadRef::input(NodeId(1), 0),
            MediaType::Video,
            MediaType::Video,
        )
        .expect("same media")
        .with_capacity(capacity);
        link.set_format(gray_link(16, 16, Rational::new(1, 25)));

        let mut pushed = 0i64;
        let mut popped = 0i64;
        let mut eof_seen = false;
        for o in ops {
            match o {
                Op::Push => {
                    if link.push(gray_frame(16, 16, pushed, 0)).is_ok() {
                        pushed += 1;
                    }
                }
                Op::Pop => {
                    if let Some(f) = link.pop() {
                        // Frames come out in the order they went in, always.
                        prop_assert_eq!(f.pts, Timestamp::new(popped));
                        popped += 1;
                    }
                }
                Op::Close => link.close(Status::Eof, Timestamp::new(pushed)),
                Op::PopStatus => {
                    if link.pop_status().is_some() {
                        prop_assert!(link.is_closed());
                        prop_assert_eq!(link.depth(), 0, "status must be behind the queue");
                    }
                }
                Op::CheckEof => {}
            }
            if link.at_eof() {
                eof_seen = true;
                prop_assert!(link.is_closed());
                prop_assert_eq!(link.depth(), 0);
            }
            // Sticky: it never goes back.
            prop_assert!(!eof_seen || link.at_eof());
        }
        // Nothing was invented and nothing vanished.
        prop_assert_eq!(popped + link.depth() as i64, pushed);
    }
}

// ------------------------------------------------------------- whole graph

fn run_chain(count: usize, target: Option<Rational>) -> Result<Vec<vaco_frame::Frame>, Error> {
    let mut graph = Graph::new().with_step_budget(100_000);
    let src = graph.add_source(
        "in",
        MediaType::Video,
        video_source_formats("in", PixFmt::Gray8),
    );
    let mid = match target {
        Some(t) => Fps::node(&mut graph, "fps", t),
        None => Invert::node(&mut graph, "invert"),
    };
    let sink = graph.add_sink("out", MediaType::Video, any_video_sink("out"));
    graph.connect(src, 0, mid, 0)?;
    graph.connect(mid, 0, sink, 0)?;
    graph.set_source_format(src, gray_link(16, 16, Rational::new(1, 25)))?;
    graph.configure()?;

    let mut out = Vec::new();
    let mut sent = 0usize;
    for _ in 0..(count * 8 + 64) {
        graph.run()?;
        loop {
            match graph.recv(sink) {
                Ok(f) => out.push(f),
                Err(Error::NeedMoreInput) => break,
                Err(Error::Eof) => {
                    assert!(graph.violations().is_empty(), "{:?}", graph.violations());
                    return Ok(out);
                }
                Err(e) => return Err(e),
            }
        }
        if sent < count {
            if graph
                .send(src, gray_frame(16, 16, sent as i64, (sent & 0xff) as u8))
                .is_ok()
            {
                sent += 1;
            }
        } else {
            graph.close_source(src, Timestamp::new(sent as i64))?;
        }
    }
    Err(Error::Unsupported("graph did not finish"))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// A 1:1 filter conserves frames exactly, for any stream length. Not one
    /// dropped at the tail, not one duplicated at a backpressure boundary.
    #[test]
    fn a_one_to_one_filter_conserves_frames(count in 0usize..40) {
        let out = run_chain(count, None).expect("runs");
        prop_assert_eq!(out.len(), count);
        for (i, f) in out.iter().enumerate() {
            prop_assert_eq!(f.pts, Timestamp::new(i as i64));
        }
    }

    /// Rate conversion produces strictly increasing timestamps in the output
    /// base, whatever the ratio, and never emits nothing from a non-empty
    /// stream.
    #[test]
    fn rate_conversion_is_monotonic_and_never_empty(
        count in 1usize..30,
        num in 1i32..60,
    ) {
        let target = Rational::new(num, 1);
        let out = run_chain(count, Some(target)).expect("runs");
        prop_assert!(!out.is_empty(), "{count} frames in, none out at {num} fps");
        let base = target.inverse();
        for (i, f) in out.iter().enumerate() {
            prop_assert_eq!(f.time_base, base);
            prop_assert_eq!(f.pts, Timestamp::new(i as i64));
        }
    }
}
