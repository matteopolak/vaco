//! Upsampling SILK's internal rate (8/12/16 kHz) to Opus's fixed 48 kHz
//! output. RFC 6716 does not specify a particular resampler — `silk/
//! resampler.c`'s IIR/FIR cascade is a reference implementation choice,
//! not a bitstream contract — so this is original engineering: a
//! straightforward zero-stuffed windowed-sinc polyphase upsampler at the
//! exact integer ratio each internal rate needs (6x/4x/3x), rather than a
//! transcription of the reference's tables.
//!
//! `Vaco-Provenance: original` for this file.

/// A persistent upsampler for one fixed integer ratio, holding the FIR
/// delay line across calls so consecutive frames stay continuous.
#[derive(Debug, Clone)]
pub struct Upsampler {
    factor: usize,
    taps: Vec<f32>,
    history: Vec<f32>,
}

impl Upsampler {
    /// `factor` in `{3, 4, 6}` (16/12/8 kHz to 48 kHz).
    #[must_use]
    pub fn new(factor: usize) -> Self {
        let factor = factor.clamp(1, 8);
        let half_taps = 4 * factor; // 8 zero-crossings of the sinc on each side.
        let n = 2 * half_taps + 1;
        let mut taps = vec![0.0f32; n];
        for (i, t) in taps.iter_mut().enumerate() {
            let x = i as isize - half_taps as isize;
            let sinc = if x == 0 {
                1.0
            } else {
                (std::f32::consts::PI * x as f32 / factor as f32).sin()
                    / (std::f32::consts::PI * x as f32 / factor as f32)
            };
            // Blackman window for stopband attenuation.
            let w = 0.42 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()
                + 0.08 * (4.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
            *t = sinc * w;
        }
        let sum: f32 = taps.iter().sum::<f32>() / factor as f32;
        if sum.abs() > 1e-9 {
            for t in &mut taps {
                *t /= sum;
            }
        }
        let history = vec![0.0f32; n];
        Self {
            factor,
            taps,
            history,
        }
    }

    /// Upsample `input` by this resampler's factor, in place across calls
    /// (continuous filter state).
    #[must_use]
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.factor <= 1 {
            return input.to_vec();
        }
        let taps_len = self.taps.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.history);
        buf.extend_from_slice(input);

        let mut out = Vec::new();
        // For each output phase, convolve the (zero-stuffed, but expressed
        // as a decimated tap selection) sinc against the un-stuffed input.
        for i in 0..input.len() {
            for phase in 0..self.factor {
                let mut acc = 0.0f32;
                // Zero-stuffed sample `k` at input index `i` maps to
                // output index `i*factor + phase`; the tap touching input
                // sample `i - m` at this phase is `taps[phase + m*factor]`.
                let mut m = 0usize;
                loop {
                    let tap_idx = phase + m * self.factor;
                    let Some(&tap) = self.taps.get(tap_idx) else {
                        break;
                    };
                    let src_idx = self.history.len() + i - m;
                    acc += tap * buf.get(src_idx).copied().unwrap_or(0.0);
                    m += 1;
                }
                out.push(acc);
            }
        }

        let keep = taps_len.min(buf.len());
        self.history = buf[buf.len() - keep..].to_vec();
        if self.history.len() < taps_len {
            let mut padded = vec![0.0f32; taps_len - self.history.len()];
            padded.extend_from_slice(&self.history);
            self.history = padded;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsampling_preserves_dc() {
        let mut up = Upsampler::new(3);
        let input = vec![1.0f32; 64];
        let out = up.process(&input);
        assert_eq!(out.len(), 192);
        // Well after the filter's own settling, a DC input should upsample
        // to (approximately) the same DC level.
        let tail_avg: f32 = out[120..].iter().sum::<f32>() / (out.len() - 120) as f32;
        assert!((tail_avg - 1.0).abs() < 0.1, "tail_avg = {tail_avg}");
    }
}
