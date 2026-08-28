//! `asdr` — measure Audio Signal-to-Distortion Ratio.
//!
//! `ffmpeg -h filter=asdr` (2026-08-23): two inputs (`input0`/`input1`),
//! one audio output, no options besides `enable=`. Same `input0` = reference
//! / `input1` = estimate convention as [`crate::apsnr`], same combined-
//! channel simplification; see that module's doc.
//!
//! **Oracle.** `SDR = 10*log10(||reference||^2 / ||reference - estimate||^2)`
//! — the standard source-separation-literature definition, independent of
//! any ffmpeg-specific source. Hand computable: reference `[1, 0]` against
//! estimate `[0, 0]` has distortion equal to the reference itself, so
//! `SDR = 10*log10(1/1) = 0 dB` exactly. Against a *scaled* copy,
//! `[2, 0]`, the distortion is `[1, 0]` (only half the energy), so
//! `SDR = 10*log10(1/1) = 0 dB` too — unlike [`crate::asisdr`], plain SDR is
//! *not* scale-invariant and penalises a pure gain change as if it were
//! error. That contrast between the two filters' test files, not either one
//! in isolation, is the oracle for "did we implement two different
//! formulas or copy one".

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, PairStats};

pub const DESC: FilterDesc = FilterDesc {
    name: "asdr",
    description: "measure Audio Signal-to-Distortion Ratio",
    inputs: common::INPUT01_PADS,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, Default)]
struct Asdr {
    stats: PairStats,
}

impl FrameSyncFilter for Asdr {
    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(a) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        if let Some(b) = event.get(1) {
            let (_fmt, _rate, _samples, _layout, ref_channels) = crate::sample::decode(&a)?;
            let (_fmt_b, _rate_b, _samples_b, _layout_b, est_channels) = crate::sample::decode(b)?;
            let n = ref_channels.len().min(est_channels.len());
            for i in 0..n {
                let (Some(r), Some(e)) = (ref_channels.get(i), est_channels.get(i)) else {
                    continue;
                };
                let len = r.len().min(e.len());
                for k in 0..len {
                    let rv = r.get(k).copied().unwrap_or(0.0);
                    let ev = e.get(k).copied().unwrap_or(0.0);
                    self.stats.observe(rv, ev);
                }
            }
            if let Some(sdr) = self.stats.sdr_db() {
                tracing::info!(
                    target: "vaco_filter_aanalysis::asdr",
                    "sdr_avg: {}",
                    if sdr.is_finite() { format!("{sdr:.2}") } else { "inf".to_owned() },
                );
            }
        }
        Ok(FrameOut::One(a))
    }

    fn flush_state(&mut self) {
        self.stats = PairStats::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Audio, req.instance),
        filter: Box::new(Synced::new(Asdr::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::PairStats;

    #[test]
    fn distortion_equal_to_reference_is_zero_decibels() {
        let mut s = PairStats::default();
        s.observe(1.0, 0.0);
        let sdr = s.sdr_db();
        assert!(sdr.is_some(), "one sample defines an SDR");
        let sdr = sdr.unwrap_or(0.0);
        assert!(sdr.abs() < 1e-9, "got {sdr}");
    }

    /// Plain SDR is scale-*variant*: doubling the estimate's amplitude is
    /// scored as distortion, unlike `asisdr`'s scale-invariant formula.
    #[test]
    fn a_pure_gain_change_is_not_free_under_plain_sdr() {
        let mut s = PairStats::default();
        s.observe(1.0, 2.0);
        let sdr = s.sdr_db();
        assert!(sdr.is_some(), "one sample defines an SDR");
        let sdr = sdr.unwrap_or(0.0);
        assert!(sdr.abs() < 1e-9, "got {sdr}");
        assert!(sdr < 50.0, "a scaled copy should not read as near-perfect");
    }
}
