//! Autocorrelation and the Levinson-Durbin recursion.

use crate::MAX_ORDER;

/// `out[lag] = sum_{i=0}^{n-1-lag} samples[i] * samples[i+lag]` for `lag in
/// 0..out.len()`.
///
/// `out.len() - 1` is the maximum lag computed, which the caller picks to
/// be the largest LPC order it intends to try (`out.len() == order + 1`).
/// Processes `min(out.len(), MAX_ORDER + 1)` lags; a longer `out` is left
/// zero-filled past that point, since no analysis this crate performs uses
/// an order past [`MAX_ORDER`].
pub fn autocorrelate(samples: &[f64], out: &mut [f64]) {
    let max_lag = out.len().min(MAX_ORDER + 1);
    for (lag, slot) in out.iter_mut().take(max_lag).enumerate() {
        let mut sum = 0.0;
        for (a, b) in samples.iter().zip(samples.iter().skip(lag)) {
            sum += a * b;
        }
        *slot = sum;
    }
}

/// The result of running Levinson-Durbin up to some maximum order: the
/// predictor coefficients and residual energy *at every intermediate
/// order*, which is what an encoder needs to pick the cheapest order
/// without re-running the recursion once per candidate.
#[derive(Clone, Copy, Debug)]
pub struct LevinsonDurbin {
    /// `coeffs[o - 1][0..o]` is the order-`o` predictor, for `o` in
    /// `1..=order_computed`. `coeffs[o - 1][j]` multiplies the sample `j +
    /// 1` steps before the one being predicted, matching FLAC's own
    /// coefficient ordering (RFC 9639 §9.2.6: "the first predictor
    /// coefficient ... multiplied with the sample directly before").
    coeffs: [[f64; MAX_ORDER]; MAX_ORDER],
    /// `reflection[o - 1]` is the order-`o` reflection coefficient (the
    /// `k` computed at that step of the recursion), useful on its own for
    /// stability analysis (`|k| < 1` at every order iff the filter is
    /// stable).
    reflection: [f64; MAX_ORDER],
    /// `error[o - 1]` is the order-`o` predictor's residual energy;
    /// `error[o]` is non-increasing in `o` by construction.
    error: [f64; MAX_ORDER],
    /// The order-0 "predict nothing" baseline energy, `autoc[0]`.
    zero_order_error: f64,
    order_computed: usize,
}

impl LevinsonDurbin {
    /// Coefficients of the order-`order` predictor, or the empty slice if
    /// `order` is `0` or exceeds [`Self::order_computed`].
    #[must_use]
    pub fn coefficients(&self, order: usize) -> &[f64] {
        if order == 0 || order > self.order_computed {
            return &[];
        }
        self.coeffs
            .get(order - 1)
            .map_or(&[] as &[f64], |row| row.get(..order).unwrap_or(&[]))
    }

    /// Residual energy of the order-`order` predictor. `order == 0` is the
    /// signal's own energy (`autoc[0]`, the "predict nothing" baseline).
    #[must_use]
    pub fn error(&self, order: usize) -> f64 {
        if order == 0 {
            return self.zero_order_error;
        }
        // Past `order_computed`, `self.error`'s backing array is still
        // zero-initialised, which would otherwise read as "perfect
        // prediction" — the most misleading possible wrong answer for a
        // caller comparing errors across orders to pick the cheapest one.
        // Infinity sorts after every real error instead.
        if order > self.order_computed {
            return f64::INFINITY;
        }
        self.error.get(order - 1).copied().unwrap_or(f64::INFINITY)
    }

    /// The reflection coefficient introduced at order `order` (`1..=`
    /// [`Self::order_computed`]). `|k| < 1` at every order is necessary and
    /// sufficient for the all-pole filter built from these coefficients to
    /// be stable.
    #[must_use]
    pub fn reflection(&self, order: usize) -> f64 {
        if order == 0 || order > self.order_computed {
            return 0.0;
        }
        self.reflection.get(order - 1).copied().unwrap_or(0.0)
    }

    /// The highest order actually computed. Less than the `max_order`
    /// requested exactly when the recursion hit a numerically singular
    /// step (`error` reached zero — the signal is already perfectly
    /// predicted by a lower order, most commonly for `autoc[0] == 0`,
    /// silence).
    #[must_use]
    pub fn order_computed(&self) -> usize {
        self.order_computed
    }
}

/// Runs Levinson-Durbin from an autocorrelation sequence, computing every
/// intermediate order from 1 up to `max_order` (clamped to
/// `min(MAX_ORDER, autoc.len() - 1)`).
///
/// `autoc[0]` is the signal energy; `autoc[0] == 0.0` (silence, or a
/// zero-length window) returns [`LevinsonDurbin::order_computed`] `== 0`
/// immediately rather than dividing by zero, since a predictor for silence
/// predicts nothing.
#[must_use]
pub fn levinson_durbin(autoc: &[f64], max_order: usize) -> LevinsonDurbin {
    let max_order = max_order.min(MAX_ORDER).min(autoc.len().saturating_sub(1));
    let mut result = LevinsonDurbin {
        coeffs: [[0.0; MAX_ORDER]; MAX_ORDER],
        reflection: [0.0; MAX_ORDER],
        error: [0.0; MAX_ORDER],
        order_computed: 0,
        zero_order_error: autoc.first().copied().unwrap_or(0.0),
    };

    let mut error = result.zero_order_error;
    if error <= 0.0 {
        return result;
    }

    // `a` holds the current-order coefficients, `a[0..order]`, in the same
    // "most recent sample first" layout the public API exposes.
    let mut a = [0.0f64; MAX_ORDER];

    for order in 1..=max_order {
        // acc = autoc[order] - sum_{j=0}^{order-2} a[j] * autoc[order-1-j]
        let mut acc = autoc.get(order).copied().unwrap_or(0.0);
        for j in 0..order.saturating_sub(1) {
            let aj = a.get(j).copied().unwrap_or(0.0);
            let r = autoc.get(order - 1 - j).copied().unwrap_or(0.0);
            acc -= aj * r;
        }
        let k = acc / error;

        // Step up: new_a[order-1] = k; new_a[j] = a[j] - k * a[order-2-j].
        let mut new_a = a;
        if let Some(slot) = new_a.get_mut(order - 1) {
            *slot = k;
        }
        for j in 0..order.saturating_sub(1) {
            let aj = a.get(j).copied().unwrap_or(0.0);
            let mirror = a.get(order - 2 - j).copied().unwrap_or(0.0);
            if let Some(slot) = new_a.get_mut(j) {
                *slot = aj - k * mirror;
            }
        }
        a = new_a;
        error *= 1.0 - k * k;

        if let Some(row) = result.coeffs.get_mut(order - 1) {
            *row = a;
        }
        if let Some(slot) = result.reflection.get_mut(order - 1) {
            *slot = k;
        }
        if let Some(slot) = result.error.get_mut(order - 1) {
            *slot = error.max(0.0);
        }
        result.order_computed = order;

        // A numerically singular step (perfect prediction, or a
        // pathological autocorrelation) makes every further order's `k`
        // a division by ~zero; stop rather than propagate NaN/Inf.
        if error <= 0.0 {
            break;
        }
    }

    result
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test assertions comparing exact hand-derived values; an out-of-range index is itself a test failure"
)]
mod tests {
    use super::*;

    #[test]
    fn autocorrelate_matches_hand_computed_lags() {
        let samples = [1.0, 2.0, 3.0, 4.0];
        let mut out = [0.0; 3];
        autocorrelate(&samples, &mut out);
        // r0 = 1+4+9+16=30; r1 = 1*2+2*3+3*4=20; r2 = 1*3+2*4=11.
        assert_eq!(out, [30.0, 20.0, 11.0]);
    }

    #[test]
    fn autocorrelate_of_empty_input_is_zero() {
        let mut out = [1.0, 1.0];
        autocorrelate(&[], &mut out);
        assert_eq!(out, [0.0, 0.0]);
    }

    /// Hand-derivation, `autoc = [r0, r1, r2] = [4, 2, 1]`:
    ///
    /// order 1: `k1 = r1/r0 = 0.5`; `a1 = [0.5]`; `E1 = r0*(1-k1^2) = 3`.
    /// order 2: `acc = r2 - a1[0]*r1 = 1 - 0.5*2 = 0`; `k2 = acc/E1 = 0`;
    /// `a2 = [a1[0] - k2*a1[0], k2] = [0.5, 0.0]`; `E2 = E1*(1-k2^2) = 3`.
    #[test]
    fn levinson_durbin_matches_hand_derivation() {
        let autoc = [4.0, 2.0, 1.0];
        let ld = levinson_durbin(&autoc, 2);
        assert_eq!(ld.order_computed(), 2);

        let o1 = ld.coefficients(1);
        assert_eq!(o1.len(), 1);
        assert!((o1[0] - 0.5).abs() < 1e-12);
        assert!((ld.error(1) - 3.0).abs() < 1e-12);
        assert!((ld.reflection(1) - 0.5).abs() < 1e-12);

        let o2 = ld.coefficients(2);
        assert_eq!(o2.len(), 2);
        assert!((o2[0] - 0.5).abs() < 1e-12);
        assert!(o2[1].abs() < 1e-12);
        assert!((ld.error(2) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn levinson_durbin_of_silence_computes_nothing() {
        let ld = levinson_durbin(&[0.0, 0.0, 0.0], 2);
        assert_eq!(ld.order_computed(), 0);
        assert!(ld.coefficients(1).is_empty());
    }

    #[test]
    fn levinson_durbin_recovers_an_exact_ar1_process() {
        // x[n] = 0.75 * x[n-1], generated exactly (no noise) from x[0]=1,
        // over a window long enough that edge truncation is negligible.
        // The order-1 predictor recovered from its autocorrelation should
        // land very close to 0.75.
        let mut samples = [0.0f64; 64];
        let mut x = 1.0f64;
        for s in &mut samples {
            *s = x;
            x *= 0.75;
        }
        let mut autoc = [0.0; 3];
        autocorrelate(&samples, &mut autoc);
        let ld = levinson_durbin(&autoc, 1);
        let c = ld.coefficients(1);
        assert_eq!(c.len(), 1);
        assert!((c[0] - 0.75).abs() < 1e-3, "got {}", c[0]);
    }

    #[test]
    fn error_is_non_increasing_in_order() {
        let mut samples = [0.0f64; 32];
        for (i, s) in samples.iter_mut().enumerate() {
            // A signal with real structure at several orders, not a pure
            // sinusoid, so higher orders genuinely keep helping.
            let t = i as f64;
            *s = (t * 0.3).sin() + 0.5 * (t * 0.9).cos();
        }
        let mut autoc = [0.0; 9];
        autocorrelate(&samples, &mut autoc);
        let ld = levinson_durbin(&autoc, 8);
        let mut prev = ld.error(0);
        for order in 1..=ld.order_computed() {
            let e = ld.error(order);
            assert!(
                e <= prev + 1e-9,
                "error increased at order {order}: {prev} -> {e}"
            );
            prev = e;
        }
    }

    #[test]
    fn order_beyond_computed_returns_empty_and_infinite_error() {
        let ld = levinson_durbin(&[1.0, 0.5], 1);
        assert!(ld.coefficients(5).is_empty());
        assert!(ld.error(5).is_infinite());
    }

    proptest::proptest! {
        #[test]
        fn levinson_durbin_never_panics(
            autoc in proptest::collection::vec(-1e6f64..1e6, 0..40),
            max_order in 0usize..40,
        ) {
            let _ = levinson_durbin(&autoc, max_order);
        }

        #[test]
        fn autocorrelate_never_panics(
            samples in proptest::collection::vec(proptest::num::f64::ANY, 0..64),
            lags in 0usize..40,
        ) {
            let mut out = vec![0.0; lags];
            autocorrelate(&samples, &mut out);
        }
    }
}
