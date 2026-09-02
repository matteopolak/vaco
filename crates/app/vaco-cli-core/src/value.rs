//! Option *values*: which grammar each one is written in, and how each parses.
//!
//! # The finding that shapes this module
//!
//! Plan 14 §2.5 says "every numeric option value goes through the expression
//! evaluator", citing `-b:v 2*1000`. **Probing ffmpeg 8.1 shows the opposite**,
//! and the distinction is not cosmetic — it decides which command lines work.
//!
//! Of the 128 argument-taking options in the `vaco` table, exactly **11 reach
//! the expression evaluator**; **41 take a plain number** and reject an
//! expression outright:
//!
//! ```text
//! ffmpeg -i in.mkv -ac 1*2 -f null -
//! Expected number for ac but found: 1*2
//! ```
//!
//! The evaluator is reached three ways, none of them "because the value is
//! numeric":
//!
//! | Route | Options |
//! |---|---|
//! | the *reference* parses it as an `AVOption` rather than a table option (a fact about ffmpeg 8.1's own grammar routing, not a claim about which of these `vaco` has implemented -- several are still refused, see `vaco-cli`'s `refuse_unimplemented_options`) | `cpucount`, `cpuflags`, `abort_on`, `profile`, `discard`, `disposition`, `apply_cropping` |
//! | the ratio grammar | `aspect`, `time_base` |
//! | a codec option reached by name | `b`, `ab`, and every component option (`-crf`, …) |
//!
//! Plan 14's own example is in the third row: `-b:v` is an `AVOption` on the
//! codec, not a table option, which is exactly why it evaluates while `-ac`
//! does not.
//!
//! Confirmed on a filtergraph-free path (`-crf`, whose range check echoes the
//! value), so the whitespace and associativity results are trustworthy — see
//! plan 13 §1b:
//!
//! ```text
//! -crf -2^2          rejected: value -4, so the sign follows the whole chain
//! -crf max(1,0/0)    rejected: NaN, so `max` is a comparison select
//! -crf ---1          rejected: "Undefined constant … in '--1'"
//! -crf 1 2           accepted: 12, so whitespace is deleted
//! -crf 0-20dB        accepted: 0.1, so the sign belongs to the literal
//! ```
//!
//! # The plain-number grammar
//!
//! [`strtod`] is `av_strtod`, which is *not* C's `strtod`: it adds the SI
//! prefixes, the `i` binary modifier, the `B` times-eight suffix and the `dB`
//! decibel suffix, and its hexadecimal is integer-only. That grammar already
//! exists — it is the number lexer of the expression language — so this module
//! calls [`vaco_expr::scan_number`] rather than growing a second copy that
//! would drift.

use core::fmt;

use vaco_expr::{Bindings, Expr};

use crate::error::CliError;

/// Which grammar an option's value is written in.
///
/// Established by probing every argument-taking option with a junk value and
/// classifying the reference's complaint — the message names the grammar that
/// rejected it. See `docs/app/vaco-cli-core.md` §Method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    /// The option takes no value.
    None,
    /// [`strtod`], whole-string, then a range check.
    Float,
    /// As [`ValueKind::Float`], and additionally required to be integral —
    /// `-fs 20dB` is rejected with `Expected int64 for fs but found 20dB`
    /// because 20 dB is 9.999999999999998. Bounds are a C `int`'s.
    ///
    /// The width matters and is not guessable: 36 of the 41 integer options are
    /// 32-bit and 5 are 64-bit, and the reference prints the bounds it checked,
    /// so `-ac 3e9` is rejected while `-fs 3e9` is not. Each option's width was
    /// read out of that message.
    Int,
    /// As [`ValueKind::Int`], with a C `int64_t`'s bounds. Only `fs`, `frames`
    /// and its three aliases.
    Int64,
    /// The expression language. See the table above for who actually gets here.
    Expr,
    /// `vaco_core::parse::duration`. Rejects expressions:
    /// `Invalid duration for option t: 1*2`.
    Duration,
    /// A frame rate: named abbreviations first, then a ratio — and the ratio
    /// grammar is expression-backed, so `-r 5*5` really is 25.
    Rate,
    /// `WxH` or a named abbreviation.
    Size,
    /// A colour name or `#rrggbb`.
    Color,
    /// Passed through verbatim.
    Str,
    /// A grammar the consuming binary owns: a filter graph, a metadata
    /// `key=value`, a hardware device specification, `-map`, a log level.
    ///
    /// Deliberately not modelled here. Each has a bespoke parser and a bespoke
    /// message, and inventing a shared shape for them would be the kind of
    /// tidy-looking abstraction that then has to be unpicked.
    Custom,
}

impl ValueKind {
    /// Whether the option consumes a following argv entry.
    #[must_use]
    pub const fn takes_value(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether the *whole* value is an expression, so it can be handed
    /// straight to [`Expression::compile`].
    ///
    /// [`ValueKind::Rate`] deliberately says no: a rate tries the named
    /// abbreviations first (`pal`, `ntsc`), and only the ratio it falls back to
    /// is expression-backed. Compiling `pal` as an expression would fail.
    #[must_use]
    pub const fn is_expression(self) -> bool {
        matches!(self, Self::Expr)
    }

    /// Whether the evaluator is reachable *somewhere* in this grammar.
    ///
    /// True for [`ValueKind::Expr`] and for [`ValueKind::Rate`], whose fallback
    /// ratio evaluates — which is why `-r 5*5` is 25.
    #[must_use]
    pub const fn reaches_evaluator(self) -> bool {
        matches!(self, Self::Expr | Self::Rate)
    }
}

/// The value bounds and integrality an option's numeric type imposes.
///
/// The reference derives these from the C type of the field being written, and
/// prints them in the out-of-range message, so they are observable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumberLimits {
    pub min: f64,
    pub max: f64,
    /// Reject a non-integral value with `Expected int64 for …`.
    pub integral: bool,
}

impl NumberLimits {
    /// A C `int` field.
    #[must_use]
    pub fn int32() -> Self {
        Self {
            min: f64::from(i32::MIN),
            max: f64::from(i32::MAX),
            integral: true,
        }
    }

    /// A C `int64_t` field.
    ///
    /// D17: the printed bounds are **not** `INT64_MIN`/`INT64_MAX`. The
    /// reference passes them through a `double` before formatting, and
    /// `INT64_MAX` is not representable, so the message reads
    /// `… not within -9223372036854775808.000000 - 9223372036854775808.000000`
    /// — an upper bound one greater than the real one. Verified with `-fs 1e30`.
    /// Reproduced rather than corrected, because the text is observable output.
    #[must_use]
    pub fn int64() -> Self {
        Self {
            min: i64::MIN as f64,
            max: i64::MAX as f64,
            integral: true,
        }
    }

    /// A floating-point field: no bounds, no integrality.
    #[must_use]
    pub fn float() -> Self {
        Self {
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            integral: false,
        }
    }

    /// Bounds without an integrality requirement.
    #[must_use]
    pub fn range(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            integral: false,
        }
    }

    /// The limits implied by a [`ValueKind`], for the two numeric kinds.
    #[must_use]
    pub fn for_kind(kind: ValueKind) -> Option<Self> {
        match kind {
            ValueKind::Int => Some(Self::int32()),
            ValueKind::Int64 => Some(Self::int64()),
            ValueKind::Float => Some(Self::float()),
            _ => None,
        }
    }
}

/// `av_strtod`: the reference's number grammar, with C `strtod`'s `endptr`
/// contract.
///
/// Returns the value and the unconsumed tail. When nothing parses, the tail is
/// the **whole original string** — including any leading whitespace that was
/// skipped — because C sets `endptr = nptr` on failure. That is what makes
/// `-ac ""` succeed as zero while `-ac " "` fails: both parse nothing, but only
/// the first has an empty tail.
#[must_use]
pub fn strtod(s: &str) -> (f64, &str) {
    let skipped = s
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .count();
    let Some(body) = s.get(skipped..) else {
        return (0.0, s);
    };
    match vaco_expr::scan_number(body) {
        Some(n) => match s.get(skipped + n.len..) {
            Some(tail) => (n.value, tail),
            None => (0.0, s),
        },
        // No conversion performed: endptr == nptr.
        None => (0.0, s),
    }
}

/// Parse a plain numeric option value, reproducing the reference's three
/// checks in the reference's order.
///
/// 1. whole-string [`strtod`] → `Expected number for {name} but found: {value}`
/// 2. range → `The value for {name} was {value} which is not within {min} - {max}`
/// 3. integrality, for integer options only →
///    `Expected int64 for {name} but found {value}`
///
/// The order is observable: `-fs 1e30` is out of range (check 2) while
/// `-fs 20dB` is in range but fractional (check 3). Note that only the second
/// message reformats its bounds; all three print the *original string* for the
/// value, never the parsed double.
///
/// # Errors
/// [`CliError::ExpectedNumber`], [`CliError::ValueOutOfRange`] or
/// [`CliError::ExpectedInteger`].
pub fn parse_number(name: &str, value: &str, limits: NumberLimits) -> Result<f64, CliError> {
    let (parsed, tail) = strtod(value);
    if !tail.is_empty() {
        return Err(CliError::ExpectedNumber {
            option: name.to_owned(),
            value: value.to_owned(),
        });
    }
    // NaN fails both comparisons, so it passes the range check exactly as the
    // reference's `>`/`<` pair does, and is then caught by the integrality
    // check for integer options. `-ac nan` reports `Expected int64 for ac but
    // found nan`; `-max_error_rate nan` is accepted. Both verified.
    if parsed < limits.min || parsed > limits.max {
        return Err(CliError::ValueOutOfRange {
            option: name.to_owned(),
            value: value.to_owned(),
            min: limits.min,
            max: limits.max,
        });
    }
    if limits.integral && parsed.fract() != 0.0 {
        return Err(CliError::ExpectedInteger {
            option: name.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(parsed)
}

/// The three named constants the option system injects into an expression-valued
/// option, bound to the option's *own* metadata.
///
/// This is a genuine dialect difference and it is easy to miss. On the
/// `AVOption` path, `default`, `max` and `min` are **constants naming this
/// option's default, maximum and minimum** — and they *shadow the builtin
/// functions of the same name*, so `max(1,2)` is not a call:
///
/// ```text
/// $ ffmpeg … -crf max(1,2) …
/// [Eval] Invalid chars '(1,2)' at the end of expression 'max(1,2)'
/// ```
///
/// while every other builtin still works (`abs(-3)`, `gcd(-7,0)`, `hypot(3,4)`,
/// `if`, `st`/`ld`, `while`, `root`, `taylor` were all accepted). Verified on
/// two independent options, `-crf` and `-cpucount`, so it is the option system's
/// binding rather than one codec's.
///
/// That the values really are the option's own was confirmed arithmetically:
/// `-crf min-1` is rejected with "Value -2.000000 … out of range [-1 - …]", and
/// crf's minimum is -1.
///
/// The filtergraph path is *not* like this — there `max` and `min` are the
/// ordinary two-argument builtins. Two dialects, one language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptionConstants {
    pub default: f64,
    pub min: f64,
    pub max: f64,
}

impl OptionConstants {
    /// The names, in the order [`OptionConstants::values`] returns them.
    pub const NAMES: &'static [&'static str] = &["default", "max", "min"];

    /// Used when the option's schema is not in hand.
    ///
    /// The names still *bind*, so `max(1,2)` is still a parse error — which is
    /// the part that decides acceptance — but they evaluate to NaN rather than
    /// to real bounds. Closing this needs the component's schema, which lives
    /// above this crate in `vaco-opts`.
    pub const UNKNOWN: Self = Self {
        default: f64::NAN,
        min: f64::NAN,
        max: f64::NAN,
    };

    #[must_use]
    pub const fn new(default: f64, min: f64, max: f64) -> Self {
        Self { default, min, max }
    }

    /// Positionally matching [`OptionConstants::NAMES`].
    #[must_use]
    pub const fn values(&self) -> [f64; 3] {
        [self.default, self.max, self.min]
    }
}

/// A compiled expression.
///
/// Parsing allocates and evaluating does not, so anything evaluated more than
/// once — `-force_key_frames expr:…`, a filter's per-frame argument — compiles
/// once and keeps this. A one-shot option value can use [`eval_once`] instead.
#[derive(Debug, Clone)]
pub struct Expression {
    expr: Expr,
    source: String,
}

impl Expression {
    /// Compile a value in the **plain** expression language, with nothing in
    /// scope.
    ///
    /// This is the filtergraph dialect, where `max` and `min` are builtin
    /// functions. For a command-line option value you almost certainly want
    /// [`Expression::compile_for_option`] instead — see [`OptionConstants`].
    ///
    /// # Errors
    /// [`CliError::BadExpression`], carrying the option name and the value.
    pub fn compile(name: &str, source: &str) -> Result<Self, CliError> {
        Self::compile_with(name, source, &Bindings::EMPTY)
    }

    /// Compile a value in the **option** dialect, where `default`, `max` and
    /// `min` are constants naming the option's own metadata.
    ///
    /// # Errors
    /// [`CliError::BadExpression`].
    pub fn compile_for_option(name: &str, source: &str) -> Result<Self, CliError> {
        Self::compile_with(name, source, &Bindings::new(OptionConstants::NAMES))
    }

    /// Compile with variables in scope, for the callers that have some.
    ///
    /// # Errors
    /// [`CliError::BadExpression`].
    pub fn compile_with(
        name: &str,
        source: &str,
        bindings: &Bindings<'_>,
    ) -> Result<Self, CliError> {
        match Expr::parse(source, bindings) {
            Ok(expr) => Ok(Self {
                expr,
                source: source.to_owned(),
            }),
            Err(e) => Err(CliError::BadExpression {
                option: name.to_owned(),
                value: source.to_owned(),
                detail: e.to_string(),
            }),
        }
    }

    /// Evaluate with no variables.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.expr.eval(&[])
    }

    /// Evaluate against variable values positionally matching the bindings.
    #[must_use]
    pub fn eval(&self, vars: &[f64]) -> f64 {
        self.expr.eval(vars)
    }

    /// The compiled form, for callers that need `vaco-expr`'s own API.
    #[must_use]
    pub const fn inner(&self) -> &Expr {
        &self.expr
    }

    /// The text it was compiled from.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

/// Compile and evaluate a value in the plain expression language.
///
/// # Errors
/// [`CliError::BadExpression`].
pub fn eval_once(name: &str, value: &str) -> Result<f64, CliError> {
    Ok(Expression::compile(name, value)?.value())
}

/// Compile and evaluate a command-line option value, in the option dialect.
///
/// `constants` binds `default`, `max` and `min`. Pass
/// [`OptionConstants::UNKNOWN`] when the option's schema is not available:
/// acceptance is unaffected, only the value of those three names.
///
/// # Errors
/// [`CliError::BadExpression`].
pub fn eval_option(name: &str, value: &str, constants: OptionConstants) -> Result<f64, CliError> {
    Ok(Expression::compile_for_option(name, value)?.eval(&constants.values()))
}

/// Compile, evaluate and range-check an expression-valued option.
///
/// This is the shape an `AVOption` numeric takes: evaluate, then apply the
/// field's bounds. The out-of-range message is the *option system's*, not the
/// command line's, so callers that want the reference's exact text there
/// should format it themselves from the returned error's fields.
///
/// # Errors
/// [`CliError::BadExpression`] or [`CliError::ValueOutOfRange`].
pub fn eval_checked(name: &str, value: &str, limits: NumberLimits) -> Result<f64, CliError> {
    let v = eval_option(
        name,
        value,
        OptionConstants::new(f64::NAN, limits.min, limits.max),
    )?;
    if v < limits.min || v > limits.max {
        return Err(CliError::ValueOutOfRange {
            option: name.to_owned(),
            value: value.to_owned(),
            min: limits.min,
            max: limits.max,
        });
    }
    Ok(v)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test that cannot set up is a failed test"
)]
#[allow(
    clippy::float_cmp,
    reason = "these assert exact reference values; an epsilon would hide a \
              wrong constant, which is the only thing worth catching here"
)]
mod tests {
    use super::*;

    // --------------------------------------------------------------- strtod

    #[test]
    fn strtod_accepts_what_the_reference_accepts() {
        // Every literal here was accepted by `ffmpeg -ac <literal>`.
        for s in [
            "2", "2.0", "+2", "-2", " 2", "0x2", "0X10", "1e0", "1e3", "2k", "2K", "2M", "2G",
            "2m", "2u", "2n", "2ki", "2Ki", "2kB", "2B", "2dB", "20dB", "-20dB", "inf", "-inf",
            "nan", "nan(1)", "2.5k", ".5", "5.", "2E", "2h", "",
        ] {
            let (_, tail) = strtod(s);
            assert!(
                tail.is_empty(),
                "reference accepts {s:?}, tail was {tail:?}"
            );
        }
    }

    #[test]
    fn strtod_rejects_what_the_reference_rejects() {
        // Every literal here produced `Expected number for ac but found: …`.
        for s in [
            "2 ", "2kBB", "2Bk", "2i", "2*1", "PI", "0b1", "2e", "0x", "0x1p4", "2 k", "1_0", " ",
            "- 2", "-  2", "+ 2", "2\t",
        ] {
            let (_, tail) = strtod(s);
            assert!(!tail.is_empty(), "reference rejects {s:?}");
        }
    }

    #[test]
    fn strtod_values() {
        assert_eq!(strtod("2").0, 2.0);
        assert_eq!(strtod("2k").0, 2000.0);
        assert_eq!(strtod("2ki").0, 2048.0);
        assert_eq!(strtod("2kB").0, 16000.0);
        assert_eq!(strtod("2h").0, 200.0);
        assert_eq!(strtod("2E").0, 2e18);
        assert_eq!(strtod("0x10").0, 16.0);
        assert!(strtod("inf").0.is_infinite());
        assert!(strtod("nan").0.is_nan());
        // `20dB` is 9.999999999999998, not 10 — which is exactly why an
        // integer option rejects it.
        assert_ne!(strtod("20dB").0, 10.0);
        assert!((strtod("20dB").0 - 10.0).abs() < 1e-12);
    }

    #[test]
    fn the_empty_string_is_zero_but_a_space_is_not_a_number() {
        // Both parse nothing; only the first has an empty tail, because C sets
        // `endptr = nptr` rather than `endptr = post-whitespace`. Verified:
        // `-ac ""` is accepted and `-ac " "` is not.
        assert_eq!(strtod(""), (0.0, ""));
        assert_eq!(strtod(" ").1, " ");
        assert_eq!(strtod("\t\n").1, "\t\n");
    }

    #[test]
    fn strtod_never_panics() {
        for s in [
            "\u{1f600}",
            "0x\u{ff}",
            "\u{0}",
            "-",
            "+",
            "0\u{1f600}",
            "e",
            ".",
        ] {
            let _ = strtod(s);
        }
    }

    // --------------------------------------------------- the three CLI checks

    #[test]
    fn parse_number_messages_are_the_reference_ones() {
        let e = parse_number("ac", "1*2", NumberLimits::int32()).unwrap_err();
        assert_eq!(e.to_string(), "Expected number for ac but found: 1*2");

        let e = parse_number("ac", "1e30", NumberLimits::int32()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "The value for ac was 1e30 which is not within -2147483648.000000 - 2147483647.000000"
        );

        let e = parse_number("fs", "20dB", NumberLimits::int64()).unwrap_err();
        assert_eq!(e.to_string(), "Expected int64 for fs but found 20dB");
    }

    #[test]
    fn the_int64_bound_prints_one_too_high() {
        // D17: `INT64_MAX` through a `double` rounds up, and the reference
        // formats the rounded value. Verified with `-fs 1e30`.
        let e = parse_number("fs", "1e30", NumberLimits::int64()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "The value for fs was 1e30 which is not within \
             -9223372036854775808.000000 - 9223372036854775808.000000"
        );
    }

    #[test]
    fn the_checks_run_in_the_reference_order() {
        // 1e30 is both out of range and integral-looking; range wins.
        assert!(matches!(
            parse_number("fs", "1e30", NumberLimits::int64()),
            Err(CliError::ValueOutOfRange { .. })
        ));
        // 20dB is in range but fractional; integrality catches it.
        assert!(matches!(
            parse_number("fs", "20dB", NumberLimits::int64()),
            Err(CliError::ExpectedInteger { .. })
        ));
        // A float option has neither check.
        assert!(parse_number("max_error_rate", "20dB", NumberLimits::float()).is_ok());
    }

    #[test]
    fn nan_passes_the_range_check_and_fails_integrality() {
        // Verified: `-ac nan` reports `Expected int64 for ac but found nan`,
        // and `-max_error_rate nan` is accepted.
        assert!(matches!(
            parse_number("ac", "nan", NumberLimits::int32()),
            Err(CliError::ExpectedInteger { .. })
        ));
        assert!(parse_number("max_error_rate", "nan", NumberLimits::float()).is_ok());
    }

    #[test]
    fn plain_numbers_that_are_accepted() {
        assert_eq!(parse_number("ac", "2", NumberLimits::int32()), Ok(2.0));
        assert_eq!(parse_number("ac", "2k", NumberLimits::int32()), Ok(2000.0));
        assert_eq!(parse_number("ac", "", NumberLimits::int32()), Ok(0.0));
        assert_eq!(parse_number("ac", " 2", NumberLimits::int32()), Ok(2.0));
    }

    // --------------------------------------------------------- the expression

    #[test]
    fn expression_values_and_their_d17_shapes() {
        // Each confirmed through `-crf`, a filtergraph-free path.
        assert_eq!(eval_once("crf", "2*10"), Ok(20.0));
        assert_eq!(eval_once("crf", "2^3^2"), Ok(64.0)); // left-associative
        assert_eq!(eval_once("crf", "-2^2"), Ok(-4.0)); // sign after the chain
        assert_eq!(eval_once("crf", "1 2"), Ok(12.0)); // whitespace deleted
        assert_eq!(eval_once("crf", "abs.(1)"), Ok(1.0)); // prefix names
        assert_eq!(eval_once("crf", "mod(-5,3)"), Ok(1.0)); // floored
        assert!(eval_once("crf", "max(1,0/0)").is_ok_and(f64::is_nan));
        // `0-20dB` is 0.1, not -10: the sign belongs to the decibel literal.
        let v = eval_once("crf", "0-20dB").unwrap();
        assert!((v - 0.1).abs() < 1e-12, "got {v}");
        // One sign character only.
        assert!(eval_once("crf", "--1").is_ok());
        assert!(eval_once("crf", "---1").is_err());
    }

    #[test]
    fn the_option_dialect_shadows_max_and_min() {
        // Verified on `-crf` and `-cpucount`: `max`/`min` are constants there,
        // so a call is trailing garbage rather than a function application.
        assert!(Expression::compile_for_option("crf", "max(1,2)").is_err());
        assert!(Expression::compile_for_option("crf", "min(1,2)").is_err());
        // Every other builtin still works on that path.
        for s in [
            "abs(-3)",
            "gte(2,1)",
            "sqrt(4)",
            "not(0)",
            "eq(1,1)",
            "gcd(-7,0)",
            "hypot(3,4)",
            "between(1,0,2)",
            "clip(5,0,3)",
            "bitand(1,3)",
            "if(0/0,7)",
            "st(0,1);ld(0)",
        ] {
            assert!(
                Expression::compile_for_option("crf", s).is_ok(),
                "the option dialect should accept {s:?}"
            );
        }
        // The plain dialect is the other way round.
        assert!(Expression::compile("x", "max(1,2)").is_ok());
    }

    #[test]
    fn the_option_constants_are_the_options_own_bounds() {
        // `-crf min-1` is rejected as -2, and crf's minimum is -1. Verified.
        let c = OptionConstants::new(23.0, -1.0, 3.402_82e38);
        assert_eq!(eval_option("crf", "min", c), Ok(-1.0));
        assert_eq!(eval_option("crf", "min-1", c), Ok(-2.0));
        assert_eq!(eval_option("crf", "default", c), Ok(23.0));
        assert_eq!(eval_option("crf", "default*0-2", c), Ok(-2.0));
        assert_eq!(eval_option("crf", "max", c), Ok(3.402_82e38));
    }

    #[test]
    fn unknown_constants_still_bind_the_names() {
        // Acceptance is what matters when the schema is absent.
        assert!(eval_option("x", "max(1,2)", OptionConstants::UNKNOWN).is_err());
        assert!(eval_option("x", "min", OptionConstants::UNKNOWN).is_ok_and(f64::is_nan));
        assert_eq!(eval_option("x", "2*10", OptionConstants::UNKNOWN), Ok(20.0));
    }

    #[test]
    fn expression_errors_use_the_reference_second_line() {
        let e = eval_once("crf", "zzz").unwrap_err();
        assert_eq!(e.to_string(), r#"Unable to parse "crf" option value "zzz""#);
        // The evaluator's own message — the address-free part of the line the
        // reference prints first — is kept alongside it.
        match e {
            CliError::BadExpression { detail, .. } => {
                assert!(detail.contains("undefined constant"), "{detail}");
                assert!(detail.contains("zzz"), "{detail}");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn compile_once_evaluate_many() {
        let e = Expression::compile_with("x", "t*2", &Bindings::new(&["t"])).unwrap();
        assert_eq!(e.eval(&[1.0]), 2.0);
        assert_eq!(e.eval(&[3.0]), 6.0);
        assert_eq!(e.source(), "t*2");
    }

    #[test]
    fn eval_checked_applies_bounds() {
        assert_eq!(
            eval_checked("crf", "2*10", NumberLimits::range(0.0, 51.0)),
            Ok(20.0)
        );
        assert!(matches!(
            eval_checked("crf", "2^3^2", NumberLimits::range(0.0, 51.0)),
            Err(CliError::ValueOutOfRange { .. })
        ));
    }

    #[test]
    fn the_two_grammars_disagree_and_that_is_the_point() {
        // The single most important consequence of the probing: an expression
        // is a hard error on a plain-number option, and a plain number is fine
        // on an expression option.
        assert!(parse_number("ac", "1*2", NumberLimits::int32()).is_err());
        assert_eq!(eval_once("crf", "1*2"), Ok(2.0));
        assert_eq!(parse_number("ac", "2", NumberLimits::int32()), Ok(2.0));
        assert_eq!(eval_once("crf", "2"), Ok(2.0));
    }

    #[test]
    fn value_kind_predicates() {
        assert!(!ValueKind::None.takes_value());
        assert!(ValueKind::Int.takes_value());
        assert_eq!(
            NumberLimits::for_kind(ValueKind::Int).map(|l| l.max),
            Some(f64::from(i32::MAX))
        );
        assert_eq!(
            NumberLimits::for_kind(ValueKind::Int64).map(|l| l.max),
            Some(i64::MAX as f64)
        );
        assert!(ValueKind::Expr.is_expression());
        // A rate is not wholly an expression: `pal` would not compile.
        assert!(!ValueKind::Rate.is_expression());
        assert!(ValueKind::Rate.reaches_evaluator());
        assert!(!ValueKind::Int.is_expression());
        assert!(!ValueKind::Duration.reaches_evaluator());
    }
}
