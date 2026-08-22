//! Sample types and the arithmetic every kernel is written against.
//!
//! # The one-implementation rule
//!
//! [`Lane`] is the arithmetic a butterfly needs, and *nothing else*. It is
//! implemented three times for scalars (`f32`, `f64`, `i32`) and once for a SIMD
//! vector wrapper ([`crate::simd::Vf32`]). Every butterfly in
//! [`crate::butterfly`] is written once against `Lane`, so the vectorised `f32`
//! kernel and the scalar `f32` reference are literally the same source with a
//! different `Lane` impl. Divergence between them is not a bug we test for; it
//! is a shape the code cannot take.
//!
//! [`Arith`] adds what a *plan* needs on top of [`Lane`] — quantising constants,
//! integer scaling — and is scalar-only. [`TxSample`] is the sealed public
//! surface.
//!
//! # Why the fixed-point differences live here
//!
//! Per-stage scaling, wide accumulation, round-half-up and saturation are all
//! `i32` [`Lane`] impl details. A kernel never branches on precision, which is
//! what makes the normative `i32` contract of [`crate::fixed`] a property of
//! this file rather than of every kernel.

use crate::fixed;

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for f64 {}
    impl Sealed for i32 {}
}

/// The arithmetic a butterfly is written against.
///
/// **Not public API.** `pub` only because [`TxSample`] names it transitively.
#[doc(hidden)]
pub trait Lane: Copy {
    /// A plan-time constant — a twiddle or an algebraic coefficient.
    ///
    /// The same type for scalars; the *element* type for vectors, so one
    /// quantised table serves both paths.
    type Const: Copy;

    /// The wide accumulator a symmetric-sum butterfly builds in.
    ///
    /// `Self` for floats; `i64` for `i32`, where plan 17's "the `i64`
    /// intermediate is mandatory" is a correctness requirement, not a tuning
    /// choice: the product of two Q31 values needs 62 bits.
    type Acc: Copy;

    /// Whether a radix-`r` stage divides its inputs by `r`.
    ///
    /// `false` for floats (the transform is unnormalised). `true` for `i32`,
    /// where it is the pinned overflow policy that makes a forward transform
    /// produce `DFT(x)/n` and — the reason it was chosen over block floating
    /// point — introduces no data-dependent shift.
    const STAGE_SCALED: bool;

    fn add(a: Self, b: Self) -> Self;
    fn sub(a: Self, b: Self) -> Self;
    fn neg(a: Self) -> Self;
    /// `a · c` for a plan-time constant `c`.
    fn mul_c(a: Self, c: Self::Const) -> Self;
    /// Divide by a stage radix. Identity for floats.
    fn div_radix(a: Self, r: u32) -> Self;

    /// Promote a lane into the accumulator without rounding.
    fn acc_of(x: Self) -> Self::Acc;
    /// `x · c`, as an accumulator.
    fn acc_mul(x: Self, c: Self::Const) -> Self::Acc;
    fn acc_neg(a: Self::Acc) -> Self::Acc;
    fn acc_mul_add(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc;
    fn acc_mul_sub(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc;
    fn acc_sum(a: Self::Acc, b: Self::Acc) -> Self::Acc;
    fn acc_diff(a: Self::Acc, b: Self::Acc) -> Self::Acc;
    /// Narrow the accumulator back to a lane, rounding and saturating.
    fn acc_finish(a: Self::Acc) -> Self;

    /// Complex multiply by a constant twiddle, `(ar + i·ai)·(wr + i·wi)`.
    ///
    /// Four multiplies, one add, one subtract — deliberately not a
    /// three-multiply Karatsuba form, whose different rounding would put the
    /// float and fixed paths on different sides of the closed forms the tests
    /// compare against.
    fn cmul_c(ar: Self, ai: Self, wr: Self::Const, wi: Self::Const) -> (Self, Self);
}

/// Everything a vectorised Stockham stage needs, bundled into one value.
///
/// Exists so [`Arith::simd_stockham_pass`] stays inside clippy's argument limit
/// and so the SIMD entry point never names a `pub(crate)` type. Borrowed from
/// the plan's own tables — the vector path and the scalar path read the *same*
/// twiddles and the *same* radix constants, which removes the most likely source
/// of a scalar/SIMD divergence before any test has to look for it.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct StageView<'a, T> {
    pub radix: usize,
    pub m: usize,
    pub s: usize,
    pub tw_re: &'a [T],
    pub tw_im: &'a [T],
    pub cos: &'a [T],
    pub sin: &'a [T],
    pub c8: T,
}

/// Scalar arithmetic a *plan* needs on top of [`Lane`].
#[doc(hidden)]
pub trait Arith:
    Lane<Const = Self>
    + sealed::Sealed
    + Copy
    + Send
    + Sync
    + Default
    + core::fmt::Debug
    + PartialEq
    + 'static
{
    const ZERO: Self;

    /// Quantise a real constant into this representation.
    fn from_f64(x: f64) -> Self;
    /// Widen back to `f64`, for tests, diagnostics and error measurement.
    fn to_f64(self) -> f64;
    /// `a · k` for a positive integer `k`, saturating.
    fn mul_int(a: Self, k: u32) -> Self;
    /// `a / k` for a positive integer `k`, with the contract's rounding.
    fn div_int(a: Self, k: u32) -> Self;

    /// `(a + b) / 2`, with **one** rounding and no intermediate overflow.
    ///
    /// The derived transforms are full of half-sums (the even/odd split in the
    /// RDFT, the `A`/`B` extraction in its inverse). Written as `add` then
    /// `div_radix` the sum would overflow Q31 before the divide; written as two
    /// halves then an add it would round twice. This does neither.
    fn half_sum(a: Self, b: Self) -> Self;
    /// `(a - b) / 2`, with the same guarantees as [`Arith::half_sum`].
    fn half_diff(a: Self, b: Self) -> Self;

    /// Run one Stockham stage with SIMD. `false` means "not handled — run the
    /// scalar pass".
    ///
    /// The hook is here, on a scalar trait, rather than being resolved by
    /// specialisation: `f32` overrides it, `f64` and `i32` take the default.
    /// `i32` deliberately has no vector path — see `docs/signal/vaco-tx.md`,
    /// "What was deferred".
    #[inline(always)]
    fn simd_stockham_pass(
        _caps: vaco_simd::Caps,
        _view: StageView<'_, Self>,
        _sr: &[Self],
        _si: &[Self],
        _dr: &mut [Self],
        _di: &mut [Self],
    ) -> bool {
        false
    }
}

/// A precision a transform can be executed in.
///
/// Sealed: `f32`, `f64` and `i32` are the whole set. `i32` is Q31 fixed point,
/// governed by the normative contract in [`crate::fixed`].
pub trait TxSample: Arith {
    /// The type of [`crate::Plan`]'s `scale` parameter.
    type Scale: Copy + core::fmt::Debug + PartialEq;

    /// The scale value meaning "do not scale".
    ///
    /// Compared once at plan time, so passing it costs nothing at execution.
    /// For `i32` this is [`crate::fixed::ONE`] — `i32::MAX`, the saturated Q31
    /// encoding of `1.0`. Multiplying by it would be off by one ULP for inputs
    /// at or above `2^30`, so the plan drops the pass instead of applying it.
    const IDENTITY_SCALE: Self::Scale;

    /// Apply the plan's scale to one sample.
    fn apply_scale(x: Self, s: Self::Scale) -> Self;

    /// A stable name, used by [`crate::PlanDescription`] and by benchmarks.
    fn precision_name() -> &'static str;
}

macro_rules! impl_float {
    ($t:ty, $name:literal, $simd:expr) => {
        impl Lane for $t {
            type Const = $t;
            type Acc = $t;

            const STAGE_SCALED: bool = false;

            #[inline(always)]
            fn add(a: Self, b: Self) -> Self {
                a + b
            }
            #[inline(always)]
            fn sub(a: Self, b: Self) -> Self {
                a - b
            }
            #[inline(always)]
            fn neg(a: Self) -> Self {
                -a
            }
            #[inline(always)]
            fn mul_c(a: Self, c: Self::Const) -> Self {
                a * c
            }
            #[inline(always)]
            fn div_radix(a: Self, _r: u32) -> Self {
                a
            }
            #[inline(always)]
            fn acc_of(x: Self) -> Self::Acc {
                x
            }
            #[inline(always)]
            fn acc_mul(x: Self, c: Self::Const) -> Self::Acc {
                x * c
            }
            #[inline(always)]
            fn acc_neg(a: Self::Acc) -> Self::Acc {
                -a
            }
            #[inline(always)]
            fn acc_mul_add(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc {
                acc + x * c
            }
            #[inline(always)]
            fn acc_mul_sub(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc {
                acc - x * c
            }
            #[inline(always)]
            fn acc_sum(a: Self::Acc, b: Self::Acc) -> Self::Acc {
                a + b
            }
            #[inline(always)]
            fn acc_diff(a: Self::Acc, b: Self::Acc) -> Self::Acc {
                a - b
            }
            #[inline(always)]
            fn acc_finish(a: Self::Acc) -> Self {
                a
            }
            #[inline(always)]
            fn cmul_c(ar: Self, ai: Self, wr: Self::Const, wi: Self::Const) -> (Self, Self) {
                (ar * wr - ai * wi, ar * wi + ai * wr)
            }
        }

        impl Arith for $t {
            const ZERO: Self = 0.0;

            #[inline(always)]
            fn from_f64(x: f64) -> Self {
                x as $t
            }
            #[inline(always)]
            #[allow(
                trivial_numeric_casts,
                clippy::cast_lossless,
                reason = "one macro body serves f32 and f64; `From` is available for one of them and a no-op cast for the other"
            )]
            fn to_f64(self) -> f64 {
                self as f64
            }
            #[inline(always)]
            fn mul_int(a: Self, k: u32) -> Self {
                a * (k as $t)
            }
            #[inline(always)]
            fn div_int(a: Self, k: u32) -> Self {
                a / (k as $t)
            }
            #[inline(always)]
            fn half_sum(a: Self, b: Self) -> Self {
                (a + b) * 0.5
            }
            #[inline(always)]
            fn half_diff(a: Self, b: Self) -> Self {
                (a - b) * 0.5
            }

            #[inline(always)]
            fn simd_stockham_pass(
                caps: vaco_simd::Caps,
                view: StageView<'_, Self>,
                sr: &[Self],
                si: &[Self],
                dr: &mut [Self],
                di: &mut [Self],
            ) -> bool {
                #[allow(clippy::redundant_closure_call)]
                ($simd)(caps, view, sr, si, dr, di)
            }
        }

        impl TxSample for $t {
            type Scale = $t;

            const IDENTITY_SCALE: Self::Scale = 1.0;

            #[inline(always)]
            fn apply_scale(x: Self, s: Self::Scale) -> Self {
                x * s
            }

            fn precision_name() -> &'static str {
                $name
            }
        }
    };
}

impl_float!(f32, "f32", crate::simd::stockham_pass_f32);
// `f64` has no vector kernel: at 2 lanes on NEON and 4 on AVX2 the win does not
// justify a second monomorphisation of every butterfly, and no codec asks for a
// hot `f64` transform. Measured, not assumed — see the benchmark report.
impl_float!(f64, "f64", |_, _, _: &[f64], _: &[f64], _: &mut [f64], _: &mut [f64]| false);

impl Lane for i32 {
    type Const = i32;
    type Acc = i64;

    const STAGE_SCALED: bool = true;

    #[inline(always)]
    fn add(a: Self, b: Self) -> Self {
        a.saturating_add(b)
    }
    #[inline(always)]
    fn sub(a: Self, b: Self) -> Self {
        a.saturating_sub(b)
    }
    #[inline(always)]
    fn neg(a: Self) -> Self {
        a.saturating_neg()
    }
    #[inline(always)]
    fn mul_c(a: Self, c: Self::Const) -> Self {
        fixed::qmul(a, c)
    }
    #[inline(always)]
    fn div_radix(a: Self, r: u32) -> Self {
        fixed::round_div(i64::from(a), i64::from(r))
    }
    #[inline(always)]
    fn acc_of(x: Self) -> Self::Acc {
        i64::from(x) << fixed::Q
    }
    #[inline(always)]
    fn acc_mul(x: Self, c: Self::Const) -> Self::Acc {
        i64::from(x) * i64::from(c)
    }
    #[inline(always)]
    fn acc_neg(a: Self::Acc) -> Self::Acc {
        a.saturating_neg()
    }
    #[inline(always)]
    fn acc_mul_add(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc {
        acc.saturating_add(i64::from(x) * i64::from(c))
    }
    #[inline(always)]
    fn acc_mul_sub(acc: Self::Acc, x: Self, c: Self::Const) -> Self::Acc {
        acc.saturating_sub(i64::from(x) * i64::from(c))
    }
    #[inline(always)]
    fn acc_sum(a: Self::Acc, b: Self::Acc) -> Self::Acc {
        a.saturating_add(b)
    }
    #[inline(always)]
    fn acc_diff(a: Self::Acc, b: Self::Acc) -> Self::Acc {
        a.saturating_sub(b)
    }
    #[inline(always)]
    fn acc_finish(a: Self::Acc) -> Self {
        fixed::round_shift(a, fixed::Q)
    }
    #[inline(always)]
    fn cmul_c(ar: Self, ai: Self, wr: Self::Const, wi: Self::Const) -> (Self, Self) {
        let re = (i64::from(ar) * i64::from(wr)).saturating_sub(i64::from(ai) * i64::from(wi));
        let im = (i64::from(ar) * i64::from(wi)).saturating_add(i64::from(ai) * i64::from(wr));
        (
            fixed::round_shift(re, fixed::Q),
            fixed::round_shift(im, fixed::Q),
        )
    }
}

impl Arith for i32 {
    const ZERO: Self = 0;

    #[inline(always)]
    fn from_f64(x: f64) -> Self {
        fixed::quantise(x)
    }
    #[inline(always)]
    fn to_f64(self) -> f64 {
        f64::from(self) / f64::from(1u32 << fixed::Q)
    }
    #[inline(always)]
    fn mul_int(a: Self, k: u32) -> Self {
        fixed::clamp_i32(i64::from(a).saturating_mul(i64::from(k)))
    }
    #[inline(always)]
    fn div_int(a: Self, k: u32) -> Self {
        fixed::round_div(i64::from(a), i64::from(k))
    }
    #[inline(always)]
    fn half_sum(a: Self, b: Self) -> Self {
        fixed::round_shift(i64::from(a) + i64::from(b), 1)
    }
    #[inline(always)]
    fn half_diff(a: Self, b: Self) -> Self {
        fixed::round_shift(i64::from(a) - i64::from(b), 1)
    }
}

impl TxSample for i32 {
    type Scale = i32;

    const IDENTITY_SCALE: Self::Scale = fixed::ONE;

    #[inline(always)]
    fn apply_scale(x: Self, s: Self::Scale) -> Self {
        fixed::qmul(x, s)
    }

    fn precision_name() -> &'static str {
        "i32"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::assertions_on_constants,
        reason = "pinning a const is the entire point: STAGE_SCALED is a contract term, and a silent flip would change every fixed-point codec's output"
    )]
    fn fixed_stage_scaling_is_on_and_float_is_off() {
        assert!(<i32 as Lane>::STAGE_SCALED);
        assert!(!<f32 as Lane>::STAGE_SCALED);
        assert!(!<f64 as Lane>::STAGE_SCALED);
    }

    #[test]
    fn fixed_cmul_matches_the_closed_form() {
        // (0.5 + 0.25i) · (0.5 - 0.5i) = 0.375 - 0.125i
        let (re, im) = <i32 as Lane>::cmul_c(1 << 30, 1 << 29, 1 << 30, -(1 << 30));
        assert_eq!(re, fixed::quantise(0.375));
        assert_eq!(im, fixed::quantise(-0.125));
    }

    #[test]
    fn fixed_div_radix_is_nearest_half_up() {
        assert_eq!(<i32 as Lane>::div_radix(8, 2), 4);
        assert_eq!(<i32 as Lane>::div_radix(7, 2), 4); // 3.5 -> 4
        assert_eq!(<i32 as Lane>::div_radix(-7, 2), -3); // -3.5 -> -3
        assert_eq!(<i32 as Lane>::div_radix(10, 3), 3);
        assert_eq!(<i32 as Lane>::div_radix(11, 3), 4);
    }

    #[test]
    fn fixed_arithmetic_saturates_rather_than_wrapping() {
        assert_eq!(<i32 as Lane>::add(i32::MAX, i32::MAX), i32::MAX);
        assert_eq!(<i32 as Lane>::sub(i32::MIN, i32::MAX), i32::MIN);
        assert_eq!(<i32 as Lane>::neg(i32::MIN), i32::MAX);
        assert_eq!(<i32 as Arith>::mul_int(i32::MAX, 4), i32::MAX);
    }
}
