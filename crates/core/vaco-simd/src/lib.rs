//! The SIMD substrate and kernel dispatch.
//!
//! This crate is the **D11 adapter** over `fearless_simd`: that crate is reachable
//! from here and nowhere else, and kernels are written against our own
//! [`KernelSet`] abstraction. If the substrate ever has to change, the interface
//! blast radius is this one crate.
//!
//! # Why not `std::simd`
//!
//! `std::simd` is safe but gives only what the *build target* permits, so a
//! distributed binary would sit at baseline x86-64 and never use AVX2 or AVX-512.
//! Runtime dispatch needs `#[target_feature]`, whose calls are `unsafe` because
//! the caller cannot prove the CPU has the feature.
//!
//! `fearless_simd` resolves this with capability tokens: a zero-sized type
//! *witnesses* that a level is available, functions are monomorphised per level,
//! and dispatch selects the best at runtime — so the intrinsic call is safe at
//! every call site and `#![forbid(unsafe_code)]` survives across the whole DSP
//! layer (D12).
//!
//! Use its `dispatch!` macro, never `kernel!`: `dispatch!` expands to no unsafe,
//! `kernel!` injects unsafe into the calling crate.

/// An instruction-set tier, resolved once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Scalar reference. Always available, and the oracle every SIMD variant is
    /// differentially tested against.
    Scalar,
    Sse2,
    Sse42,
    Avx2,
    Avx512,
    /// aarch64. A single level, so Apple Silicon and ARM servers carry no
    /// multiversioning cost at all.
    Neon,
}

impl Tier {
    /// Detect the best tier this CPU supports. Call once; cache the result.
    #[must_use]
    pub fn detect() -> Self {
        todo!("P0-03 freeze: fearless_simd::Level::new() mapped onto Tier")
    }
}

/// A table of function pointers for one DSP area, resolved once at construction.
///
/// The cost model matches hand-written asm: one indirect call amortised over a
/// whole frame or slice, not per pixel. Ordinary safe `fn` pointers — no `dyn`,
/// because a vtable indirection inside a per-pixel loop is exactly what we are
/// avoiding.
///
/// Every kernel has a scalar reference implementation, and `vaco-checkasm`
/// verifies each SIMD variant against it over randomised and edge-case input. A
/// kernel without a differential test does not merge.
pub trait KernelSet: Sized + Send + Sync + 'static {
    /// Build the table for a tier. Must return a complete table for every tier,
    /// falling back to scalar for kernels not yet vectorised.
    fn for_tier(tier: Tier) -> Self;

    /// The scalar table, used as the differential-test oracle.
    #[must_use]
    fn reference() -> Self {
        Self::for_tier(Tier::Scalar)
    }
}

/// Operations `fearless_simd` does not provide, composed from ones it does.
///
/// Measured during the D12 adoption review. Most compose cheaply; the exception
/// is widening multiply-add, which has no composition and is the single largest
/// performance risk in the project (plan 12).
pub mod ops {
    /// Unsigned saturating add: `min(a, !b) + b`. 3 operations.
    #[must_use]
    pub fn saturating_add_u8(a: u8, b: u8) -> u8 {
        a.saturating_add(b)
    }

    /// Rounded average: `(a | b) - ((a ^ b) >> 1)`. 4 operations, exact, and
    /// stays in width — better than the obvious widen-add-shift-narrow.
    #[must_use]
    pub const fn rounded_avg_u8(a: u8, b: u8) -> u8 {
        (a | b) - ((a ^ b) >> 1)
    }

    /// Absolute difference: `max(a, b) - min(a, b)`. 3 operations.
    #[must_use]
    pub const fn abs_diff_u8(a: u8, b: u8) -> u8 {
        a.abs_diff(b)
    }
}
