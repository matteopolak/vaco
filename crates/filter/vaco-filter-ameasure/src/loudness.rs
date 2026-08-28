//! The gated BS.1770-4 loudness scanner: K-weighting, 100 ms sub-blocks,
//! the 400 ms/75%-overlap gating window for Integrated Loudness, and the
//! 3 s window for Loudness Range. One definition (D19), shared by
//! [`crate::ebur128`] (which reports it directly) and
//! [`crate::replaygain`] (which subtracts it from a -18 LUFS target).
//!
//! # The gating algorithm
//!
//! Per ITU-R BS.1770-4 §5 / EBU Tech 3341: split the K-weighted signal into
//! 400 ms blocks, 100 ms apart (75% overlap); each block's loudness is
//! `-0.691 + 10*log10(z)` where `z` is the channel-weighted mean square
//! ([`crate::kweight::loudness_from_z`]). Stage 1 (absolute gate): discard
//! blocks below -70 LUFS. Stage 2 (relative gate): compute the average `z`
//! of the surviving blocks, map it to a loudness, subtract 10 LU, and
//! discard blocks below *that*. Integrated Loudness is the loudness of the
//! average `z` of what remains.
//!
//! Loudness Range (EBU Tech 3342) is the same shape at a 3 s window: gate
//! at -70 LUFS absolute and -20 LU relative to the (energy-averaged)
//! surviving short-term loudness values, then report the spread between
//! their 10th and 95th percentiles.
//!
//! **What is not implemented**: true peak (a 4x-oversampled peak per
//! BS.1770-4 Annex 2). This crate reports the plain sample peak instead —
//! the same documented simplification `vaco-filter-adynamics::loudnorm`
//! already makes for the same reason (no oversampling filter here yet).

use std::collections::VecDeque;

use crate::kweight::{self, KWeight};

/// 100 ms — both the gating hop and the sub-block granularity everything
/// else is built from.
const SUBBLOCK_SECONDS: f64 = 0.1;
/// 400 ms / 100 ms.
const MOMENTARY_SUBBLOCKS: usize = 4;
/// 3 s / 100 ms.
const SHORT_TERM_SUBBLOCKS: usize = 30;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const RELATIVE_GATE_OFFSET_LU: f64 = -10.0;
const LRA_RELATIVE_GATE_OFFSET_LU: f64 = -20.0;

#[derive(Debug)]
pub(crate) struct LoudnessMeter {
    kw: Vec<KWeight>,
    weights: Vec<f64>,
    sample_rate: f64,
    subblock_len: usize,
    cur_sumsq: Vec<f64>,
    cur_count: usize,
    subblock_ring: VecDeque<f64>,
    integrated_blocks: Vec<f64>,
    short_term_loudness: Vec<f64>,
    sample_peak: f64,
}

impl LoudnessMeter {
    pub(crate) fn new(sample_rate: f64) -> Self {
        let sample_rate = sample_rate.max(1.0);
        Self {
            kw: Vec::new(),
            weights: Vec::new(),
            sample_rate,
            subblock_len: (sample_rate * SUBBLOCK_SECONDS).round().max(1.0) as usize,
            cur_sumsq: Vec::new(),
            cur_count: 0,
            subblock_ring: VecDeque::new(),
            integrated_blocks: Vec::new(),
            short_term_loudness: Vec::new(),
            sample_peak: 0.0,
        }
    }

    /// (Re)establish per-channel state from the negotiated layout. Safe to
    /// call again if the channel count changes mid-stream — it resets the
    /// scan, which is the only sane response to the geometry changing under
    /// it.
    pub(crate) fn configure(&mut self, channels: usize, layout: &vaco_chlayout::ChannelLayout) {
        if self.kw.len() == channels {
            return;
        }
        self.kw = (0..channels).map(|_| KWeight::new(self.sample_rate)).collect();
        self.weights = (0..channels)
            .map(|i| {
                let ch = u32::try_from(i).ok().and_then(|i| layout.channel_at(i));
                kweight::channel_weight(ch)
            })
            .collect();
        self.cur_sumsq = vec![0.0; channels];
        self.cur_count = 0;
    }

    /// Feed one frame's decoded channel data.
    pub(crate) fn feed(&mut self, channels: &[Vec<f64>]) {
        let len = channels.iter().map(Vec::len).min().unwrap_or(0);
        for i in 0..len {
            for (c, ch) in channels.iter().enumerate() {
                let Some(&x) = ch.get(i) else { continue };
                self.sample_peak = self.sample_peak.max(x.abs());
                let Some(kw) = self.kw.get_mut(c) else { continue };
                let y = kw.process(x);
                if let Some(slot) = self.cur_sumsq.get_mut(c) {
                    *slot += y * y;
                }
            }
            self.cur_count += 1;
            if self.cur_count >= self.subblock_len {
                self.finish_subblock();
            }
        }
    }

    fn finish_subblock(&mut self) {
        if self.cur_count == 0 {
            return;
        }
        let weighted: f64 = self
            .cur_sumsq
            .iter()
            .zip(self.weights.iter())
            .map(|(&s, &w)| s * w)
            .sum();
        self.subblock_ring.push_back(weighted);
        while self.subblock_ring.len() > SHORT_TERM_SUBBLOCKS {
            self.subblock_ring.pop_front();
        }
        let n = self.subblock_ring.len();
        let denom = self.subblock_len.max(1) as f64;

        if n >= MOMENTARY_SUBBLOCKS {
            let sum: f64 = self
                .subblock_ring
                .iter()
                .rev()
                .take(MOMENTARY_SUBBLOCKS)
                .sum();
            let z = sum / (MOMENTARY_SUBBLOCKS as f64 * denom);
            self.integrated_blocks.push(z);
        }
        if n >= SHORT_TERM_SUBBLOCKS {
            let sum: f64 = self.subblock_ring.iter().sum();
            let z = sum / (SHORT_TERM_SUBBLOCKS as f64 * denom);
            self.short_term_loudness.push(kweight::loudness_from_z(z));
        }

        for slot in &mut self.cur_sumsq {
            *slot = 0.0;
        }
        self.cur_count = 0;
    }

    /// Integrated Loudness (LUFS), per the two-stage gate. `None` when the
    /// stream never produced one complete 400 ms block.
    pub(crate) fn integrated_lufs(&self) -> Option<f64> {
        gated_integrated(&self.integrated_blocks)
    }

    /// Loudness Range (LU), per EBU Tech 3342. `0.0` when there is not
    /// enough data to define a range (matches the reference's own
    /// behaviour on short input, observed 2026-08-23).
    pub(crate) fn loudness_range_lu(&self) -> f64 {
        loudness_range(&self.short_term_loudness)
    }

    pub(crate) fn sample_peak_linear(&self) -> f64 {
        self.sample_peak
    }

    /// Discard everything accumulated so far without forgetting the
    /// negotiated channel count/weights — what a seek needs, and cheaper
    /// than rebuilding the whole meter.
    pub(crate) fn reset(&mut self) {
        for kw in &mut self.kw {
            kw.reset();
        }
        for slot in &mut self.cur_sumsq {
            *slot = 0.0;
        }
        self.cur_count = 0;
        self.subblock_ring.clear();
        self.integrated_blocks.clear();
        self.short_term_loudness.clear();
        self.sample_peak = 0.0;
    }
}

/// The two-stage BS.1770-4 gate over a track's 400 ms block loudnesses,
/// given as pre-divided mean squares `z` (one per block).
fn gated_integrated(blocks: &[f64]) -> Option<f64> {
    let stage1: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|&z| kweight::loudness_from_z(z) > ABSOLUTE_GATE_LUFS)
        .collect();
    if stage1.is_empty() {
        return None;
    }
    let mean1 = stage1.iter().sum::<f64>() / stage1.len() as f64;
    let relative_gate = kweight::loudness_from_z(mean1) + RELATIVE_GATE_OFFSET_LU;
    let stage2: Vec<f64> = stage1
        .into_iter()
        .filter(|&z| kweight::loudness_from_z(z) > relative_gate)
        .collect();
    if stage2.is_empty() {
        return None;
    }
    let mean2 = stage2.iter().sum::<f64>() / stage2.len() as f64;
    Some(kweight::loudness_from_z(mean2))
}

/// EBU Tech 3342 Loudness Range: gate the short-term loudness values, then
/// report the spread between their 10th and 95th percentiles.
fn loudness_range(short_term: &[f64]) -> f64 {
    let stage1: Vec<f64> = short_term
        .iter()
        .copied()
        .filter(|&l| l > ABSOLUTE_GATE_LUFS)
        .collect();
    if stage1.is_empty() {
        return 0.0;
    }
    // Energy-average the gated loudness values to find the relative gate,
    // matching the same z-domain average the integrated-loudness gate uses.
    let mean_z = stage1
        .iter()
        .map(|&l| 10f64.powf((l + 0.691) / 10.0))
        .sum::<f64>()
        / stage1.len() as f64;
    let relative_gate = kweight::loudness_from_z(mean_z) + LRA_RELATIVE_GATE_OFFSET_LU;
    let mut stage2: Vec<f64> = stage1.into_iter().filter(|&l| l > relative_gate).collect();
    if stage2.len() < 2 {
        return 0.0;
    }
    stage2.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p10 = percentile(&stage2, 0.10);
    let p95 = percentile(&stage2, 0.95);
    (p95 - p10).max(0.0)
}

/// Linear-interpolated percentile of an already-sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted.first().copied().unwrap_or(0.0);
    }
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor().max(0.0) as usize;
    let hi = rank.ceil().max(0.0) as usize;
    let lo_v = sorted.get(lo).copied().unwrap_or(0.0);
    let hi_v = sorted.get(hi).copied().unwrap_or(lo_v);
    let frac = rank - rank.floor();
    lo_v + (hi_v - lo_v) * frac
}

#[cfg(test)]
mod tests {
    use super::{gated_integrated, loudness_range, percentile};
    use crate::kweight;

    #[test]
    fn absolute_gate_drops_quiet_blocks_entirely() {
        // A -80 LUFS block (well below the -70 LUFS absolute gate) mixed
        // with -20 LUFS blocks: the quiet block must not exist as far as
        // the integrated value is concerned.
        let quiet_z = 10f64.powf((-80.0 + 0.691) / 10.0);
        let loud_z = 10f64.powf((-20.0 + 0.691) / 10.0);
        let with_quiet = gated_integrated(&[quiet_z, loud_z, loud_z, loud_z]);
        let without_quiet = gated_integrated(&[loud_z, loud_z, loud_z]);
        assert!((with_quiet.unwrap_or(f64::NAN) - without_quiet.unwrap_or(f64::NAN)).abs() < 1e-9);
    }

    #[test]
    fn no_blocks_is_not_a_defined_integrated_loudness() {
        assert_eq!(gated_integrated(&[]), None);
    }

    #[test]
    fn uniform_loudness_has_zero_range() {
        let l = kweight::loudness_from_z(0.1);
        let range = loudness_range(&vec![l; 40]);
        assert!(range.abs() < 1e-6, "got {range}");
    }

    #[test]
    fn percentile_of_a_sorted_run_is_linearly_interpolated() {
        let v = vec![0.0, 10.0, 20.0, 30.0, 40.0];
        assert!((percentile(&v, 0.0) - 0.0).abs() < 1e-9);
        assert!((percentile(&v, 1.0) - 40.0).abs() < 1e-9);
        assert!((percentile(&v, 0.5) - 20.0).abs() < 1e-9);
    }
}
