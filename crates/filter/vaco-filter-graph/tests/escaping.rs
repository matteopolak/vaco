//! The three levels of escaping, kept apart on purpose.
//!
//! Plan 13 §1b records two occasions on which an agent probed a *parser*
//! through a filtergraph and measured the filtergraph's own unescaping instead.
//! This crate is that unescaping, so these tests state which level each vector
//! belongs to and what the reference does with it.
//!
//! The reference entry point used throughout is the `movie` filter, whose
//! `filename` option is echoed back verbatim in its error message:
//!
//! ```sh
//! ffmpeg -f lavfi -i "movie=<vector>" -f null -
//! #   -> Failed to avformat_open_input '<what the option layer received>'
//! ```
//!
//! That is two levels — graph scan, then option scan — which is exactly the
//! composition this crate has to reproduce, so it is the right probe. Anything
//! claiming to measure a *single* level through a filtergraph would not be.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    reason = "test code"
)]

use vaco_filter_graph::ast::parse;
use vaco_filter_graph::lex::{self, Quirk, StopSet};

/// Both levels, as `movie=<src>` would see them: graph scan, then the option
/// scan on the first argument.
fn both_levels(src: &str) -> String {
    let ast =
        parse(&format!("movie={src}")).unwrap_or_else(|e| panic!("{src:?}: {}", e.render(src)));
    let filter = &ast.chains[0].filters[0];
    filter
        .arguments()
        .unwrap()
        .first()
        .map(vaco_filter_graph::Arg::value)
        .unwrap_or_default()
}

#[test]
fn the_graph_level_unescapes_which_is_why_an_escaped_colon_still_splits() {
    // The single most load-bearing measurement in this crate.
    //   ffmpeg -f lavfi -i "movie=a\:b"  ->  'a'
    // If the graph level had left `\:` alone, the option layer would have
    // honoured the escape and the filename would have been `a:b`.
    assert_eq!(both_levels(r"a\:b"), "a");
    assert_eq!(both_levels(r"a\\:b"), "a:b");
}

#[test]
fn a_graph_level_quote_hides_a_colon_from_the_graph_but_not_from_the_option() {
    // ffmpeg -f lavfi -i "movie=a'b:c'd"  ->  'ab'  — the graph level ate the
    // quotes, so the `:` it protected is bare by the time the option level
    // splits, and the first argument is `ab` rather than `ab:cd`.
    assert_eq!(both_levels("a'b:c'd"), "ab");
    // ffmpeg -f lavfi -i "movie=\'a:b\'"  ->  'a:b' (quote survives to level 1)
    assert_eq!(both_levels(r"\'a:b\'"), "a:b");
}

#[test]
fn separators_that_belong_to_the_graph_need_escaping_there() {
    // ffmpeg -f lavfi -i "movie=a\,b"  ->  'a,b'
    assert_eq!(both_levels(r"a\,b"), "a,b");
    assert_eq!(both_levels(r"a\;b"), "a;b");
    assert_eq!(both_levels(r"a\[b"), "a[b");
    assert_eq!(both_levels("'a,b;c[d]'"), "a,b;c[d]");
}

#[test]
fn a_backslash_survives_two_levels_only_as_four() {
    // ffmpeg -f lavfi -i 'movie=a\\\\b'  ->  'a\b'
    assert_eq!(both_levels(r"a\\\\b"), r"a\b");
    // ffmpeg -f lavfi -i 'movie=a\\b'    ->  'ab'
    assert_eq!(both_levels(r"a\\b"), "ab");
}

#[test]
fn whitespace_is_data_in_the_middle_and_trimmed_at_the_ends() {
    // ffmpeg -f lavfi -i "movie=a b"     -> 'a b'
    // ffmpeg -f lavfi -i "movie=  ab  "  -> 'ab'
    // ffmpeg -f lavfi -i "movie='ab '"   -> 'ab'   (the option level trims too)
    // ffmpeg -f lavfi -i "movie=\' ab \'"-> ' ab ' (quoted at the option level)
    assert_eq!(both_levels("a b"), "a b");
    assert_eq!(both_levels("  ab  "), "ab");
    assert_eq!(both_levels("'ab '"), "ab");
    assert_eq!(both_levels(r"\' ab \'"), " ab ");
    // ffmpeg -f lavfi -i 'movie=a\\ ' -> 'a\\'  — the escaped backslash is data,
    // the bare space after it is not, so the trim takes the space and leaves
    // a lone trailing backslash for the option level to keep.
    assert_eq!(both_levels(r"a\\ "), r"a\");
}

#[test]
fn the_canonical_worked_example_of_plan_16_is_correct() {
    // Verified byte-for-byte against ffmpeg 8.1. The level-2 string below is
    // exactly what plan 16 §2.3 prints, and the reference echoes the level-0
    // text back:
    //
    //   $ ffmpeg -f lavfi -i "$(cat vector)" -f null -
    //   Failed to avformat_open_input
    //     'this is a 'string': may contain one, or more, special characters'
    const LEVEL2: &str =
        r"this is a \\\'string\\\'\\: may contain one\, or more\, special characters";
    const WANT: &str = "this is a 'string': may contain one, or more, special characters";
    assert_eq!(both_levels(LEVEL2), WANT);

    // The quoted-run variant most users prefer, also verified:
    //   movie='this is a '\\\''string'\\\''\: may contain one, or more, special characters'
    const QUOTED: &str =
        r"'this is a '\\\''string'\\\''\: may contain one, or more, special characters'";
    assert_eq!(both_levels(QUOTED), WANT);
}

#[test]
fn an_expression_argument_survives_the_graph_layer_intact() {
    // Filter arguments are very often `vaco-expr` expressions, and the graph
    // layer's job is to hand them over unharmed. Checked by parsing the
    // recovered text with the real expression engine rather than by eyeballing
    // the string.
    let bindings = vaco_expr::Bindings::new(&["N", "T", "W", "H"]);
    for src in [
        "(N+1)*40",
        "if(gt(T\\,10)\\,W/2\\,H/2)",
        "'lt(mod(N,2),1)'",
        r"max(1\,min(N\,100))",
    ] {
        let ast = parse(&format!("geq=lum={src}")).unwrap();
        let arg = ast.chains[0].filters[0].arguments().unwrap();
        let text = arg[0].value();
        vaco_expr::Expr::parse(&text, &bindings)
            .unwrap_or_else(|e| panic!("{src:?} -> {text:?}: {e:?}"));
    }
}

#[test]
fn the_two_leniencies_the_reference_has_are_recorded_not_rejected() {
    // ffmpeg -f lavfi -i "movie='ab"   -> 'ab'   (no error)
    // ffmpeg -f lavfi -i 'movie=ab\'   -> 'ab\'  (no error)
    // `vaco_core::escape::unescape` rejects both; matching it here would fail
    // command lines that work today.
    let ast = parse("movie='ab").unwrap();
    assert_eq!(both_levels("'ab"), "ab");
    assert_eq!(
        ast.quirks.first().map(|q| q.0),
        Some(Quirk::UnterminatedQuote)
    );

    let ast = parse(r"movie=ab\").unwrap();
    assert_eq!(
        ast.quirks.first().map(|q| q.0),
        Some(Quirk::TrailingBackslash)
    );
    assert_eq!(ast.chains[0].filters[0].args.as_deref(), Some(r"ab\"));
}

#[test]
fn escaping_and_scanning_are_inverse_at_every_level() {
    for level in [
        StopSet::GRAPH,
        StopSet::ARG,
        StopSet::NAME,
        StopSet::LABEL,
        StopSet::LIST,
    ] {
        for text in [
            "plain",
            "a:b",
            "a,b;c",
            "a|b",
            "a=b",
            "[x]",
            r"c:\path\to\file",
            "it's",
            "  padded  ",
            "",
        ] {
            let encoded = lex::escape(text, level);
            let mut at = 0;
            let back = lex::next_token(&encoded, &mut at, level);
            assert_eq!(
                back.text, text,
                "level {level:?} on {text:?} -> {encoded:?}"
            );
            assert_eq!(
                at,
                encoded.len(),
                "level {level:?} did not consume {encoded:?}"
            );
        }
    }
}
