//! The evaluator.
//!
//! Everything is `f64`. Every deviation from what IEEE-754 or a C library would
//! do by default is marked `D17:` with the probe that established it.
//!
//! `clippy::float_cmp` is off for the whole module on purpose. Every exact
//! comparison here *is* the specification: truthiness is `x != 0`, `not(x)` is
//! `x == 0`, and the secant loop in `root` terminates on an exact fixed point.
//! Comparing "within some margin of error" would be a behavioural change, not
//! a robustness improvement.
#![allow(clippy::float_cmp, reason = "exact comparison is the specification")]

use core::fmt;

use crate::expr::{Expr, Limits, Op};
use crate::func::Func;

/// The ten internal registers `st` and `ld` address.
///
/// The reference keeps these **across evaluations** of the same expression: an
/// expression `st(0,ld(0)+1)` evaluated once per audio sample yields 1, 2, 3,
/// 4 — verified. So they live in the caller, not in [`Expr`], which is also
/// what makes one `Expr` shareable between threads.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Registers {
    slots: [f64; 10],
}

impl Registers {
    /// All ten registers zeroed, which is the reference's initial state.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: [0.0; 10] }
    }

    /// Reads register `index`, clamped to 0..=9 exactly as `ld` clamps it.
    #[must_use]
    pub fn get(&self, index: usize) -> f64 {
        self.slots.get(index.min(9)).copied().unwrap_or(0.0)
    }

    /// Writes register `index`, clamped to 0..=9 exactly as `st` clamps it.
    pub fn set(&mut self, index: usize, value: f64) {
        if let Some(slot) = self.slots.get_mut(index.min(9)) {
            *slot = value;
        }
    }
}

/// The log level `print(t)` reports when no level is given.
///
/// Matches the reference's default of `AV_LOG_INFO`.
pub const DEFAULT_PRINT_LEVEL: f64 = 32.0;

/// Everything an evaluation needs besides the expression.
///
/// Built with a chain of `with_*` methods so the common per-frame call stays a
/// single line and allocates nothing:
///
/// ```
/// # use vaco_expr::{Bindings, Expr, Registers, Context};
/// let e = Expr::parse("if(gt(a,16/9),1280,-1)", &Bindings::new(&["a"]))?;
/// let mut regs = Registers::new();
/// let v = e.eval_with(&mut Context::new(&[1.85], &mut regs));
/// assert_eq!(v, 1280.0);
/// # Ok::<(), vaco_expr::ParseError>(())
/// ```
pub struct Context<'a> {
    vars: &'a [f64],
    regs: &'a mut Registers,
    limits: Limits,
    print: Option<&'a mut (dyn FnMut(f64, f64) + 'a)>,
    funcs: Option<&'a mut (dyn FnMut(u16, &[f64]) -> f64 + 'a)>,
    now: Option<f64>,
}

impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("vars", &self.vars)
            .field("regs", &self.regs)
            .field("limits", &self.limits)
            .field("print", &self.print.is_some())
            .field("funcs", &self.funcs.is_some())
            .field("now", &self.now)
            .finish()
    }
}

impl<'a> Context<'a> {
    /// Variable values, positionally matching the [`crate::Bindings`] the
    /// expression was parsed with, plus a register file.
    #[must_use]
    pub fn new(vars: &'a [f64], regs: &'a mut Registers) -> Self {
        Self {
            vars,
            regs,
            limits: Limits::default(),
            print: None,
            funcs: None,
            now: None,
        }
    }

    /// Routes `print(t[,level])` to `sink`. Without one, `print` is silent and
    /// still returns its first argument, as the reference does.
    #[must_use]
    pub fn with_print(mut self, sink: &'a mut (dyn FnMut(f64, f64) + 'a)) -> Self {
        self.print = Some(sink);
        self
    }

    /// Supplies the caller functions declared through
    /// [`crate::Bindings::with_functions`]. The `u16` is the function's index
    /// in that slice.
    #[must_use]
    pub fn with_functions(mut self, funcs: &'a mut (dyn FnMut(u16, &[f64]) -> f64 + 'a)) -> Self {
        self.funcs = Some(funcs);
        self
    }

    /// Pins what `time(0)` returns, in seconds. Without it the system clock is
    /// read. Tests and reproducible renders want this pinned.
    #[must_use]
    pub const fn with_time(mut self, seconds: f64) -> Self {
        self.now = Some(seconds);
        self
    }

    /// Overrides the evaluation limits (only `max_while_iterations` is read
    /// here; the other two bound parsing).
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The register file, so a caller can seed `random` or inspect state.
    #[must_use]
    pub fn registers(&self) -> &Registers {
        self.regs
    }

    /// Mutable access to the register file.
    pub fn registers_mut(&mut self) -> &mut Registers {
        self.regs
    }
}

impl Expr {
    /// Evaluates with a fresh register file and no print sink.
    ///
    /// `vars` matches the [`crate::Bindings`] used at parse time, positionally.
    /// A slice shorter than [`Expr::var_count`] yields `NaN` for the missing
    /// variables rather than panicking — this crate never panics on any input.
    #[must_use]
    pub fn eval(&self, vars: &[f64]) -> f64 {
        let mut regs = Registers::new();
        self.eval_with(&mut Context::new(vars, &mut regs))
    }

    /// Evaluates with a caller-supplied [`Context`].
    #[must_use]
    pub fn eval_with(&self, ctx: &mut Context<'_>) -> f64 {
        let mut budget = ctx.limits.max_iterations;
        self.node(ctx, self.root, &mut budget)
    }

    fn op(&self, index: u32) -> Op {
        self.nodes
            .get(index as usize)
            .copied()
            .unwrap_or(Op::Const(f64::NAN))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per language construct reads better than a dispatch table"
    )]
    fn node(&self, ctx: &mut Context<'_>, index: u32, budget: &mut u64) -> f64 {
        match self.op(index) {
            Op::Const(v) => v,
            Op::Var(i) => ctx.vars.get(i as usize).copied().unwrap_or(f64::NAN),
            Op::Neg(a) => -self.node(ctx, a, budget),
            Op::Add(a, b) => self.node(ctx, a, budget) + self.node(ctx, b, budget),
            Op::Mul(a, b) => self.node(ctx, a, budget) * self.node(ctx, b, budget),
            Op::Div(a, b) => self.node(ctx, a, budget) / self.node(ctx, b, budget),
            Op::Pow(a, b) => self.node(ctx, a, budget).powf(self.node(ctx, b, budget)),
            Op::Seq(a, b) => {
                let _ = self.node(ctx, a, budget);
                self.node(ctx, b, budget)
            }
            Op::Call(func, argc, args) => self.call(ctx, func, argc, args, budget),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the builtin table is one flat match by design"
    )]
    fn call(
        &self,
        ctx: &mut Context<'_>,
        func: Func,
        argc: u8,
        args: [u32; 3],
        budget: &mut u64,
    ) -> f64 {
        let a0 = arg(args, 0);
        let a1 = arg(args, 1);
        let a2 = arg(args, 2);

        // The lazy forms first: these must NOT evaluate every argument.
        match func {
            // D17: truthiness is `x != 0`, so NaN is TRUE. `if(0/0,7)` is 7,
            // while `ifnot(0/0,7)` and `not(0/0)` are both 0. Verified. A
            // conventional "is this number true" test would treat NaN as
            // neither, and several languages make it false.
            Func::If => {
                return if self.node(ctx, a0, budget) != 0.0 {
                    self.node(ctx, a1, budget)
                } else if argc == 3 {
                    self.node(ctx, a2, budget)
                } else {
                    0.0
                };
            }
            Func::IfNot => {
                return if self.node(ctx, a0, budget) == 0.0 {
                    self.node(ctx, a1, budget)
                } else if argc == 3 {
                    self.node(ctx, a2, budget)
                } else {
                    0.0
                };
            }
            Func::While => {
                let mut last = f64::NAN;
                while self.node(ctx, a0, budget) != 0.0 {
                    last = self.node(ctx, a1, budget);
                    if !spend(budget) {
                        break;
                    }
                }
                return last;
            }
            Func::Taylor => return self.taylor(ctx, argc, args, budget),
            Func::Root => return self.root(ctx, args, budget),
            Func::St => {
                let index = reg_index(self.node(ctx, a0, budget));
                let value = self.node(ctx, a1, budget);
                ctx.regs.set(index, value);
                return value;
            }
            _ => {}
        }

        let x = self.node(ctx, a0, budget);
        let y = if argc >= 2 {
            self.node(ctx, a1, budget)
        } else {
            f64::NAN
        };
        let z = if argc >= 3 {
            self.node(ctx, a2, budget)
        } else {
            f64::NAN
        };

        match func {
            Func::Abs => x.abs(),
            Func::Acos => x.acos(),
            Func::Asin => x.asin(),
            Func::Atan => x.atan(),
            Func::Atan2 => x.atan2(y),
            // between/clip/lerp use `min`/`max` in the reference's argument
            // order (x, min, max); NaN falls through both comparisons.
            Func::Between => f64::from(u8::from(x >= y && x <= z)),
            Func::BitAnd => bitwise(x, y, |a, b| a & b),
            Func::BitOr => bitwise(x, y, |a, b| a | b),
            Func::Ceil => x.ceil(),
            // D17: `clip` yields NaN when the bounds are unordered, including
            // when either bound is NaN -- `clip(0,1,0)`, `clip(5,0/0,1)` and
            // `clip(5,0,0/0)` are all NaN. A conventional clamp would either
            // panic on min>max or silently pick one bound.
            Func::Clip => {
                if y <= z {
                    if x < y {
                        y
                    } else if x > z {
                        z
                    } else {
                        x
                    }
                } else {
                    f64::NAN
                }
            }
            Func::Cos => x.cos(),
            Func::Cosh => x.cosh(),
            Func::Eq => f64::from(u8::from(x == y)),
            Func::Exp => x.exp(),
            Func::Floor => x.floor(),
            Func::Gauss => (-x * x / 2.0).exp() / (2.0 * core::f64::consts::PI).sqrt(),
            Func::Gcd => binary_gcd(x as i64, y as i64) as f64,
            Func::Gt => f64::from(u8::from(x > y)),
            Func::Gte => f64::from(u8::from(x >= y)),
            Func::Hypot => x.hypot(y),
            Func::IsInf => f64::from(u8::from(x.is_infinite())),
            Func::IsNan => f64::from(u8::from(x.is_nan())),
            Func::Ld => ctx.regs.get(reg_index(x)),
            Func::Lerp => x + z * (y - x),
            Func::Log => x.ln(),
            Func::Lt => f64::from(u8::from(x < y)),
            Func::Lte => f64::from(u8::from(x <= y)),
            // D17: `max`/`min` are comparison selects, not `fmax`/`fmin`. They
            // propagate NaN asymmetrically: `max(0/0,1)` is 1 but `max(1,0/0)`
            // is NaN, and the same for `min`. C's fmax/fmin would return the
            // non-NaN operand in both directions. Verified in all four cases.
            Func::Max => {
                if x > y {
                    x
                } else {
                    y
                }
            }
            Func::Min => {
                if x < y {
                    x
                } else {
                    y
                }
            }
            // D17: `mod` is a floored modulo built as `x - floor(x/y)*y`, not
            // C's `fmod` (which truncates). `mod(-5,3)` is 1, where fmod gives
            // -2; `mod(5,-3)` is -1, where fmod gives 2. It also inherits the
            // formula's NaN edges: `mod(5,0)` and `mod(2,1/0)` are NaN rather
            // than the NaN/2 that fmod would give.
            Func::Mod => x - (x / y).floor() * y,
            Func::Not => f64::from(u8::from(x == 0.0)),
            Func::Pow => x.powf(y),
            Func::Print => {
                let level = if argc >= 2 { y } else { DEFAULT_PRINT_LEVEL };
                if let Some(sink) = ctx.print.as_deref_mut() {
                    sink(x, level);
                }
                x
            }
            Func::Random => {
                let index = reg_index(x);
                next_random(ctx, index)
            }
            Func::RandomI => {
                let index = reg_index(x);
                let r = next_random(ctx, index);
                y + (z - y) * r
            }
            Func::Round => x.round(),
            Func::Sgn => {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }
            Func::Sin => x.sin(),
            Func::Sinh => x.sinh(),
            Func::Sqrt => x.sqrt(),
            Func::Squish => 1.0 / (1.0 + (4.0 * x).exp()),
            Func::Tan => x.tan(),
            Func::Tanh => x.tanh(),
            Func::Time => ctx.now.unwrap_or_else(wallclock_seconds),
            Func::Trunc => x.trunc(),
            Func::Extern(id) => {
                let values = [x, y, z];
                let n = usize::from(argc).min(values.len());
                let slice = values.get(..n).unwrap_or(&[]);
                ctx.funcs.as_deref_mut().map_or(f64::NAN, |f| f(id, slice))
            }
            // Handled above; unreachable in practice, and returning NaN keeps
            // the evaluator total rather than adding a panic.
            Func::If | Func::IfNot | Func::While | Func::Taylor | Func::Root | Func::St => f64::NAN,
        }
    }

    /// `taylor(expr, x[, idx])` — sums `expr(i) * x^i / i!` for i = 0..1000.
    ///
    /// The register is set to `i` before each evaluation and restored
    /// afterwards, which is verified: `st(0,7);taylor(ld(0),1);ld(0)` is 7.
    /// There is no early exit on a zero term — `taylor(ld(0)-5,1)` sums the
    /// whole series (-10.873127313836182) rather than stopping at i=5.
    fn taylor(&self, ctx: &mut Context<'_>, argc: u8, args: [u32; 3], budget: &mut u64) -> f64 {
        let body = arg(args, 0);
        let x = self.node(ctx, arg(args, 1), budget);
        let index = if argc == 3 {
            reg_index(self.node(ctx, arg(args, 2), budget))
        } else {
            0
        };
        let saved = ctx.regs.get(index);
        let mut term = 1.0_f64;
        let mut sum = 0.0_f64;
        for i in 0..1000_u32 {
            if !spend(budget) {
                break;
            }
            ctx.regs.set(index, f64::from(i));
            let v = self.node(ctx, body, budget);
            sum += term * v;
            term *= x / f64::from(i + 1);
            if term == 0.0 {
                break;
            }
        }
        ctx.regs.set(index, saved);
        sum
    }

    /// `root(expr, max)` — finds an `x` for which `expr(ld(0))` is zero.
    ///
    /// A plain secant iteration seeded with `x0 = 0`, `x1 = max`. That model
    /// reproduces the reference bit-for-bit on nine of the ten cases probed,
    /// including the surprising ones: `root(ld(0)-2,1)` is exactly 2 (outside
    /// the interval), `root(1,10)` is 10 and `root(ld(0),10)` is 0.
    ///
    /// # Known divergence
    ///
    /// `root(cos(ld(0)),10)` gives 7.853981633974484 in the reference (5*PI/2)
    /// where an unconstrained secant wanders off to -139.8. The reference must
    /// constrain the iterate once the endpoints bracket a sign change; the
    /// exact rule could not be established by black-box probing. Recorded in
    /// `docs/core/vaco-expr.md` rather than guessed at.
    fn root(&self, ctx: &mut Context<'_>, args: [u32; 3], budget: &mut u64) -> f64 {
        let body = arg(args, 0);
        let x_max = self.node(ctx, arg(args, 1), budget);
        let saved = ctx.regs.get(0);

        let mut x0 = 0.0_f64;
        let mut x1 = x_max;
        ctx.regs.set(0, x0);
        let mut f0 = self.node(ctx, body, budget);
        ctx.regs.set(0, x1);
        let mut f1 = self.node(ctx, body, budget);

        for _ in 0..1000_u32 {
            if f1 == f0 || !spend(budget) {
                break;
            }
            let x2 = x1 - f1 * (x1 - x0) / (f1 - f0);
            if !x2.is_finite() || x2 == x1 {
                break;
            }
            x0 = x1;
            f0 = f1;
            x1 = x2;
            ctx.regs.set(0, x1);
            f1 = self.node(ctx, body, budget);
        }
        ctx.regs.set(0, saved);
        x1
    }
}

/// Charges one iteration to the shared loop budget.
///
/// Returns `false` once it is exhausted, at which point every loop unwinds and
/// evaluation finishes with whatever it has. Saturating rather than wrapping,
/// so `u64::MAX` really does mean "no limit worth reaching".
fn spend(budget: &mut u64) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

/// Reads argument slot `n`, or the sentinel when the call has fewer arguments.
fn arg(args: [u32; 3], n: usize) -> u32 {
    args.get(n).copied().unwrap_or(u32::MAX)
}

/// Register indices are clamped, never rejected.
///
/// # D17: out-of-range `ld`/`st` indices clamp to 0..=9
///
/// `st(100,5);ld(100)` stores and loads the same slot and yields 5, while
/// `ld(100)` on its own is 0 — both are register 9. `ld(-1)` is register 0, and
/// `st(3.7,5);ld(3)` is 5, so the index truncates towards zero first. NaN
/// becomes 0 and the infinities become 0 and 9. All verified. A conventional
/// implementation would reject the index or wrap it.
fn reg_index(v: f64) -> usize {
    // `as i64` in Rust is defined as saturating with NaN mapping to zero, which
    // is exactly what the reference's hardware conversion does on the platform
    // this was measured on.
    (v as i64).clamp(0, 9) as usize
}

/// `bitand`/`bitor`.
///
/// # D17: NaN in, NaN out — but the infinities convert
///
/// A bitwise operation on a float has to convert first, and C leaves an
/// out-of-range conversion undefined. The reference short-circuits NaN to NaN
/// (`bitand(0/0,3)` is NaN, not 0) while letting the infinities convert:
/// `bitand(1/0,3)` is 3 and `bitand(-1/0,3)` is 0, i.e. `INT64_MAX` and
/// `INT64_MIN`. Rust's `as i64` saturates by definition, so it reproduces that
/// without relying on undefined behaviour. `bitor(1e19,1)` confirms the
/// saturation from the other side.
fn bitwise(x: f64, y: f64, op: fn(i64, i64) -> i64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    op(x as i64, y as i64) as f64
}

/// Binary (Stein's) GCD over `i64`, matching the reference's edge cases.
///
/// `gcd(-7,0)` is -7 and `gcd(0,5)` is 5 — a zero operand returns the *other*
/// operand unchanged, sign included. `gcd(-12,-18)` is 6, `gcd(1/0,6)` is 1
/// (`INT64_MAX` is odd) and `gcd(0/0,6)` is 6 (NaN converts to 0). All
/// verified; the documentation calls negative and zero inputs undefined, so
/// these are behaviours a caller can observe but should not rely on.
fn binary_gcd(first: i64, second: i64) -> i64 {
    if first == 0 {
        return second;
    }
    if second == 0 {
        return first;
    }
    let zeros_first = first.trailing_zeros();
    let zeros_second = second.trailing_zeros();
    let shift = zeros_first.min(zeros_second);
    // Both are non-zero, so shifting out the trailing zeros leaves an odd
    // magnitude; `unsigned_abs` then cannot overflow even for `i64::MIN`,
    // whose trailing-zero count is 63.
    let mut odd_first = (first >> zeros_first).unsigned_abs();
    let mut odd_second = (second >> zeros_second).unsigned_abs();
    while odd_first != odd_second {
        if odd_first > odd_second {
            odd_first -= odd_second;
            odd_first >>= odd_first.trailing_zeros();
        } else {
            odd_second -= odd_first;
            odd_second >>= odd_second.trailing_zeros();
        }
    }
    odd_first.wrapping_shl(shift).cast_signed()
}

/// One step of the pseudo-random generator behind `random` and `randomi`.
///
/// # Known divergence
///
/// This is a 64-bit LCG whose state lives in the addressed register, matching
/// the documented contract (seed stored as a 64-bit unsigned integer, result in
/// 0..1, state advanced). It does **not** reproduce the reference's bit stream:
/// the reference's map from seed to first output was measured at many points
/// and shown to be non-affine, so it is not an LCG at all, and no standard
/// mixer tried (splitmix64, murmur3 `fmix64`, xorshift64*, LCG+xorshift over
/// all 63 shifts, `av_lfg`'s MD5 seeding) reproduces it. The measured vectors
/// are recorded in `docs/core/vaco-expr.md` so this can be closed later.
fn next_random(ctx: &mut Context<'_>, index: usize) -> f64 {
    let current = ctx.regs.get(index);
    let seed = if current.is_nan() { 0 } else { current as u64 };
    let next = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    ctx.regs.set(index, next as f64);
    (next as f64) / 18_446_744_073_709_551_615.0
}

fn wallclock_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
