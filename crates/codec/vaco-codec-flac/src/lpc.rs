//! Native LPC subframe encoding, built on `vaco-codec-dsp-lpc`'s shared
//! autocorrelation/Levinson-Durbin/quantisation primitives (D-07) rather
//! than a from-scratch fourth implementation of the same analysis math —
//! this crate is exactly the "future format needs classic LPC" case that
//! crate's own doc names.
//!
//! Windowing (this crate applies none — a plain rectangular window) is
//! this encoder's own choice, not `vaco-codec-dsp-lpc`'s: the shared crate
//! takes already-windowed (or unwindowed) samples, by design, so a
//! different choice here never needs an upstream change. Skipping a
//! window costs compression, never correctness — the same trade this
//! crate's fixed-predictor-only encoder already made on purpose (see
//! `encoder.rs`'s module doc).
//!
//! Vaco-Spec-Ref: rfc-9639-flac Section 9.2.6, "Linear Predictor Subframe"

use vaco_codec_dsp_lpc::{MAX_ORDER, autocorrelate, levinson_durbin, predict, quantize};

/// Coefficient precision this encoder always requests. RFC 9639 §9.2.6
/// stores `precision - 1` in a 4-bit field (`0b1111` forbidden), so 15 is
/// that field's own maximum; a smaller value only trades compression for
/// smaller coefficients, which correctness does not need less of.
pub const PRECISION: u32 = 15;

/// Orders this encoder tries, alongside the fixed-predictor family
/// `encoder.rs::choose_subframe` already tries. Every candidate — fixed or
/// LPC — is measured by its own actual encoded bit cost, so trying more
/// orders can only ever help, never hurt correctness; it costs encode
/// time, which is why this list is short rather than 1..=32.
pub const ORDERS: [usize; 4] = [2, 4, 8, 12];

/// One quantised LPC candidate for `samples`, plus the residual it
/// produces.
#[derive(Debug)]
pub struct Candidate {
    pub order: usize,
    pub precision: u32,
    pub shift: u32,
    pub qcoeffs: Vec<i32>,
    pub residual: Vec<i32>,
}

/// Analyse `samples` at `order`, returning `None` when that order is not
/// usable for this block: too few samples, a degenerate (silent, giving
/// `autoc[0] == 0`) window, the recursion terminating early, or a residual
/// that would need RFC 9639 §9.2.7.3's one forbidden value (`i32::MIN`) —
/// in every case the caller has other candidates (the fixed family, at
/// minimum VERBATIM) to fall back to, so this never needs to force a bad
/// choice through.
#[must_use]
pub fn candidate(samples: &[i32], order: usize) -> Option<Candidate> {
    if order == 0 || order > MAX_ORDER || samples.len() <= order {
        return None;
    }
    let samples_f64: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
    let mut autoc = vec![0.0; order + 1];
    autocorrelate(&samples_f64, &mut autoc);
    if autoc.first().copied().unwrap_or(0.0) <= 0.0 {
        return None;
    }

    let ld = levinson_durbin(&autoc, order);
    if ld.order_computed() < order {
        return None;
    }

    let q = quantize(ld.coefficients(order), PRECISION);
    if q.order != order {
        return None;
    }
    let qcoeffs = q.coefficients().to_vec();

    let residual = lpc_residual(samples, &qcoeffs, q.shift, order);
    if residual.contains(&i32::MIN) {
        return None;
    }

    Some(Candidate {
        order,
        precision: PRECISION,
        shift: q.shift,
        qcoeffs,
        residual,
    })
}

/// `residual[i] = samples[order + i].wrapping_sub(predict(history))`,
/// history built from the *original* samples (not a reconstruction) —
/// correct because RFC 9639's arithmetic is exact integer arithmetic: with
/// the same coefficients and shift, a decoder's reconstruction from this
/// residual reproduces `samples` exactly, so "original" and "reconstructed"
/// history are the same sequence by construction. Wrapping, not checked,
/// matching `fixed::residual`'s own documented "arithmetic wraps" contract
/// for this crate's predictors.
fn lpc_residual(samples: &[i32], qcoeffs: &[i32], shift: u32, order: usize) -> Vec<i32> {
    let mut out = Vec::new();
    if order == 0 || order > MAX_ORDER {
        return out;
    }
    for i in order..samples.len() {
        let mut history = [0i32; MAX_ORDER];
        for (k, slot) in history.iter_mut().take(order).enumerate() {
            *slot = i
                .checked_sub(1 + k)
                .and_then(|idx| samples.get(idx))
                .copied()
                .unwrap_or(0);
        }
        let Some(history) = history.get(..order) else {
            continue;
        };
        let pred = predict(history, qcoeffs, shift);
        let actual = i64::from(samples.get(i).copied().unwrap_or(0));
        // Truncating, deliberately: see this function's own doc.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "wrapping residual arithmetic, matching fixed::residual's own convention"
        )]
        let wrapped = actual.wrapping_sub(pred) as i32;
        out.push(wrapped);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// A signal with real order-2 structure (not noise, not a straight
    /// line): the LPC candidate must exist and its residual must be
    /// smaller in magnitude, on average, than the raw signal -- otherwise
    /// prediction bought nothing and something is wrong.
    #[test]
    fn candidate_reduces_residual_magnitude_on_structured_signal() {
        let samples: Vec<i32> = (0..256)
            .map(|i| {
                let t = f64::from(i);
                ((t * 0.1).sin() * 5000.0 + (t * 0.03).cos() * 2000.0) as i32
            })
            .collect();
        let c = candidate(&samples, 4).expect("order-4 candidate should exist for this signal");
        assert_eq!(c.residual.len(), samples.len() - 4);

        let signal_energy: i64 = samples.iter().map(|&s| i64::from(s).abs()).sum();
        let residual_energy: i64 = c.residual.iter().map(|&r| i64::from(r).abs()).sum();
        assert!(
            residual_energy < signal_energy,
            "LPC residual ({residual_energy}) was not smaller than the raw signal ({signal_energy})"
        );
    }

    /// The residual/coefficient pair must let a decoder reconstruct the
    /// exact original samples -- the one property that actually matters
    /// for a lossless format. Reimplements the reconstruction by hand
    /// (RFC 9639 §9.2.6's own arithmetic) rather than calling
    /// `vaco_codec_dsp_lpc::synthesize`, so this is a real round-trip
    /// check and not a tautology against the function under test.
    #[test]
    fn residual_plus_warmup_reconstructs_the_original_exactly() {
        let samples: Vec<i32> = (0..64)
            .map(|i| ((f64::from(i) * 0.2).sin() * 10000.0) as i32)
            .collect();
        let order = 4;
        let c = candidate(&samples, order).expect("order-4 candidate should exist");

        let mut recon: Vec<i64> = samples.iter().take(order).map(|&s| i64::from(s)).collect();
        for &r in &c.residual {
            let n = recon.len();
            let mut sum: i64 = 0;
            for (j, &coeff) in c.qcoeffs.iter().enumerate() {
                let Some(idx) = n.checked_sub(1 + j) else {
                    continue;
                };
                let hist = recon.get(idx).copied().unwrap_or(0);
                sum += i64::from(coeff) * hist;
            }
            let pred = sum >> c.shift;
            recon.push(pred + i64::from(r));
        }
        let recon_i32: Vec<i32> = recon.iter().map(|&v| v as i32).collect();
        assert_eq!(recon_i32, samples);
    }

    #[test]
    fn silence_yields_no_candidate() {
        let samples = [0i32; 32];
        assert!(candidate(&samples, 4).is_none());
    }

    #[test]
    fn too_few_samples_yields_no_candidate() {
        let samples = [1i32, 2, 3];
        assert!(candidate(&samples, 8).is_none());
    }

    #[test]
    fn order_zero_or_above_max_yields_no_candidate() {
        let samples = [1i32; 64];
        assert!(candidate(&samples, 0).is_none());
        assert!(candidate(&samples, MAX_ORDER + 1).is_none());
    }
}
