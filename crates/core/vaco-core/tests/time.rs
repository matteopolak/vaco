//! `Timestamp`: rescaling against an independently derived reference, and
//! cross-base comparison against exact rational arithmetic.
//!
//! The reference below rounds by *deriving every mode from the floor quotient*,
//! where the implementation truncates towards zero and adjusts. Two different
//! derivations agreeing on several thousand cases including `i64::MIN`/`MAX` is
//! the point; a reference that reused the implementation's shape would only be
//! testing that the code equals itself.

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
use vaco_core::{Duration, ExactDuration, Rational, Rounding, TimeBase, Timestamp, rescale_rnd};

// --------------------------------------------------------------- reference

/// `a * b / c`, rounded, derived from the floor quotient. `None` on `c == 0`
/// or on a result that leaves `i64`.
fn reference(a: i64, b: i64, c: i64, mode: Rounding) -> Option<i64> {
    if c == 0 {
        return None;
    }
    let n = i128::from(a) * i128::from(b);
    let d = i128::from(c);
    // Normalise the divisor positive; the *value* keeps its own sign.
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    let floor = n.div_euclid(d);
    let rem = n.rem_euclid(d); // 0 <= rem < d
    let value_is_negative = n < 0;
    let result = if rem == 0 {
        floor
    } else {
        let ceil = floor + 1;
        match mode {
            Rounding::Down => floor,
            Rounding::Up => ceil,
            Rounding::Zero => {
                if value_is_negative {
                    ceil
                } else {
                    floor
                }
            }
            Rounding::Infinity => {
                if value_is_negative {
                    floor
                } else {
                    ceil
                }
            }
            Rounding::NearestAwayFromZero => match (2 * rem).cmp(&d) {
                Ordering::Greater => ceil,
                Ordering::Less => floor,
                // A tie goes away from zero.
                Ordering::Equal => {
                    if value_is_negative {
                        floor
                    } else {
                        ceil
                    }
                }
            },
        }
    };
    i64::try_from(result).ok()
}

// -------------------------------------------------------------- strategies

#[test]
fn duration_preserves_native_ticks_without_a_microsecond_intermediate() {
    for (ticks, base) in [
        (655_360, Rational::new(1, 28_224_000)),
        (1, Rational::new(1, 90_000)),
        (1, Rational::new(1001, 30_000)),
        (1024, Rational::new(1, 44_100)),
        (-655_360, Rational::new(1, 28_224_000)),
    ] {
        let duration = Timestamp::new(ticks).to_duration(base).unwrap();
        assert_eq!(duration.to_ticks(base), Some(ticks), "{ticks} at {base}");
        assert_eq!(
            ExactDuration::from_duration(duration).as_ratio(),
            ExactDuration::from_ticks(ticks, base).unwrap().as_ratio()
        );
    }
}

#[test]
fn duration_distinguishes_values_that_share_a_rounded_microsecond() {
    let base = Rational::new(1, 28_224_000);
    let first = Timestamp::new(1).to_duration(base).unwrap();
    let second = Timestamp::new(2).to_duration(base).unwrap();
    assert!(Duration::ZERO < first);
    assert!(first < second);
}

#[test]
fn exact_duration_arithmetic_and_integer_boundaries() {
    let third = Duration::from_ticks(1, Rational::new(1, 3)).unwrap();
    let half = Duration::from_ticks(1, Rational::new(1, 2)).unwrap();
    assert_eq!(third.checked_add(half).unwrap().as_ratio(), (5, 6));
    assert_eq!(third.checked_sub(half).unwrap().as_ratio(), (-1, 6));
    assert_eq!(third.checked_sub(third), Some(Duration::ZERO));
    assert_eq!(Duration::from_micros(500_000), half);
    assert_eq!(Duration::default().as_ratio(), (0, 1));

    for micros in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(Duration::from_micros(micros).as_micros(), micros);
    }
    let huge = Duration::from_ticks(i64::MAX, Rational::new(i32::MAX, 1)).unwrap();
    assert_eq!(huge.checked_micros(Rounding::default()), None);
    assert_eq!(huge.as_micros(), i64::MAX);
    assert_eq!(huge.to_ticks(Rational::new(i32::MAX, 1)), Some(i64::MAX));
    let negative = Duration::ZERO.checked_sub(huge).unwrap();
    assert_eq!(negative.as_micros(), i64::MIN);
}

#[test]
fn rounded_duration_conversion_handles_products_wider_than_u128() {
    let mut duration = Duration::from_micros(1_000_000_000_000_000);
    for denominator in [2_147_483_647, 2_147_483_629, 2_147_483_587] {
        duration = duration
            .checked_add(Duration::from_ticks(1, Rational::new(1, denominator)).unwrap())
            .unwrap();
    }
    // The three added fractions total under two nanoseconds. They must not
    // change nearest-microsecond output, even though direct multiplication overflows.
    assert!(duration.as_ratio().0.checked_mul(1_000_000).is_none());
    assert_eq!(
        duration.checked_micros(Rounding::default()),
        Some(1_000_000_000_000_000)
    );
    assert_eq!(duration.as_micros(), 1_000_000_000_000_000);
    assert_eq!(
        Duration::ZERO.checked_sub(duration).unwrap().as_micros(),
        -1_000_000_000_000_000
    );
}

#[test]
fn exact_duration_keeps_fractional_microseconds() {
    let duration = ExactDuration::from_ticks(1024, Rational::new(1, 44_100)).unwrap();
    assert_eq!(duration.as_ratio(), (256, 11_025));
    assert_eq!(
        duration.to_duration(Rounding::NearestAwayFromZero),
        Some(Duration::from_micros(23_220))
    );
}

#[test]
fn exact_duration_ordering_does_not_cross_multiply() {
    let third = ExactDuration::from_ticks(1, Rational::new(1, 3)).unwrap();
    let half = ExactDuration::from_ticks(1, Rational::new(1, 2)).unwrap();
    let negative = ExactDuration::from_ticks(-1, Rational::new(1, 3)).unwrap();
    assert!(third < half);
    assert!(negative < third);
    assert_eq!(negative.cmp(&negative), Ordering::Equal);
}

fn edge_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        4 => any::<i64>(),
        3 => -1_000_000i64..1_000_000,
        3 => prop::sample::select(vec![
            i64::MIN, i64::MIN + 1, -1, 0, 1, 2, -2, 90_000, 1_000_000,
            i64::MAX - 1, i64::MAX,
        ]),
    ]
}

fn small_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        3 => -1_000_000i64..1_000_000,
        2 => prop::sample::select(vec![
            i64::from(i32::MIN), -1001, -1000, -1, 1, 24, 25, 1000, 1001, 30_000,
            48_000, 90_000, 1_000_000, i64::from(i32::MAX),
        ]),
    ]
}

/// A base that names a real tick length: finite, non-zero numerator.
fn any_base() -> impl Strategy<Value = TimeBase> {
    prop_oneof![
        3 => prop::sample::select(vec![
            Rational::new(1, 90_000),
            Rational::new(1, 1_000_000),
            Rational::new(1, 1000),
            Rational::new(1001, 30_000),
            Rational::new(1, 25),
            Rational::new(1, 48_000),
            Rational::new(-1, 25),
            Rational::new(1, -25),
            Rational::new(i32::MAX, 1),
            Rational::new(1, i32::MAX),
            Rational::new(i32::MIN, 1),
            Rational::new(1, i32::MIN),
        ]),
        2 => (1i32..1_000_000, 1i32..1_000_000).prop_map(|(n, d)| Rational::new(n, d)),
    ]
}

// ------------------------------------------------------------------- units

#[test]
fn absent_stays_absent() {
    let tb = Rational::new(1, 1000);
    assert_eq!(
        Timestamp::NONE.rescale(tb, tb, Rounding::default()),
        Timestamp::NONE
    );
    assert!(Timestamp::NONE.ticks().is_none());
    assert_eq!(Timestamp::NONE.to_string(), "N/A");
    assert_eq!(Timestamp::NONE.compare(tb, Timestamp::new(0), tb), None);
}

#[test]
fn unusable_bases_are_refused_not_guessed() {
    let ts = Timestamp::new(1000);
    let good = Rational::new(1, 1000);
    for bad in [Rational::UNDEFINED, Rational::INFINITY, Rational::ZERO] {
        assert_eq!(ts.rescale(bad, good, Rounding::default()), Timestamp::NONE);
        assert_eq!(ts.rescale(good, bad, Rounding::default()), Timestamp::NONE);
        assert_eq!(ts.compare(bad, ts, good), None);
    }
}

#[test]
fn ntsc_frames_land_on_exact_ticks() {
    // Frame 30 at 30000/1001 fps, expressed in the 90 kHz base every MPEG
    // container uses. 30 * 1001/30000 s = 1.001 s = 90090 ticks, exactly.
    let frame = Timestamp::new(30);
    let fps_base = Rational::new(1001, 30_000);
    let ticks = frame.rescale(fps_base, Rational::new(1, 90_000), Rounding::Zero);
    assert_eq!(ticks.ticks(), Some(90_090));
    // And back again, with no drift.
    assert_eq!(
        ticks
            .rescale(
                Rational::new(1, 90_000),
                fps_base,
                Rounding::NearestAwayFromZero
            )
            .ticks(),
        Some(30)
    );
}

#[test]
fn two_hours_of_ntsc_does_not_drift() {
    // 216 000 frames at 30000/1001 is exactly 7207.2 s. Through the
    // microsecond base and back, every frame index must return unchanged —
    // this is the accumulation an f64 pipeline gets wrong.
    let fps_base = Rational::new(1001, 30_000);
    for frame in [0i64, 1, 12_345, 215_999, 216_000] {
        let us = Timestamp::new(frame).rescale(fps_base, TimeBase::MICROSECONDS, Rounding::Zero);
        let back = us.rescale(
            TimeBase::MICROSECONDS,
            fps_base,
            Rounding::NearestAwayFromZero,
        );
        assert_eq!(back.ticks(), Some(frame), "frame {frame}");
    }
}

#[test]
fn every_rounding_mode_on_the_same_input() {
    let tb = Rational::new(1, 3);
    let to = Rational::new(1, 1);
    // 5 ticks of 1/3 s is 5/3 s = 1.666… ticks of 1 s.
    let cases = [
        (Rounding::Zero, 1),
        (Rounding::Infinity, 2),
        (Rounding::Down, 1),
        (Rounding::Up, 2),
        (Rounding::NearestAwayFromZero, 2),
    ];
    for (mode, want) in cases {
        assert_eq!(
            Timestamp::new(5).rescale(tb, to, mode).ticks(),
            Some(want),
            "{mode:?}"
        );
    }
    // …and the mirror image, at -5/3.
    let cases = [
        (Rounding::Zero, -1),
        (Rounding::Infinity, -2),
        (Rounding::Down, -2),
        (Rounding::Up, -1),
        (Rounding::NearestAwayFromZero, -2),
    ];
    for (mode, want) in cases {
        assert_eq!(
            Timestamp::new(-5).rescale(tb, to, mode).ticks(),
            Some(want),
            "{mode:?}"
        );
    }
}

#[test]
fn ties_go_away_from_zero() {
    let from = Rational::new(1, 2);
    let to = Rational::new(1, 1);
    assert_eq!(
        Timestamp::new(1)
            .rescale(from, to, Rounding::NearestAwayFromZero)
            .ticks(),
        Some(1)
    );
    assert_eq!(
        Timestamp::new(-1)
            .rescale(from, to, Rounding::NearestAwayFromZero)
            .ticks(),
        Some(-1)
    );
    assert_eq!(
        Timestamp::new(3)
            .rescale(from, to, Rounding::NearestAwayFromZero)
            .ticks(),
        Some(2)
    );
    assert_eq!(
        Timestamp::new(-3)
            .rescale(from, to, Rounding::NearestAwayFromZero)
            .ticks(),
        Some(-2)
    );
}

#[test]
fn overflow_saturates_in_rescale_and_refuses_in_checked() {
    let from = Rational::new(1, 1);
    let to = Rational::new(1, 1_000_000);
    let ts = Timestamp::new(i64::MAX);
    assert_eq!(ts.rescale(from, to, Rounding::Zero).ticks(), Some(i64::MAX));
    assert_eq!(ts.checked_rescale(from, to, Rounding::Zero), None);
    let ts = Timestamp::new(i64::MIN);
    assert_eq!(ts.rescale(from, to, Rounding::Zero).ticks(), Some(i64::MIN));
    assert_eq!(rescale_rnd(i64::MAX, 1_000_000, 1, Rounding::Zero), None);
    assert_eq!(rescale_rnd(1, 1, 0, Rounding::Zero), None);
    assert_eq!(rescale_rnd(i64::MIN, 1, 1, Rounding::Zero), Some(i64::MIN));
}

#[test]
fn cross_base_comparison_is_exact() {
    // 1/3 s in a 1/3 base against 1/3 s approximated in a 1/1000000 base:
    // the microsecond value is strictly smaller, and no float would say so.
    let a = Timestamp::new(1);
    let b = Timestamp::new(333_333);
    assert_eq!(
        a.compare(Rational::new(1, 3), b, TimeBase::MICROSECONDS),
        Some(Ordering::Greater)
    );
    assert_eq!(
        Timestamp::new(1).compare(
            Rational::new(1, 25),
            Timestamp::new(3600),
            Rational::new(1, 90_000)
        ),
        Some(Ordering::Equal)
    );
}

// ---------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Every rounding mode agrees with the independent reference, at every
    /// magnitude including the `i64` extremes.
    #[test]
    fn rescale_rnd_matches_the_reference(
        a in edge_i64(),
        b in small_i64(),
        c in small_i64(),
        mode in prop::sample::select(Rounding::ALL.to_vec()),
    ) {
        prop_assert_eq!(rescale_rnd(a, b, c, mode), reference(a, b, c, mode), "{:?}", mode);
    }

    /// The error a rescale introduces never exceeds the mode's stated bound.
    #[test]
    fn rescale_stays_within_the_stated_bound(
        ticks in -1_000_000_000i64..1_000_000_000,
        from in any_base(),
        to in any_base(),
        mode in prop::sample::select(Rounding::ALL.to_vec()),
    ) {
        let Some(out) = Timestamp::new(ticks).checked_rescale(from, to, mode) else {
            return Ok(());
        };
        let got = out.ticks().unwrap();
        // Exact target, as a rational: ticks * from / to.
        let (fnum, fden) = (i128::from(from.num), i128::from(from.den));
        let (tnum, tden) = (i128::from(to.num), i128::from(to.den));
        let (num, den) = (i128::from(ticks) * fnum * tden, fden * tnum);
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        // |got - num/den| <= bound  <=>  |got*den - num| <= bound*den.
        let err = (i128::from(got) * den - num).abs();
        let bound = if mode == Rounding::NearestAwayFromZero { den } else { 2 * den };
        prop_assert!(2 * err <= bound, "err {} den {} mode {:?}", err, den, mode);
    }

    /// A round trip through another base returns to within one tick of the
    /// coarser of the two grids — never silently further.
    #[test]
    fn rescale_round_trips(
        ticks in -1_000_000i64..1_000_000,
        from in any_base(),
        to in any_base(),
        mode in prop::sample::select(Rounding::ALL.to_vec()),
    ) {
        let ts = Timestamp::new(ticks);
        let Some(there) = ts.checked_rescale(from, to, mode) else { return Ok(()); };
        let Some(back) = there.checked_rescale(to, from, mode) else { return Ok(()); };
        // One `to` tick, measured in `from` ticks, rounded up — plus one for the
        // second rounding.
        let one_tick = Timestamp::new(1)
            .checked_rescale(to, from, Rounding::Infinity)
            .and_then(Timestamp::ticks)
            .unwrap_or(i64::MAX)
            .saturating_abs();
        let drift = i128::from(back.ticks().unwrap()) - i128::from(ticks);
        prop_assert!(
            drift.abs() <= i128::from(one_tick) + 1,
            "drift {} exceeds {} + 1", drift, one_tick
        );
    }

    /// Rescaling a base to itself is the identity, whatever the mode.
    #[test]
    fn rescale_to_the_same_base_is_identity(
        ticks in edge_i64(),
        base in any_base(),
        mode in prop::sample::select(Rounding::ALL.to_vec()),
    ) {
        prop_assert_eq!(
            Timestamp::new(ticks).rescale(base, base, mode).ticks(),
            Some(ticks)
        );
    }

    /// Cross-base comparison agrees with exact rational arithmetic, is
    /// antisymmetric, and never goes through a float.
    #[test]
    fn compare_is_exact_and_antisymmetric(
        a in edge_i64(),
        b in edge_i64(),
        atb in any_base(),
        btb in any_base(),
    ) {
        let (ta, tb_) = (Timestamp::new(a), Timestamp::new(b));
        let got = ta.compare(atb, tb_, btb).unwrap();
        // Reference: seconds are a*an/ad vs b*bn/bd, with both denominators
        // made positive so cross-multiplication preserves the order.
        let norm = |r: Rational| {
            let (n, d) = (i128::from(r.num), i128::from(r.den));
            if d < 0 { (-n, -d) } else { (n, d) }
        };
        let (an, ad) = norm(atb);
        let (bn, bd) = norm(btb);
        let want = (i128::from(a) * an * bd).cmp(&(i128::from(b) * bn * ad));
        prop_assert_eq!(got, want);
        prop_assert_eq!(tb_.compare(btb, ta, atb).unwrap(), got.reverse());
    }

    /// Comparison in one shared base is plain integer comparison.
    #[test]
    fn compare_in_one_base_is_integer_order(a in edge_i64(), b in edge_i64(), base in any_base()) {
        let want = if base.to_f64() >= 0.0 { a.cmp(&b) } else { a.cmp(&b).reverse() };
        prop_assert_eq!(
            Timestamp::new(a).compare(base, Timestamp::new(b), base).unwrap(),
            want
        );
    }

    /// Nothing panics, whatever base or tick count arrives.
    #[test]
    fn rescale_is_total(
        ticks in edge_i64(),
        from in (any::<i32>(), any::<i32>()).prop_map(|(n, d)| Rational::new(n, d)),
        to in (any::<i32>(), any::<i32>()).prop_map(|(n, d)| Rational::new(n, d)),
        mode in prop::sample::select(Rounding::ALL.to_vec()),
    ) {
        let ts = Timestamp::new(ticks);
        let _ = ts.rescale(from, to, mode);
        let _ = ts.checked_rescale(from, to, mode);
        let _ = ts.compare(from, ts, to);
        let _ = ts.to_seconds(from);
        let _ = ts.to_duration(from);
    }
}
