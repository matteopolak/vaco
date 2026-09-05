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
//!
//! # The four things this crate exports
//!
//! | Item | Role |
//! |---|---|
//! | [`Tier`] | Which instruction set we resolved to. A plain enum, used to *select* a kernel table. |
//! | [`Caps`] | The capability **proof**. A newtype over the substrate's `Level`; the only thing [`dispatch_kernel!`] accepts. |
//! | [`KernelSet`] | A table of `fn` pointers for one DSP area, built once per [`Tier`]. |
//! | [`ops`] | Every operation the substrate does not provide, composed from ones it does, under our own names. |
//!
//! `Tier` and `Caps` are deliberately separate. `Tier` is `Ord` and cheap to
//! compare, store and print; `Caps` carries the zero-sized token that makes the
//! intrinsic call safe and therefore cannot be synthesised from a `Tier`.
//!
//! # Authoring a kernel
//!
//! See the [`example`] module for a complete, tested worked example
//! (yuv420p → rgb24). The shape is always the same:
//!
//! 1. A **scalar reference**, always compiled, definitionally correct.
//! 2. One `#[inline(always)]` body generic over `S: `[`Lanes`], monomorphised
//!    once per level.
//! 3. A dispatching wrapper built with [`dispatch_kernel!`].
//! 4. A [`KernelSet`] holding the `fn` pointers, resolved once by the caller's
//!    constructor so the indirect call is paid per row, never per pixel.
//! 5. A proptest against the scalar reference. A kernel without one does not merge.
//!
//! `#[inline(always)]` on rule 2 is a **correctness-of-codegen requirement**, not
//! a tuning knob: it is how the dispatched level's target-feature context reaches
//! the body. A kernel that fails to inline is compiled at the baseline, is still
//! correct, and is silently slow.
//!
//! # Two rules that are worth more than any composition in [`ops`]
//!
//! Both came out of the PF-0.0 measurements, both are worth several times what
//! the gap compositions cost, and both are invisible to every correctness test.
//!
//! **Rule A — batch, until you spill.** LLVM unrolls a `for x in a.iter().zip(b)`
//! loop four times and does not unroll a `chunks_exact` loop at all. Processing
//! four vectors per iteration took `rounded_avg_u8` from 1.55x to 1.00x against
//! its native instruction. But batching the 8-tap FIR to two output vectors made
//! it *worse* — 1.12x to 1.36x — because one stack spill became six. Batch until
//! the register file runs out, and check the spill count rather than trusting
//! the rule.
//!
//! **Rule B — never carry a single accumulator.** A loop-carried vector
//! accumulator is a chain of dependent adds with nothing to fill the latency.
//! LLVM splits a scalar reduction into eight accumulators automatically and will
//! not do that to a hand-written loop, because it has no reason to think the loop
//! is latency-bound. One accumulator measured **3.90x**; four measured **0.99x**.
//!
//! # Measurements
//!
//! `docs/core/simd-adoption-measurements.md` records what the substrate actually
//! costs on real hardware, with the disassembly behind every number. Read it
//! before assuming a gap composition is free — several of them turn out to be
//! free, and two of the plans' specific recommendations turn out to be wrong.
//!
//! The short version, on aarch64 with LLVM 22: the compositions for unsigned
//! saturating add/sub, rounded average, absolute difference, integer `abs` and
//! the `pmaddwd` shape all get reconstructed into the native instruction by
//! LLVM's peephole combiner, and measure 1.00x or better. The one genuine gap is
//! **signed saturating add/sub on `i16`**, at 1.46x. A second gap the plans never
//! named is that there is no `i16 -> u8` saturating narrow; see
//! [`ops::simd::pack_u8_from_i16`].

// `#[inline(always)]` is not a tuning knob in this crate: it is how the
// target-feature context of a dispatched level reaches a kernel body. A kernel
// that fails to inline is compiled at the ambient baseline — still correct,
// silently slow, and invisible to every correctness test. Clippy's advice not to
// use it is right in general and wrong here, so it is turned off once, at the
// crate root, rather than annotated onto forty functions.
#![allow(
    clippy::inline_always,
    reason = "mandatory for target-feature propagation; see crate docs"
)]

// The substrate, re-exported under a private-looking name so that
// `dispatch_kernel!` can expand to a path that resolves in a consumer crate
// WITHOUT that crate naming `fearless_simd` in its own `Cargo.toml`. That is
// what keeps the D11 boundary CI-checkable: `fearless_simd` appears in exactly
// one manifest under `crates/`.
//
// Not public API. Nothing outside this crate may name it.
#[doc(hidden)]
pub use fearless_simd as __substrate;

/// The capability token trait every kernel body is generic over.
///
/// A `pub use` of the substrate's `Simd` trait rather than a newtype, and that
/// is deliberate (plan 11 §5.3, "the honest boundary"): the substrate carries
/// 1,453 generated trait methods whose whole performance model depends on
/// `#[inline(always)]` propagating the target-feature context through the call
/// chain. A wrapper layer that failed to inline would silently produce
/// non-vectorised code — a worse failure than the coupling it removes.
///
/// What *is* held strictly: every operation whose semantics we care about lives
/// in [`ops`] under our own name, so a substrate swap is a change to [`ops`],
/// not to kernel bodies.
pub use fearless_simd::Simd as Lanes;

/// Attributes for declaring vectorization contracts.
///
/// `#[vaco::must_vectorize]` marks a free SIMD kernel whose stable id and
/// compiler symbol are declared once in the repository `vecheck.toml`.
pub mod vaco {
    pub use vaco_vecheck_macros::must_vectorize;
}

pub mod example;
pub mod ops;
pub mod testing;

/// Everything a kernel module needs.
///
/// ```
/// use vaco_simd::prelude::*;
///
/// #[inline(always)]
/// fn double<S: Lanes>(simd: S, values: &mut [u32]) {
///     let mut chunks = values.chunks_exact_mut(S::u32s::N);
///     for chunk in &mut chunks {
///         (S::u32s::from_slice(simd, chunk) * 2).store_slice(chunk);
///     }
///     for v in chunks.into_remainder() {
///         *v *= 2;
///     }
/// }
///
/// let mut values = [1u32, 2, 3, 4, 5];
/// let caps = Caps::detect();
/// vaco_simd::dispatch_kernel!(caps, simd => double(simd, &mut values));
/// assert_eq!(values, [2, 4, 6, 8, 10]);
/// ```
pub mod prelude {
    pub use crate::{Caps, KernelSet, Lanes, Tier, dispatch_kernel, ops};
    pub use fearless_simd::prelude::*;
    // Vector types are not traits, so the substrate's own prelude does not carry
    // them. These are the fixed-width types kernels reach for when a shuffle
    // table or an interleave forces 128-bit block granularity.
    pub use fearless_simd::{SimdFrom, SimdInto, f32x4, i8x16, i16x8, i32x4, u8x16, u16x8, u32x4};
}

/// An instruction-set tier, resolved once at startup.
///
/// This is a *label*, not a proof. It says which kernel table to build; it
/// cannot be used to call a level-specific kernel. See [`Caps`] for the proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
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
    ///
    /// Cheap to call repeatedly anyway: on x86 the substrate caches its CPU
    /// probe in a `LazyLock`, and on aarch64 and wasm the level is a compile-time
    /// constant.
    #[must_use]
    pub fn detect() -> Self {
        Caps::detect().tier()
    }

    /// The scalar tier, spelled as a function so kernel tables can write
    /// `t == Tier::scalar()` without importing the variant.
    #[must_use]
    pub const fn scalar() -> Self {
        Self::Scalar
    }

    /// Whether this tier means "use the scalar reference implementations".
    #[must_use]
    pub const fn is_scalar(self) -> bool {
        matches!(self, Self::Scalar)
    }

    /// A stable lowercase name, used by benchmark and `checkasm` reporting.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Sse2 => "sse2",
            Self::Sse42 => "sse4.2",
            Self::Avx2 => "avx2",
            Self::Avx512 => "avx512",
            Self::Neon => "neon",
        }
    }

    /// Parse the stable configuration spelling for an instruction-set tier.
    ///
    /// This is shared by the `VACO_TIER` environment override and diagnostics
    /// so the externally visible names cannot drift from [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "scalar" => Some(Self::Scalar),
            "sse2" => Some(Self::Sse2),
            "sse4.2" => Some(Self::Sse42),
            "avx2" => Some(Self::Avx2),
            "avx512" => Some(Self::Avx512),
            "neon" => Some(Self::Neon),
            _ => None,
        }
    }
}

impl core::fmt::Display for Tier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// The process configuration, parsed only when dispatch is first requested.
///
/// Re-reading an environment variable on every [`Caps::detect`] call would
/// make a diagnostic override part of each kernel's hot setup path.
static ENV_TIER_CAP: std::sync::LazyLock<Option<Tier>> = std::sync::LazyLock::new(|| {
    std::env::var("VACO_TIER")
        .ok()
        .as_deref()
        .and_then(Tier::from_name)
});

/// A runtime proof of which SIMD instructions this CPU actually has.
///
/// Newtype over the substrate's `Level`. It is `Copy` and holds only zero-sized
/// tokens, so passing it costs nothing — detect once at construction time and
/// carry it, rather than calling [`Caps::detect`] inside a loop.
///
/// This is the only value [`dispatch_kernel!`] accepts, which is what makes a
/// kernel call safe: the token *is* the evidence the target feature is present.
#[derive(Debug, Clone, Copy)]
pub struct Caps(fearless_simd::Level);

impl Caps {
    /// Probe the CPU. On x86 the first call runs `cpuid` and the result is
    /// cached by the substrate; elsewhere the level is statically known. On
    /// its first call, a valid `VACO_TIER` value can cap dispatch to a
    /// supported lower tier for differential testing; unsupported, unavailable
    /// and invalid values leave the detected capability unchanged.
    #[must_use]
    pub fn detect() -> Self {
        let detected = Self(fearless_simd::Level::new());
        (*ENV_TIER_CAP)
            .and_then(|max| detected.capped_at(max))
            .unwrap_or(detected)
    }

    /// The strongest level the *build target* statically guarantees, ignoring
    /// runtime detection.
    ///
    /// Useful for `-C target-cpu=native` builds and for tests that want a
    /// deterministic level. Prefer [`Caps::detect`] everywhere else.
    #[must_use]
    pub fn baseline() -> Self {
        Self(fearless_simd::Level::baseline())
    }

    /// The [`Tier`] label for this proof.
    #[must_use]
    pub fn tier(self) -> Tier {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if self.0.as_avx512().is_some() {
                return Tier::Avx512;
            }
            if self.0.as_avx2().is_some() {
                return Tier::Avx2;
            }
            if self.0.as_sse4_2().is_some() {
                return Tier::Sse42;
            }
            if self.0.as_sse2().is_some() {
                return Tier::Sse2;
            }
            Tier::Scalar
        }
        #[cfg(target_arch = "aarch64")]
        {
            if self.0.as_neon().is_some() {
                Tier::Neon
            } else {
                Tier::Scalar
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            // wasm32-simd128 and everything else. `is_fallback()` is the only
            // predicate guaranteed to exist on every target.
            if self.0.is_fallback() {
                Tier::Scalar
            } else {
                Tier::Sse2
            }
        }
    }

    /// The substrate level. **Not public API** — an implementation detail of
    /// [`dispatch_kernel!`], which must be able to name it from a consumer crate.
    #[doc(hidden)]
    #[must_use]
    pub fn __level(self) -> fearless_simd::Level {
        self.0
    }

    /// Return the strongest supported token no stronger than `max`.
    ///
    /// A cap can only lower a proof derived from this CPU; it never constructs
    /// a capability token from configuration. `None` therefore means the
    /// requested tier is not a dispatchable tier for this target or CPU.
    fn capped_at(self, max: Tier) -> Option<Self> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let level = match max {
                Tier::Sse2 => self.0.as_sse2().map(fearless_simd::Level::Sse2),
                Tier::Sse42 => self.0.as_sse4_2().map(fearless_simd::Level::Sse4_2),
                Tier::Avx2 => self.0.as_avx2().map(fearless_simd::Level::Avx2),
                Tier::Avx512 => self.0.as_avx512().map(fearless_simd::Level::Avx512),
                Tier::Scalar | Tier::Neon => None,
            }?;
            Some(Self(level))
        }
        #[cfg(target_arch = "aarch64")]
        {
            if max == Tier::Neon {
                Some(Self(fearless_simd::Level::Neon(self.0.as_neon()?)))
            } else {
                None
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = max;
            None
        }
    }
}

impl Default for Caps {
    fn default() -> Self {
        Self::detect()
    }
}

/// Run a level-generic kernel body under the best available capability token.
///
/// Wraps the substrate's `dispatch!`. Verified against `fearless_simd` v0.7.0:
/// the expansion is a `match` over the level that binds a token and calls
/// `Simd::vectorize(token, || body)` — a safe trait method. **Nothing in the
/// expansion is `unsafe`**, so `#![forbid(unsafe_code)]` holds in every crate
/// that uses this macro. The substrate's other entry point, `kernel!`, *does*
/// expand `unsafe` into the calling crate and is therefore closed to us.
///
/// The body is repeated once per compiled level, so keep it to a single call to
/// an `#[inline(always)]` generic function.
///
/// The body runs inside a closure, so `?` and early `return` do not escape it.
/// Return a value from the macro instead, or use `ControlFlow`.
///
/// ```
/// use vaco_simd::prelude::*;
///
/// #[inline(always)]
/// fn sum_u32<S: Lanes>(_simd: S, xs: &[u32]) -> u32 {
///     xs.iter().copied().fold(0, u32::wrapping_add)
/// }
///
/// let caps = Caps::detect();
/// let total = vaco_simd::dispatch_kernel!(caps, simd => sum_u32(simd, &[1, 2, 3]));
/// assert_eq!(total, 6);
/// ```
#[macro_export]
macro_rules! dispatch_kernel {
    ($caps:expr, $simd:pat => $op:expr) => {
        $crate::__substrate::dispatch!($crate::Caps::__level($caps), $simd => $op)
    };
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

    /// The table for the CPU we are actually running on.
    ///
    /// This is what a consumer's constructor calls. Detection is cheap (see
    /// [`Tier::detect`]) but the result should still be stored, not re-derived
    /// per row.
    #[must_use]
    fn select() -> Self {
        Self::for_tier(Tier::detect())
    }

    /// Names of the kernels in this table, in a stable order.
    ///
    /// Used by `vaco-checkasm` to report which kernel diverged or which one is
    /// being timed. Defaults to empty so that adding a kernel set is cheap;
    /// override it for any set that should appear in the checkasm matrix.
    #[must_use]
    fn kernel_names() -> &'static [&'static str] {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_matches_this_machine() {
        let tier = Tier::detect();
        if cfg!(target_arch = "aarch64") {
            // D12 risk 2 / plan 12 checklist item: NEON must be reachable.
            assert_eq!(tier, Tier::Neon, "aarch64 must resolve to Neon");
        } else if cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
            assert!(tier >= Tier::Sse2, "x86-64 guarantees at least SSE2");
        }
    }

    #[test]
    fn tier_ordering_is_capability_ordering() {
        assert!(Tier::Scalar < Tier::Sse2);
        assert!(Tier::Sse2 < Tier::Sse42);
        assert!(Tier::Sse42 < Tier::Avx2);
        assert!(Tier::Avx2 < Tier::Avx512);
    }

    #[test]
    fn tier_names_round_trip_through_the_configuration_parser() {
        for tier in [
            Tier::Scalar,
            Tier::Sse2,
            Tier::Sse42,
            Tier::Avx2,
            Tier::Avx512,
            Tier::Neon,
        ] {
            assert_eq!(Tier::from_name(tier.name()), Some(tier));
        }
        assert_eq!(Tier::from_name("not-a-tier"), None);
    }

    #[test]
    fn a_cap_never_synthesizes_an_unavailable_capability() {
        let detected = Caps::detect();
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(
                detected.capped_at(Tier::Sse2).map(Caps::tier),
                Some(Tier::Sse2)
            );
            assert!(detected.capped_at(Tier::Neon).is_none());
        }
        #[cfg(target_arch = "x86")]
        assert!(detected.capped_at(Tier::Neon).is_none());
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(
                detected.capped_at(Tier::Neon).map(Caps::tier),
                Some(Tier::Neon)
            );
            assert!(detected.capped_at(Tier::Sse2).is_none());
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn environment_override_caps_dispatch_before_first_detection() {
        if std::env::var_os("VACO_SIMD_CAP_CHILD").is_some() {
            assert_eq!(Caps::detect().tier(), Tier::Sse2);
            return;
        }

        let child = std::env::current_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .arg("--exact")
                .arg("tests::environment_override_caps_dispatch_before_first_detection")
                .arg("--nocapture")
                .env("VACO_SIMD_CAP_CHILD", "1")
                .env("VACO_TIER", "sse2")
                .output()
        });
        assert!(child.is_ok());
        if let Ok(output) = child {
            assert!(output.status.success());
        }
    }

    /// The native `u8` lane count for a token. Written as a generic function
    /// because the token inside `dispatch_kernel!` is an anonymous `impl Simd`
    /// and its associated types cannot be named at the call site.
    #[inline(always)]
    fn u8_lanes<S: Lanes>(_simd: S) -> usize {
        <S::u8s as fearless_simd::SimdBase<S>>::N
    }

    #[test]
    fn dispatch_returns_a_value_and_binds_a_token() {
        // Proves the bound token really is a capability token: the native width
        // is a property of the level, not of the build target.
        let caps = Caps::detect();
        let n = dispatch_kernel!(caps, simd => u8_lanes(simd));
        assert!(n >= 16, "native u8 width {n} is below the 128-bit floor");
        assert!(n.is_power_of_two());
    }

    #[test]
    fn baseline_is_never_stronger_than_detected() {
        assert!(Caps::baseline().tier() <= Caps::detect().tier());
    }
}
