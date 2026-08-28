//! `replaygain` — `ReplayGain` scanner.
//!
//! `ffmpeg -h filter=replaygain` (2026-08-23): no input options; two
//! read-only report values, `track_gain` (dB) and `track_peak` (linear
//! amplitude, *not* dB — confirmed by running it: `track_peak = 0.088367`
//! against a signal whose peak was well under 1.0, not a dB figure).
//!
//! `ReplayGain` 2.0 (the version every modern `ReplayGain`-writing tool
//! targets, and a published, ffmpeg-independent specification) defines
//! `track_gain = target_lufs - measured_integrated_loudness`, target
//! `-18 LUFS`, using the same ITU-R BS.1770 loudness this crate's
//! `ebur128` reports — reused here via [`crate::loudness::LoudnessMeter`]
//! rather than re-measured a second way (D19: one loudness scanner).
//! `track_peak` is the plain linear sample peak, per the RG2.0 spec (peak
//! is used for clipping-prevention headroom, which needs amplitude, not
//! dB).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::loudness::LoudnessMeter;

/// `ReplayGain` 2.0's reference level.
const TARGET_LUFS: f64 = -18.0;

pub const DESC: FilterDesc = FilterDesc {
    name: "replaygain",
    description: "ReplayGain scanner",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
struct ReplayGain {
    meter: LoudnessMeter,
    sample_rate: f64,
}

impl FrameFilter for ReplayGain {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
            self.meter = LoudnessMeter::new(self.sample_rate);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, layout, channels) = crate::sample::decode(&input)?;
        self.meter.configure(channels.len(), &layout);
        self.meter.feed(&channels);
        Ok(FrameOut::One(input))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let measured = self.meter.integrated_lufs().unwrap_or(TARGET_LUFS);
        let track_gain = TARGET_LUFS - measured;
        let track_peak = self.meter.sample_peak_linear();
        tracing::info!(
            target: "vaco_filter_aanalysis::replaygain",
            "track_gain = {track_gain:+.2} dB",
        );
        tracing::info!(
            target: "vaco_filter_aanalysis::replaygain",
            "track_peak = {track_peak:.6}",
        );
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.meter.reset();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    let filter = ReplayGain {
        meter: LoudnessMeter::new(48_000.0),
        sample_rate: 48_000.0,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::TARGET_LUFS;
    use crate::loudness::LoudnessMeter;
    use vaco_chlayout::ChannelLayout;

    /// A track already measured at the -18 LUFS target needs (close to)
    /// zero gain — checked against the target constant directly, not by
    /// re-running `ReplayGain::flush`.
    #[test]
    fn a_track_at_target_loudness_needs_no_gain() {
        let fs = 48_000.0;
        let mut meter = LoudnessMeter::new(fs);
        meter.configure(1, &ChannelLayout::MONO);
        // -18 LUFS unweighted corresponds to mean square z = 10^((-18+0.691)/10).
        let z = 10f64.powf((TARGET_LUFS + 0.691) / 10.0);
        let amplitude = (2.0 * z).sqrt();
        let n = (fs * 4.0) as usize;
        let mut buf = vec![0.0f64; n];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = amplitude * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / fs).sin();
        }
        meter.feed(&[buf]);
        let measured = meter.integrated_lufs().unwrap_or(TARGET_LUFS);
        let gain = TARGET_LUFS - measured;
        assert!(gain.abs() < 1.0, "expected close to 0 dB gain, got {gain}");
    }
}
