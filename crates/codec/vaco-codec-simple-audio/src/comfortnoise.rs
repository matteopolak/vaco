//! RFC 3389 RTP comfort noise (CN): the Silence Insertion Descriptor (SID)
//! payload plus a noise generator driven by it.
//!
//! RFC 3389 §3 defines the *wire format* precisely — a noise-level byte
//! (`0..127`, in `-dBov`) followed by `M` quantised LPC reflection
//! coefficients, one byte each — and says explicitly (§2) that "the comfort
//! noise analysis and synthesis... are unspecified and left
//! implementation-specific." So unlike [`crate::qoa`] and [`crate::dfpwm`],
//! there is no reference behaviour to match: any generator that (a) shapes
//! its noise using the transmitted reflection coefficients and (b) matches
//! the transmitted level is a conforming implementation, and no two
//! conforming implementations are expected to produce the same samples.
//!
//! # How it works
//!
//! Decode: dequantise the level and each reflection coefficient (RFC 3389
//! §3.1/§3.2's formulas, transcribed exactly), convert the reflection
//! coefficients to a direct-form all-pole filter via the standard
//! Levinson-Durbin step-up recursion (the textbook inverse of the
//! recursion `vaco-codec-dsp-lpc` runs forward), then drive that filter
//! with white noise and rescale the whole block so its RMS matches the
//! requested level exactly — the filter shapes the noise's *spectrum*, the
//! rescale fixes its *level*, decoupling the two rather than depending on
//! the filter's own incidental gain.
//!
//! Encode: analyse a block of real audio with `vaco-codec-dsp-lpc`
//! (autocorrelation + Levinson-Durbin) to get reflection coefficients
//! directly — no separate conversion needed, since that crate computes
//! them as part of the same recursion — and quantise them plus the block's
//! measured level per RFC 3389 §3.1/§3.2.
//!
//! # What is not covered
//!
//! RFC 3389 carries no sample count or duration; a real deployment infers
//! how long a CN period lasts from the *difference between RTP
//! timestamps*, which is transport state this codec-level abstraction does
//! not see. [`Config::frame_samples`] is this crate's own fixed-block
//! substitute (default: 20ms, following ordinary RTP packetisation
//! practice), configurable per instance. Comfort noise is a mono,
//! per-channel-of-a-voice-call concept with no standard multi-channel
//! form, so only mono is implemented.

use vaco_codec_dsp_lpc::{autocorrelate_dispatched, levinson_durbin};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Reflection coefficients past this order are dropped on encode and
/// ignored on decode (RFC 3389 §3: "The decoder may reduce the model order
/// by setting higher order reflection coefficients to zero"). Comfort
/// noise's own spectral envelope does not need a high-order model; this
/// bounds both the analysis cost and a maliciously long SID payload's decode
/// cost.
pub const MAX_MODEL_ORDER: usize = 20;

/// A tiny, seedable, deterministic noise source (`SplitMix64`, D. Lemire /
/// S. Vigna's public-domain construction) — comfort noise has no need for
/// cryptographic quality, only for a fast, reproducible stream so a decode
/// replays identically given the same seed.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[-1.0, 1.0)`.
    fn next_signed_unit(&mut self) -> f64 {
        let bits = self.next_u64() >> 11; // 53 significant bits
        let u = (bits as f64) / (1u64 << 53) as f64;
        u * 2.0 - 1.0
    }
}

/// Dequantised comfort-noise parameters (RFC 3389 §3.1/§3.2).
#[derive(Debug, Clone)]
pub struct SidFrame {
    /// `0..=127`, in `-dBov`.
    pub level: u8,
    /// One reflection coefficient per transmitted byte, each in `(-1, 1)`.
    pub reflection: Vec<f64>,
}

/// Parse a raw SID payload: byte 0 is the level, every following byte an
/// 8-bit quantised reflection-coefficient index.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] on an empty payload.
pub fn parse(budget: &mut Budget, payload: &[u8]) -> Result<SidFrame> {
    let Some(&level_byte) = payload.first() else {
        return Err(Error::InvalidData("comfortnoise: empty SID payload"));
    };
    let level = level_byte & 0x7F;
    let coeff_bytes = payload.get(1..).unwrap_or(&[]);
    let order = coeff_bytes.len().min(MAX_MODEL_ORDER);
    let mut reflection = budget.alloc::<f64>(order)?;
    for (slot, &n) in reflection.iter_mut().zip(coeff_bytes.iter()) {
        // RFC 3389 §3.2: k_i(N_i) = 258*(N_i-127)/32768, N_i in 0..=254
        // (255 reserved — treated as the neutral 0 coefficient rather than
        // rejecting the whole frame, matching the RFC's own tolerance for
        // a receiver that "may reduce the model order").
        let n = if n == 255 { 127 } else { n };
        *slot = 258.0 * (f64::from(n) - 127.0) / 32768.0;
    }
    Ok(SidFrame { level, reflection })
}

/// Build a raw SID payload from a level and reflection coefficients (the
/// inverse of [`parse`]).
#[must_use]
pub fn build(level: u8, reflection: &[f64]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(level & 0x7F);
    for &k in reflection {
        let n = (k * 32768.0 / 258.0 + 127.0).round().clamp(0.0, 254.0);
        out.push(n as u8);
    }
    out
}

/// The Levinson-Durbin step-up recursion, run in reverse: reflection
/// coefficients to direct-form predictor coefficients (`vaco-codec-dsp-lpc`
/// runs the forward direction as part of its own analysis; this is its
/// textbook inverse, standard in any linear-prediction reference such as
/// Rabiner & Schafer's *Digital Processing of Speech Signals* §8.3).
fn reflection_to_direct(k: &[f64]) -> Vec<f64> {
    let mut a: Vec<f64> = Vec::new();
    for (stage, &km) in k.iter().enumerate() {
        let order = stage + 1;
        let mut next = vec![0.0f64; order];
        for i in 0..stage {
            let ai = a.get(i).copied().unwrap_or(0.0);
            let mirror = a.get(stage - 1 - i).copied().unwrap_or(0.0);
            if let Some(slot) = next.get_mut(i) {
                *slot = ai - km * mirror;
            }
        }
        if let Some(last) = next.last_mut() {
            *last = km;
        }
        a = next;
    }
    a
}

/// Per-instance configuration: sample rate/frame size, since RFC 3389
/// carries neither (see the module doc's "What is not covered").
#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sample_rate: u32,
    pub frame_samples: u32,
    pub seed: u64,
}

impl Default for Config {
    fn default() -> Self {
        // RFC 3389 §4: the static RTP payload type for CN is defined for an
        // 8kHz clock; 20ms is ordinary RTP packetisation practice.
        Self {
            sample_rate: 8_000,
            frame_samples: 160,
            seed: 0x00A0_157E,
        }
    }
}

/// Generate `cfg.frame_samples` of comfort noise from one SID frame's
/// parameters, using `rng_state` as the running noise-source seed (a
/// caller keeping one [`Config`]-derived generator alive across many
/// packets should thread the same seed through, the way this module's own
/// `Generator` does).
fn synthesize(
    budget: &mut Budget,
    frame: &SidFrame,
    samples: u32,
    rng: &mut Rng,
) -> Result<Vec<i16>> {
    let a = reflection_to_direct(&frame.reflection);
    let n = samples as usize;
    let mut excitation = budget.alloc::<f64>(n)?;
    for e in &mut excitation {
        *e = rng.next_signed_unit();
    }

    let mut history: Vec<f64> = Vec::new();
    let mut shaped = budget.alloc::<f64>(n)?;
    for (slot, &e) in shaped.iter_mut().zip(excitation.iter()) {
        let mut pred = 0.0f64;
        let hist_len = history.len();
        for (i, coef) in a.iter().enumerate() {
            if let Some(idx) = hist_len.checked_sub(1 + i) {
                pred += coef * history.get(idx).copied().unwrap_or(0.0);
            }
        }
        let x = e + pred;
        *slot = x;
        history.push(x);
        if history.len() > a.len().max(1) {
            history.remove(0);
        }
    }

    // -dBov to a full-scale-relative target RMS, then rescale the shaped
    // noise so its measured RMS matches it exactly — decoupling the
    // filter's own gain from the requested level (see the module doc).
    let target_rms = 32767.0 * 10f64.powf(-f64::from(frame.level) / 20.0);
    let measured_sq: f64 = shaped.iter().map(|x| x * x).sum();
    let measured_rms = (measured_sq / (n.max(1) as f64)).sqrt();
    let scale = if measured_rms > 1e-9 {
        target_rms / measured_rms
    } else {
        0.0
    };

    let mut out = budget.alloc::<i16>(n)?;
    for (slot, &x) in out.iter_mut().zip(shaped.iter()) {
        *slot = (x * scale).round().clamp(-32768.0, 32767.0) as i16;
    }
    Ok(out)
}

/// A comfort-noise decoder's running state: just the noise source, since
/// every SID frame is independently decodable (no history a real spectral
/// envelope needs to carry between frames — each SID simply replaces the
/// model).
#[derive(Debug)]
pub struct Generator {
    rng: Rng,
}

impl Generator {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { rng: Rng(seed) }
    }

    /// # Errors
    /// Propagates a [`vaco_limits`] allocation failure.
    pub fn generate(
        &mut self,
        budget: &mut Budget,
        frame: &SidFrame,
        samples: u32,
    ) -> Result<Vec<i16>> {
        synthesize(budget, frame, samples, &mut self.rng)
    }
}

/// Analyse `samples` (mono) and produce the SID parameters RFC 3389 would
/// transmit for them: order `order.min(MAX_MODEL_ORDER)` reflection
/// coefficients plus the block's measured level.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if `samples` is empty.
pub fn analyze(samples: &[i16], order: usize) -> Result<SidFrame> {
    if samples.is_empty() {
        return Err(Error::InvalidData(
            "comfortnoise: cannot analyse an empty block",
        ));
    }
    let order = order
        .min(MAX_MODEL_ORDER)
        .min(samples.len().saturating_sub(1));

    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    let rms = (sum_sq / (samples.len() as f64)).sqrt().max(1.0);
    let dbov = 20.0 * (rms / 32767.0).log10();
    let level = (-dbov).round().clamp(0.0, 127.0) as u8;

    if order == 0 {
        return Ok(SidFrame {
            level,
            reflection: Vec::new(),
        });
    }

    let windowed: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
    let mut autoc = vec![0.0f64; order + 1];
    autocorrelate_dispatched(&windowed, &mut autoc);
    let ld = levinson_durbin(&autoc, order);
    let computed = ld.order_computed();
    let mut reflection = Vec::new();
    for o in 1..=computed {
        reflection.push(ld.reflection(o));
    }
    Ok(SidFrame { level, reflection })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn build_parse_round_trips_the_wire_format() {
        let reflection = vec![0.4, -0.2, 0.1];
        let wire = build(42, &reflection);
        let mut budget = Budget::new(Limits::permissive());
        let frame = parse(&mut budget, &wire).unwrap();
        assert_eq!(frame.level, 42);
        assert_eq!(frame.reflection.len(), 3);
        for (a, b) in reflection.iter().zip(frame.reflection.iter()) {
            // 8-bit quantisation, ~1/128 of the (-1,1) range.
            assert!((a - b).abs() < 0.02, "{a} vs {b}");
        }
    }

    #[test]
    fn generated_noise_matches_the_requested_level() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = SidFrame {
            level: 20,
            reflection: vec![0.3, -0.1],
        };
        let mut generator = Generator::new(0x1234_5678);
        let samples = generator.generate(&mut budget, &frame, 4000).unwrap();
        let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let rms = (sum_sq / samples.len() as f64).sqrt();
        let target = 32767.0 * 10f64.powf(-20.0 / 20.0);
        // A finite noise block's measured RMS fluctuates around its target;
        // this checks the generator lands in the right regime, not an
        // exact match.
        assert!(
            (rms - target).abs() / target < 0.25,
            "rms {rms} vs target {target}"
        );
    }

    #[test]
    fn zero_order_model_still_produces_shaped_level() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = SidFrame {
            level: 10,
            reflection: Vec::new(),
        };
        let mut generator = Generator::new(1);
        let samples = generator.generate(&mut budget, &frame, 800).unwrap();
        assert_eq!(samples.len(), 800);
        assert!(samples.iter().any(|&s| s != 0));
    }

    #[test]
    fn analyze_never_panics_and_bounds_the_model_order() {
        let samples: Vec<i16> = (0..50).map(|i| (i * 37) as i16).collect();
        let frame = analyze(&samples, 200).unwrap();
        assert!(frame.reflection.len() <= MAX_MODEL_ORDER);
    }

    #[test]
    fn parse_never_panics_on_arbitrary_bytes() {
        let mut budget = Budget::new(Limits::permissive());
        for len in [0usize, 1, 2, 5, 40] {
            let data: Vec<u8> = (0..len).map(|i| (i * 61 + 3) as u8).collect();
            let _ = parse(&mut budget, &data);
        }
    }
}
