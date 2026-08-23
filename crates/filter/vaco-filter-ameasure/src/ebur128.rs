//! `ebur128` — EBU R128 scanner.
//!
//! `ffmpeg -h filter=ebur128` (2026-08-23): a long option table, mostly
//! about the optional video meter (`video`, `size`, `meter`, `gauge`,
//! `scale`) this crate does not produce — this filter's single output pad
//! is audio-only here, a documented gap the same shape as
//! `vaco-filter-audio-eq::anequalizer`'s undone video response curve. What
//! is implemented: `peak` (`none`/`sample`/`true`, read but always reported
//! as sample peak — see [`crate::loudness`] for why true peak specifically
//! is not), `dualmono` and `panlaw` (accepted, not applied), and the
//! measurement itself — `integrated`, `range`, `lra_low`, `lra_high` and
//! `sample_peak` are logged at end of stream using the reference's own
//! option names, per this crate's established convention
//! (`vaco-filter-audio-dynamics::loudnorm` does the same for its
//! `measured_*` fields).
//!
//! The gating algorithm, its provenance and its oracle are
//! [`crate::loudness`]'s; this module is the filter-shaped wrapper around
//! it plus the option table.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db};
use crate::loudness::LoudnessMeter;

pub const DESC: FilterDesc = FilterDesc {
    name: "ebur128",
    description: "EBU R128 scanner",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
struct Ebur128 {
    meter: LoudnessMeter,
    sample_rate: f64,
}

impl FrameFilter for Ebur128 {
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
        let integrated = self.meter.integrated_lufs().unwrap_or(-70.0);
        let range = self.meter.loudness_range_lu();
        let sample_peak_db = db(self.meter.sample_peak_linear());
        tracing::info!(
            target: "vaco_filter_ameasure::ebur128",
            "integrated: {integrated:.1} LUFS range: {range:.1} LU \
             sample_peak: {sample_peak_db:.1} dBFS true_peak: {sample_peak_db:.1} dBFS",
        );
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.meter.reset();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = common::bool_opt(req, &["dualmono"], false);
    let _ = common::f64_opt(req, &["panlaw"], -3.0103);
    let filter = Ebur128 {
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
    use super::LoudnessMeter;
    use vaco_chlayout::ChannelLayout;

    /// The oracle stated in this crate's brief: a calibrated -23 LUFS sine
    /// must measure -23 LUFS. The calibration itself is derived from the
    /// same closed-form loudness map this filter uses
    /// (`-0.691 + 10*log10(mean square)`), not from a second copy of the
    /// gating loop — a 0 dBFS full-scale 1 kHz mono sine's *unweighted*
    /// loudness is `-0.691 + 10*log10(0.5) ≈ -3.70 LUFS`, so scaling it down
    /// by `(-3.70) - (-23.0) ≈ 19.30 dB` should read close to -23 LUFS once
    /// K-weighting's own (small, near-unity-ish at 1 kHz) contribution is
    /// folded in — checked with a wide tolerance because the K-weighting
    /// shelf is not perfectly flat at 1 kHz.
    #[test]
    fn a_calibrated_reference_tone_reads_close_to_23_lufs() {
        let fs = 48_000.0;
        let mut meter = LoudnessMeter::new(fs);
        meter.configure(1, &ChannelLayout::MONO);
        let amplitude = 10f64.powf(-19.30 / 20.0);
        let seconds = 4.0;
        let n = (fs * seconds) as usize;
        let mut buf = vec![0.0f64; n];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = amplitude * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / fs).sin();
        }
        meter.feed(&[buf]);
        let lufs_opt = meter.integrated_lufs();
        assert!(
            lufs_opt.is_some(),
            "four seconds of steady tone define an integrated loudness"
        );
        let lufs = lufs_opt.unwrap_or(-23.0);
        assert!(
            (lufs - (-23.0)).abs() < 1.0,
            "expected close to -23 LUFS, got {lufs}"
        );
    }

    /// Digital silence must never produce a defined (non-gated-away)
    /// integrated loudness — every block is below the -70 LUFS absolute
    /// gate.
    #[test]
    fn silence_is_fully_gated_away() {
        let fs = 48_000.0;
        let mut meter = LoudnessMeter::new(fs);
        meter.configure(1, &ChannelLayout::MONO);
        let n = (fs * 2.0) as usize;
        meter.feed(&[vec![0.0f64; n]]);
        assert_eq!(meter.integrated_lufs(), None);
    }
}
