//! `apsnr` — measure Audio Peak Signal-to-Noise Ratio.
//!
//! `ffmpeg -h filter=apsnr` (2026-08-23): two inputs (`input0`/`input1`),
//! one audio output; no options besides the generic `enable=` timeline
//! (accepted, not applied — see `docs/filter/vaco-filter-aanalysis.md`).
//! `input0` is treated as the reference and passed through unchanged;
//! `input1` is the signal under test. That direction is this crate's own
//! documented convention, not a measured fact about the reference. The
//! statistic is accumulated over every channel combined rather than
//! reported per channel — a documented simplification against the
//! reference, which (per its option table's `XR` flags) exposes a running
//! value per channel plus an overall one.
//!
//! **Oracle.** `PSNR = 10*log10(peak^2 / MSE)`, `peak = 1.0` in the
//! normalized sample domain every filter in this crate decodes into. Hand
//! computable on two samples: reference `[1, 0]` against itself has
//! `MSE = 0`, i.e. infinite PSNR; against `[0, 0]` has `MSE = 0.5`, i.e.
//! `PSNR = 10*log10(1/0.5) = 10*log10(2) ≈ 3.01 dB` — arithmetic anyone can
//! check without running this filter.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, Synced};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, PairStats};

pub const DESC: FilterDesc = FilterDesc {
    name: "apsnr",
    description: "measure Audio Peak Signal-to-Noise Ratio",
    inputs: common::INPUT01_PADS,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, Default)]
struct Apsnr {
    stats: PairStats,
}

impl FrameSyncFilter for Apsnr {
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
            if let Some(psnr) = self.stats.psnr_db() {
                tracing::info!(
                    target: "vaco_filter_aanalysis::apsnr",
                    "psnr_avg: {}",
                    if psnr.is_finite() { format!("{psnr:.2}") } else { "inf".to_owned() },
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
        filter: Box::new(Synced::new(Apsnr::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::PairStats;

    /// Hand-computed, as the module doc states.
    #[test]
    fn identical_signals_are_infinite_psnr() {
        let mut s = PairStats::default();
        s.observe(1.0, 1.0);
        s.observe(0.0, 0.0);
        assert_eq!(s.psnr_db(), Some(f64::INFINITY));
    }

    #[test]
    fn ones_and_zeros_give_three_decibels() {
        let mut s = PairStats::default();
        s.observe(1.0, 0.0);
        s.observe(0.0, 0.0);
        let psnr = s.psnr_db();
        assert!(psnr.is_some(), "two samples define a PSNR");
        let psnr = psnr.unwrap_or(0.0);
        assert!((psnr - 3.0103).abs() < 1e-3, "got {psnr}");
    }
}
