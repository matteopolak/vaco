//! Allocation budgets, fuel counters and progress guards for untrusted input.
//!
//! Unbounded allocation and non-termination from attacker-controlled input
//! are handled structurally, not by review: [`Limits`] is immutable, shared
//! policy, and [`Budget`] is the per-instance meter with two-phase
//! reservation — check plausibility, then spend only what arrived. Splitting
//! them keeps consumption deterministic (no atomics, no interior
//! mutability), so the same input always exhausts the same budget at the
//! same point and a fuzz finding regresses cleanly.
//!
//! `Budget` is a required constructor parameter, never `Option<&Budget>`:
//! `clippy.toml` denies `Vec::with_capacity`/`reserve`/`reserve_exact`
//! project-wide in favour of [`Budget::alloc`], so the compiler catches an
//! unbounded allocation rather than relying on review.
//!
//! ```
//! use vaco_limits::{Budget, Limits};
//!
//! struct BoxParser { budget: Budget }
//! impl BoxParser {
//!     fn new(limits: Limits) -> Self { Self { budget: Budget::new(limits) } }
//!
//!     fn read_box(&mut self, declared_len: u64, available: &[u8]) -> Result<Vec<u8>, vaco_limits::LimitError> {
//!         let reservation = self.budget.reserve(declared_len)?;
//!         let n = usize::try_from(declared_len).unwrap_or(usize::MAX).min(available.len());
//!         let mut buf = reservation.alloc::<u8>(n)?;
//!         buf.copy_from_slice(&available[..n]);
//!         Ok(buf)
//!     }
//! }
//!
//! let mut p = BoxParser::new(Limits::strict());
//! assert!(p.read_box(1 << 40, b"short").is_err());
//! assert!(p.read_box(5, b"short").is_ok());
//! ```
//!
//! [`Limits::permissive`] is the CLI default, [`Limits::strict`] the library
//! default; individual caps adjust via the `with_*` methods.

#![forbid(unsafe_code)]

mod budget;
mod error;
mod limits;
mod progress;

pub use budget::{Budget, IncrementalVec, Reservation};
pub use error::{LimitError, Result};
pub use limits::Limits;
pub use progress::ProgressGuard;
