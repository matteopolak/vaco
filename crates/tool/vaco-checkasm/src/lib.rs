#![forbid(unsafe_code)]
//! The differential harness: verifies optimised kernels against their scalar
//! reference, across whatever runtime-dispatched SIMD tier the machine
//! running the check actually has.
//!
//! There is no hand-written assembly in this tree to check (D2: SIMD comes
//! from `std::simd`/`fearless_simd`/autovectorisation, never inline asm), so
//! unlike its namesake this crate is not proving an asm routine matches a C
//! reference. It is proving something narrower and just as easy to get
//! wrong: that a `#[inline(always)]` body written once and monomorphised per
//! [`vaco_simd::Tier`] computes the *same function* as the scalar reference
//! it was optimised from, at every case that matters — not just the
//! mid-range random input that both a correct kernel and a broken one pass.
//!
//! # The three pieces
//!
//! * [`Kernel`] — implement this once per kernel. It says how to build the
//!   corpus and how to run the scalar and vector sides over one case.
//! * [`Differential`] — runs a [`Kernel`]'s corpus and produces a [`Report`].
//! * [`edge`] — the deterministic input generators: vector-width boundaries,
//!   integer saturation limits, float specials. Random input finds
//!   average-case bugs; these target where SIMD divergence actually lives.
//!
//! # A minimal kernel, end to end
//!
//! ```
//! use vaco_checkasm::{Differential, Kernel};
//!
//! struct Double;
//!
//! impl Kernel for Double {
//!     const NAME: &'static str = "example::double";
//!     type Case = Vec<i32>;
//!     type Lane = i32;
//!
//!     fn cases() -> Vec<Self::Case> {
//!         vec![vec![], vec![0], vec![1, -1, i32::MAX, i32::MIN]]
//!     }
//!
//!     fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
//!         case.iter().map(|x| x.wrapping_mul(2)).collect()
//!     }
//!
//!     fn vector(case: &Self::Case) -> Vec<Self::Lane> {
//!         // Stand-in for a real `#[inline(always)]` SIMD body: shift left
//!         // by one is `x * 2`, wrapping the same way at the extremes.
//!         case.iter().map(|x| x.wrapping_shl(1)).collect()
//!     }
//! }
//!
//! let report = Differential::<Double>::run();
//! assert!(report.is_clean(), "{report}");
//! ```
//!
//! # Why cross-tier coverage is per-machine, not per-run
//!
//! [`Kernel::vector`] should call through a [`vaco_simd::KernelSet`]'s
//! `select()` table (resolved from [`vaco_simd::Caps::detect`]), so the tier
//! actually under test is whichever one the CPU running the check has.
//! Forcing a *weaker* tier on stronger hardware would need a capability token
//! fabricated without evidence — every `assume_supported` in `fearless_simd`
//! is `unsafe`, which D2 closes to us — so there is no way to make one
//! process exercise every tier. Coverage of SSE2 through AVX-512 and NEON
//! accumulates across the machines CI actually runs on.
//!
//! See [`kernels`] for a real kernel wired through this crate as a worked
//! example and as the `verify` CLI's own self-check.

pub mod bench;
pub mod differential;
pub mod edge;
pub mod kernels;

pub use differential::{Differential, Divergence, Kernel, Mismatch, Report};
