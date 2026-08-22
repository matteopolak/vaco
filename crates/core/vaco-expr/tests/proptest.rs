//! Property tests.
//!
//! Three invariants worth a generator rather than examples:
//!
//! 1. **Round trip.** A generated tree rendered to source, parsed and evaluated
//!    must equal the tree interpreted directly — bit for bit, since the whole
//!    contract of this crate is exact f64 agreement.
//! 2. **Whitespace is deleted.** Parsing any string must give the same result
//!    as parsing that string with every whitespace byte removed.
//! 3. **Totality.** No input parses to a panic, and nothing that parses
//!    evaluates to one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp,
    clippy::many_single_char_names,
    reason = "test code; assertions are the point and the inputs are known"
)]

use proptest::prelude::*;
use vaco_expr::{Bindings, Expr, strip_whitespace};

/// A small arithmetic tree, generated and then rendered and re-parsed.
#[derive(Debug, Clone)]
enum Tree {
    Num(f64),
    Neg(Box<Tree>),
    Add(Box<Tree>, Box<Tree>),
    Mul(Box<Tree>, Box<Tree>),
    Div(Box<Tree>, Box<Tree>),
    Pow(Box<Tree>, Box<Tree>),
    Abs(Box<Tree>),
    Min(Box<Tree>, Box<Tree>),
}

impl Tree {
    /// Fully parenthesised, so the rendering never depends on the precedence
    /// rules under test — a round-trip test that relied on them would pass for
    /// the wrong reason.
    fn render(&self) -> String {
        match self {
            // `{v:?}` is the shortest representation that round-trips, and the
            // lexer accepts everything it can produce, `inf` and `NaN`
            // included.
            Self::Num(v) => format!("({v:?})"),
            Self::Neg(a) => format!("(-{})", a.render()),
            Self::Add(a, b) => format!("({}+{})", a.render(), b.render()),
            Self::Mul(a, b) => format!("({}*{})", a.render(), b.render()),
            Self::Div(a, b) => format!("({}/{})", a.render(), b.render()),
            Self::Pow(a, b) => format!("({}^{})", a.render(), b.render()),
            Self::Abs(a) => format!("abs({})", a.render()),
            Self::Min(a, b) => format!("min({},{})", a.render(), b.render()),
        }
    }

    fn value(&self) -> f64 {
        match self {
            Self::Num(v) => *v,
            Self::Neg(a) => -a.value(),
            Self::Add(a, b) => a.value() + b.value(),
            Self::Mul(a, b) => a.value() * b.value(),
            Self::Div(a, b) => a.value() / b.value(),
            Self::Pow(a, b) => a.value().powf(b.value()),
            Self::Abs(a) => a.value().abs(),
            // Mirrors the reference's comparison select, not `f64::min`.
            Self::Min(a, b) => {
                let (x, y) = (a.value(), b.value());
                if x < y { x } else { y }
            }
        }
    }
}

fn tree() -> impl Strategy<Value = Tree> {
    let leaf = prop_oneof![
        (-1e6f64..1e6).prop_map(Tree::Num),
        (-40i32..40).prop_map(|e| Tree::Num(f64::from(e))),
        Just(Tree::Num(0.0)),
        Just(Tree::Num(f64::INFINITY)),
        Just(Tree::Num(f64::NAN)),
    ];
    leaf.prop_recursive(5, 40, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(|a| Tree::Neg(Box::new(a))),
            inner.clone().prop_map(|a| Tree::Abs(Box::new(a))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Tree::Add(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Tree::Mul(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Tree::Div(Box::new(a), Box::new(b))),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Tree::Pow(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| Tree::Min(Box::new(a), Box::new(b))),
        ]
    })
}

proptest! {
    #[test]
    fn render_parse_eval_round_trips(t in tree()) {
        let src = t.render();
        let parsed = Expr::parse(&src, &Bindings::EMPTY);
        prop_assert!(parsed.is_ok(), "rejected our own rendering: {src}");
        let value = parsed.map_or(f64::NAN, |e| e.eval(&[]));
        let expected = t.value();
        prop_assert!(
            value.to_bits() == expected.to_bits() || (value.is_nan() && expected.is_nan()),
            "{src}: got {value:?} ({:#018x}), expected {expected:?} ({:#018x})",
            value.to_bits(),
            expected.to_bits()
        );
    }

    /// Whitespace is deleted before parsing, so it can never change a result —
    /// not even inside a number, where it merely concatenates digits.
    #[test]
    fn whitespace_never_changes_the_outcome(src in "[0-9a-zA-Z_ \t\n+*/^;,().-]{0,60}") {
        let stripped = strip_whitespace(&src);
        let a = Expr::parse(&src, &Bindings::EMPTY).map(|e| e.eval(&[]).to_bits());
        let b = Expr::parse(&stripped, &Bindings::EMPTY).map(|e| e.eval(&[]).to_bits());
        prop_assert_eq!(a.is_ok(), b.is_ok(), "{:?} vs {:?}", src, stripped);
        if let (Ok(a), Ok(b)) = (a, b) {
            prop_assert_eq!(a, b);
        }
    }

    /// Parsing arbitrary text must not panic, and anything that does parse must
    /// evaluate without panicking. This overlaps the fuzz target deliberately:
    /// proptest runs in CI on every commit, the fuzzer does not.
    #[test]
    fn arbitrary_text_is_total(src in "\\PC{0,80}") {
        if let Ok(e) = Expr::parse(&src, &Bindings::new(&["x", "y"])) {
            let _ = e.eval(&[1.0, 2.0]);
        }
    }

    /// The same, biased hard towards the language's own alphabet so the
    /// generator actually reaches the parser's interesting states.
    #[test]
    fn expression_shaped_text_is_total(src in "[a-zA-Z0-9_.+*/^;,() -]{0,80}") {
        if let Ok(e) = Expr::parse(&src, &Bindings::new(&["x", "y"])) {
            let _ = e.eval(&[1.0, 2.0]);
        }
    }

    /// A number literal never claims more bytes than it was given, and a
    /// literal parsed on its own evaluates to the value the scanner reported.
    #[test]
    fn number_scanning_is_consistent(src in "[-+0-9a-fA-FxXeEpPkKMGiB.]{0,24}") {
        if let Some(n) = vaco_expr::scan_number(&src) {
            prop_assert!(n.len <= src.len());
            prop_assert!(n.len > 0);
            if n.len == src.len() {
                let parsed = Expr::parse(&src, &Bindings::EMPTY);
                prop_assert!(parsed.is_ok(), "scanner accepted {src:?} but parser did not");
                let v = parsed.map_or(f64::NAN, |e| e.eval(&[]));
                prop_assert!(
                    v.to_bits() == n.value.to_bits() || (v.is_nan() && n.value.is_nan())
                );
            }
        }
    }
}
