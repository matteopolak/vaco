//! Log-spectral distance (LSD) for audio, computed block-by-block with
//! `vaco_tx::reference::rdft`.
//!
//! ```text
//! for each block of `BLOCK` samples:
//!   Xk = rdft(source block), Yk = rdft(distorted block)         (k = 0..=N/2)
//!   LSD_block = sqrt( mean_k( (10 log10(|Xk|^2 / |Yk|^2))^2 ) )
//! LSD = mean over blocks
//! ```
//!
//! LSD is naturally lower-is-better and zero at identity, so
//! [`SpectralDistance::score`] returns its negation to satisfy [`Metric`]'s
//! higher-is-better contract.
//!
//! # Why `vaco_tx::reference::rdft` and not `rustfft`
//!
//! `rustfft` is a dev-only test oracle, while `vaco_tx::reference` is the
//! workspace's inspection-friendly transform for conformance work. Its O(n²)
//! cost is accepted at the block sizes below.
//!
//! # Scope
//!
//! `Signal::planes[0]` is one channel of samples read through
//! [`crate::metrics::sample`]. Multi-channel callers must score each channel
//! separately so averaging cannot hide a channel-specific divergence.
//!
//! # Performance
//!
//! `BLOCK = 512` costs roughly 262k operations per block per signal. That is
//! suitable for conformance clips of a few hundred blocks, not full-length
//! per-frame scoring.

use crate::compare::quality::{Metric, Signal};
use crate::metrics::sample::{geometry_matches, sample_at};

const BLOCK: usize = 512;
/// Floor for a bin's power before taking `log10`, so a silent bin does not
/// produce `-infinity` and poison the block's mean.
const POWER_FLOOR: f64 = 1e-10;

/// Log-spectral-distance audio metric, negated to fit the "higher is
/// better" [`Metric`] convention.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpectralDistance;

fn channel_samples(signal: &Signal<'_>) -> Option<Vec<f64>> {
    let &plane = signal.planes.first()?;
    let &stride = signal.strides.first()?;
    let mut out = Vec::new();
    for x in 0..signal.width {
        let s = sample_at(plane, stride, x, 0, signal.depth)?;
        out.push(f64::from(s));
    }
    Some(out)
}

fn block_lsd(source: &[f64], distorted: &[f64]) -> Option<f64> {
    if source.len() != distorted.len() || source.is_empty() {
        return None;
    }
    let (sr, si) = vaco_tx::reference::rdft(source);
    let (dr, di) = vaco_tx::reference::rdft(distorted);
    let bins = sr.len().min(si.len()).min(dr.len()).min(di.len());
    if bins == 0 {
        return None;
    }

    let mut sum_sq = 0.0_f64;
    for k in 0..bins {
        let (Some(&sr_k), Some(&si_k), Some(&dr_k), Some(&di_k)) =
            (sr.get(k), si.get(k), dr.get(k), di.get(k))
        else {
            continue;
        };
        let source_power = (sr_k * sr_k + si_k * si_k).max(POWER_FLOOR);
        let distorted_power = (dr_k * dr_k + di_k * di_k).max(POWER_FLOOR);
        let ratio_db = 10.0 * (source_power / distorted_power).log10();
        sum_sq += ratio_db * ratio_db;
    }
    Some((sum_sq / bins as f64).sqrt())
}

impl Metric for SpectralDistance {
    fn name(&self) -> &'static str {
        "spectral-lsd"
    }

    fn score(&self, source: &Signal<'_>, distorted: &Signal<'_>) -> Result<f64, String> {
        if !geometry_matches(source, distorted) {
            return Err(format!(
                "spectral-lsd: geometry mismatch ({}x{}@{} vs {}x{}@{})",
                source.width,
                source.height,
                source.depth,
                distorted.width,
                distorted.height,
                distorted.depth
            ));
        }
        let src_samples = channel_samples(source)
            .ok_or_else(|| "spectral-lsd: could not read source samples".to_owned())?;
        let dst_samples = channel_samples(distorted)
            .ok_or_else(|| "spectral-lsd: could not read distorted samples".to_owned())?;
        if src_samples.is_empty() {
            return Err("spectral-lsd: empty signal".to_owned());
        }

        let mut total = 0.0_f64;
        let mut blocks = 0u64;
        for chunk_start in (0..src_samples.len()).step_by(BLOCK) {
            let end = (chunk_start + BLOCK).min(src_samples.len());
            let (Some(s_chunk), Some(d_chunk)) = (
                src_samples.get(chunk_start..end),
                dst_samples.get(chunk_start..end),
            ) else {
                continue;
            };
            if let Some(lsd) = block_lsd(s_chunk, d_chunk) {
                total += lsd;
                blocks += 1;
            }
        }

        if blocks == 0 {
            return Err("spectral-lsd: no complete block could be scored".to_owned());
        }
        Ok(-(total / blocks as f64))
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::SpectralDistance;
    use crate::compare::quality::{Metric, Signal};

    fn tone_samples(len: usize, freq_bins: f64) -> Vec<u8> {
        (0..len)
            .map(|i| {
                let phase = std::f64::consts::TAU * freq_bins * i as f64 / len as f64;
                let v = (phase.sin() * 100.0 + 128.0).round();
                v.clamp(0.0, 255.0) as u8
            })
            .collect()
    }

    fn signal(samples: &[u8]) -> Signal<'_> {
        Signal {
            planes: vec![samples],
            strides: vec![1],
            width: samples.len() as u32,
            height: 1,
            depth: 8,
        }
    }

    #[test]
    fn identical_signals_score_near_zero_lsd_negated() {
        let samples = tone_samples(1024, 7.0);
        let a = signal(&samples);
        let b = signal(&samples);
        let score = SpectralDistance.score(&a, &b).unwrap();
        assert!(score.abs() < 1e-6, "got {score}, expected ~0 (negated LSD)");
    }

    #[test]
    fn a_distorted_tone_scores_lower_than_a_barely_touched_one() {
        let source = tone_samples(1024, 7.0);
        let slightly_off: Vec<u8> = source.iter().map(|&v| v.saturating_add(1)).collect();
        let very_off = tone_samples(1024, 31.0); // a completely different tone

        let src = signal(&source);
        let close = signal(&slightly_off);
        let far = signal(&very_off);

        let close_score = SpectralDistance.score(&src, &close).unwrap();
        let far_score = SpectralDistance.score(&src, &far).unwrap();
        assert!(
            close_score > far_score,
            "close {close_score} should score higher (less negative) than far {far_score}"
        );
    }

    #[test]
    fn empty_signal_is_an_error() {
        let a = signal(&[]);
        let b = signal(&[]);
        assert!(SpectralDistance.score(&a, &b).is_err());
    }

    #[test]
    fn geometry_mismatch_is_an_error() {
        let a_data = tone_samples(256, 3.0);
        let b_data = tone_samples(300, 3.0);
        let a = signal(&a_data);
        let b = signal(&b_data);
        assert!(SpectralDistance.score(&a, &b).is_err());
    }
}
