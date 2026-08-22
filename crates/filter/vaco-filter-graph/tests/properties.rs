//! Property tests. Plan 13 §3.2: proptest earns its place where there is a
//! round trip or an invariant, and this crate has both.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_filter_graph::ast::parse;
use vaco_filter_graph::lex::{self, StopSet};
use vaco_filter_graph::mock::MockRegistry;

/// Text a user might legitimately put in an argument, label or name: awkward on
/// purpose, since escaping is the whole point.
fn nasty() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "a",
            "b",
            " ",
            "\t",
            ":",
            ";",
            ",",
            "[",
            "]",
            "'",
            "\\",
            "=",
            "|",
            "@",
            "é",
            "\u{1f600}",
        ]),
        0..12,
    )
    .prop_map(|parts| parts.concat())
}

fn levels() -> impl Strategy<Value = StopSet> {
    prop::sample::select(vec![
        StopSet::GRAPH,
        StopSet::ARG,
        StopSet::NAME,
        StopSet::LABEL,
        StopSet::LIST,
        StopSet::EQ,
        StopSet::NONE,
    ])
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The parser is fed untrusted input, so this is the property that matters
    /// most: it terminates and does not panic, whatever it is given.
    #[test]
    fn parsing_arbitrary_text_never_panics(src in ".{0,200}") {
        let _ = parse(&src);
    }

    #[test]
    fn parsing_arbitrary_bracket_soup_never_panics(
        src in prop::collection::vec(
            prop::sample::select(vec!["[", "]", ",", ";", "=", "'", "\\", "a", "@", ":", " "]),
            0..80,
        ).prop_map(|v| v.concat())
    ) {
        let _ = parse(&src);
    }

    /// One level of escaping is exactly invertible by one scan, at every level.
    #[test]
    fn escape_and_scan_are_inverse(text in nasty(), level in levels()) {
        let encoded = lex::escape(&text, level);
        let mut at = 0;
        let token = lex::next_token(&encoded, &mut at, level);
        prop_assert_eq!(&token.text, &text);
        prop_assert_eq!(at, encoded.len());
        prop_assert!(token.quirks.is_empty());
    }

    /// Splitting on a separator and re-joining with it is the identity, as long
    /// as each piece is escaped for that level. This is the "split before
    /// unescaping" contract stated as an equation.
    #[test]
    fn split_then_join_round_trips(pieces in prop::collection::vec(nasty(), 1..6)) {
        let joined = pieces
            .iter()
            .map(|p| lex::escape(p, StopSet::ARG))
            .collect::<Vec<_>>()
            .join(":");
        let back: Vec<String> = lex::split_raw(&joined, StopSet::ARG)
            .into_iter()
            .map(|(p, _)| lex::unescape(p))
            .collect();
        prop_assert_eq!(back, pieces);
    }

    /// `parse(print(parse(s))) == parse(s)`, structurally. Plan 16 §2.4 asks for
    /// exactly this, because `Ast` is what `-dumpgraph` and any future GUI
    /// consume.
    #[test]
    fn printing_round_trips_through_the_parser(
        names in prop::collection::vec("[a-z][a-z0-9_]{0,5}", 1..4),
        args in prop::collection::vec(nasty(), 1..4),
        labels in prop::collection::vec("[a-z0-9:]{1,4}", 0..3),
    ) {
        let mut src = String::new();
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                src.push(',');
            }
            src.push_str(name);
            if let Some(a) = args.get(i) {
                src.push('=');
                src.push_str(&lex::escape(a, StopSet::GRAPH));
            }
        }
        for l in &labels {
            src.push('[');
            src.push_str(l);
            src.push(']');
        }
        // Only meaningful when the constructed text parses at all.
        let Ok(first) = parse(&src) else { return Ok(()); };
        let printed = first.to_string();
        let second = parse(&printed)
            .map_err(|e| TestCaseError::fail(format!("{printed:?}: {e}")))?;
        prop_assert_eq!(first.without_spans(), second.without_spans());
        prop_assert_eq!(printed.clone(), second.to_string());
    }

    /// Building never panics either, whatever the description says.
    #[test]
    fn building_arbitrary_descriptions_never_panics(
        src in prop::collection::vec(
            prop::sample::select(vec![
                "null", "invert", "split", "merge", "counter", "anull", "zzz",
                ",", ";", "[a]", "[b]", "=", "n=1", "outputs=3", "inputs=2", ":",
            ]),
            0..14,
        ).prop_map(|v| v.concat())
    ) {
        let registry = MockRegistry::new();
        let _ = vaco_filter_graph::parse_and_build(&src, &registry);
    }
}

/// A parse that succeeds must consume the whole description: anything else
/// would mean silently ignoring part of what the user asked for.
#[test]
fn a_successful_parse_accounts_for_every_filter() {
    for src in [
        "null",
        "null,null",
        "null;null",
        "[a]null[b];[b]null[c]",
        "sws_flags=x;null",
    ] {
        let ast = parse(src).unwrap();
        let total: usize = ast.chains.iter().map(|c| c.filters.len()).sum();
        assert!(total > 0, "{src:?}");
        assert!(ast.chains.iter().all(|c| !c.filters.is_empty()), "{src:?}");
    }
}
