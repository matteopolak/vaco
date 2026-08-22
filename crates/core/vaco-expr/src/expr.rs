//! The compiled expression: a flat node arena, its bindings, and its limits.

use crate::func::Func;

/// One node of a compiled expression.
///
/// Children are indices into [`Expr::nodes`], not pointers: the whole
/// expression is one allocation, so evaluating it walks contiguous memory
/// instead of chasing a `Box` graph. Parsing allocates; evaluating does not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Op {
    Const(f64),
    /// Index into the value slice passed to [`Expr::eval`].
    Var(u32),
    /// Unary minus, applied *after* the whole `^` chain — see [`crate::parse`].
    Neg(u32),
    Add(u32, u32),
    Mul(u32, u32),
    Div(u32, u32),
    Pow(u32, u32),
    /// `a;b` — evaluate both, yield `b`.
    Seq(u32, u32),
    /// A function call. Unused argument slots are `u32::MAX`.
    Call(Func, u8, [u32; 3]),
}

/// Names the parser may resolve, supplied by the caller.
///
/// Variable names are resolved to slice indices at parse time, so evaluation
/// never does a string comparison. This is the whole point of separating parse
/// from eval: a filter parses `w`, `h`, `t`, `n` once and then evaluates
/// millions of times with a four-element `&[f64]`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bindings<'a> {
    vars: &'a [&'a str],
    funcs: &'a [(&'a str, u8)],
}

impl<'a> Bindings<'a> {
    /// No variables and no caller functions — only the builtins.
    pub const EMPTY: Self = Self {
        vars: &[],
        funcs: &[],
    };

    /// Binds `vars` to slice positions 0, 1, 2, ... in that order.
    #[must_use]
    pub const fn new(vars: &'a [&'a str]) -> Self {
        Self { vars, funcs: &[] }
    }

    /// Adds caller-supplied functions as `(name, arity)` pairs.
    ///
    /// Arity is exact; a call with a different count is a parse error, the same
    /// as for a builtin. The evaluator dispatches them through
    /// [`crate::Context::with_functions`], keyed by position in this slice.
    #[must_use]
    pub const fn with_functions(mut self, funcs: &'a [(&'a str, u8)]) -> Self {
        self.funcs = funcs;
        self
    }

    /// The bound variable names, in slice order.
    #[must_use]
    pub const fn vars(&self) -> &'a [&'a str] {
        self.vars
    }

    /// The bound caller functions.
    #[must_use]
    pub const fn functions(&self) -> &'a [(&'a str, u8)] {
        self.funcs
    }
}

/// Bounds on how much expression the parser will accept and how long
/// evaluation may run.
///
/// The two parse limits are not arbitrary: they reproduce the reference's
/// acceptance boundary exactly, which was measured by bisection.
///
/// | Shape | Reference accepts | Reference rejects | Limit that decides it |
/// |---|---|---|---|
/// | `((((…1…))))` | 99 deep | 100 deep | `max_parse_depth` |
/// | `abs(abs(…1…))` | 99 deep | 100 deep | `max_parse_depth` |
/// | `1+1+1…` | 100 operators | 101 operators | `max_node_depth` |
/// | `1;1;1…` | 100 operators | 101 operators | `max_node_depth` |
///
/// `max_node_depth` also bounds the evaluator's own recursion, which is why it
/// is a limit and not merely a compatibility knob: without it a long flat chain
/// would build a left-deep tree and overflow the stack during evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum nesting of parenthesised or argument sub-expressions.
    pub max_parse_depth: u32,
    /// Maximum depth of the resulting node tree.
    pub max_node_depth: u32,
    /// Maximum total loop iterations in one evaluation, shared by `while`,
    /// `root` and `taylor`.
    ///
    /// # Divergence from the reference
    ///
    /// `while(1,x)` makes the reference loop forever — verified: `ffmpeg` had
    /// to be `SIGKILL`ed, it does not even answer `SIGTERM` because it never
    /// leaves the evaluator. We refuse to reproduce a hang (D6 makes
    /// non-termination a fuzzing finding), so a loop that exhausts this budget
    /// stops and yields its last value.
    ///
    /// The budget is shared rather than per-loop because `root` and `taylor`
    /// are individually bounded at 1000 iterations but *nest*: three nested
    /// `taylor` calls are a billion body evaluations, which is a hang by any
    /// practical measure even though every individual loop terminates. The
    /// default leaves a single `taylor` (1000 iterations) and even two nested
    /// ones (a million) untouched.
    ///
    /// Set it to `u64::MAX` to get the reference's unbounded behaviour back.
    pub max_iterations: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parse_depth: 100,
            max_node_depth: 101,
            max_iterations: 1 << 24,
        }
    }
}

/// A parsed, reusable expression.
///
/// Cheap to clone (one `Vec`), `Send + Sync`, and free of interior mutability:
/// evaluation state lives in the caller's [`crate::Registers`], so one `Expr`
/// can be shared by several threads each with their own registers.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub(crate) nodes: Vec<Op>,
    pub(crate) root: u32,
    pub(crate) var_count: usize,
    pub(crate) uses_registers: bool,
    pub(crate) limits: Limits,
}

impl Expr {
    /// How many variables this expression was parsed against.
    ///
    /// The slice given to [`Expr::eval`] should be at least this long.
    #[must_use]
    pub const fn var_count(&self) -> usize {
        self.var_count
    }

    /// Whether the expression reads or writes `ld`/`st` registers, directly or
    /// through `random`, `root` or `taylor`.
    ///
    /// A caller in a per-frame loop can use this to decide whether registers
    /// have to persist between frames. They do in the reference: an expression
    /// evaluated once per audio sample with body `st(0,ld(0)+1)` counts
    /// 1, 2, 3, 4 — verified.
    #[must_use]
    pub const fn uses_registers(&self) -> bool {
        self.uses_registers
    }

    /// Number of nodes in the compiled form. Diagnostic only.
    #[must_use]
    pub const fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The limits this expression was parsed under; also the evaluation
    /// defaults.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }
}
