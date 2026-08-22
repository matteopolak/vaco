//! Behaviour that the captured vectors cannot express: limits, the evaluation
//! context, and the register model.

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

use vaco_expr::{Bindings, Context, Expr, Limits, ParseErrorKind, Registers};

fn ok(src: &str) -> f64 {
    Expr::parse(src, &Bindings::EMPTY).map_or(f64::NAN, |e| e.eval(&[]))
}

fn kind(src: &str) -> Option<ParseErrorKind> {
    Expr::parse(src, &Bindings::EMPTY).err().map(|e| e.kind)
}

// ---------------------------------------------------------------- limits

/// The reference's acceptance boundary, measured by bisection against
/// `ffmpeg 8.1`. Nesting is limited by parse depth, flat chains by node depth;
/// the two limits are separate because the reference reports different errors
/// for them (EINVAL for nesting, ENOMEM for chains).
#[test]
fn depth_limits_match_the_reference_boundary() {
    let nest = |d: usize| format!("{}1{}", "(".repeat(d), ")".repeat(d));
    assert!(Expr::parse(&nest(99), &Bindings::EMPTY).is_ok());
    assert_eq!(kind(&nest(100)), Some(ParseErrorKind::TooDeep));

    let calls = |d: usize| format!("{}1{}", "abs(".repeat(d), ")".repeat(d));
    assert!(Expr::parse(&calls(99), &Bindings::EMPTY).is_ok());
    assert_eq!(kind(&calls(100)), Some(ParseErrorKind::TooDeep));

    for op in ["+1", "*1", ";1"] {
        let chain = |n: usize| format!("1{}", op.repeat(n));
        assert!(
            Expr::parse(&chain(100), &Bindings::EMPTY).is_ok(),
            "100 x {op} should parse"
        );
        assert_eq!(
            kind(&chain(101)),
            Some(ParseErrorKind::TooDeep),
            "101 x {op} should be rejected"
        );
    }
}

/// A deep expression must not overflow the stack while being evaluated either.
/// The node-depth limit is what guarantees that, so it is worth asserting that
/// an expression right at the limit actually evaluates.
#[test]
fn expression_at_the_depth_limit_still_evaluates() {
    let src = format!("1{}", "+1".repeat(100));
    let e = Expr::parse(&src, &Bindings::EMPTY).expect("at the limit");
    assert_eq!(e.eval(&[]), 101.0);
}

// ----------------------------------------------------------------- while

/// The reference loops forever on `while(1,...)` — it has to be `SIGKILL`ed.
/// We stop at the budget and return the last value instead.
#[test]
fn unbounded_while_stops_at_the_budget() {
    let e = Expr::parse("st(0,0);while(1,st(0,ld(0)+1))", &Bindings::EMPTY).expect("parses");
    let mut regs = Registers::new();
    let limits = Limits {
        max_iterations: 1000,
        ..Limits::default()
    };
    let v = e.eval_with(&mut Context::new(&[], &mut regs).with_limits(limits));
    assert!(v.is_finite(), "should terminate, got {v}");
    assert!(v >= 1000.0, "should have run the whole budget, got {v}");
}

#[test]
fn bounded_while_is_unaffected_by_the_budget() {
    assert_eq!(ok("st(0,0);while(lt(ld(0),5),st(0,ld(0)+1))"), 5.0);
    assert!(ok("while(0,1)").is_nan());
}

// -------------------------------------------------------------- registers

/// Verified against the reference: an expression evaluated once per sample
/// with body `st(0,ld(0)+1)` yields 1, 2, 3, 4 — the registers survive between
/// evaluations, so they belong to the caller and not to the `Expr`.
#[test]
fn registers_persist_across_evaluations() {
    let e = Expr::parse("st(0,ld(0)+1)", &Bindings::EMPTY).expect("parses");
    let mut regs = Registers::new();
    let seen: Vec<f64> = (0..4)
        .map(|_| e.eval_with(&mut Context::new(&[], &mut regs)))
        .collect();
    assert_eq!(seen, vec![1.0, 2.0, 3.0, 4.0]);
    assert!(e.uses_registers());
    assert!(
        !Expr::parse("1+1", &Bindings::EMPTY)
            .expect("parses")
            .uses_registers()
    );
}

#[test]
fn eval_starts_from_fresh_registers() {
    let e = Expr::parse("st(0,ld(0)+1)", &Bindings::EMPTY).expect("parses");
    assert_eq!(e.eval(&[]), 1.0);
    assert_eq!(e.eval(&[]), 1.0);
}

// -------------------------------------------------------------- variables

#[test]
fn variables_resolve_positionally() {
    let b = Bindings::new(&["w", "h", "main_w"]);
    let e = Expr::parse("w/h+main_w", &b).expect("parses");
    assert_eq!(e.var_count(), 3);
    assert_eq!(e.eval(&[1920.0, 1080.0, 7.0]), 1920.0 / 1080.0 + 7.0);
}

/// The prefix matcher must not let a short name eat a longer one: `w` is a
/// variable, but `w2` and `width` are not — the byte after the match has to be
/// outside `[A-Za-z0-9_]`.
#[test]
fn prefix_matching_respects_identifier_boundaries() {
    let b = Bindings::new(&["w"]);
    assert!(Expr::parse("w", &b).is_ok());
    assert!(Expr::parse("w+1", &b).is_ok());
    assert!(Expr::parse("w2", &b).is_err());
    assert!(Expr::parse("width", &b).is_err());
    // But a non-identifier byte does terminate a match, which is why the
    // reference accepts `abs.(1)`.
    assert_eq!(ok("abs.(1)"), 1.0);
    assert!(Expr::parse("abs_(1)", &Bindings::EMPTY).is_err());
}

/// A variable named like a builtin function shadows it, because constants are
/// matched before any `(` is looked for. Verified indirectly by `PI(1)` being
/// rejected as trailing garbage rather than as an unknown function.
#[test]
fn variables_shadow_function_names() {
    let b = Bindings::new(&["abs"]);
    assert_eq!(Expr::parse("abs", &b).expect("parses").eval(&[5.0]), 5.0);
    assert_eq!(
        kind_with("abs(1)", &b),
        Some(ParseErrorKind::TrailingGarbage)
    );
}

fn kind_with(src: &str, b: &Bindings<'_>) -> Option<ParseErrorKind> {
    Expr::parse(src, b).err().map(|e| e.kind)
}

/// A short value slice is not a panic; the missing variables read as NaN.
#[test]
fn missing_variable_values_are_nan_not_a_panic() {
    let e = Expr::parse("a+b", &Bindings::new(&["a", "b"])).expect("parses");
    assert!(e.eval(&[1.0]).is_nan());
    assert!(e.eval(&[]).is_nan());
}

// ------------------------------------------------------------ context bits

#[test]
fn print_reaches_the_sink_and_returns_its_argument() {
    let e = Expr::parse("print(7)+print(8,16)", &Bindings::EMPTY).expect("parses");
    let mut regs = Registers::new();
    let mut seen: Vec<(f64, f64)> = Vec::new();
    let mut sink = |v: f64, l: f64| seen.push((v, l));
    let v = e.eval_with(&mut Context::new(&[], &mut regs).with_print(&mut sink));
    assert_eq!(v, 15.0);
    assert_eq!(
        seen,
        vec![(7.0, vaco_expr::DEFAULT_PRINT_LEVEL), (8.0, 16.0)]
    );
}

#[test]
fn print_without_a_sink_is_silent_and_transparent() {
    assert_eq!(ok("print(7)"), 7.0);
}

#[test]
fn time_can_be_pinned() {
    let e = Expr::parse("time(0)", &Bindings::EMPTY).expect("parses");
    let mut regs = Registers::new();
    let v = e.eval_with(&mut Context::new(&[], &mut regs).with_time(1234.5));
    assert_eq!(v, 1234.5);
}

#[test]
fn time_without_a_pin_reads_the_clock() {
    // Not a value assertion — just that it is a plausible wallclock and that
    // nothing panics when the clock is read.
    let v = ok("time(0)");
    assert!(v > 1.7e9, "wallclock looked wrong: {v}");
}

#[test]
fn caller_functions_are_dispatched_by_index() {
    let b = Bindings::new(&["x"]).with_functions(&[("dbl", 1), ("addup", 2)]);
    let e = Expr::parse("dbl(x)+addup(1,2)", &b).expect("parses");
    let mut regs = Registers::new();
    let mut calls = |id: u16, args: &[f64]| match id {
        0 => args.first().copied().unwrap_or(f64::NAN) * 2.0,
        _ => args.iter().sum(),
    };
    let v = e.eval_with(&mut Context::new(&[5.0], &mut regs).with_functions(&mut calls));
    assert_eq!(v, 13.0);
    // Arity is enforced for caller functions exactly as for builtins.
    assert_eq!(kind_with("dbl(1,2)", &b), Some(ParseErrorKind::WrongArity));
}

#[test]
fn caller_function_without_a_dispatcher_is_nan_not_a_panic() {
    let b = Bindings::EMPTY.with_functions(&[("f", 1)]);
    assert!(Expr::parse("f(1)", &b).expect("parses").eval(&[]).is_nan());
}

// ---------------------------------------------------------- error kinds

#[test]
fn rejection_reasons_match_the_reference_categories() {
    assert_eq!(kind("nosuchfn(1)"), Some(ParseErrorKind::UnknownFunction));
    assert_eq!(kind("foo"), Some(ParseErrorKind::UndefinedConstant));
    assert_eq!(kind("(1"), Some(ParseErrorKind::MissingCloseParen));
    assert_eq!(kind("if(1,2,3,4)"), Some(ParseErrorKind::MissingCloseParen));
    assert_eq!(kind("1)"), Some(ParseErrorKind::TrailingGarbage));
    assert_eq!(kind("1,2"), Some(ParseErrorKind::TrailingGarbage));
    assert_eq!(kind("PI(1)"), Some(ParseErrorKind::TrailingGarbage));
    assert_eq!(kind("max(1)"), Some(ParseErrorKind::WrongArity));
    assert_eq!(kind("sin(1,2)"), Some(ParseErrorKind::WrongArity));
    assert_eq!(kind(""), Some(ParseErrorKind::UndefinedConstant));
}

/// Names that look like they should exist and must stay rejected — every one
/// was probed against the reference and came back "Unknown function".
#[test]
fn plausible_but_nonexistent_functions_stay_rejected() {
    for name in [
        "log2",
        "log10",
        "exp2",
        "cbrt",
        "sign",
        "fmod",
        "sinc",
        "asinh",
        "acosh",
        "atanh",
        "bitxor",
        "bitnot",
        "shl",
        "shr",
        "xor",
        "and",
        "or",
        "neg",
        "int",
        "frac",
        "step",
        "smoothstep",
        "avg",
    ] {
        assert_eq!(
            kind(&format!("{name}(1)")),
            Some(ParseErrorKind::UnknownFunction),
            "{name} must not exist"
        );
    }
}

// `errors_convert_into_the_core_taxonomy` moved to `vaco-core`, along with the
// `From<ParseError> for vaco_core::Error` impl it exercises. This crate no
// longer depends on `vaco-core` — that one edge was blocking `vaco-core` from
// using the evaluator, which its ratio grammar needs.

// -------------------------------------------------------------- utilities

#[test]
fn decibel_helper_agrees_with_the_literal_grammar() {
    for db in [0.0, 6.0, 20.0, -20.0, 100.0, -100.0] {
        let literal = ok(&format!("{db}dB"));
        assert_eq!(
            literal.to_bits(),
            vaco_expr::from_decibels(db).to_bits(),
            "{db}dB"
        );
    }
}
