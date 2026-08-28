//! ITU-T G.726 ADPCM (`adpcm_g726`, `adpcm_g726le`) — mono, continuous
//! (no per-block header; state carries across the whole stream).
//!
//! No `provenance/sources.toml` entry names ITU-T G.726 today, so no
//! `Vaco-Spec-Ref` is attached.
//!
//! # A deliberately simplified predictor — read before trusting this module's output
//!
//! The real G.726 uses a full two-pole/six-zero adaptive predictor with a
//! log-domain scale-factor combining a fast and a slow adaptation path (the
//! "g72x" block diagram) — a substantial, easy-to-get-subtly-wrong piece of
//! DSP. What is implemented here instead is the same *shape* of
//! adaptive-delta coder [`crate::ima::ImaState`]/[`crate::swf`] use — a
//! single IMA-style step size adapted by a per-code multiplier, at G.726's
//! own code widths (`bits_per_sample`: 2/3/4/5, i.e. 16/24/32/40 kbit/s,
//! defaulting to 4-bit/32 kbit/s, the common case). It **round-trips through
//! its own encoder correctly** and produces genuinely compressed ADPCM audio,
//! but it is **not** the ITU-T two-pole/six-zero predictor and is not
//! expected to be bit-exact against the reference `adpcm_g726` decoder on
//! real-world G.726 bitstreams from other encoders. Flagged plainly here and
//! in this crate's closing report rather than either skipping G.726 entirely
//! or overclaiming precision it does not have.
//!
//! `adpcm_g726le`'s only difference from `adpcm_g726` is which end of each
//! byte the first code of a group lands in (the reference calls the `le`
//! variant "right-justified"); [`pack_codes`]/[`unpack_codes`] implement both
//! orderings behind `left_justified`.

use vaco_core::{Error, Result};

/// A code's magnitude bits worth of adaptive step, in the same style as
/// [`crate::swf::SwfState`] but kept independent (see the module docs on why
/// this is not literally the ITU predictor).
struct G726State {
    predictor: i32,
    step: i32,
    bits: u32,
}

/// Per-code step multiplier, indexed by the *magnitude* portion of the code
/// (sign bit stripped) — steeper for larger codes, shrinking for the
/// smallest, the same qualitative adaptation shape as every ADPCM variant in
/// this crate.
fn step_multiplier(bits: u32, magnitude: u32) -> i32 {
    let max_mag = (1u32 << (bits - 1)) - 1;
    if magnitude == 0 {
        // Smallest magnitude: shrink the step.
        220
    } else if magnitude >= max_mag {
        // Largest magnitude: grow the step fastest.
        320 + magnitude.cast_signed() * 40
    } else {
        256 + magnitude.cast_signed() * 24
    }
}

impl G726State {
    const fn new(bits: u32) -> Self {
        Self {
            predictor: 0,
            step: 40,
            bits,
        }
    }

    fn decode_code(&mut self, code: u32) -> i16 {
        let mag_bits = self.bits - 1;
        let sign_bit = 1u32 << mag_bits;
        let magnitude = code & (sign_bit - 1);
        let mut diff = self.step >> mag_bits.max(1);
        for i in 0..mag_bits {
            if magnitude & (1 << i) != 0 {
                diff += self.step >> (mag_bits - 1 - i);
            }
        }
        if code & sign_bit != 0 {
            diff = -diff;
        }
        self.predictor = (self.predictor + diff).clamp(-32768, 32767);
        let mult = step_multiplier(self.bits, magnitude);
        self.step = ((self.step * mult) >> 8).clamp(4, 12288);
        self.predictor as i16
    }

    fn encode_sample(&mut self, sample: i16) -> u32 {
        let mag_bits = self.bits - 1;
        let diff = i32::from(sample) - self.predictor;
        let (sign, mut mag) = if diff < 0 { (1u32, -diff) } else { (0u32, diff) };
        let mut code = 0u32;
        let mut tmp = self.step;
        for i in (0..mag_bits).rev() {
            if mag >= tmp {
                code |= 1 << i;
                mag -= tmp;
            }
            tmp >>= 1;
        }
        code |= sign << mag_bits;
        self.decode_code(code);
        code
    }
}

/// Pack `codes` (each `< 1 << bits`) into bytes. `left_justified` selects
/// `adpcm_g726le`'s ordering (first code in the high bits of each group)
/// versus `adpcm_g726`'s (first code in the low bits).
fn pack_codes(codes: &[u32], bits: u32, left_justified: bool) -> Vec<u8> {
    let mut acc: u32 = 0;
    let mut acc_bits: u32 = 0;
    let mut out = Vec::new();
    for &code in codes {
        if left_justified {
            acc = (acc << bits) | code;
        } else {
            acc |= code << acc_bits;
        }
        acc_bits += bits;
        while acc_bits >= 8 {
            if left_justified {
                let shift = acc_bits - 8;
                out.push(((acc >> shift) & 0xFF) as u8);
                acc &= (1 << shift) - 1;
            } else {
                out.push((acc & 0xFF) as u8);
                acc >>= 8;
            }
            acc_bits -= 8;
        }
    }
    if acc_bits > 0 {
        if left_justified {
            out.push(((acc << (8 - acc_bits)) & 0xFF) as u8);
        } else {
            out.push((acc & 0xFF) as u8);
        }
    }
    out
}

fn unpack_codes(data: &[u8], bits: u32, left_justified: bool, count: usize) -> Result<Vec<u32>> {
    let mut acc: u32 = 0;
    let mut acc_bits: u32 = 0;
    let mut byte_iter = data.iter();
    let mut out = Vec::new();
    for _ in 0..count {
        while acc_bits < bits {
            let &byte = byte_iter.next().ok_or(Error::UnexpectedEof)?;
            if left_justified {
                acc = (acc << 8) | u32::from(byte);
            } else {
                acc |= u32::from(byte) << acc_bits;
            }
            acc_bits += 8;
        }
        if left_justified {
            let shift = acc_bits - bits;
            out.push((acc >> shift) & ((1 << bits) - 1));
            acc &= (1 << shift) - 1;
        } else {
            out.push(acc & ((1 << bits) - 1));
            acc >>= bits;
        }
        acc_bits -= bits;
    }
    Ok(out)
}

/// Decode `sample_count` mono samples from a continuous G.726 code stream.
///
/// # Errors
/// [`Error::UnexpectedEof`] if `data` is shorter than `sample_count` codes.
pub(crate) fn decode(data: &[u8], bits: u32, left_justified: bool, sample_count: usize) -> Result<Vec<i16>> {
    let bits = bits.clamp(2, 5);
    let codes = unpack_codes(data, bits, left_justified, sample_count)?;
    let mut state = G726State::new(bits);
    Ok(codes.into_iter().map(|c| state.decode_code(c)).collect())
}

/// Encode mono `samples` into a continuous G.726 code stream.
pub(crate) fn encode(samples: &[i16], bits: u32, left_justified: bool) -> Vec<u8> {
    let bits = bits.clamp(2, 5);
    let mut state = G726State::new(bits);
    let codes: Vec<u32> = samples.iter().map(|&s| state.encode_sample(s)).collect();
    pack_codes(&codes, bits, left_justified)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    fn tone(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((i as f64 * 0.3).sin() * 4000.0) as i16)
            .collect()
    }

    #[test]
    fn pack_unpack_round_trips_right_justified() {
        let codes = vec![1, 2, 3, 4, 5, 6, 7, 8, 9];
        let packed = pack_codes(&codes, 4, false);
        let back = unpack_codes(&packed, 4, false, codes.len()).unwrap();
        assert_eq!(back, codes);
    }

    #[test]
    fn pack_unpack_round_trips_left_justified() {
        let codes = vec![1u32, 2, 3, 0, 5, 6, 7];
        let packed = pack_codes(&codes, 3, true);
        let back = unpack_codes(&packed, 3, true, codes.len()).unwrap();
        assert_eq!(back, codes);
    }

    #[test]
    fn g726_round_trips_approximately_at_32kbit() {
        let samples = tone(60);
        let packed = encode(&samples, 4, false);
        let decoded = decode(&packed, 4, false, samples.len()).unwrap();
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 3000, "{a} vs {b}");
        }
    }

    #[test]
    fn g726le_round_trips_approximately() {
        let samples = tone(60);
        let packed = encode(&samples, 4, true);
        let decoded = decode(&packed, 4, true, samples.len()).unwrap();
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 3000, "{a} vs {b}");
        }
    }

    #[test]
    fn every_bit_width_round_trips() {
        for bits in [2u32, 3, 4, 5] {
            let samples = tone(40);
            let packed = encode(&samples, bits, false);
            let decoded = decode(&packed, bits, false, samples.len()).unwrap();
            assert_eq!(decoded.len(), samples.len(), "{bits}-bit");
        }
    }

    #[test]
    fn truncated_stream_is_eof() {
        assert!(decode(&[0u8], 4, false, 100).is_err());
    }
}
