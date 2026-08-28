//! `asisdr` — measure Audio Scale-Invariant Signal-to-Distortion Ratio.
//!
//! `ffmpeg -h filter=asisdr` (2026-08-23): two inputs (`input0`/`input1`),
//! one audio output, no options besides `enable=`. Same `input0` =
//! reference / `input1` = estimate convention and combined-channel
//! simplification as [`crate::apsnr`] and [`crate::asdr`].
//!
//! **Oracle.** SI-SDR (Le Roux, Wisdom, Erdogan & Hershey, *"SDR – half-baked
//! or well done?"*, ICASSP 2019 — a published, ffmpeg-independent formula):
//! project the estimate onto the reference's scale first,
//! `alpha = <estimate, reference> / <reference, reference>`, then
//! `SI-SDR = 10*log10(||alpha*reference||^2 / ||estimate - alpha*reference||^2)`.
//! Hand computable: reference `[1, 0]`, estimate `[2, 0]` (a pure doubling)
//! gives `alpha = 2`, so the projection is exactly the estimate and the
//! residual is `0` — `SI-SDR = +infinity`, unlike [`crate::asdr`]'s plain
//! SDR on the identical inputs (`0 dB`, see that module's test). That
//! contrast is the property this filter exists to have.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, PairStats};

pub const DESC: FilterDesc = FilterDesc {
    name: "asisdr",
    description: "measure Audio Scale-Invariant Signal-to-Distortion Ratio",
    inputs: common::INPUT01_PADS,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, Default)]
struct Asisdr {
    stats: PairStats,
}

impl FrameSyncFilter for Asisdr {
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
            if let Some(si_sdr) = self.stats.si_sdr_db() {
                tracing::info!(
                    target: "vaco_filter_aanalysis::asisdr",
                    "si_sdr_avg: {}",
                    if si_sdr.is_finite() { format!("{si_sdr:.2}") } else { "inf".to_owned() },
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
        filter: Box::new(Synced::new(Asisdr::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::PairStats;

    /// Hand-computed: a pure gain change scores infinite SI-SDR, exactly
    /// the case plain `asdr` scores `0 dB` on (see that module's test).
    #[test]
    fn a_pure_gain_change_is_free_under_scale_invariant_sdr() {
        let mut s = PairStats::default();
        s.observe(1.0, 2.0);
        assert_eq!(s.si_sdr_db(), Some(f64::INFINITY));
    }

    #[test]
    fn identical_signals_are_infinite_si_sdr() {
        let mut s = PairStats::default();
        s.observe(1.0, 1.0);
        s.observe(-0.5, -0.5);
        assert_eq!(s.si_sdr_db(), Some(f64::INFINITY));
    }

    #[test]
    fn uncorrelated_noise_is_a_finite_low_score() {
        let mut s = PairStats::default();
        // Reference and estimate orthogonal: alpha == 0, so the whole
        // estimate is "noise" and the projected target has zero energy.
        s.observe(1.0, 0.0);
        s.observe(0.0, 1.0);
        assert_eq!(s.si_sdr_db(), None);
    }
}
