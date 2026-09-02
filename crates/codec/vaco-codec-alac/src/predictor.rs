//! ALAC's real adaptive linear predictor (`unpc_block` in Apple's
//! reference).
//!
//! # Provenance — supersedes an earlier, self-invented design
//!
//! An earlier version of this module was a sign-sign LMS filter of this
//! crate's own design, chosen specifically to need no transmitted
//! coefficients — because issue #285's brief read as a blanket prohibition
//! on consulting any ALAC reference. That was too strict: Apple's ALAC
//! reference (<https://github.com/macosforge/alac>) is Apache License
//! 2.0 — confirmed directly (`curl` the `LICENSE` file) — which sits outside
//! this project's D7/D15 clean-room rule (specifically about FFmpeg/libav
//! GPL code, not a blanket ban on every reference implementation). A
//! self-invented predictor with self-invented adaptation constants cannot
//! decode a real encoder's coefficients, which is exactly why the old
//! design was self-interoperable only.
//!
//! This is a from-scratch Rust translation of `codec/dp_dec.c`'s
//! `unpc_block` **general case** (the file also carries hand-unrolled
//! `numactive == 4`/`== 8` fast paths that compute the identical
//! arithmetic; not needed here since this is not a hot loop optimisation
//! pass) — the sign-sign coefficient adaptation that gives each tap's
//! coefficient a `±1` nudge per sample, order of adaptation from the
//! highest-lag tap down, stopping early once the running "would this have
//! helped" residual `del0` crosses zero. Two substitutions made with
//! confidence rather than transcribed as bit tricks: `sign_of_int` (a
//! manual `(-i as u32) >> 31 | (i >> 31)` dance) is exactly Rust's
//! `i32::signum`, and the reference's own leading-zero-count workaround is
//! `u32::leading_zeros` elsewhere in this crate — both are well-known,
//! provably equivalent simplifications of code the reference's own
//! comments say was written to help a slower compiler, not to encode
//! anything meaningful about the format.
//!
//! `Vaco-Spec-Ref: alac-agc-source codec/dp_dec.c unpc_block (general-case
//! branch), Apple Inc., Apache License 2.0`

use vaco_limits::Budget;

/// Largest predictor order a 5-bit header field (`numU`/`numV`, 0..=31 —
/// [`kALACMaxCoefs`] in the reference is 16, but the field itself allows up
/// to 31) can name.
pub(crate) const MAX_ORDER: usize = 32;

/// Truncate `v` to `chanbits` bits, sign-extending from bit `chanbits - 1` —
/// `(del << chanshift) >> chanshift` in the reference, `chanshift = 32 -
/// chanbits`.
fn wrap_to_chanbits(v: i32, chanbits: u32) -> i32 {
    let chanshift = 32u32.saturating_sub(chanbits);
    if chanshift == 0 || chanshift >= 32 {
        v
    } else {
        (v << chanshift) >> chanshift
    }
}

/// `unpc_block`: reconstruct `num` true samples from `pc1`'s residuals,
/// given `coefs` (updated in place, exactly as the reference's decoder
/// leaves them for the next block) and the predictor `order` ("numactive").
///
/// `order == 0` is a pass-through (the residual *is* the sample -- a real
/// encoder may choose this for content a predictor cannot help with, but
/// `frame_codec::encode` no longer always does; see that function's doc for
/// why `order == 0` used to be this crate's only choice and stopped being
/// one). `order == 31` is the reference's "apply a first-difference
/// integrator, no coefficients" mode, used as the first pass of `modeU ==
/// 1` two-stage prediction.
pub(crate) fn unpc_block(
    pc1: &[i32],
    coefs: &mut [i32],
    order: usize,
    chanbits: u32,
    denshift: u32,
    budget: &mut Budget,
) -> vaco_core::Result<Vec<i32>> {
    let num = pc1.len();
    let mut out: Vec<i32> = budget.alloc(num)?;
    if num == 0 {
        return Ok(out);
    }
    if let Some(slot) = out.get_mut(0) {
        *slot = pc1.first().copied().unwrap_or(0);
    }
    if order == 0 {
        for j in 1..num {
            if let Some(slot) = out.get_mut(j) {
                *slot = pc1.get(j).copied().unwrap_or(0);
            }
        }
        return Ok(out);
    }
    if order == 31 {
        let mut prev = out.first().copied().unwrap_or(0);
        for j in 1..num {
            let del = pc1.get(j).copied().unwrap_or(0).wrapping_add(prev);
            prev = wrap_to_chanbits(del, chanbits);
            if let Some(slot) = out.get_mut(j) {
                *slot = prev;
            }
        }
        return Ok(out);
    }

    let order = order.min(MAX_ORDER).min(num.saturating_sub(1).max(1));
    let denhalf: i64 = if denshift == 0 {
        0
    } else {
        1i64 << (denshift - 1)
    };

    let warmup_end = order.min(num.saturating_sub(1));
    for j in 1..=warmup_end {
        let prev = out.get(j - 1).copied().unwrap_or(0);
        let del = pc1.get(j).copied().unwrap_or(0).wrapping_add(prev);
        if let Some(slot) = out.get_mut(j) {
            *slot = wrap_to_chanbits(del, chanbits);
        }
    }

    let lim = order + 1;
    for j in lim..num {
        let top = out.get(j.wrapping_sub(lim)).copied().unwrap_or(0);
        let mut sum1: i64 = 0;
        for k in 0..order {
            let coef = i64::from(coefs.get(k).copied().unwrap_or(0));
            let hist = out
                .get(j.wrapping_sub(1).wrapping_sub(k))
                .copied()
                .unwrap_or(0);
            // Deliberately `hist - top`, the opposite sign from the
            // adaptation step's `dd = top - hist` below -- the reference
            // itself uses both polarities in the same function
            // (`sum1 += coefs[k] * (pout[-k] - top)` vs. `dd = top -
            // pout[-k]`), not a typo to "simplify" away.
            sum1 = sum1.wrapping_add(coef.wrapping_mul(i64::from(hist.wrapping_sub(top))));
        }
        let del = pc1.get(j).copied().unwrap_or(0);
        let mut del0 = del;
        let sg = del.signum();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the reference's own `sum1 >> denshift` result is added back into a chanbits-wide sample, always in i32 range"
        )]
        let predicted = ((sum1.wrapping_add(denhalf)) >> denshift) as i32;
        let reconstructed = del.wrapping_add(top).wrapping_add(predicted);
        if let Some(slot) = out.get_mut(j) {
            *slot = wrap_to_chanbits(reconstructed, chanbits);
        }

        if sg > 0 {
            for k in (0..order).rev() {
                let hist = out
                    .get(j.wrapping_sub(1).wrapping_sub(k))
                    .copied()
                    .unwrap_or(0);
                let dd = top.wrapping_sub(hist);
                let sgn = dd.signum();
                if let Some(c) = coefs.get_mut(k) {
                    *c = c.wrapping_sub(sgn);
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "same bound as `predicted` above"
                )]
                let contribution =
                    ((i64::from(sgn).wrapping_mul(i64::from(dd))) >> denshift) as i32;
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "order - k is always < MAX_ORDER (32), fits comfortably in i32"
                )]
                let weight = (order - k) as i32;
                del0 = del0.wrapping_sub(weight.wrapping_mul(contribution));
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let hist = out
                    .get(j.wrapping_sub(1).wrapping_sub(k))
                    .copied()
                    .unwrap_or(0);
                let dd = top.wrapping_sub(hist);
                let sgn = dd.signum();
                if let Some(c) = coefs.get_mut(k) {
                    *c = c.wrapping_add(sgn);
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "same bound as `predicted` above"
                )]
                let contribution =
                    ((i64::from(-sgn).wrapping_mul(i64::from(dd))) >> denshift) as i32;
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "order - k is always < MAX_ORDER (32)"
                )]
                let weight = (order - k) as i32;
                del0 = del0.wrapping_sub(weight.wrapping_mul(contribution));
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// `pc_block`: the write-side mirror of [`unpc_block`] -- `codec/dp_enc.c`'s
/// `pc_block` in the reference, same `Vaco-Spec-Ref` as this module's
/// top-level doc. Computes `num` residuals from `samp`'s true samples,
/// adapting `coefs` in place using *exactly* [`unpc_block`]'s own update
/// rule (not a redundant reimplementation of it by coincidence: a decoder
/// reconstructing this function's residuals has to arrive at the identical
/// coefficient state after every sample, since lossless means the
/// reconstructed samples this function's caller would see are bit-for-bit
/// `samp` itself, and the adaptation rule needs only the residual just
/// produced, which is data both sides have. Any divergence between the two
/// update rules desyncs prediction for the rest of the block, which is
/// exactly the class of defect `frame_codec::encode`'s doc describes for
/// the `order == 0` predecessor of this function's caller).
///
/// `order == 0` / `order == 31` are the exact inverses of [`unpc_block`]'s
/// own special cases (verbatim pass-through, first-difference integrator).
/// The general case inverts each `unpc_block` step algebraically:
/// `reconstructed = del + top + predicted` (wrapped to `chanbits`) becomes
/// `del = reconstructed - top - predicted`, deliberately *not* wrapped to
/// `chanbits` itself -- only the reconstructed *sum* ever gets truncated in
/// the reference, never the transmitted residual alone, and `samp[j]`
/// (`reconstructed`) is already in range, so plain wrapping subtraction
/// reproduces the exact residual [`unpc_block`] needs to invert back to it.
pub(crate) fn pc_block(
    samp: &[i32],
    coefs: &mut [i32],
    order: usize,
    chanbits: u32,
    denshift: u32,
) -> Vec<i32> {
    let num = samp.len();
    let mut out = vec![0i32; num];
    if num == 0 {
        return out;
    }
    let s = |i: usize| samp.get(i).copied().unwrap_or(0);
    if let Some(slot) = out.get_mut(0) {
        *slot = s(0);
    }
    if order == 0 {
        for j in 1..num {
            if let Some(slot) = out.get_mut(j) {
                *slot = s(j);
            }
        }
        return out;
    }
    if order == 31 {
        for j in 1..num {
            let del = wrap_to_chanbits(s(j).wrapping_sub(s(j - 1)), chanbits);
            if let Some(slot) = out.get_mut(j) {
                *slot = del;
            }
        }
        return out;
    }

    let order = order.min(MAX_ORDER).min(num.saturating_sub(1).max(1));
    let denhalf: i64 = if denshift == 0 {
        0
    } else {
        1i64 << (denshift - 1)
    };

    let warmup_end = order.min(num.saturating_sub(1));
    for j in 1..=warmup_end {
        let del = wrap_to_chanbits(s(j).wrapping_sub(s(j - 1)), chanbits);
        if let Some(slot) = out.get_mut(j) {
            *slot = del;
        }
    }

    let lim = order + 1;
    for j in lim..num {
        let top = s(j.wrapping_sub(lim));
        let mut sum1: i64 = 0;
        for k in 0..order {
            let coef = i64::from(coefs.get(k).copied().unwrap_or(0));
            let hist = s(j.wrapping_sub(1).wrapping_sub(k));
            sum1 = sum1.wrapping_add(coef.wrapping_mul(i64::from(hist.wrapping_sub(top))));
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "same bound unpc_block's own `predicted` relies on"
        )]
        let predicted = ((sum1.wrapping_add(denhalf)) >> denshift) as i32;
        // Wrapped to `chanbits` *before* anything else touches it, unlike
        // `unpc_block`'s `reconstructed` (which is the only thing that
        // gets wrapped there): the transmitted residual must itself fit
        // the wire format's `chanbits`-wide escape coding (verified by
        // `frame_codec::tests::full_scale_extremes_round_trip`, which
        // failed exactly this way -- silently truncated instead of
        // rejected -- before this wrap was added), and a decoder recovers
        // this exact wrapped value from the bitstream and uses *its* sign
        // for the adaptation below, so using anything else here would
        // desync the two sides' coefficient state without either side
        // ever seeing an error.
        let raw_del = s(j).wrapping_sub(top).wrapping_sub(predicted);
        let del = wrap_to_chanbits(raw_del, chanbits);
        if let Some(slot) = out.get_mut(j) {
            *slot = del;
        }

        let mut del0 = del;
        let sg = del.signum();
        if sg > 0 {
            for k in (0..order).rev() {
                let hist = s(j.wrapping_sub(1).wrapping_sub(k));
                let dd = top.wrapping_sub(hist);
                let sgn = dd.signum();
                if let Some(c) = coefs.get_mut(k) {
                    *c = c.wrapping_sub(sgn);
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "same bound as `predicted` above"
                )]
                let contribution =
                    ((i64::from(sgn).wrapping_mul(i64::from(dd))) >> denshift) as i32;
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "order - k is always < MAX_ORDER (32)"
                )]
                let weight = (order - k) as i32;
                del0 = del0.wrapping_sub(weight.wrapping_mul(contribution));
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..order).rev() {
                let hist = s(j.wrapping_sub(1).wrapping_sub(k));
                let dd = top.wrapping_sub(hist);
                let sgn = dd.signum();
                if let Some(c) = coefs.get_mut(k) {
                    *c = c.wrapping_add(sgn);
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "same bound as `predicted` above"
                )]
                let contribution =
                    ((i64::from(-sgn).wrapping_mul(i64::from(dd))) >> denshift) as i32;
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "order - k is always < MAX_ORDER (32)"
                )]
                let weight = (order - k) as i32;
                del0 = del0.wrapping_sub(weight.wrapping_mul(contribution));
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::permissive())
    }

    #[test]
    fn order_zero_is_pass_through() {
        let pc1 = [10, -5, 3, 3, -100];
        let mut coefs = [0i32; 0];
        let out = unpc_block(&pc1, &mut coefs, 0, 16, 9, &mut budget()).unwrap();
        assert_eq!(out, pc1);
    }

    #[test]
    fn order_thirty_one_integrates_and_wraps() {
        // A first-difference stream: residual 1 each step should integrate
        // to a ramp, wrapped to 8 bits.
        let pc1 = [5, 1, 1, 1, 1, 1];
        let mut coefs = [0i32; 0];
        let out = unpc_block(&pc1, &mut coefs, 31, 8, 9, &mut budget()).unwrap();
        assert_eq!(out, vec![5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn the_first_post_warmup_sample_matches_an_independently_computed_formula() {
        // Order 1, so `lim = 2`: the first predicted sample is `out[2]`.
        // Computed here as fresh, independent arithmetic straight from this
        // module's doc (`sum1 = sum(coef[k] * (hist[k] - top))`, `predicted
        // = (sum1 + denhalf) >> denshift`, `reconstructed = residual + top
        // + predicted`) -- not by tracing this crate's own code by hand
        // (an earlier version of this test did exactly that, got the
        // formula's sign backwards in the same direction its hand-trace
        // did, and passed while the implementation was wrong; this
        // regression is why the check below is a second, separately
        // written calculation instead of a pinned constant).
        let denshift = 4u32;
        let coef = 16i64; // coefs[0]
        let top = 0i64; // out[0], warm-up
        let hist = 2i64; // out[1], warm-up
        let denhalf = 1i64 << (denshift - 1);
        let sum1 = coef * (hist - top);
        let predicted = (sum1 + denhalf) >> denshift;
        let residual = 0i64; // pc1[2]
        let expected = residual + top + predicted;

        let mut coefs = [coef as i32];
        let pc1 = vec![0i32, 2, 0];
        let out = unpc_block(&pc1, &mut coefs, 1, 16, denshift, &mut budget()).unwrap();
        assert_eq!(i64::from(out[2]), expected);
    }

    #[test]
    fn never_panics_on_a_short_or_empty_buffer() {
        let mut coefs = [0i32; 4];
        assert!(
            unpc_block(&[], &mut coefs, 4, 16, 9, &mut budget())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            unpc_block(&[42], &mut coefs, 4, 16, 9, &mut budget()).unwrap(),
            vec![42]
        );
    }

    /// `pc_block` (encode) followed by `unpc_block` (decode), sharing zero-
    /// seeded coefficients the same way `frame_codec::encode`/`decode` do
    /// (the header transmits the *starting* coefficients, and both sides
    /// adapt them identically from there) -- must reproduce the original
    /// samples exactly, for every order this crate's encoder can choose.
    fn pc_roundtrips(samples: &[i32], order: usize, denshift: u32) {
        pc_roundtrips_chanbits(samples, order, denshift, 16);
    }

    fn pc_roundtrips_chanbits(samples: &[i32], order: usize, denshift: u32, chanbits: u32) {
        let mut enc_coefs = vec![0i32; order.min(MAX_ORDER)];
        let residuals = pc_block(samples, &mut enc_coefs, order, chanbits, denshift);
        assert_eq!(residuals.len(), samples.len());
        let mut dec_coefs = vec![0i32; order.min(MAX_ORDER)];
        let decoded = unpc_block(
            &residuals,
            &mut dec_coefs,
            order,
            chanbits,
            denshift,
            &mut budget(),
        )
        .unwrap();
        assert_eq!(decoded, samples, "order {order} denshift {denshift}");
        // The two sides must also leave the *same* adapted coefficient
        // state behind, since a real multi-packet stream never resets
        // mid-block and any drift here would only show up several blocks
        // later as a much harder to trace divergence.
        assert_eq!(
            enc_coefs, dec_coefs,
            "order {order} denshift {denshift}: coefs diverged"
        );
    }

    #[test]
    fn pc_block_order_zero_is_pass_through() {
        pc_roundtrips(&[10, -5, 3, 3, -100], 0, 9);
    }

    #[test]
    fn pc_block_order_thirty_one_round_trips() {
        pc_roundtrips(&[5, 6, 7, 8, 9, 10, 10, 9, 3, -400, -401], 31, 9);
    }

    #[test]
    fn pc_block_general_order_round_trips_a_sine_like_signal() {
        // A smooth, correlated signal -- exactly the shape a real adaptive
        // predictor exists to exploit -- across several orders and
        // denshifts this crate's encoder might plausibly choose.
        let samples: Vec<i32> = (0..2000)
            .map(|i| {
                let x = f64::from(i) * 0.03;
                (x.sin() * 12000.0 + (x * 2.7).cos() * 3000.0) as i32
            })
            .collect();
        for &order in &[1usize, 2, 4, 8, 16] {
            for &denshift in &[9u32, 12, 14] {
                pc_roundtrips(&samples, order, denshift);
            }
        }
    }

    #[test]
    fn pc_block_general_order_round_trips_noise_and_extremes() {
        // Adversarial content: full-scale extremes and a cheap PRNG's worth
        // of noise, neither of which a real predictor helps much with, but
        // which must still round-trip exactly -- correctness under content
        // the predictor is bad at matters at least as much as under content
        // it is good at.
        let mut state = 0x1234_5678u32;
        let noisy: Vec<i32> = (0..1500)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                // A genuine i16-range value from the low 16 bits: signed
                // modulo of a possibly-negative `state as i32` (the bug an
                // earlier version of this line had) would silently escape
                // that range for about half of all `state` values, which
                // is not a legitimate 16-bit sample -- `chanbits = 16`
                // below promises every input sample already fits, and
                // that promise, not this function's own correctness, is
                // what an out-of-range value would actually be testing.
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "deliberately keeping only the low 16 bits"
                )]
                i32::from(state as i16)
            })
            .collect();
        let extremes: Vec<i32> = (0..300)
            .map(|i| {
                if i % 2 == 0 {
                    i32::from(i16::MAX)
                } else {
                    i32::from(i16::MIN)
                }
            })
            .collect();
        for &order in &[4usize, 8] {
            pc_roundtrips(&noisy, order, 9);
            pc_roundtrips(&extremes, order, 9);
        }
    }

    #[test]
    fn pc_block_short_buffers_never_panic() {
        for len in 0..=3 {
            let samples: Vec<i32> = (0..len).map(|i| i * 7 - 3).collect();
            pc_roundtrips(&samples, 8, 9);
        }
    }
}
