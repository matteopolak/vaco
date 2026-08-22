//! `Rational`: unit tests at the boundaries, property tests everywhere else.
//!
//! The boundaries are where overflow bugs hide, so `i32::MIN`, `i32::MAX` and a
//! zero denominator are tested explicitly rather than left to the sampler to
//! stumble into.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::float_cmp,
    clippy::many_single_char_names,
    reason = "test code; assertions are the point and the inputs are known"
)]

use std::cmp::Ordering;

use proptest::prelude::*;
use vaco_core::Rational;

// --------------------------------------------------------------- reference

/// Canonical `(num, den)` in `i128`, computed independently of the crate:
/// `den > 0` for a finite value, `den == 0` with `num` in `{-1, 0, 1}`
/// otherwise. No gcd — the comparison below does not need lowest terms.
fn widen(r: Rational) -> (i128, i128) {
    let (n, d) = (i128::from(r.num), i128::from(r.den));
    match d.cmp(&0) {
        Ordering::Equal => (n.signum(), 0),
        Ordering::Less => (-n, -d),
        Ordering::Greater => (n, d),
    }
}

/// `0` finite, `1`/`-1` for ±inf, `-2` undefined — the order key.
fn class(r: Rational) -> i32 {
    let (n, d) = widen(r);
    if d != 0 {
        0
    } else if n == 0 {
        -2
    } else if n > 0 {
        1
    } else {
        -1
    }
}

/// The exact order, derived from the definition rather than from the crate.
fn reference_cmp(a: Rational, b: Rational) -> Ordering {
    match class(a).cmp(&class(b)) {
        Ordering::Equal => {}
        o => return o,
    }
    let (an, ad) = widen(a);
    let (bn, bd) = widen(b);
    if ad == 0 {
        return Ordering::Equal;
    }
    (an * bd).cmp(&(bn * ad))
}

// -------------------------------------------------------------- strategies

fn edge_i32() -> impl Strategy<Value = i32> {
    prop_oneof![
        4 => any::<i32>(),
        3 => -64i32..64,
        3 => prop::sample::select(vec![
            i32::MIN, i32::MIN + 1, i32::MIN + 2, -30000, -1001, -1000, -2, -1, 0,
            1, 2, 1000, 1001, 24, 25, 30000, 90000, i32::MAX - 1, i32::MAX,
        ]),
    ]
}

fn any_rational() -> impl Strategy<Value = Rational> {
    (edge_i32(), edge_i32()).prop_map(|(n, d)| Rational::new(n, d))
}

// ------------------------------------------------------------- unit: basics

#[test]
fn constants_are_what_they_say() {
    assert_eq!(Rational::default(), Rational::UNDEFINED);
    assert!(Rational::UNDEFINED.is_undefined());
    assert!(!Rational::UNDEFINED.is_defined());
    assert!(Rational::INFINITY.is_infinite());
    assert!(Rational::NEG_INFINITY.is_infinite());
    assert!(Rational::ZERO.is_zero());
    assert!(!Rational::UNDEFINED.is_zero());
    assert_eq!(Rational::MICROSECONDS, Rational::new(1, 1_000_000));
}

#[test]
fn equality_is_by_value() {
    assert_eq!(Rational::new(1, 2), Rational::new(2, 4));
    assert_eq!(Rational::new(1, 2), Rational::new(-1, -2));
    assert_eq!(Rational::new(1, 0), Rational::new(7, 0));
    assert_ne!(Rational::new(1, 0), Rational::new(-1, 0));
    assert_ne!(Rational::new(0, 0), Rational::new(0, 1));
    assert_eq!(Rational::new(0, 0), Rational::new(0, 0));
}

#[test]
fn ntsc_is_not_2997() {
    let r = Rational::new(30000, 1001);
    assert_eq!(r.reduced(), r);
    assert_ne!(r, Rational::new(2997, 100));
    assert!(r > Rational::new(2997, 100));
    assert_eq!(r.to_string(), "30000/1001");
    assert_eq!("30000/1001".parse::<Rational>().unwrap(), r);
}

#[test]
fn undefined_has_no_order_but_has_a_total_one() {
    let u = Rational::UNDEFINED;
    assert_eq!(u.partial_cmp(&Rational::ONE), None);
    assert_eq!(Rational::ONE.partial_cmp(&u), None);
    assert_eq!(u.partial_cmp(&u), None);
    // cmp_exact is total and puts undefined below -inf.
    assert_eq!(u.cmp_exact(u), Ordering::Equal);
    assert_eq!(u.cmp_exact(Rational::NEG_INFINITY), Ordering::Less);
    assert_eq!(
        Rational::INFINITY.cmp_exact(Rational::ONE),
        Ordering::Greater
    );
}

// ------------------------------------------------------ unit: the boundaries

#[test]
fn reduce_at_i32_min() {
    // i32::MIN / -1 is 2^31, which no i32 numerator can hold: saturate, never wrap.
    let r = Rational::new(i32::MIN, -1).reduced();
    assert_eq!(r, Rational::new(i32::MAX, 1));
    assert!(Rational::new(i32::MIN, -1).checked_reduced().is_none());

    // i32::MIN / -2 reduces to 2^30 / 1, which fits exactly.
    assert_eq!(
        Rational::new(i32::MIN, -2).reduced(),
        Rational::new(1 << 30, 1)
    );
    assert_eq!(
        Rational::new(i32::MIN, 1).reduced(),
        Rational::new(i32::MIN, 1)
    );
    assert_eq!(
        Rational::new(i32::MIN, 2).reduced(),
        Rational::new(-(1 << 30), 1)
    );

    // 1 / i32::MIN: the sign has to move to the numerator, and 2^31 does not fit
    // there either. The result is the closest representable rational, not a wrap.
    let r = Rational::new(1, i32::MIN).reduced();
    assert!(r.num < 0 && r.den > 0);
    assert!(r.to_f64() < 0.0 && r.to_f64() > -1e-9);
}

#[test]
fn multiplication_does_not_overflow_at_the_boundaries() {
    assert_eq!(
        Rational::new(i32::MAX, 1) * Rational::new(1, i32::MAX),
        Rational::ONE
    );
    assert_eq!(
        Rational::new(i32::MIN, 1) * Rational::new(1, i32::MIN),
        Rational::ONE
    );
    assert_eq!(
        Rational::new(1, 1001) * Rational::new(1001, 1),
        Rational::ONE
    );
    // 2^31-1 squared does not fit; the operator approximates, checked_mul refuses.
    let big = Rational::new(i32::MAX, 1);
    assert!(big.checked_mul(big).is_none());
    assert_eq!(big * big, Rational::new(i32::MAX, 1));

    assert_eq!(
        Rational::new(30000, 1001) * Rational::new(1001, 30000),
        Rational::ONE
    );
}

#[test]
fn infinity_and_undefined_propagate_without_panicking() {
    assert_eq!(Rational::INFINITY * Rational::ONE, Rational::INFINITY);
    assert_eq!(
        Rational::INFINITY * Rational::new(-2, 1),
        Rational::NEG_INFINITY
    );
    assert_eq!(Rational::INFINITY * Rational::ZERO, Rational::UNDEFINED);
    assert_eq!(Rational::UNDEFINED * Rational::ONE, Rational::UNDEFINED);
    assert_eq!(Rational::ONE / Rational::ZERO, Rational::INFINITY);
    assert_eq!(Rational::ZERO / Rational::ZERO, Rational::UNDEFINED);
    assert_eq!(
        Rational::INFINITY + Rational::NEG_INFINITY,
        Rational::UNDEFINED
    );
    assert_eq!(Rational::INFINITY + Rational::ONE, Rational::INFINITY);
    assert_eq!(
        Rational::new(i32::MIN, 1) - Rational::new(i32::MIN, 1),
        Rational::ZERO
    );
    assert_eq!(-Rational::new(i32::MIN, 1), Rational::new(i32::MIN, -1));
}

#[test]
fn negation_at_the_double_i32_min() {
    // Found by proptest: `i32::MIN / i32::MIN` is exactly 1, and neither field
    // can be negated in place without wrapping back onto itself.
    let r = Rational::new(i32::MIN, i32::MIN);
    assert_eq!(r, Rational::ONE);
    assert_eq!(-r, Rational::new(-1, 1));
    assert_eq!(r - r, Rational::ZERO);
    assert_eq!(Rational::ZERO - r, Rational::new(-1, 1));
    assert_eq!(r.checked_sub(r), Some(Rational::ZERO));
}

#[test]
fn approximate_hits_the_broadcast_rates() {
    assert_eq!(
        Rational::approximate(30000.0 / 1001.0, 1001),
        Rational::new(30000, 1001)
    );
    assert_eq!(
        Rational::approximate(24000.0 / 1001.0, 1001),
        Rational::new(24000, 1001)
    );
    assert_eq!(Rational::approximate(0.25, 100), Rational::new(1, 4));
    assert_eq!(Rational::approximate(-0.5, 100), Rational::new(-1, 2));
    assert_eq!(Rational::approximate(0.0, 100), Rational::ZERO);
    assert_eq!(Rational::approximate(f64::NAN, 100), Rational::UNDEFINED);
    assert_eq!(
        Rational::approximate(f64::INFINITY, 100),
        Rational::INFINITY
    );
    assert_eq!(
        Rational::approximate(f64::NEG_INFINITY, 100),
        Rational::NEG_INFINITY
    );
    // A denominator budget of 1 forces an integer.
    assert_eq!(Rational::approximate(2.6, 1), Rational::new(3, 1));
    // Beyond i32 the answer saturates rather than wrapping.
    assert_eq!(
        Rational::approximate(1e30, 1000),
        Rational::new(i32::MAX, 1)
    );
    assert_eq!(
        Rational::approximate(-1e30, 1000),
        Rational::new(-i32::MAX, 1)
    );
    // pi to a thousandth: the classic convergent.
    assert_eq!(
        Rational::approximate(std::f64::consts::PI, 113),
        Rational::new(355, 113)
    );
}

#[test]
fn reduce_helper_reports_exactness() {
    assert_eq!(Rational::reduce(6, 8, 100), (Rational::new(3, 4), true));
    // Coprime, and far too large for a denominator budget of 10.
    let (r, exact) = Rational::reduce(1_000_000_007, 3_141_592_653, 10);
    assert!(!exact);
    assert_eq!(r, Rational::new(1, 3));
    // Exactly reducible, but still over the budget: not exact either.
    let (r, exact) = Rational::reduce(1_000_000_007, 3_000_000_021, 10);
    assert!(exact, "3 * 1000000007 reduces to 1/3, which fits");
    assert_eq!(r, Rational::new(1, 3));
    assert_eq!(Rational::reduce(i64::MAX, 1, i64::MAX).0.den, 1);
}

// ---------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// `reduced()` returns the same number, in canonical form, whenever that
    /// number is representable at all; when it is not, it is still close.
    #[test]
    fn reduced_preserves_value(r in any_rational()) {
        let red = r.reduced();
        prop_assert!(red.den >= 0);
        if red.den == 0 {
            prop_assert!((-1..=1).contains(&red.num));
        }
        if let Some(exact) = r.checked_reduced() {
            prop_assert_eq!(red, exact);
            prop_assert_eq!(reference_cmp(red, r), Ordering::Equal);
        } else {
            // Not representable: the fallback must still be finite, signed
            // correctly and close to the true value.
            prop_assert!(red.den > 0);
            prop_assert_eq!(red.signum(), r.signum());
            let err = (red.to_f64() - r.to_f64()).abs();
            prop_assert!(err <= r.to_f64().abs() / 1e9 + 1e-9, "err {}", err);
        }
        // Canonical form: reducing twice changes nothing.
        prop_assert_eq!(red.reduced(), red);
    }

    /// The reduced form really is in lowest terms.
    #[test]
    fn reduced_is_coprime(r in any_rational()) {
        let red = r.reduced();
        if red.den > 0 {
            let (mut a, mut b) = (u64::from(red.num.unsigned_abs()), u64::from(red.den.unsigned_abs()));
            while b != 0 { let t = a % b; a = b; b = t; }
            if red.num != 0 {
                prop_assert_eq!(a, 1);
            }
        }
    }

    /// `cmp_exact` agrees with the exact rational value, computed independently.
    #[test]
    fn cmp_exact_matches_the_reference(a in any_rational(), b in any_rational()) {
        prop_assert_eq!(a.cmp_exact(b), reference_cmp(a, b));
        prop_assert_eq!(a.cmp_exact(b), b.cmp_exact(a).reverse());
        prop_assert_eq!(a == b, a.cmp_exact(b) == Ordering::Equal);
    }

    /// It is a total order: transitive on every triple.
    #[test]
    fn cmp_exact_is_transitive(a in any_rational(), b in any_rational(), c in any_rational()) {
        if a.cmp_exact(b) != Ordering::Greater && b.cmp_exact(c) != Ordering::Greater {
            prop_assert_ne!(a.cmp_exact(c), Ordering::Greater);
        }
        prop_assert_eq!(a.cmp_exact(a), Ordering::Equal);
    }

    /// `PartialOrd` is `None` exactly for undefined operands, and otherwise
    /// agrees with the total order. Equality and ordering never disagree.
    #[test]
    fn partial_ord_is_consistent_with_eq(a in any_rational(), b in any_rational()) {
        match a.partial_cmp(&b) {
            None => prop_assert!(a.is_undefined() || b.is_undefined()),
            Some(o) => {
                prop_assert!(!a.is_undefined() && !b.is_undefined());
                prop_assert_eq!(o, a.cmp_exact(b));
                prop_assert_eq!(o == Ordering::Equal, a == b);
            }
        }
    }

    /// Equal values hash equally — required because equality is by value.
    #[test]
    fn hash_agrees_with_eq(a in any_rational(), b in any_rational()) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let h = |r: Rational| { let mut s = DefaultHasher::new(); r.hash(&mut s); s.finish() };
        if a == b {
            prop_assert_eq!(h(a), h(b));
        }
    }

    /// `checked_*` are exact when they answer at all, and always canonical.
    #[test]
    fn checked_arithmetic_is_exact(a in any_rational(), b in any_rational()) {
        let (an, ad) = widen(a);
        let (bn, bd) = widen(b);
        if ad > 0 && bd > 0 {
            if let Some(p) = a.checked_mul(b) {
                let (pn, pd) = widen(p);
                prop_assert!(pd > 0);
                prop_assert_eq!(pn * (ad * bd), (an * bn) * pd);
            }
            if let Some(s) = a.checked_add(b) {
                let (sn, sd) = widen(s);
                prop_assert!(sd > 0);
                prop_assert_eq!(sn * (ad * bd), (an * bd + bn * ad) * sd);
            }
            if let Some(d) = a.checked_sub(b) {
                let (dn, dd) = widen(d);
                prop_assert_eq!(dn * (ad * bd), (an * bd - bn * ad) * dd);
            }
        }
    }

    /// The operator forms never panic and never wrap, whatever they are given.
    #[test]
    fn operators_are_total(a in any_rational(), b in any_rational()) {
        let _ = a * b;
        let _ = a / b;
        let _ = a + b;
        let _ = a - b;
        let _ = -a;
        let _ = a.to_f64();
        let _ = a.inverse();
    }

    /// `approximate` respects its denominator budget and lands within it.
    #[test]
    fn approximate_is_within_its_bound(
        v in -1.0e6f64..1.0e6,
        max_den in 1i32..100_000,
    ) {
        let r = Rational::approximate(v, max_den);
        prop_assert!(r.den > 0 && r.den <= max_den, "den {} > {}", r.den, max_den);
        // The result is a best approximation for its own denominator, so it is
        // within half a step of that denominator's grid. Stating it this way
        // rather than as `1/max_den` is deliberate: the `i32` numerator budget
        // can bind first for large values, and then no fraction with the full
        // `max_den` exists to be compared against.
        let err = (r.to_f64() - v).abs();
        let bound = 0.5 / f64::from(r.den) + v.abs() * 1e-12 + 1e-12;
        prop_assert!(err <= bound, "err {} > {} for {}/{}", err, bound, r.num, r.den);
    }

    /// An exactly representable ratio is recovered exactly when the budget allows.
    #[test]
    fn approximate_recovers_exact_ratios(n in -100_000i32..100_000, d in 1i32..100_000) {
        let r = Rational::new(n, d).reduced();
        prop_assert_eq!(Rational::approximate(r.to_f64(), r.den), r);
    }

    /// Display and `FromStr` round-trip on the stored pair, unreduced.
    #[test]
    fn display_roundtrips(r in any_rational()) {
        let s = r.to_string();
        let back: Rational = s.parse().unwrap();
        prop_assert_eq!(back.num, r.num);
        prop_assert_eq!(back.den, r.den);
    }
}
