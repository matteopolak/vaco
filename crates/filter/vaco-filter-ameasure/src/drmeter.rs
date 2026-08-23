//! `drmeter` — measure audio dynamic range.
//!
//! `ffmpeg -h filter=drmeter` (2026-08-23): `length` (seconds, from 0.01 to
//! 10, default 3) — the block size the "DR" (dynamic range) algorithm
//! divides the stream into.
//!
//! # The algorithm
//!
//! This is the Pleasurize Music Foundation / "TT Dynamic Range Meter"
//! algorithm, published independently of any ffmpeg-specific source (it
//! predates the filter and is the industry-standard "DR" number printed on
//! mastering reports): split each channel into `length`-second blocks; for
//! each block compute `rms = sqrt(2 * mean(x^2))` (the meter's own
//! convention — the factor of 2 makes a full-scale sine block read `1.0`,
//! matching its peak, rather than its usual `0.707`) and `peak = max(|x|)`;
//! take the loudest 20% of blocks by `rms` (rounded up, at least one) and
//! combine them as a quadratic mean (`rms_top20`); take the **second**
//! largest per-block peak across the channel (`peak_2nd` — using the
//! runner-up rather than the single loudest block is what keeps one
//! transient from dominating the denominator); `DR = -20*log10(rms_top20 /
//! peak_2nd)`.
//!
//! **Oracle.** This is a closed form: for a pure full-scale sine that fills
//! whole blocks, every block's `rms` is `1.0` (by the `sqrt(2)` convention
//! above) and every block's `peak` is `1.0`, so `DR = 0` exactly — a
//! property this module's test checks without re-running `DrMeter` itself.
//! At *equal* sustained loudness (identical per-block RMS, so the top-20%
//! selection is not diluted by unrelated quiet blocks — a confound a first
//! version of this test ran into with a single-outlier-among-silence case),
//! a higher peak-to-RMS crest factor must read a larger DR: an
//! uncompressed track's occasional transients well above its sustained
//! level score higher than a heavily limited one whose peaks are squashed
//! down near that level — the qualitative direction the metric exists to
//! capture. Exact agreement with the reference's own edge-case behaviour
//! (it prints `nan` for some short inputs, observed 2026-08-23) is not
//! claimed — a documented gap.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "drmeter",
    description: "measure audio dynamic range",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// One channel's block accumulator plus its completed-block history.
#[derive(Debug, Clone, Default)]
struct ChannelBlocks {
    sum_sq: f64,
    peak: f64,
    n: usize,
    rms_history: Vec<f64>,
    peak_history: Vec<f64>,
}

impl ChannelBlocks {
    fn observe(&mut self, x: f64, block_len: usize) {
        self.sum_sq += x * x;
        self.peak = self.peak.max(x.abs());
        self.n += 1;
        if self.n >= block_len.max(1) {
            self.finish_block();
        }
    }

    fn finish_block(&mut self) {
        if self.n == 0 {
            return;
        }
        let mean_sq = self.sum_sq / self.n as f64;
        self.rms_history.push((2.0 * mean_sq).sqrt());
        self.peak_history.push(self.peak);
        self.sum_sq = 0.0;
        self.peak = 0.0;
        self.n = 0;
    }

    /// `DR` for this channel, per the module doc's formula. `None` when
    /// there is not enough data to define it (no completed blocks, or a
    /// zero denominator).
    fn dr(&self) -> Option<f64> {
        if self.rms_history.is_empty() {
            return None;
        }
        let take = self
            .rms_history
            .len()
            .div_ceil(5)
            .max(1)
            .min(self.rms_history.len());
        let mut rms_sorted = self.rms_history.clone();
        rms_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let top: Vec<f64> = rms_sorted.into_iter().take(take).collect();
        let quad_mean_sq = top.iter().map(|v| v * v).sum::<f64>() / top.len() as f64;
        let rms_top20 = quad_mean_sq.sqrt();

        let mut peaks_sorted = self.peak_history.clone();
        peaks_sorted.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let peak_2nd = peaks_sorted
            .get(1)
            .copied()
            .or_else(|| peaks_sorted.first().copied())?;

        if rms_top20 <= 0.0 || peak_2nd <= 0.0 {
            return None;
        }
        Some(-20.0 * (rms_top20 / peak_2nd).log10())
    }
}

#[derive(Debug, Clone, Default)]
struct DrMeter {
    length_s: f64,
    sample_rate: f64,
    channels: Vec<ChannelBlocks>,
}

impl FrameFilter for DrMeter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        if self.channels.len() != channels.len() {
            self.channels.resize(channels.len(), ChannelBlocks::default());
        }
        let block_len = ((self.length_s * self.sample_rate).round().max(1.0)) as usize;
        for (acc, ch) in self.channels.iter_mut().zip(channels.iter()) {
            for &s in ch {
                acc.observe(s, block_len);
            }
        }
        Ok(FrameOut::One(input))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let mut total = 0.0;
        let mut count = 0u32;
        for (i, acc) in self.channels.iter_mut().enumerate() {
            acc.finish_block();
            let dr = acc.dr();
            tracing::info!(
                target: "vaco_filter_ameasure::drmeter",
                "Channel {}: DR: {}",
                i + 1,
                dr.map_or_else(|| "nan".to_owned(), |v| format!("{v:.1}")),
            );
            if let Some(v) = dr {
                total += v;
                count += 1;
            }
        }
        let overall = if count > 0 {
            format!("{:.1}", total / f64::from(count))
        } else {
            "nan".to_owned()
        };
        tracing::info!(
            target: "vaco_filter_ameasure::drmeter",
            "Overall DR: {overall}",
        );
        self.channels.clear();
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.channels.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = DrMeter {
        length_s: common::f64_opt(req, &["length"], 3.0),
        sample_rate: 48_000.0,
        channels: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::ChannelBlocks;

    /// A full-scale sine that fills whole blocks reads `DR == 0` exactly:
    /// every block's `rms` (with the meter's `sqrt(2)` convention) and
    /// `peak` are both `1.0`, so the ratio in the formula is `1.0` and
    /// `-20*log10(1.0) == 0`. This is the formula's own fixed point, not a
    /// number read off `DrMeter::flush`.
    #[test]
    fn full_scale_uniform_blocks_have_zero_dynamic_range() {
        let mut acc = ChannelBlocks::default();
        // Five blocks, each with rms=1/sqrt(2) (i.e. sum_sq/n = 0.5) and
        // peak=1.0 — exactly a full-scale sine's per-block statistics.
        for _ in 0..5 {
            acc.sum_sq = 0.5 * 1000.0;
            acc.peak = 1.0;
            acc.n = 1000;
            acc.finish_block();
        }
        let dr = acc.dr();
        assert!(dr.is_some(), "five blocks define a DR value");
        let dr = dr.unwrap_or(0.0);
        assert!(dr.abs() < 1e-9, "expected DR == 0, got {dr}");
    }

    /// The property the meter exists to capture: for the *same* sustained
    /// loudness (every block's RMS is identical, so the top-20% selection
    /// is not diluted by unrelated quiet blocks — the confound the
    /// algorithm's "single outlier" case above ran into), a higher peak-to-
    /// RMS crest factor (an uncompressed track's occasional transients
    /// poking well above its sustained level) must read a *larger* DR than
    /// a heavily limited one (peaks squashed down near the sustained
    /// level).
    #[test]
    fn higher_crest_factor_at_equal_loudness_reads_higher_dynamic_range() {
        let build = |peak: f64| {
            let mut acc = ChannelBlocks::default();
            for _ in 0..10 {
                acc.sum_sq = 0.01 * 1000.0; // identical sustained RMS in every block
                acc.peak = peak;
                acc.n = 1000;
                acc.finish_block();
            }
            let dr = acc.dr();
            assert!(dr.is_some(), "ten blocks define a DR value");
            dr.unwrap_or(0.0)
        };
        let dynamic = build(0.9); // occasional transient well above the sustained level
        let limited = build(0.15); // peak barely above the sustained level
        assert!(
            dynamic > limited,
            "expected the higher-crest-factor signal to read a higher DR: \
             dynamic={dynamic} limited={limited}"
        );
        assert!(dynamic > 10.0, "expected a clearly large DR, got {dynamic}");
        assert!(limited < 3.0, "expected a clearly small DR, got {limited}");
    }

    #[test]
    fn no_blocks_is_not_a_defined_dr() {
        let acc = ChannelBlocks::default();
        assert_eq!(acc.dr(), None);
    }
}
