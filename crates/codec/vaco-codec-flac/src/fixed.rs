//! The five fixed polynomial predictors (orders 0 through 4).
//!
//! LPC is not implemented — see the crate-level doc comment — so the fixed
//! family is the only prediction this encoder does. All arithmetic wraps
//! (`wrapping_*`) rather than panicking on overflow: the decode side
//! (Claxon) reconstructs with the identical wrapping rule, so a value that
//! "overflows" mid-computation is still recovered bit-for-bit, matching how
//! every real FLAC implementation treats this arithmetic as intentionally
//! modular rather than checked.
//!
//! Vaco-Spec-Ref: rfc-9639-flac Section 9.2.5, "Fixed Predictor Subframe"

/// The highest fixed predictor order this crate will try.
pub const MAX_ORDER: usize = 4;

/// Residual for fixed predictor `order` (0..=4), for every sample from
/// `order` onward. `samples[..order]` are the warm-up samples and are not
/// part of the residual at all.
///
/// # Panics
///
/// Never: `order` above 4 or at least as large as `samples.len()` just
/// yields an empty residual.
#[must_use]
pub fn residual(samples: &[i32], order: usize) -> Vec<i32> {
    if order > MAX_ORDER || samples.len() <= order {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in order..samples.len() {
        let predicted = predict(samples, i, order);
        let Some(&actual) = samples.get(i) else {
            continue;
        };
        out.push(actual.wrapping_sub(predicted));
    }
    out
}

/// The order-`order` prediction for `samples[i]`, from the `order` samples
/// immediately before it.
fn predict(samples: &[i32], i: usize, order: usize) -> i32 {
    // Coefficients for orders 1..=4, most-recent sample first (index 0 of
    // each row multiplies `samples[i - 1]`). Order 0 always predicts 0 and
    // is not represented here.
    const COEFFS: [&[i32]; 5] = [&[], &[1], &[2, -1], &[3, -3, 1], &[4, -6, 4, -1]];
    let Some(coeffs) = COEFFS.get(order) else {
        return 0;
    };
    let mut acc: i32 = 0;
    for (j, &c) in coeffs.iter().enumerate() {
        let Some(idx) = i.checked_sub(j + 1) else {
            return 0;
        };
        let Some(&s) = samples.get(idx) else {
            return 0;
        };
        acc = acc.wrapping_add(c.wrapping_mul(s));
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::residual;

    #[test]
    fn order_zero_residual_is_the_signal_itself() {
        let samples = [3, -5, 7, 0];
        assert_eq!(residual(&samples, 0), vec![3, -5, 7, 0]);
    }

    #[test]
    fn order_one_is_first_difference() {
        let samples = [10, 12, 9, 9];
        assert_eq!(residual(&samples, 1), vec![2, -3, 0]);
    }

    #[test]
    fn matches_claxons_reference_vector() {
        // The forward transform of Claxon's own `verify_predict_fixed` data:
        // reconstructing these residuals against those samples must recover
        // the original signal exactly, which is the property that actually
        // matters (this crate never reads Claxon's decode-side code, only
        // its published, versioned test vector as an independent check).
        let samples = [
            -729, -722, -667, -583, -486, -359, -225, -91, 59, 209, 354, 497, 630, 740, 812, 845,
        ];
        let r = residual(&samples, 3);
        // Re-apply the order-3 fixed predictor by hand and confirm it
        // reproduces `samples[3..]` from the warm-up plus these residuals.
        let mut recon: Vec<i32> = samples.get(..3).map(<[i32]>::to_vec).unwrap_or_default();
        for &e in &r {
            let n = recon.len();
            let a1 = recon.get(n - 1).copied().unwrap_or(0);
            let a2 = recon.get(n - 2).copied().unwrap_or(0);
            let a3 = recon.get(n - 3).copied().unwrap_or(0);
            let pred = 3i32
                .wrapping_mul(a1)
                .wrapping_sub(3i32.wrapping_mul(a2))
                .wrapping_add(a3);
            recon.push(pred.wrapping_add(e));
        }
        assert_eq!(recon, samples);
    }

    #[test]
    fn short_input_yields_empty_residual() {
        assert_eq!(residual(&[1, 2], 4), Vec::<i32>::new());
    }
}
