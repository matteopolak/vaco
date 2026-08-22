//! Structured expression fuzzing.
//!
//! `expr_parse` feeds raw bytes, which mostly exercises the rejection paths —
//! random text is very unlikely to reach a three-argument `taylor` nested
//! inside a `while`. This target builds syntactically valid source from a
//! token grammar instead, so the fuzzer spends its budget on the *evaluator*
//! and on the deep-nesting limits rather than on the first character.
//!
//! Two properties are checked beyond "does not panic":
//!
//! 1. Whitespace is deleted rather than skipped, so inserting it anywhere must
//!    not change the result (crate docs, `strip_whitespace`).
//! 2. Parsing is deterministic and evaluation is pure with respect to a fresh
//!    register file — the same source evaluated twice from `Registers::new()`
//!    gives the same bits.
//! fuzz-crate: vaco-expr
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_expr::{Bindings, Context, Expr, Limits, Registers, strip_whitespace};

/// One piece of expression source. The fuzzer picks a sequence of these and
/// they are concatenated; most sequences are syntactically invalid, but a
/// useful fraction are not, and the valid ones reach far deeper than raw bytes
/// ever would.
#[derive(Arbitrary, Debug)]
enum Token {
    Number(u16),
    Decimal(u16, u8),
    Hex(u32),
    SiSuffixed(u8, bool),
    Decibels(i16),
    Variable(u8),
    Constant(u8),
    Function(u8),
    Comma,
    Open,
    Close,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Semicolon,
    Space,
}

const NAMES: &[&str] = &["w", "h", "t", "n"];
const VALUES: &[f64] = &[1920.0, 1080.0, 0.5, 3.0];
const CONSTS: &[&str] = &["PI", "E", "PHI"];
const FUNCS: &[&str] = &[
    "abs(", "if(", "ifnot(", "while(", "taylor(", "root(", "st(", "ld(", "clip(", "between(",
    "lerp(", "mod(", "gcd(", "bitand(", "max(", "min(", "print(", "random(", "randomi(", "not(",
    "sqrt(", "hypot(", "squish(", "gauss(", "pow(",
];
const SI: &[&str] = &["y", "z", "a", "f", "p", "n", "u", "m", "c", "d", "h", "k", "M", "G", "T"];

fn pick<'a>(table: &[&'a str], index: u8) -> &'a str {
    table
        .get(usize::from(index) % table.len().max(1))
        .copied()
        .unwrap_or("1")
}

fn render(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Number(v) => out.push_str(&v.to_string()),
            Token::Decimal(a, b) => out.push_str(&format!("{a}.{b}")),
            Token::Hex(v) => out.push_str(&format!("0x{v:x}")),
            Token::SiSuffixed(p, binary) => {
                out.push('2');
                out.push_str(pick(SI, *p));
                if *binary {
                    out.push('i');
                }
            }
            Token::Decibels(v) => out.push_str(&format!("{v}dB")),
            Token::Variable(i) => out.push_str(pick(NAMES, *i)),
            Token::Constant(i) => out.push_str(pick(CONSTS, *i)),
            Token::Function(i) => out.push_str(pick(FUNCS, *i)),
            Token::Comma => out.push(','),
            Token::Open => out.push('('),
            Token::Close => out.push(')'),
            Token::Plus => out.push('+'),
            Token::Minus => out.push('-'),
            Token::Star => out.push('*'),
            Token::Slash => out.push('/'),
            Token::Caret => out.push('^'),
            Token::Semicolon => out.push(';'),
            Token::Space => out.push(' '),
        }
    }
    out
}

fn evaluate(src: &str, limits: Limits) -> Option<u64> {
    let expr = Expr::parse_with(src, &Bindings::new(NAMES), limits).ok()?;
    let mut regs = Registers::new();
    Some(
        expr.eval_with(
            &mut Context::new(VALUES, &mut regs)
                .with_limits(limits)
                .with_time(1_700_000_000.0),
        )
        .to_bits(),
    )
}

fuzz_target!(|tokens: Vec<Token>| {
    if tokens.len() > 512 {
        return;
    }
    let src = render(&tokens);
    let limits = Limits {
        max_iterations: 4096,
        ..Limits::default()
    };

    let first = evaluate(&src, limits);
    // Evaluation from a fresh register file is a pure function of the source.
    assert_eq!(first, evaluate(&src, limits), "evaluation is not deterministic");

    // Whitespace is deleted before parsing, so it cannot change the outcome.
    let stripped = strip_whitespace(&src);
    assert_eq!(
        first,
        evaluate(&stripped, limits),
        "whitespace changed the result: {src:?} vs {stripped:?}"
    );
});
