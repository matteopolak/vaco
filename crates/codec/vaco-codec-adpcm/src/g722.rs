//! ITU-T G.722 sub-band ADPCM (`adpcm_g722`) — mono, continuous, one byte per
//! input sample pair at the reference's 64 kbit/s mode (6-bit low sub-band
//! code in the low bits, 2-bit high sub-band code in the top two).
//!
//! No `provenance/sources.toml` entry names ITU-T G.722 today, so no
//! `Vaco-Spec-Ref` is attached.
//!
//! # This is the lowest-confidence codec in this crate — read before trusting it
//!
//! The real G.722 splits the input into two sub-bands with a 24-tap FIR
//! quadrature mirror filter (ITU-T G.722 Table 1a/1b) and separately ADPCM-
//! codes each band with its own two-pole/six-zero adaptive predictor (the
//! same "g72x" shape [`crate::g726`] documents *not* implementing exactly).
//! Reproducing the QMF's 24 published taps from memory without a primary
//! text to check them against is exactly the kind of table this project's
//! own conventions warn against shipping unverified (`planning/AGENT-
//! CONSTRAINTS.md`'s "three tiers" section), so this module does not attempt
//! it. What it implements instead is a **reversible two-point Haar/lifting
//! transform** (`high = x0 - x1; low = x1 + (high >> 1)`, the same
//! integer-reversible split JPEG2000's 5/3 wavelet uses) as a stand-in
//! sub-band split, followed by [`crate::g726`]'s same adaptive-delta coder on
//! each band at 6 bits (low) / 2 bits (high). It is **not** the ITU-T QMF and
//! is not expected to interoperate with the reference `adpcm_g722` codec —
//! it decodes its own encoder's output correctly and produces genuinely
//! sub-band-compressed audio, which is the honest, flagged-as-such coverage
//! this batch's brief allows for rather than skipping G.722 outright.

use vaco_core::{Error, Result};

/// The same shape as [`crate::g726`]'s per-band state, kept local so this
/// module's low/high bands can carry independent step sizes without needing
/// `crate::g726`'s items to be `pub(crate)`.
struct Band {
    predictor: i32,
    step: i32,
    bits: u32,
}

impl Band {
    const fn new(bits: u32) -> Self {
        Self {
            predictor: 0,
            step: 8,
            bits,
        }
    }

    fn decode_code(&mut self, code: u32) -> i32 {
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
        self.predictor += diff;
        let mult = if magnitude == 0 { 220 } else { 256 + magnitude.cast_signed() * 32 };
        self.step = ((self.step * mult) >> 8).clamp(2, 8192);
        self.predictor
    }

    fn encode_sample(&mut self, sample: i32) -> u32 {
        let mag_bits = self.bits - 1;
        let diff = sample - self.predictor;
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

const LOW_BITS: u32 = 6;
const HIGH_BITS: u32 = 2;

/// Decode `sample_count` mono 16-bit samples from a continuous G.722-shaped
/// byte stream (see the module docs on what this is and is not).
///
/// # Errors
/// [`Error::UnexpectedEof`] if `data` is shorter than `sample_count / 2`
/// bytes (rounded up).
pub(crate) fn decode(data: &[u8], sample_count: usize) -> Result<Vec<i16>> {
    let mut low_band = Band::new(LOW_BITS);
    let mut high_band = Band::new(HIGH_BITS);
    let pairs = sample_count.div_ceil(2);
    let mut out = Vec::new();
    for p in 0..pairs {
        let byte = *data.get(p).ok_or(Error::UnexpectedEof)?;
        let low_code = u32::from(byte) & 0x3F;
        let high_code = (u32::from(byte) >> 6) & 0x03;
        let low = low_band.decode_code(low_code);
        let high = high_band.decode_code(high_code);
        // Invert the reversible lifting split: high = x0-x1, low = x1+(high>>1).
        let x1 = low - (high >> 1);
        let x0 = high + x1;
        out.push(x0.clamp(-32768, 32767) as i16);
        if out.len() < sample_count {
            out.push(x1.clamp(-32768, 32767) as i16);
        }
    }
    out.truncate(sample_count);
    Ok(out)
}

/// Encode mono `samples` into a continuous G.722-shaped byte stream. An odd
/// final sample is paired with itself (`high` collapses to 0 for that pair).
pub(crate) fn encode(samples: &[i16]) -> Vec<u8> {
    let mut low_band = Band::new(LOW_BITS);
    let mut high_band = Band::new(HIGH_BITS);
    let mut out = Vec::new();
    for pair in samples.chunks(2) {
        let x0 = i32::from(*pair.first().unwrap_or(&0));
        let x1 = pair.get(1).map_or(x0, |&v| i32::from(v));
        let high = x0 - x1;
        let low = x1 + (high >> 1);
        let low_code = low_band.encode_sample(low);
        let high_code = high_band.encode_sample(high);
        out.push(((high_code & 0x03) << 6) as u8 | (low_code & 0x3F) as u8);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code exercising the codec, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    fn tone(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| ((i as f64 * 0.2).sin() * 5000.0) as i16)
            .collect()
    }

    #[test]
    fn even_length_round_trips_approximately() {
        let samples = tone(64);
        let packed = encode(&samples);
        assert_eq!(packed.len(), samples.len() / 2);
        let decoded = decode(&packed, samples.len()).unwrap();
        assert_eq!(decoded.len(), samples.len());
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert!((i32::from(*a) - i32::from(*b)).abs() < 4000, "{a} vs {b}");
        }
    }

    #[test]
    fn odd_length_is_handled() {
        let samples = tone(41);
        let packed = encode(&samples);
        let decoded = decode(&packed, samples.len()).unwrap();
        assert_eq!(decoded.len(), samples.len());
    }

    #[test]
    fn silence_stays_near_silent() {
        let samples = vec![0i16; 20];
        let packed = encode(&samples);
        let decoded = decode(&packed, samples.len()).unwrap();
        for s in decoded {
            assert!(s.abs() < 200, "{s}");
        }
    }

    #[test]
    fn truncated_stream_is_eof() {
        assert!(decode(&[], 10).is_err());
    }
}
