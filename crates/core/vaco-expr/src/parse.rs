//! Recursive-descent parser.
//!
//! The grammar is the reference's, and several of its shapes are unusual. Each
//! one is marked `D17:` below with what the conventional form would be, what
//! the reference does instead, and the probe that established it. None of them
//! may be "corrected": they decide which command lines are accepted and what
//! they evaluate to.
//!
//! ```text
//! expr     := subexpr (';' subexpr)*            left-associative
//! subexpr  := term (('+'|'-') term)*            operator NOT consumed — see D17 below
//! term     := factor (('*'|'/') factor)*        left-associative
//! factor   := unary ('^' unary)*                left-associative, sign applied last
//! unary    := ['+'|'-'] primary                 AT MOST ONE sign character
//! primary  := number | '(' expr ')' | name | name '(' expr [, expr [, expr]] ')'
//! ```

use crate::error::{ParseError, ParseErrorKind};
use crate::expr::{Bindings, Expr, Limits, Op};
use crate::func::{BUILTINS, Func};
use crate::lex::{scan_number, strip_whitespace, strmatch};

/// The three constants the language defines itself.
const CONSTANTS: &[(&str, f64)] = &[
    ("PI", core::f64::consts::PI),
    ("E", core::f64::consts::E),
    ("PHI", 1.618_033_988_749_895_f64),
];

impl Expr {
    /// Parses `src` with the default [`Limits`].
    ///
    /// # Errors
    ///
    /// Returns the reference's rejection reason; see [`ParseErrorKind`].
    pub fn parse(src: &str, bindings: &Bindings<'_>) -> Result<Self, ParseError> {
        Self::parse_with(src, bindings, Limits::default())
    }

    /// Parses `src` with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns the reference's rejection reason; see [`ParseErrorKind`].
    pub fn parse_with(
        src: &str,
        bindings: &Bindings<'_>,
        limits: Limits,
    ) -> Result<Self, ParseError> {
        // D17: whitespace is deleted from the whole string before parsing, not
        // skipped between tokens. `crate::lex::strip_whitespace` has the
        // evidence; the consequence here is that the parser never has to think
        // about whitespace at all.
        let text = strip_whitespace(src);
        let mut p = Parser {
            s: &text,
            pos: 0,
            nodes: Vec::new(),
            depths: Vec::new(),
            bindings,
            limits,
            depth: 0,
            uses_registers: false,
        };
        let root = p.parse_expr()?;
        if p.pos != text.len() {
            return Err(p.err(ParseErrorKind::TrailingGarbage));
        }
        Ok(Self {
            nodes: p.nodes,
            root,
            var_count: bindings.vars().len(),
            uses_registers: p.uses_registers,
            limits,
        })
    }
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    nodes: Vec<Op>,
    /// Tree depth of the node at the same index, so the depth limit costs one
    /// `max` per node rather than a traversal.
    depths: Vec<u32>,
    bindings: &'a Bindings<'a>,
    limits: Limits,
    depth: u32,
    uses_registers: bool,
}

type PResult<T> = Result<T, ParseError>;

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        self.s.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<u8> {
        self.rest().as_bytes().first().copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn err(&self, kind: ParseErrorKind) -> ParseError {
        let tail = self.rest();
        let cut = tail.char_indices().nth(64).map_or(tail.len(), |(i, _)| i);
        ParseError {
            kind,
            offset: self.pos,
            tail: tail.get(..cut).unwrap_or(tail).to_owned(),
        }
    }

    fn depth_of(&self, node: u32) -> u32 {
        self.depths.get(node as usize).copied().unwrap_or(0)
    }

    fn push(&mut self, op: Op, children: &[u32]) -> PResult<u32> {
        let depth = 1 + children
            .iter()
            .map(|c| self.depth_of(*c))
            .max()
            .unwrap_or(0);
        if depth > self.limits.max_node_depth {
            return Err(self.err(ParseErrorKind::TooDeep));
        }
        let idx = self.nodes.len();
        // A u32 index caps an expression at 4 billion nodes; the depth limits
        // above make that unreachable long before it matters, but the cast is
        // checked anyway so the parser stays total.
        let Ok(idx) = u32::try_from(idx) else {
            return Err(self.err(ParseErrorKind::TooDeep));
        };
        self.nodes.push(op);
        self.depths.push(depth);
        Ok(idx)
    }

    // ----------------------------------------------------------------- expr

    fn parse_expr(&mut self) -> PResult<u32> {
        self.depth += 1;
        let r = self.parse_expr_inner();
        self.depth -= 1;
        r
    }

    fn parse_expr_inner(&mut self) -> PResult<u32> {
        if self.depth > self.limits.max_parse_depth {
            return Err(self.err(ParseErrorKind::TooDeep));
        }
        let mut e = self.parse_subexpr()?;
        while self.eat(b';') {
            let rhs = self.parse_subexpr()?;
            e = self.push(Op::Seq(e, rhs), &[e, rhs])?;
        }
        Ok(e)
    }

    /// Additive level.
    ///
    /// # D17: subtraction is addition of a signed term
    ///
    /// Conventionally `a - b` is a subtraction node. Here the loop **does not
    /// consume** the `-`; it hands the operator to the operand parser, which
    /// treats it as the term's own unary sign. So every additive node is an
    /// addition and `a-b` is `add(a, -b)`.
    ///
    /// That is invisible for ordinary arithmetic and very visible for decibel
    /// literals, where the sign belongs to the literal rather than to the
    /// operator: `0-20dB` evaluates to **0.1**, not to -10, because it parses
    /// as `add(0, -20dB)` and `-20dB` is `10^(-20/20)`. Verified against the
    /// reference, along with `1*-20dB` = 0.1 and `-(20dB)` = -10.
    ///
    /// It also explains `1-+1` = 0 and `1--1` = 2: the operand parser takes one
    /// sign character and the number lexer takes the next.
    fn parse_subexpr(&mut self) -> PResult<u32> {
        let mut e = self.parse_term()?;
        while matches!(self.peek(), Some(b'+' | b'-')) {
            let rhs = self.parse_term()?;
            e = self.push(Op::Add(e, rhs), &[e, rhs])?;
        }
        Ok(e)
    }

    fn parse_term(&mut self) -> PResult<u32> {
        let mut e = self.parse_factor()?;
        loop {
            let op = match self.peek() {
                Some(b'*') => Op::Mul as fn(u32, u32) -> Op,
                Some(b'/') => Op::Div as fn(u32, u32) -> Op,
                _ => break,
            };
            self.pos += 1;
            let rhs = self.parse_factor()?;
            e = self.push(op(e, rhs), &[e, rhs])?;
        }
        Ok(e)
    }

    /// Power level.
    ///
    /// # D17: `^` is left-associative, and the base's sign is applied last
    ///
    /// Mathematically `a^b^c` is `a^(b^c)` — exponentiation is right-
    /// associative in every textbook and in every language that has the
    /// operator. Here `2^3^2` is **64**, i.e. `(2^3)^2`, not 512.
    ///
    /// And unary minus binds *looser* than `^` on the base but *tighter* on the
    /// exponent: `-2^2` is -4 (the sign is applied to the finished chain) while
    /// `2^-2` is 0.25 (the exponent takes its own sign). `2^-2^2` is 0.0625,
    /// which is `pow(pow(2,-2),2)` — both rules at once. All three verified.
    fn parse_factor(&mut self) -> PResult<u32> {
        let (mut e, negated) = self.parse_unary()?;
        while self.eat(b'^') {
            let (rhs, rhs_neg) = self.parse_unary()?;
            let rhs = if rhs_neg {
                self.push(Op::Neg(rhs), &[rhs])?
            } else {
                rhs
            };
            e = self.push(Op::Pow(e, rhs), &[e, rhs])?;
        }
        if negated {
            e = self.push(Op::Neg(e), &[e])?;
        }
        Ok(e)
    }

    /// Unary sign.
    ///
    /// # D17: exactly one sign character, and never a decibel's own minus
    ///
    /// A conventional unary parser loops, so `---1` is -1. Here it consumes
    /// **at most one** `+` or `-` and then hands over to the primary parser,
    /// whose number lexer accepts a sign of its own. That is why `--1` is 1
    /// (sign, then the literal `-1`) while `---1` is a *parse error* — the
    /// lexer cannot read `--1` as a number — and why `--PI` and `--abs(1)` are
    /// errors too, since neither `-PI` nor `-abs` is a literal. All verified.
    ///
    /// The second rule is the decibel guard: a `-` that starts a `<number>dB`
    /// literal is left for the lexer, because `-20dB` means "minus twenty
    /// decibels" (0.1) and not "the negative of twenty decibels" (-10).
    fn parse_unary(&mut self) -> PResult<(u32, bool)> {
        let mut negated = false;
        if !starts_db_literal(self.rest()) {
            match self.peek() {
                Some(b'-') => {
                    negated = true;
                    self.pos += 1;
                }
                Some(b'+') => self.pos += 1,
                _ => {}
            }
        }
        let node = self.parse_primary()?;
        Ok((node, negated))
    }

    // -------------------------------------------------------------- primary

    fn parse_primary(&mut self) -> PResult<u32> {
        let rest = self.rest();

        if let Some(n) = scan_number(rest) {
            self.pos += n.len;
            return self.push(Op::Const(n.value), &[]);
        }

        if self.eat(b'(') {
            let inner = self.parse_expr()?;
            if !self.eat(b')') {
                return Err(self.err(ParseErrorKind::MissingCloseParen));
            }
            return Ok(inner);
        }

        // D17: constants and variables are matched by PREFIX against the raw
        // input, before any attempt to find a '('. That is why `PI(1)` is
        // rejected for trailing garbage rather than as an unknown function --
        // `PI` matched and consumed two bytes. See crate::lex::strmatch.
        //
        // Caller-supplied names are tried before the three builtin constants,
        // so a caller may shadow PI/E/PHI. The reference's own order here could
        // not be established by probing (no shipped filter names a variable
        // PI, E or PHI), so this is our choice, recorded rather than assumed.
        for (i, name) in self.bindings.vars().iter().enumerate() {
            if strmatch(rest, name) {
                self.pos += name.len();
                let Ok(i) = u32::try_from(i) else {
                    return Err(self.err(ParseErrorKind::UndefinedConstant));
                };
                return self.push(Op::Var(i), &[]);
            }
        }
        for (name, value) in CONSTANTS {
            if strmatch(rest, name) {
                self.pos += name.len();
                return self.push(Op::Const(*value), &[]);
            }
        }

        self.parse_call(rest)
    }

    /// A function call.
    ///
    /// # D17: the name is "everything up to the next `(`"
    ///
    /// A conventional parser reads an identifier from a defined character set.
    /// The reference scans forward to the next `(` anywhere in the remaining
    /// input and treats everything before it as the name. Observable:
    /// `foo+abs(1)` reports *unknown function* (the name it collected was
    /// `foo+abs`) rather than *undefined constant*, and `abs)1(` fails while
    /// parsing an empty argument — it collected `abs)1` as the name and
    /// started arguments after the `(`. Both verified.
    ///
    /// The collected name is then matched by the same prefix rule as constants,
    /// which is why `abs.(1)` is `abs(1)` but `abs_(1)` is unknown.
    fn parse_call(&mut self, rest: &str) -> PResult<u32> {
        let Some(open) = rest.find('(') else {
            return Err(self.err(ParseErrorKind::UndefinedConstant));
        };
        let name = rest.get(..open).unwrap_or("");
        let name_start = self.pos;
        self.pos += open + 1;

        let mut args = [u32::MAX; 3];
        let mut argc = 0usize;
        loop {
            let arg = self.parse_expr()?;
            if let Some(slot) = args.get_mut(argc) {
                *slot = arg;
            }
            argc += 1;
            if argc == args.len() || !self.eat(b',') {
                break;
            }
        }
        if !self.eat(b')') {
            return Err(self.err(ParseErrorKind::MissingCloseParen));
        }

        let Ok(argc_u8) = u8::try_from(argc) else {
            return Err(self.err(ParseErrorKind::WrongArity));
        };
        let (func, min, max) = self.resolve(name).ok_or_else(|| ParseError {
            kind: ParseErrorKind::UnknownFunction,
            offset: name_start,
            tail: name.to_owned(),
        })?;
        if argc_u8 < min || argc_u8 > max {
            return Err(ParseError {
                kind: ParseErrorKind::WrongArity,
                offset: name_start,
                tail: name.to_owned(),
            });
        }
        if func.touches_registers() {
            self.uses_registers = true;
        }
        let children: Vec<u32> = args.iter().copied().take(argc).collect();
        self.push(Op::Call(func, argc_u8, args), &children)
    }

    fn resolve(&self, name: &str) -> Option<(Func, u8, u8)> {
        for (n, f, min, max) in BUILTINS {
            if strmatch(name, n) {
                return Some((*f, *min, *max));
            }
        }
        for (i, (n, arity)) in self.bindings.functions().iter().enumerate() {
            if strmatch(name, n) {
                let id = u16::try_from(i).ok()?;
                return Some((Func::Extern(id), *arity, *arity));
            }
        }
        None
    }
}

/// True when `s` begins with a `-<number>dB` literal.
///
/// The reference does not let the unary parser swallow that minus, because a
/// decibel literal carries its own sign. See [`Parser::parse_unary`].
fn starts_db_literal(s: &str) -> bool {
    if !s.starts_with('-') {
        return false;
    }
    scan_number(s).is_some_and(|n| {
        // `scan_number` already applied the dB conversion, so the only way to
        // know it was a dB literal is to look at what it consumed.
        s.get(..n.len).is_some_and(|lit| lit.ends_with("dB"))
    })
}
