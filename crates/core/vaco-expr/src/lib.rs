//! The arithmetic expression language used by filters and the command line.
//!
//! Filter arguments such as `scale=w='if(gt(a,16/9),1280,-1)'`, `volume=-6dB`,
//! `drawtext=x='(w-tw)/2'`, and timeline `enable=` take an expression. This
//! crate parses that language and evaluates it.
//!
//! ```
//! use vaco_expr::{Bindings, Expr};
//!
//! let bindings = Bindings::new(&["a"]);
//! let expr = Expr::parse("if(gt(a,16/9),1280,-1)", &bindings)?;
//!
//! assert_eq!(expr.eval(&[1.85]), 1280.0);     // wider than 16:9
//! assert_eq!(expr.eval(&[4.0 / 3.0]), -1.0);  // 4:3, keep the source width
//! # Ok::<(), vaco_expr::ParseError>(())
//! ```
//!
//! Parsing allocates; evaluating does not. Bind variable names once, then
//! evaluate a compiled [`Expr`] against per-frame `&[f64]` values. Evaluation
//! state lives in caller-owned [`Registers`], so an expression is cheap to
//! clone, `Send + Sync`, and shareable across worker threads.
//!
//! Operators, lowest precedence first, are `;`, `+`/`-`, `*`/`/`, `^`, and unary
//! signs. Constants are `PI`, `E`, and `PHI`; [`Func`] lists the 51 builtins.
//! Numbers support hexadecimal, SI and binary prefixes, the times-eight `B`
//! suffix, and decibels such as `-20dB` (0.1).
//!
//! The grammar and numeric results match the reference, including measured D17
//! quirks: `2^3^2` is 64 (left-associative), `-2^2` is -4, `0-20dB` is 0.1,
//! whitespace is deleted, `---1` is rejected, `max(1,0/0)` is NaN, `if(0/0,7)`
//! is 7, `mod(-5,3)` is 1, `ld(100)` clamps to register 9, and names match by
//! prefix. Each deviation is marked beside the implementation and backed by a
//! probe; see `docs/core/vaco-expr.md` for the complete measurements.
//!
//! `while` is bounded by [`Limits::max_iterations`] rather than hanging, and
//! long flat chains are accepted past the reference's depth boundary.
#![forbid(unsafe_code)]

mod error;
mod eval;
mod expr;
mod func;
mod lex;
mod parse;

pub use error::{ParseError, ParseErrorKind};
pub use eval::{Context, DEFAULT_PRINT_LEVEL, Registers};
pub use expr::{Bindings, Expr, Limits};
pub use func::Func;
pub use lex::{Number, from_decibels, scan_number, strip_whitespace, strmatch};
