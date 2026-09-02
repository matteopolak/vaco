//! SILK's normalized-LSF (NLSF) decode: two-stage vector quantization,
//! stabilization, and conversion to LPC coefficients. RFC 6716 §4.2.7.5,
//! from `silk/NLSF_decode.c`, `NLSF_unpack.c`, `NLSF_VQ_weights_laroia.c`,
//! `NLSF_stabilize.c` and `NLSF2A.c`.
//!
//! NLSF values are carried as Q15 integers (`0..=32767`, real value
//! `nlsf/32768`) through stage-1/2 decode and stabilization, exactly as the
//! reference does — that recurrence's rounding and the stabilizer's
//! ordering decisions are integer-exact by definition, not an approximation
//! of a real quantity. [`nlsf_to_lpc`] switches to `f32` because it is
//! genuinely computing a continuous trig transform; see its own doc for why
//! it does not need the reference's piecewise-linear cosine table to be a
//! faithful decoder.

use crate::range::RangeDecoder;
use crate::silk::tables::{NLSF_EXT_ICDF, NlsfCodebook};

const NLSF_QUANT_MAX_AMPLITUDE: i32 = 4;
const NLSF_W_Q: i32 = 2;

/// `NLSF_unpack.c`'s `silk_NLSF_unpack`: for a first-stage codebook index,
/// the per-coefficient entropy-table selector and backward-predictor
/// coefficient.
fn nlsf_unpack(cb: &NlsfCodebook, cb1_index: usize) -> (Vec<usize>, Vec<i32>) {
    let mut ec_ix = vec![0usize; cb.order];
    let mut pred = vec![0i32; cb.order];
    let base = cb1_index * cb.order / 2;
    for i in (0..cb.order).step_by(2) {
        let Some(&entry) = cb.ec_sel.get(base + i / 2) else {
            break;
        };
        let entry = i32::from(entry);
        ec_ix[i] = ((entry >> 1) & 7) as usize * (2 * NLSF_QUANT_MAX_AMPLITUDE as usize + 1);
        pred[i] = i32::from(
            cb.pred
                .get(i + ((entry & 1) as usize) * (cb.order - 1))
                .copied()
                .unwrap_or(0),
        );
        if let (Some(ix1), Some(p1)) = (ec_ix.get_mut(i + 1), pred.get_mut(i + 1)) {
            *ix1 = ((entry >> 5) & 7) as usize * (2 * NLSF_QUANT_MAX_AMPLITUDE as usize + 1);
            *p1 = i32::from(
                cb.pred
                    .get(i + 1 + (((entry >> 4) & 1) as usize) * (cb.order - 1))
                    .copied()
                    .unwrap_or(0),
            );
        }
    }
    (ec_ix, pred)
}

/// Decode the first-stage vector index and every residual index.
/// `celt/decode_indices.c`'s NLSF section, `psDec->psNLSF_CB` half.
pub fn decode_nlsf_indices(
    dec: &mut RangeDecoder<'_>,
    cb: &NlsfCodebook,
    signal_type_voiced_half: bool,
) -> (usize, Vec<i32>) {
    let row = usize::from(signal_type_voiced_half) * cb.n_vectors;
    let row_icdf = cb
        .cb1_icdf
        .get(row..row + cb.n_vectors)
        .unwrap_or(cb.cb1_icdf);
    let cb1_index = dec.icdf(row_icdf, 8).unwrap_or(0).max(0) as usize;
    let (ec_ix, _pred) = nlsf_unpack(cb, cb1_index.min(cb.n_vectors.saturating_sub(1)));
    let mut indices = vec![0i32; cb.order];
    for i in 0..cb.order {
        let table = cb
            .ec_icdf
            .get(*ec_ix.get(i).unwrap_or(&0)..)
            .unwrap_or(cb.ec_icdf);
        let mut ix = dec.icdf(table, 8).unwrap_or(0);
        if ix == 0 {
            ix -= dec.icdf(&NLSF_EXT_ICDF, 8).unwrap_or(0);
        } else if ix == 2 * NLSF_QUANT_MAX_AMPLITUDE {
            ix += dec.icdf(&NLSF_EXT_ICDF, 8).unwrap_or(0);
        }
        if let Some(slot) = indices.get_mut(i) {
            *slot = ix - NLSF_QUANT_MAX_AMPLITUDE;
        }
    }
    (cb1_index.min(cb.n_vectors.saturating_sub(1)), indices)
}

/// `NLSF_decode.c`'s `silk_NLSF_decode`: turn a first-stage index and its
/// residual indices into a stabilized Q15 NLSF vector.
#[must_use]
pub fn nlsf_decode(cb: &NlsfCodebook, cb1_index: usize, residual_indices: &[i32]) -> Vec<i32> {
    let order = cb.order;
    let cb1_row = cb
        .cb1
        .get(cb1_index * order..cb1_index * order + order)
        .unwrap_or(&[]);
    let mut nlsf: Vec<i32> = cb1_row.iter().map(|&v| i32::from(v) << 7).collect();
    nlsf.resize(order, 0);

    let (_ec_ix, pred) = nlsf_unpack(cb, cb1_index);

    // Predictive residual dequantizer (`silk_NLSF_residual_dequant`), run
    // from the last coefficient to the first.
    let quant_step_q16 = (cb.quant_step_size * 65536.0) as i32;
    let mut res = vec![0i32; order];
    let mut out_q10 = 0i32;
    for i in (0..order).rev() {
        let pred_q10 = (out_q10 * pred.get(i).copied().unwrap_or(0)) >> 8;
        let ix = residual_indices.get(i).copied().unwrap_or(0);
        let mut o = ix << 10;
        if o > 0 {
            o -= 102; // NLSF_QUANT_LEVEL_ADJ (0.1) in Q10, rounded.
        } else if o < 0 {
            o += 102;
        }
        out_q10 = pred_q10 + ((i64::from(o) * i64::from(quant_step_q16)) >> 16) as i32;
        if let Some(slot) = res.get_mut(i) {
            *slot = out_q10;
        }
    }

    // Laroia weights from the first-stage codebook vector, then apply the
    // inverse-square-root-weighted residual.
    let weights = laroia_weights(&nlsf, order);
    for i in 0..order {
        let w = weights.get(i).copied().unwrap_or(1.0).max(1e-6);
        // `sqrt(W_tmp_QW << (18 - NLSF_W_Q))` == `sqrt(W_tmp_QW) * 256` for
        // `NLSF_W_Q == 2`.
        let w_q9 = (f64::from(w).sqrt() * 256.0).max(1.0);
        let r = f64::from(res.get(i).copied().unwrap_or(0));
        let delta = (r * 16384.0) / w_q9;
        let v = f64::from(nlsf.get(i).copied().unwrap_or(0)) + delta;
        if let Some(slot) = nlsf.get_mut(i) {
            *slot = (v.round() as i32).clamp(0, 32767);
        }
    }

    stabilize(&mut nlsf, cb.delta_min, order);
    nlsf
}

/// `NLSF_VQ_weights_laroia.c`, evaluated directly on the (still-unscaled,
/// Q15-integer) first-stage vector. The formula only ever adds/divides
/// magnitudes, so doing it in `f64` rather than the reference's `Q(NLSF_W_Q)`
/// fixed point changes nothing but rounding in the fifth decimal place.
fn laroia_weights(nlsf: &[i32], d: usize) -> Vec<f32> {
    let g = |i: usize| f64::from(nlsf.get(i).copied().unwrap_or(0));
    let mut w = vec![1.0f32; d];
    if d < 2 {
        return w;
    }
    let scale = f64::from(1i32 << (15 + NLSF_W_Q));
    let inv = |gap: f64| scale / gap.max(1.0);

    let mut tmp1 = inv(g(0));
    let mut tmp2 = inv(g(1) - g(0));
    if let Some(slot) = w.first_mut() {
        *slot = (tmp1 + tmp2) as f32;
    }
    let mut k = 1usize;
    while k < d - 1 {
        tmp1 = inv(g(k + 1) - g(k));
        if let Some(slot) = w.get_mut(k) {
            *slot = (tmp1 + tmp2) as f32;
        }
        tmp2 = inv(g(k + 2) - g(k + 1));
        if let Some(slot) = w.get_mut(k + 1) {
            *slot = (tmp1 + tmp2) as f32;
        }
        k += 2;
    }
    if let Some(slot) = w.last_mut() {
        *slot = (inv(32768.0 - g(d - 1)) + tmp2) as f32;
    }
    w
}

/// `NLSF_stabilize.c`: enforce minimum spacing between consecutive NLSFs
/// (and from the band edges), exactly as specified — this is a discrete
/// ordering algorithm on Q15 integers, not something a float re-derivation
/// would improve on.
fn stabilize(nlsf: &mut [i32], delta_min: &[f32], l: usize) {
    let dmin: Vec<i32> = delta_min
        .iter()
        .map(|&v| (v * 32768.0).round() as i32)
        .collect();
    let d = |i: usize| dmin.get(i).copied().unwrap_or(1);

    for _ in 0..20 {
        let mut min_diff = nlsf.first().copied().unwrap_or(0) - d(0);
        let mut idx = 0i32;
        for i in 1..l {
            let diff =
                nlsf.get(i).copied().unwrap_or(0) - (nlsf.get(i - 1).copied().unwrap_or(0) + d(i));
            if diff < min_diff {
                min_diff = diff;
                idx = i as i32;
            }
        }
        let diff = 32768 - (nlsf.get(l.wrapping_sub(1)).copied().unwrap_or(0) + d(l));
        if diff < min_diff {
            min_diff = diff;
            idx = l as i32;
        }
        if min_diff >= 0 {
            return;
        }

        if idx == 0 {
            if let Some(slot) = nlsf.first_mut() {
                *slot = d(0);
            }
        } else if idx as usize == l {
            if let Some(slot) = nlsf.get_mut(l - 1) {
                *slot = 32768 - d(l);
            }
        } else {
            let i = idx as usize;
            let mut min_center = 0i32;
            for k in 0..i {
                min_center += d(k);
            }
            min_center += d(i) >> 1;
            let mut max_center = 32768i32;
            for k in (i + 1..=l).rev() {
                max_center -= d(k);
            }
            max_center -= d(i) >> 1;
            let mid = (i64::from(nlsf.get(i - 1).copied().unwrap_or(0))
                + i64::from(nlsf.get(i).copied().unwrap_or(0))
                + 1)
                >> 1;
            let center = (mid as i32).clamp(min_center, max_center);
            if let Some(slot) = nlsf.get_mut(i - 1) {
                *slot = center - (d(i) >> 1);
            }
            let prev = nlsf.get(i - 1).copied().unwrap_or(0);
            if let Some(slot) = nlsf.get_mut(i) {
                *slot = prev + d(i);
            }
        }
    }

    // Fallback: sort and force minimum spacing.
    nlsf.sort_unstable();
    if let Some(first) = nlsf.first_mut() {
        *first = (*first).max(d(0));
    }
    for i in 1..l {
        let prev = nlsf.get(i - 1).copied().unwrap_or(0);
        if let Some(slot) = nlsf.get_mut(i) {
            *slot = (*slot).max(prev + d(i));
        }
    }
    if let Some(last) = nlsf.get_mut(l - 1) {
        *last = (*last).min(32768 - d(l));
    }
    for i in (0..l.saturating_sub(1)).rev() {
        let next = nlsf.get(i + 1).copied().unwrap_or(32767);
        if let Some(slot) = nlsf.get_mut(i) {
            *slot = (*slot).min(next - d(i + 1));
        }
    }
}

/// `NLSF2A.c`'s `silk_NLSF2A`, using an exact `cos()` in place of the
/// reference's 128-entry piecewise-linear approximation.
///
/// The reference's own doc says the table "is not accurate LSFs, but the
/// two functions [NLSF2A/A2NLSF] are accurate inverses of each other" —
/// i.e. any consistent angle mapping works as long as the encoder and
/// decoder agree, which only matters for round-tripping the *same*
/// implementation. A decoder reconstructing an already-encoded NLSF has no
/// such constraint: it only needs *a* mapping from the transmitted Q15
/// value to a stable, correctly-ordered LPC filter, and `cos` gives exactly
/// that with less transcription risk than the lookup table. Root ordering
/// still matters and is preserved (even NLSF indices are is the `P` /
/// symmetric polynomial, odd indices the `Q` / antisymmetric one — see
/// `NLSF2A_find_poly`'s call sites in the reference for why alternating
/// parity, not the specific in-parity permutation, is what the algorithm
/// depends on).
#[must_use]
pub fn nlsf_to_lpc(nlsf_q15: &[i32], order: usize) -> Vec<f32> {
    let cos_vals: Vec<f32> = (0..order)
        .map(|k| {
            2.0 * (std::f32::consts::PI * f32::from(nlsf_q15.get(k).copied().unwrap_or(0) as i16)
                / 32768.0)
                .cos()
        })
        .collect();
    let dd = order / 2;
    let p_in: Vec<f32> = (0..dd)
        .map(|k| cos_vals.get(2 * k).copied().unwrap_or(0.0))
        .collect();
    let q_in: Vec<f32> = (0..dd)
        .map(|k| cos_vals.get(2 * k + 1).copied().unwrap_or(0.0))
        .collect();
    let p = find_poly(&p_in, dd);
    let q = find_poly(&q_in, dd);

    // `silk_NLSF2A`'s combination step produces `a32_QA1` one bit wider
    // (`QA+1`) than `P`/`Q`'s own `QA` scale -- the P(z) +/- Q(z) recombination
    // that recovers `A(z)` from its symmetric/antisymmetric factors doubles
    // the scale by construction, not a rounding artifact. Halving here keeps
    // `p`/`q`/`a` all in the same "real" convention this module uses
    // throughout, matching the reference's final `QA+1 -> Q12` shift.
    let mut a = vec![0.0f32; order];
    for k in 0..dd {
        let ptmp = p.get(k + 1).copied().unwrap_or(0.0) + p.get(k).copied().unwrap_or(0.0);
        let qtmp = q.get(k + 1).copied().unwrap_or(0.0) - q.get(k).copied().unwrap_or(0.0);
        if let Some(slot) = a.get_mut(k) {
            *slot = -(qtmp + ptmp) * 0.5;
        }
        if let Some(slot) = a.get_mut(order - k - 1) {
            *slot = (qtmp - ptmp) * 0.5;
        }
    }
    stabilize_lpc(&mut a);
    a
}

/// `NLSF2A_find_poly`.
fn find_poly(c_lsf: &[f32], dd: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dd + 1];
    if let Some(slot) = out.first_mut() {
        *slot = 1.0;
    }
    if dd == 0 {
        return out;
    }
    if let Some(slot) = out.get_mut(1) {
        *slot = -c_lsf.first().copied().unwrap_or(0.0);
    }
    for k in 1..dd {
        // `NLSF2A_find_poly` indexes its *unstrided* `cLSF` pointer (a view
        // into the interleaved cos-table starting at 0 or 1) as `cLSF[2*k]`
        // to pick out every other entry. `c_lsf` here is already the
        // pre-extracted (P- or Q-side) `dd`-length array the caller built by
        // doing that striding once, so the equivalent read is `c_lsf[k]` --
        // re-striding it with `2*k` walked off the end for `k >= dd/2` and
        // silently substituted zero, corrupting most of the polynomial.
        let ftmp = c_lsf.get(k).copied().unwrap_or(0.0);
        let prev = out.get(k - 1).copied().unwrap_or(0.0);
        let cur = out.get(k).copied().unwrap_or(0.0);
        let next = 2.0 * prev - ftmp * cur;
        if let Some(slot) = out.get_mut(k + 1) {
            *slot = next;
        }
        for n in (2..=k).rev() {
            let a = out.get(n - 2).copied().unwrap_or(0.0);
            let b = out.get(n - 1).copied().unwrap_or(0.0);
            if let Some(slot) = out.get_mut(n) {
                *slot += a - ftmp * b;
            }
        }
        if let Some(slot) = out.get_mut(1) {
            *slot -= ftmp;
        }
    }
    out
}

/// Not a spec-derived step: a stability safeguard beyond what `NLSF2A`
/// itself guarantees for a *correctly quantized* filter. Since this
/// decoder does not chase the reference's exact fixed-point saturation
/// behaviour (see the module doc), a synthesis filter driven by a
/// corrupted or edge-case bitstream could otherwise ring up without bound;
/// this applies mild bandwidth expansion until the direct-form
/// coefficients imply reflection coefficients of magnitude `< 1`, checked
/// via the standard step-down (Schur) recursion.
fn stabilize_lpc(a: &mut [f32]) {
    for attempt in 0..16 {
        if is_stable(a) {
            return;
        }
        let gamma = 1.0 - 0.02 * (attempt as f32 + 1.0);
        for (k, v) in a.iter_mut().enumerate() {
            *v *= gamma.powi(k as i32 + 1);
        }
    }
}

fn is_stable(a: &[f32]) -> bool {
    let n = a.len();
    let mut coeffs = a.to_vec();
    for m in (1..=n).rev() {
        let Some(&k) = coeffs.get(m - 1) else {
            return false;
        };
        if !(-0.9999..=0.9999).contains(&k) {
            return false;
        }
        if m == 1 {
            break;
        }
        let denom = 1.0 - k * k;
        if denom.abs() < 1e-6 {
            return false;
        }
        let mut next = vec![0.0f32; m - 1];
        for i in 0..m - 1 {
            let ai = coeffs.get(i).copied().unwrap_or(0.0);
            let am = coeffs.get(m - 2 - i).copied().unwrap_or(0.0);
            if let Some(slot) = next.get_mut(i) {
                *slot = (ai - k * am) / denom;
            }
        }
        coeffs = next;
    }
    true
}

/// `decode_parameters.c`'s NLSF interpolation for the first half-frame of a
/// 20 ms subframe when `NLSFInterpCoef_Q2 < 4`.
#[must_use]
pub fn interpolate_nlsf(prev: &[i32], curr: &[i32], coef_q2: i32) -> Vec<i32> {
    prev.iter()
        .zip(curr.iter())
        .map(|(&p, &c)| p + ((coef_q2 * (c - p)) >> 2))
        .collect()
}
