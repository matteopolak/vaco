//! Deterministic edge-case generators.
//!
//! Random input finds average-case bugs. The interesting SIMD divergences live
//! at the loop tail, at saturation boundaries, at the representable extremes,
//! and — for floats — at the values ordinary arithmetic never produces on its
//! own. Every generator here is a pure function of a size, so a failing case
//! is reproducible from the report alone; nothing needs a seed.
//!
//! [`vaco_simd::testing`] already sweeps every *byte pattern* at every length
//! from 0 to 193 for `u8`-lane kernels — reuse it directly for that shape.
//! What is missing, and what this module adds, is everything that shape does
//! not cover: lengths expressed in *elements* rather than bytes (so a kernel
//! whose native width is 4 or 8 `i32` lanes gets the same tail coverage a
//! byte-oriented kernel gets for free), integer boundaries away from `u8`, and
//! float specials.

/// Every native vector width the runtime substrate can resolve to, in
/// elements, for a lane type whose byte width is `lane_bytes`.
///
/// `vaco-simd`'s tiers are 128-bit (`SSE2`/`SSE4.2`/`NEON`/wasm), 256-bit
/// (`AVX2`) and 512-bit (`AVX-512`) — see [`vaco_simd::Tier`]. Widths beyond
/// what `lane_bytes` divides evenly are simply skipped.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the divisor is checked non-zero and exactly-dividing on the line above"
)]
pub fn element_widths(lane_bytes: usize) -> Vec<usize> {
    [16usize, 32, 64]
        .into_iter()
        .filter_map(|bits| (lane_bytes > 0 && bits % lane_bytes == 0).then_some(bits / lane_bytes))
        .collect()
}

/// Lengths that straddle every width in `widths`: `width - 1`, `width`,
/// `width + 1`, and the same one and two widths further out — the zero-length,
/// sub-vector, exact-fit and spilling-tail cases a random-length sweep will
/// only hit by luck.
///
/// Always includes `0` and `1`. Deduplicated and sorted, so a caller can loop
/// over the result once.
#[must_use]
pub fn lengths_around(widths: &[usize]) -> Vec<usize> {
    let mut out = vec![0usize, 1usize];
    for &w in widths {
        if w == 0 {
            continue;
        }
        for multiple in 1..=3usize {
            let base = w * multiple;
            out.push(base.saturating_sub(1));
            out.push(base);
            out.push(base + 1);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `u8` values worth probing in isolation: the additive identity, both ends of
/// the range, and the midpoint where a signed reinterpretation flips sign.
#[must_use]
pub fn boundaries_u8() -> Vec<u8> {
    vec![0, 1, 127, 128, 129, 254, 255]
}

/// `i16` values at the saturating-arithmetic boundaries: both extremes, the
/// values one step inside them (where a saturating op and a wrapping op start
/// to disagree), and the ordinary landmarks.
#[must_use]
pub fn boundaries_i16() -> Vec<i16> {
    vec![
        i16::MIN,
        i16::MIN + 1,
        -2,
        -1,
        0,
        1,
        2,
        i16::MAX - 1,
        i16::MAX,
    ]
}

/// `i32` values at the saturating-arithmetic boundaries. Same shape as
/// [`boundaries_i16`], one width up.
#[must_use]
pub fn boundaries_i32() -> Vec<i32> {
    vec![
        i32::MIN,
        i32::MIN + 1,
        -2,
        -1,
        0,
        1,
        2,
        i32::MAX - 1,
        i32::MAX,
    ]
}

/// `f32` values ordinary arithmetic never produces on its own: signed zeros,
/// the smallest and largest normals, the smallest subnormal (denormal) of
/// each sign, the infinities, and NaN.
///
/// A kernel comparing these lane-for-lane against a scalar reference should
/// not use plain `==` for the comparison — `NaN != NaN` — see
/// [`crate::Kernel::lanes_match`].
#[must_use]
pub fn float_specials_f32() -> Vec<f32> {
    vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        f32::from_bits(1),             // smallest positive subnormal
        f32::from_bits(1 | (1 << 31)), // smallest negative subnormal
        f32::EPSILON,
        f32::MIN,
        f32::MAX,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ]
}

/// A ramp `0, 1, ..., len - 1` reduced into `0..=max` by wrapping, so the
/// pattern still varies past 256 elements without ever leaving the domain a
/// caller declares valid.
#[must_use]
pub fn ramp_bounded(len: usize, max: i32) -> Vec<i32> {
    if max < 0 {
        return vec![0; len];
    }
    let period = i64::from(max) + 1;
    (0..len)
        .map(|i| {
            let i = i64::try_from(i).unwrap_or(i64::MAX);
            i32::try_from(i % period).unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_widths_skips_indivisible_bit_widths() {
        // i32 lanes: 128/32=4, 256/32=8, 512/32=16.
        assert_eq!(element_widths(4), vec![4, 8, 16]);
        // A 3-byte lane type divides none of them.
        assert!(element_widths(3).is_empty());
    }

    #[test]
    fn lengths_around_covers_the_tail_of_every_width() {
        let lens = lengths_around(&[4, 8]);
        for w in [4usize, 8] {
            assert!(lens.contains(&(w - 1)));
            assert!(lens.contains(&w));
            assert!(lens.contains(&(w + 1)));
        }
        assert!(lens.contains(&0));
        assert!(lens.contains(&1));
        assert_eq!(lens, {
            let mut sorted = lens.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted
        });
    }

    #[test]
    fn float_specials_include_both_nan_and_both_zeros() {
        let specials = float_specials_f32();
        assert!(specials.iter().any(|f| f.is_nan()));
        assert!(specials.iter().any(|f| *f == 0.0 && f.is_sign_positive()));
        assert!(specials.iter().any(|f| *f == 0.0 && f.is_sign_negative()));
    }

    #[test]
    fn ramp_bounded_never_leaves_the_declared_domain() {
        let r = ramp_bounded(1000, 255);
        assert!(r.iter().all(|&v| (0..=255).contains(&v)));
    }
}
