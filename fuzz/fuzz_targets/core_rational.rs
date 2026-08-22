//! Exact rational arithmetic at the boundaries.
//!
//! Overflow here is silent and corrupts timestamps rather than crashing, which
//! is why this is fuzzed rather than only proptested.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::Rational;

fuzz_target!(|data: (i32, i32, i32, i32)| {
    let (a, b, c, d) = data;
    let x = Rational::new(a, b);
    let y = Rational::new(c, d);

    // Reduction preserves value, so ordering against any third value is unchanged.
    assert_eq!(x.cmp_exact(y), x.reduced().cmp_exact(y.reduced()));

    // The total order is antisymmetric even at i32::MIN.
    assert_eq!(x.cmp_exact(y), y.cmp_exact(x).reverse());

    // Operators saturate rather than wrapping; checked_* agrees when it returns.
    if let Some(m) = x.checked_mul(y) {
        assert_eq!(m, x * y);
    }
});
