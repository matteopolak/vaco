//! Per-sample widening, narrowing and fused-scale conversions.

/// Round-half-away-from-zero, saturating to `i16`. The rounding rule is a
/// design choice (see module docs), not a measured contract.
#[must_use]
pub fn clip_i16(x: f32) -> i16 {
    clip_i32_from_f32(x, f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

/// Round-half-away-from-zero, saturating to the full `i32` range representable
/// exactly in `f32` (`i32::MIN..=i32::MAX`, via `f32`'s own saturating cast,
/// which already clamps `NaN` to `0` and out-of-range values to the nearest
/// finite bound).
#[must_use]
pub fn clip_i32(x: f32) -> i32 {
    round_half_away_from_zero(x) as i32
}

/// Round-half-away-from-zero, saturating to `u8`.
#[must_use]
pub fn clip_u8(x: f32) -> u8 {
    clip_i32_from_f32(x, 0.0, f32::from(u8::MAX)) as u8
}

fn round_half_away_from_zero(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.round()
    }
}

fn clip_i32_from_f32(x: f32, lo: f32, hi: f32) -> i32 {
    round_half_away_from_zero(x).clamp(lo, hi) as i32
}

/// `dst[i] = src[i] as f32` for `i in 0..min(dst.len(), src.len())`.
///
/// Exact: every `i16` value is exactly representable in `f32`.
pub fn int16_to_float(dst: &mut [f32], src: &[i16]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = f32::from(*s);
    }
}

/// `dst[i] = src[i] as f32 / 2^31`, the standard "full-scale integer to
/// `[-1, 1)`" normalisation used when a decoder's internal accumulator is a
/// 32-bit fixed-point value representing a signal in that range.
///
/// Not exact past 24 significant bits (`f32`'s mantissa), which is the same
/// precision loss every float-based audio decoder already accepts for its
/// working format.
pub fn int32_to_float(dst: &mut [f32], src: &[i32]) {
    const SCALE: f32 = 1.0 / 2_147_483_648.0; // 1 / 2^31
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (*s as f32) * SCALE;
    }
}

/// `dst[i] = src[i] as f32 * mul`, fused so the caller never materialises an
/// unscaled intermediate array. This is the shape a transform's own output
/// normalisation constant takes: the transform produces integers on some
/// internal scale, and `mul` folds both the `1/2^31`-style normalisation and
/// the transform's own gain into one multiply.
pub fn int32_to_float_fmul_scalar(dst: &mut [f32], src: &[i32], mul: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = (*s as f32) * mul;
    }
}

/// `dst[i] *= mul`, in place over `dst` truncated to `src`'s length — used
/// where a caller already has floats in `dst` and only needs the scale
/// applied (window normalisation, overlap-add gain).
///
/// Takes `src` only to decide how many elements to touch, matching every
/// other function's "operate on the shorter of the two lengths" contract;
/// pass `dst` itself as `src` for the common case of scaling the whole
/// buffer.
pub fn scale_float(dst: &mut [f32], src_len_from: &[f32], mul: f32) {
    let n = dst.len().min(src_len_from.len());
    for d in dst.iter_mut().take(n) {
        *d *= mul;
    }
}

/// `dst[i] = clip_i16(src[i])`.
pub fn float_to_int16(dst: &mut [i16], src: &[f32]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = clip_i16(*s);
    }
}

/// `dst[i] = clip_i32(src[i] * 2^31)` — the inverse of [`int32_to_float`],
/// used when a decoder needs to hand its float working buffer to a muxer or
/// filter expecting 32-bit PCM.
pub fn float_to_int32(dst: &mut [i32], src: &[f32]) {
    const SCALE: f64 = 2_147_483_648.0; // 2^31
    for (d, s) in dst.iter_mut().zip(src) {
        // f64 for the multiply: f32 * 2^31 loses precision exactly where it
        // matters (the low bits of a value near full scale), and the
        // intermediate easily exceeds i32::MAX before clamping.
        let scaled = f64::from(*s) * SCALE;
        *d = if scaled.is_nan() {
            0
        } else {
            scaled.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "these conversions are defined to be bit-exact at the tested values"
)]
mod tests {
    use super::*;

    #[test]
    fn clip_i16_saturates_both_directions() {
        assert_eq!(clip_i16(0.0), 0);
        assert_eq!(clip_i16(32767.4), 32767);
        assert_eq!(clip_i16(32768.0), i16::MAX);
        assert_eq!(clip_i16(1_000_000.0), i16::MAX);
        assert_eq!(clip_i16(-32768.6), i16::MIN);
        assert_eq!(clip_i16(-1_000_000.0), i16::MIN);
        assert_eq!(clip_i16(f32::NAN), 0);
    }

    #[test]
    fn clip_i16_rounds_half_away_from_zero() {
        assert_eq!(clip_i16(0.5), 1);
        assert_eq!(clip_i16(-0.5), -1);
        assert_eq!(clip_i16(1.5), 2);
        assert_eq!(clip_i16(-1.5), -2);
    }

    #[test]
    fn int16_to_float_is_exact_at_extremes() {
        let src = [i16::MIN, -1, 0, 1, i16::MAX];
        let mut dst = [0.0f32; 5];
        int16_to_float(&mut dst, &src);
        assert_eq!(dst, [-32768.0, -1.0, 0.0, 1.0, 32767.0]);
    }

    #[test]
    fn int16_float_roundtrip_is_exact_for_every_i16() {
        // Exhaustive: 65536 values, cheap, and this is exactly the property
        // that matters — every i16 must survive int16_to_float then
        // float_to_int16 unchanged.
        for x in i16::MIN..=i16::MAX {
            let mut f = [0.0f32];
            int16_to_float(&mut f, &[x]);
            let mut back = [0i16];
            float_to_int16(&mut back, &f);
            assert_eq!(back[0], x, "roundtrip failed for {x}");
        }
    }

    #[test]
    fn int32_to_float_matches_hand_computed_values() {
        let src = [0i32, 1 << 30, -(1 << 30), i32::MAX, i32::MIN];
        let mut dst = [0.0f32; 5];
        int32_to_float(&mut dst, &src);
        assert_eq!(dst[0], 0.0);
        assert!((dst[1] - 0.5).abs() < 1e-6);
        assert!((dst[2] - (-0.5)).abs() < 1e-6);
        assert!((dst[3] - 1.0).abs() < 1e-6);
        assert_eq!(dst[4], -1.0);
    }

    #[test]
    fn fmul_scalar_matches_plain_multiply() {
        let src = [10i32, -10, 0];
        let mut dst = [0.0f32; 3];
        int32_to_float_fmul_scalar(&mut dst, &src, 2.5);
        assert_eq!(dst, [25.0, -25.0, 0.0]);
    }

    #[test]
    fn shorter_dst_truncates_rather_than_panics() {
        let src = [1i16, 2, 3, 4];
        let mut dst = [0.0f32; 2];
        int16_to_float(&mut dst, &src);
        assert_eq!(dst, [1.0, 2.0]);
    }

    #[test]
    fn float_to_int32_roundtrips_representable_values() {
        let src = [0.0f32, 0.5, -0.5, 0.999_999_9, -1.0];
        let mut dst = [0i32; 5];
        float_to_int32(&mut dst, &src);
        let mut back = [0.0f32; 5];
        int32_to_float(&mut back, &dst);
        for (a, b) in src.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    proptest::proptest! {
        #[test]
        fn clip_i16_never_panics(x in proptest::num::f32::ANY) {
            let _ = clip_i16(x);
        }

        #[test]
        fn float_to_int32_never_panics(x in proptest::num::f32::ANY) {
            let mut dst = [0i32];
            float_to_int32(&mut dst, &[x]);
        }
    }
}
