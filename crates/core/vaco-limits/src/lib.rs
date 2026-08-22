//! Allocation budgets, fuel counters and progress guards for untrusted input.
//!
//! # What this crate is for
//!
//! Vaco is `#![forbid(unsafe_code)]`, so the classic media-parser bug — memory
//! corruption from an attacker-controlled length — is not reachable. What *is*
//! reachable is three other classes (plan 13 §2.2), and this crate addresses two
//! of them structurally rather than by asking reviewers to be careful:
//!
//! | Class | Mechanism |
//! |---|---|
//! | Unbounded allocation | [`Budget`] — a required constructor parameter, with two-phase reservation |
//! | Non-termination | [`Budget::consume_fuel`] and [`ProgressGuard`] |
//! | Panics | not this crate: `clippy::unwrap_used` / `panic` / `indexing_slicing` are `deny` workspace-wide |
//!
//! # The shape
//!
//! [`Limits`] is immutable policy: caps, shared freely, `Send + Sync`. [`Budget`]
//! is the per-instance meter: counters, `&mut self`, single owner. Splitting them
//! is what makes consumption deterministic — no atomics, no interior mutability,
//! so the same input always exhausts the same budget at the same point and a
//! fuzz finding minimises and regresses cleanly.
//!
//! # Why a budget is a positional parameter
//!
//! Because an `Option<&Budget>` gets passed `None` at three in the morning.
//! `clippy.toml` denies `Vec::with_capacity`, `Vec::reserve` and
//! `Vec::reserve_exact` project-wide and names [`Budget::alloc`] as the
//! replacement, so the compiler — not the reviewer — is what notices a parser
//! sizing a buffer from a header without a cap.
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//!
//! // A parser takes the budget; it cannot be constructed without one.
//! struct BoxParser { budget: Budget }
//! impl BoxParser {
//!     fn new(limits: Limits) -> Self { Self { budget: Budget::new(limits) } }
//!
//!     fn read_box(&mut self, declared_len: u64, available: &[u8]) -> Result<Vec<u8>, vaco_limits::LimitError> {
//!         // Phase 1: is the declared size even plausible?
//!         let reservation = self.budget.reserve(declared_len)?;
//!         // Phase 2: only spend what really arrived.
//!         let n = usize::try_from(declared_len).unwrap_or(usize::MAX).min(available.len());
//!         let mut buf = reservation.alloc::<u8>(n)?;
//!         buf.copy_from_slice(&available[..n]);
//!         Ok(buf)
//!     }
//! }
//!
//! let mut p = BoxParser::new(Limits::strict());
//! assert!(p.read_box(1 << 40, b"short").is_err());   // 1 TiB header, rejected up front
//! assert!(p.read_box(5, b"short").is_ok());
//! ```
//!
//! # Configuration
//!
//! [`Limits::permissive`] (the CLI default), [`Limits::strict`] (the library
//! default, and what embedders get), [`Limits::tiny`] (for `limit_*` fuzz
//! targets). Individual caps are adjusted with the `with_*` methods.
//!
//! # Dependencies
//!
//! `vaco-core` for the shared [`Error`](vaco_core::Error) taxonomy that
//! [`LimitError`] converts into, and `thiserror`. Nothing else.

#![forbid(unsafe_code)]

mod budget;
mod error;
mod limits;
mod progress;

pub use budget::{Budget, IncrementalVec, Reservation};
pub use error::{LimitError, Result};
pub use limits::Limits;
pub use progress::ProgressGuard;
