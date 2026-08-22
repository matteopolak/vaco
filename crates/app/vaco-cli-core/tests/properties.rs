//! Property tests for the invariants the unit tests can only sample.
//!
//! Three families:
//!
//! * **Round trips.** A parsed specifier renders to text that parses back to
//!   the same specifier. Same for `-map`. This is what stops a "clever"
//!   canonicalisation from quietly losing a field.
//! * **Totality.** No input, however hostile, panics. Argument vectors are
//!   untrusted (D6) and every lint that would let a panic through is denied, so
//!   this is checked rather than assumed.
//! * **Matcher agreement.** `matches` and `select` cannot disagree, selection is
//!   always a subsequence of container order, and an index token always yields
//!   at most one stream.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]
#![allow(
    clippy::float_cmp,
    reason = "these assert exact values, and an epsilon would hide the only \
              thing worth catching -- a wrong constant"
)]

use std::ffi::OsString;

use proptest::prelude::*;
use vaco_cli_core::{
    Disposition, Expression, MapSpec, MatchCtx, NumberLimits, OptionConstants, StreamInfo,
    StreamSpecifier, eval_option, ffmpeg, ffprobe, parse_number, split, strtod,
};
use vaco_core::MediaType;

/// Text drawn from the specifier alphabet, so the generator actually reaches
/// the interesting states instead of spending its budget on rejected garbage.
fn spec_text() -> impl Strategy<Value = String> {
    let token = prop::sample::select(vec![
        "v", "V", "a", "s", "d", "t", "u", "p:", "g:", "i:", "m:", "disp:", "#", ":", "0", "1",
        "10", "0x2", "010", "default", "forced", "+", "-", "x", "\\", "", "k", "99",
    ]);
    prop::collection::vec(token, 0..7).prop_map(|v| v.concat())
}

/// Anything at all, including things no specifier alphabet contains.
fn wild_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => spec_text(),
        1 => ".{0,24}",
    ]
}

fn stream_strategy() -> impl Strategy<Value = StreamInfo> {
    (
        prop::option::of(prop::sample::select(vec![
            MediaType::Video,
            MediaType::Audio,
            MediaType::Subtitle,
            MediaType::Data,
            MediaType::Attachment,
        ])),
        any::<i64>(),
        any::<bool>(),
        0u32..3,
        prop::sample::select(vec![
            Disposition::NONE,
            Disposition::DEFAULT,
            Disposition::ATTACHED_PIC,
            Disposition::DEFAULT | Disposition::FORCED,
        ]),
    )
        .prop_map(
            |(media_type, id, codec_known, dim, disposition)| StreamInfo {
                index: 0,
                id,
                media_type,
                disposition,
                tags: vaco_core::Dict::new(),
                codec_known,
                width: dim,
                height: dim,
                sample_rate: dim * 16_000,
            },
        )
}

fn ctx_streams() -> impl Strategy<Value = Vec<StreamInfo>> {
    prop::collection::vec(stream_strategy(), 0..6).prop_map(|mut v| {
        for (i, s) in v.iter_mut().enumerate() {
            s.index = u32::try_from(i).unwrap_or(u32::MAX);
        }
        v
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Parsing never panics and never hangs, whatever the bytes.
    #[test]
    fn specifier_parsing_is_total(s in wild_text()) {
        let _ = StreamSpecifier::parse(&s);
    }

    #[test]
    fn map_parsing_is_total(s in wild_text()) {
        let _ = MapSpec::parse(&s);
    }

    /// `canonical()` is a real inverse of `parse`, not merely a debug rendering.
    #[test]
    fn specifier_canonical_round_trips(s in spec_text()) {
        if let Ok(spec) = StreamSpecifier::parse(&s) {
            let text = spec.canonical();
            let back = StreamSpecifier::parse(&text)
                .unwrap_or_else(|e| panic!("{s:?} -> {text:?} failed to reparse: {e}"));
            prop_assert_eq!(spec, back, "via {:?}", text);
        }
    }

    #[test]
    fn map_display_round_trips(s in spec_text()) {
        if let Ok(m) = MapSpec::parse(&s) {
            let text = m.to_string();
            let back = MapSpec::parse(&text)
                .unwrap_or_else(|e| panic!("{s:?} -> {text:?} failed to reparse: {e}"));
            prop_assert_eq!(m, back, "via {:?}", text);
        }
    }

    /// Selection is a subsequence of container order with no repeats, and every
    /// index it yields exists.
    #[test]
    fn selection_is_well_formed(s in spec_text(), streams in ctx_streams()) {
        let Ok(spec) = StreamSpecifier::parse(&s) else { return Ok(()) };
        let ctx = MatchCtx::streams(&streams);
        let picked = spec.select(&ctx);
        prop_assert!(
            picked.windows(2).all(|w| w.first() < w.last()),
            "not strictly increasing: {picked:?}"
        );
        for i in &picked {
            prop_assert!((*i as usize) < streams.len());
        }
        // An index token selects at most one stream.
        if spec.index.is_some() {
            prop_assert!(picked.len() <= 1);
        }
        // The empty specifier selects everything.
        if spec.is_empty() {
            prop_assert_eq!(picked.len(), streams.len());
        }
    }

    /// `matches` is exactly membership of `select`.
    #[test]
    fn matches_agrees_with_select(s in spec_text(), streams in ctx_streams()) {
        let Ok(spec) = StreamSpecifier::parse(&s) else { return Ok(()) };
        let ctx = MatchCtx::streams(&streams);
        let picked = spec.select(&ctx);
        for i in 0..u32::try_from(streams.len()).unwrap_or(0) {
            prop_assert_eq!(spec.matches(&ctx, i), picked.contains(&i));
        }
    }

    /// Splitting an arbitrary argument vector never panics, and when it
    /// succeeds the structural invariants hold.
    #[test]
    fn splitting_is_total_and_well_formed(argv in prop::collection::vec(wild_arg(), 0..12)) {
        for table in [ffmpeg(), ffprobe()] {
            let Ok(cl) = split(&table, &argv) else { continue };

            // Every global option really is global; every grouped and orphaned
            // option really is not.
            for o in &cl.global {
                let d = o.desc.expect("a deferred option is never hoisted");
                prop_assert!(d.flags.contains(vaco_cli_core::OptFlags::GLOBAL));
            }
            for o in cl.groups.iter().flat_map(|g| &g.opts).chain(&cl.orphaned) {
                if let Some(d) = o.desc {
                    prop_assert!(!d.flags.contains(vaco_cli_core::OptFlags::GLOBAL));
                }
            }

            // Group indices are 0..n within each kind, in argv order.
            for kind in [vaco_cli_core::GroupKind::Input, vaco_cli_core::GroupKind::Output] {
                let seen: Vec<u32> = cl.of_kind(kind).map(|g| g.index).collect();
                let want: Vec<u32> = (0..u32::try_from(seen.len()).unwrap_or(0)).collect();
                prop_assert_eq!(seen, want);
            }

            // Nothing is lost: every argv entry is either a URL, an option
            // name, or an option's value.
            let counted = cl.global.len()
                + cl.orphaned.len()
                + cl.groups.iter().map(|g| g.opts.len()).sum::<usize>();
            prop_assert!(counted + cl.groups.len() <= argv.len());

            // Validation is total too.
            let _ = cl.validate();

            // So is specifier resolution on whatever survived.
            for o in cl.global.iter().chain(cl.groups.iter().flat_map(|g| &g.opts)) {
                let _ = o.stream_spec();
                let _ = o.metadata_spec();
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// `strtod` reports exactly what it consumed, always.
    ///
    /// The `Expected number for …` check is `*endptr == 0`, so a tail that is
    /// not a true suffix would make acceptance disagree with the reference.
    #[test]
    fn strtod_tail_is_always_a_suffix(s in wild_text()) {
        let (_, tail) = strtod(&s);
        prop_assert!(s.ends_with(tail));
        prop_assert!(tail.len() <= s.len());
    }

    /// C's contract: no conversion means `endptr == nptr`, leading whitespace
    /// included. That is what makes `-ac ""` legal and `-ac " "` not.
    #[test]
    fn strtod_consumes_nothing_or_starts_at_the_number(s in wild_text()) {
        let (value, tail) = strtod(&s);
        if tail.len() == s.len() {
            prop_assert_eq!(tail, s.as_str());
            prop_assert_eq!(value.to_bits(), 0.0f64.to_bits());
        }
    }

    /// Every `f64` survives a round trip through the number grammar, because
    /// Rust's `{:?}` is round-trip exact and `av_strtod` is a superset of the
    /// decimal syntax it emits — `inf` and `NaN` included.
    #[test]
    fn every_f64_round_trips_through_strtod(bits in any::<u64>()) {
        let v = f64::from_bits(bits);
        let text = format!("{v:?}");
        let (back, tail) = strtod(&text);
        prop_assert_eq!(tail, "", "{:?} left {:?}", text, tail);
        if v.is_nan() {
            prop_assert!(back.is_nan());
        } else {
            prop_assert_eq!(back.to_bits(), v.to_bits(), "via {:?}", text);
        }
    }

    /// Acceptance implies full consumption, and a bounded kind never returns a
    /// value outside its bounds.
    #[test]
    fn parse_number_respects_its_own_contract(s in wild_text()) {
        for limits in [NumberLimits::int32(), NumberLimits::int64(), NumberLimits::float()] {
            if let Ok(v) = parse_number("opt", &s, limits) {
                prop_assert_eq!(strtod(&s).1, "");
                prop_assert!(v >= limits.min && v <= limits.max);
                if limits.integral {
                    prop_assert_eq!(v.fract(), 0.0);
                }
            }
        }
    }

    /// Compiling once and evaluating is the same as evaluating once, and
    /// evaluation is deterministic.
    #[test]
    fn expression_evaluation_is_deterministic(s in expr_text()) {
        let c = OptionConstants::new(1.0, -2.0, 3.0);
        let once = eval_option("opt", &s, c);
        let compiled = Expression::compile_for_option("opt", &s);
        prop_assert_eq!(once.is_ok(), compiled.is_ok());
        if let (Ok(a), Ok(e)) = (once, compiled) {
            let b = e.eval(&c.values());
            prop_assert_eq!(a.to_bits(), b.to_bits());
            prop_assert_eq!(e.eval(&c.values()).to_bits(), b.to_bits());
            prop_assert_eq!(e.source(), s.as_str());
        }
    }

    /// Both value grammars are total over the same corpus.
    ///
    /// They disagree constantly — that is the point, and the specific
    /// disagreements are pinned by the recorded transcript in
    /// `tests/conformance.rs`. What matters here is that neither ever panics on
    /// text written for the other.
    #[test]
    fn both_value_grammars_are_total_over_the_same_corpus(s in expr_text()) {
        let plain = parse_number("opt", &s, NumberLimits::float());
        let expr = eval_option("opt", &s, OptionConstants::UNKNOWN);
        // A value both accept must agree, since a bare number means the same
        // thing in either grammar.
        if let (Ok(a), Ok(b)) = (plain, expr) {
            prop_assert_eq!(a.to_bits(), b.to_bits(), "grammars disagree on {:?}", s);
        }
    }
}

/// Text drawn from the expression alphabet.
fn expr_text() -> impl Strategy<Value = String> {
    let token = prop::sample::select(vec![
        "1", "2", "0", "+", "-", "*", "/", "^", "(", ")", ",", ";", "min", "max", "default", "PI",
        "E", "abs", "gcd", "if", "st", "ld", "20dB", "2k", "0x10", " ", "nan", "inf", "",
    ]);
    prop::collection::vec(token, 0..8).prop_map(|v| v.concat())
}

/// Argv entries drawn from a mix of real option spellings and noise.
fn wild_arg() -> impl Strategy<Value = OsString> {
    prop_oneof![
        4 => prop::sample::select(vec![
            "-i", "-y", "-n", "-c:v", "-c:a:1", "-map", "-metadata:s:v:0", "-t", "-f", "--",
            "-", "-vf", "-nostats", "-/filter:v", "-shortest", "-re", "-ss", "--help", "-qwerty",
            "-c:", "-c:zzz", "-y:vv",
        ]).prop_map(OsString::from),
        3 => prop::sample::select(vec![
            "in.mkv", "out.mp4", "null", "copy", "1", "0:v", "title=x", "libx264", "",
        ]).prop_map(OsString::from),
        1 => ".{0,10}".prop_map(OsString::from),
    ]
}
