//! The arithmetic expression language used by filters and the command line.
//!
//! Very little of the reference tool's surface takes a plain number.
//! `-vf scale=w='if(gt(a,16/9),1280,-1)'`, `volume=volume='-6dB'`,
//! `drawtext=x='(w-tw)/2'`, `-force_key_frames` and every timeline `enable=`
//! take an *expression*. This crate parses that language and evaluates it.
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
//! # How it is meant to be used
//!
//! Parsing allocates; evaluating does not. A filter binds its variable names
//! once, parses once, and then evaluates per frame against a `&[f64]` whose
//! positions match the names it bound:
//!
//! ```
//! use vaco_expr::{Bindings, Context, Expr, Registers};
//!
//! let expr = Expr::parse("t*n", &Bindings::new(&["t", "n"]))?;
//! let mut regs = Registers::new();          // `ld`/`st` state, reused
//! for n in 0..3u32 {
//!     let vars = [f64::from(n) / 25.0, f64::from(n)];
//!     let _ = expr.eval_with(&mut Context::new(&vars, &mut regs));
//! }
//! # Ok::<(), vaco_expr::ParseError>(())
//! ```
//!
//! [`Expr`] is `Send + Sync` and cheap to clone, and the mutable evaluation
//! state lives in the caller's [`Registers`], so one compiled expression can be
//! shared across worker threads.
//!
//! # The language
//!
//! Operators, lowest precedence first: `;` (sequence, yields the right-hand
//! value), `+` `-`, `*` `/`, `^`, unary `+` `-`. Constants: `PI`, `E`, `PHI`.
//! Fifty-one builtin functions — see [`Func`] for the exact set, which was
//! established by probing every candidate name against the reference rather
//! than by reading a list.
//!
//! Numbers accept `0x` hexadecimal, the International System prefixes
//! (`2k` = 2000), an `i` suffix for binary prefixes (`2ki` = 2048), a `B`
//! suffix for times-eight (`2kB` = 16000), and a `dB` suffix
//! (`-20dB` = 0.1).
//!
//! # Fidelity
//!
//! The language is an interface: user command lines depend on it, so it has to
//! agree with the reference exactly, including where the reference is odd. Per
//! D17, every such oddity is reproduced and marked with a `D17:` comment naming
//! the conventional behaviour, the reference's behaviour, and the probe that
//! established it. The short list, all verified:
//!
//! | Behaviour | Conventional | Here |
//! |---|---|---|
//! | `2^3^2` | 512 (right-assoc) | **64** (left-assoc) |
//! | `-2^2` | 4 | **-4** (sign applied after the chain) |
//! | `0-20dB` | -10 | **0.1** (the sign belongs to the literal) |
//! | `"1 2"` | two tokens | **12** (whitespace is deleted, not skipped) |
//! | `---1` | -1 | **parse error** (one sign character only) |
//! | `max(1,0/0)` | 1 (`fmax`) | **NaN** (comparison select) |
//! | `if(0/0,7)` | 0 or an error | **7** (NaN is truthy) |
//! | `mod(-5,3)` | -2 (`fmod`) | **1** (floored) |
//! | `ld(100)` | error | **register 9** (clamped) |
//! | `abs.(1)` | error | **1** (names match by prefix) |
//!
//! Three things deliberately differ, each documented where it lives:
//! `while` gets an iteration budget rather than hanging the process
//! ([`Limits::max_iterations`]), long flat operator chains are accepted
//! where the reference rejects them past 100, and `random`/`randomi` do not
//! reproduce the reference's bit stream. See `docs/core/vaco-expr.md`.
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
