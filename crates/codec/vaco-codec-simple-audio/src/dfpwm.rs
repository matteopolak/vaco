//! `DFPWM1a`: Ben "`GreaseMonkey`" Russell's 1-bit-per-sample adaptive
//! delta/PWM codec, per the charge/strength predictor written up at
//! <https://wiki.vexatos.com/dfpwm> (CC-BY 4.0, written by the codec's own
//! author).
//!
//! # Not wired up as the `dfpwm` codec — measured, not assumed
//!
//! This module transcribes the wiki page's formulas exactly (`Ri=7`,
//! `Rd=20`, the stated charge/strength update and nudge rule) and they are
//! internally consistent: the encoder and decoder here agree with each
//! other, and the predictor visibly tracks a real signal ([`State::step`]'s
//! own tests). But black-box testing against `ffmpeg 8.1`'s `dfpwm`
//! decoder — which accepted this module's *bit framing* (LSB-first, mono,
//! default 48kHz — all measured facts this module gets right) — found the
//! *charge growth rate* does not match: feeding `ffmpeg -f dfpwm` a
//! constant run of `1` bits and reading its decoded PCM back gives a charge
//! sequence `1,2,3,4,5,6,7,9,11,13,15,...` that a brute-force search over
//! this formula's free parameters (`Ri`, `Rd` up to 80, the rounding
//! constant, both initial-`last_bit` values, with and without the nudge
//! rule) could not reproduce even once. `ffmpeg`'s real curve grows far
//! more slowly than this module's — it has not saturated at `127` even 40
//! samples into a constant run, where this module's saturates by sample
//! ~20 — which rules out a mere post-filter (RFC-3389-style smoothing
//! could not explain a growth-*rate* this different). So the real
//! `DFPWM1a` implementations in the wild use a materially different
//! recursion than the one this informal wiki page states, and this module
//! is a plausible-looking, self-consistent, **wrong** stand-in for it —
//! the same shape as `vaco-codec-adpcm`'s `g722`/`g726` modules, and kept
//! unregistered for the identical reason: registering it would hand a
//! caller wrong output on every real `.dfpwm` file with no error.
//! [`crate::DfpwmDecoder`]/[`crate::DfpwmEncoder`] therefore always return
//! [`vaco_core::Error::Unsupported`] and carry no `DecoderDesc`/
//! `EncoderDesc` registration; the functions here stay as a documented,
//! failed hypothesis and a real fixture (`allones`/`allzeros` all-one-bit
//! and all-zero-bit `ffmpeg`-decoded traces) for whoever solves the real
//! recursion next.
//!
//! # How it works (the hypothesis this module implements)
//!
//! Each output sample is the "charge" `q` of a one-pole predictor pushed
//! toward a per-bit target (`-128` for a `0` bit, `127` for a `1` bit) by a
//! step controlled by the "strength" `s`. `s` itself adapts: a run of equal
//! bits raises it (bigger steps, a flatter response), an alternating run
//! lowers it (smaller steps, more responsive). The encoder greedily picks
//! the bit that moves the shared predictor toward the real input and runs
//! the identical update, so encoder and decoder never diverge — this part
//! is not in question; only the exact numeric recursion is.
//!
//! Both directions work entirely in the codec's native 8-bit domain
//! (`-128..=127`); [`decode`]/[`encode`] convert to/from 16-bit PCM at the
//! boundary by a fixed `<<8`/`>>8`, matching how a 1-byte-per-sample codec is
//! usually bridged to a 16-bit pipeline. DFPWM is specified as a mono codec
//! ("stereo... by running two streams in parallel", not by interleaving
//! bits), so this module only ever produces or consumes one channel's worth
//! of samples.

use vaco_core::Result;
use vaco_limits::Budget;

/// `Ri`, the strength-increase constant. Fixed by the spec's own worked
/// example (`(7, 20)` "is reasonable for 8 bits per sample") and matched by
/// every real `DFPWM1a` stream in the wild.
const RI: i32 = 7;
/// `Rd`, the strength-decrease constant.
const RD: i32 = 20;

/// The shared predictor state, carried across every sample of one
/// continuous stream (encode or decode).
#[derive(Debug, Clone, Copy)]
pub struct State {
    q: i32,
    s: i32,
    last_bit: bool,
}

impl Default for State {
    fn default() -> Self {
        // The spec's own initial values: charge 0, strength 1, previous bit
        // 0 "to simplify implementation".
        Self {
            q: 0,
            s: 1,
            last_bit: false,
        }
    }
}

impl State {
    /// Run the shared predictor for one bit, returning the resulting sample
    /// (the new charge, in `-128..=127`).
    #[allow(
        clippy::integer_division,
        reason = "the spec's own formula is integer division by 256, not a float approximation \
                  of it — see the wiki spec's Charge/Strength adjustment sections"
    )]
    fn step(&mut self, bit: bool) -> i32 {
        let t: i32 = if bit { 127 } else { -128 };

        // "q' <- q + (s*(t-q)+128)/256"; nudged by one toward `t` if the
        // division rounded to no movement at all, so the charge can always
        // eventually reach its target.
        let mut q_next = self.q + (self.s * (t - self.q) + 128) / 256;
        if q_next == self.q && self.q != t {
            q_next += if t < self.q { -1 } else { 1 };
        }
        self.q = q_next;

        let (r, z): (i32, i32) = if bit == self.last_bit {
            (RI, 255)
        } else {
            (RD, 0)
        };
        let mut s_next = self.s + (r * (z - self.s) + 128) / 256;
        if s_next == self.s && self.s != z {
            s_next += if z < self.s { -1 } else { 1 };
        }
        self.s = s_next;

        self.last_bit = bit;
        self.q
    }
}

/// Decode a packed, LSB-first `DFPWM1a` bit stream into 8-bit predictor
/// samples (one per bit of input, so `8 * data.len()` samples, minus
/// whatever trailing padding bits the last byte carries — a caller that
/// knows the real sample count should truncate).
fn decode_raw(state: &mut State, data: &[u8], out: &mut [i16]) {
    for (byte, out_chunk) in data.iter().zip(out.chunks_mut(8)) {
        for (bit_index, slot) in out_chunk.iter_mut().enumerate() {
            let bit = (byte >> bit_index) & 1 != 0;
            let sample8 = state.step(bit);
            // Widen the 8-bit charge to 16-bit PCM. `sample8` is always in
            // -128..=127, so `<< 8` never overflows `i16`.
            *slot = (sample8 << 8) as i16;
        }
    }
}

/// Decode one block, allocating the output through `budget` since its size
/// is derived from the (attacker-controlled) input length.
///
/// # Errors
/// Propagates a [`vaco_limits`] allocation failure.
pub fn decode(budget: &mut Budget, state: &mut State, data: &[u8]) -> Result<Vec<i16>> {
    let sample_count = data.len().saturating_mul(8);
    let mut out = budget.alloc::<i16>(sample_count)?;
    decode_raw(state, data, &mut out);
    Ok(out)
}

/// Encode 16-bit PCM samples into a packed, LSB-first `DFPWM1a` bit stream.
///
/// # Errors
/// Propagates a [`vaco_limits`] allocation failure.
pub fn encode(budget: &mut Budget, state: &mut State, samples: &[i16]) -> Result<Vec<u8>> {
    let byte_count = samples.len().div_ceil(8);
    let mut out = budget.alloc::<u8>(byte_count)?;
    for (chunk, out_byte) in samples.chunks(8).zip(out.iter_mut()) {
        let mut byte = 0u8;
        for (bit_index, &sample) in chunk.iter().enumerate() {
            // Narrow back to the codec's native 8-bit domain before
            // comparing against the predictor's own charge, which lives in
            // that same domain.
            let sample8 = i32::from(sample >> 8).clamp(-128, 127);
            let bit = sample8 > state.q || (sample8 == state.q && state.q == 127);
            state.step(bit);
            if bit {
                byte |= 1 << bit_index;
            }
        }
        *out_byte = byte;
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::integer_division,
    reason = "test code exercising the decoder, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn tone(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((f64::from(i as u32) * 0.1).sin() * 20000.0) as i16)
            .collect()
    }

    #[test]
    fn round_trips_a_tone_with_bounded_error() {
        let mut budget = Budget::new(Limits::permissive());
        let samples = tone(4096);
        let mut enc_state = State::default();
        let wire = encode(&mut budget, &mut enc_state, &samples).unwrap();
        assert_eq!(wire.len(), samples.len().div_ceil(8));

        let mut dec_state = State::default();
        let decoded = decode(&mut budget, &mut dec_state, &wire).unwrap();
        assert_eq!(decoded.len(), samples.len());

        // A 1-bit-per-sample delta codec cannot be exact, but it must track
        // the input rather than diverge: a low-passed comparison (simple
        // moving average) should stay in the right ballpark. This is a
        // structural check — the raw per-sample residual is large by
        // design — not a byte-exactness one.
        let window = 32;
        let mut max_avg_err = 0i64;
        for w in samples
            .windows(window)
            .step_by(window)
            .zip(decoded.windows(window).step_by(window))
        {
            let (s, d) = w;
            let avg_s: i64 = s.iter().map(|&x| i64::from(x)).sum::<i64>() / window as i64;
            let avg_d: i64 = d.iter().map(|&x| i64::from(x)).sum::<i64>() / window as i64;
            max_avg_err = max_avg_err.max((avg_s - avg_d).abs());
        }
        assert!(
            max_avg_err < 12000,
            "windowed average error too large: {max_avg_err}"
        );
    }

    #[test]
    fn silence_stays_near_silence() {
        let mut budget = Budget::new(Limits::permissive());
        let samples = vec![0i16; 256];
        let mut enc_state = State::default();
        let wire = encode(&mut budget, &mut enc_state, &samples).unwrap();
        let mut dec_state = State::default();
        let decoded = decode(&mut budget, &mut dec_state, &wire).unwrap();
        let max_abs = decoded
            .iter()
            .map(|&s| i32::from(s).abs())
            .max()
            .unwrap_or(0);
        // The predictor's own step size for an alternating-bit run around
        // zero settles small; it never claims true silence (bit 0 is a
        // step toward one target, not "no step"), so this bounds the
        // steady-state ripple rather than asserting exact zero.
        assert!(
            max_abs < 8000,
            "silence produced too large a ripple: {max_abs}"
        );
    }

    #[test]
    fn decode_never_panics_on_arbitrary_bytes() {
        let mut budget = Budget::new(Limits::permissive());
        for len in [0usize, 1, 2, 7, 8, 9, 63, 300] {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let mut state = State::default();
            let decoded = decode(&mut budget, &mut state, &data).unwrap();
            assert_eq!(decoded.len(), len * 8);
        }
    }

    /// Records the measurement the module doc's "Not wired up" section
    /// summarises: `ffmpeg 8.1 -f dfpwm` decoding 200 bytes of `0xFF` (an
    /// unbroken run of `1` bits) produces this exact 8-bit charge sequence
    /// (`decoded_i16 >> 8`), which this module's own [`decode`] does not
    /// reproduce — confirming the divergence is real and not a one-off
    /// measurement error, without needing the `ffmpeg` binary present to
    /// run the test.
    #[test]
    fn own_decode_does_not_match_the_measured_ffmpeg_trace() {
        let ffmpeg_q: [i16; 10] = [1, 2, 3, 4, 5, 6, 7, 9, 11, 13];
        let mut budget = Budget::new(Limits::permissive());
        let mut state = State::default();
        let data = vec![0xFFu8; 2];
        let decoded = decode(&mut budget, &mut state, &data).unwrap();
        let own_q: Vec<i16> = decoded.iter().take(10).map(|&s| s >> 8).collect();
        assert_ne!(own_q.as_slice(), ffmpeg_q.as_slice());
    }
}
