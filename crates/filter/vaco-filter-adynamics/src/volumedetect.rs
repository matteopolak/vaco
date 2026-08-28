//! `volumedetect` — detect audio volume.
//!
//! `ffmpeg -h filter=volumedetect` has no options at all (confirmed:
//! `ffmpeg -h filter=volumedetect` prints no `AVOptions` section, 2026-08-23).
//! Its entire contract is the text it emits at end of stream: the reference
//! prints `max_volume: <N> dB` and `mean_volume: <N> dB` via `av_log` at
//! `AV_LOG_INFO`, plus a per-dB histogram. This crate has no `av_log`
//! equivalent, so it logs the same two lines through `tracing::info!` at
//! `flush_state` (called on end of stream via `Simple`'s adapter) instead —
//! the histogram lines are not reproduced, a documented gap, since nothing
//! downstream of this crate consumes them yet.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db};

pub const DESC: FilterDesc = FilterDesc {
    name: "volumedetect",
    description: "detect audio volume",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Default)]
struct VolumeDetect {
    peak: f64,
    sum_sq: f64,
    count: u64,
}

impl FrameFilter for VolumeDetect {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        for ch in &channels {
            for &s in ch {
                self.peak = self.peak.max(s.abs());
                self.sum_sq += s * s;
                self.count += 1;
            }
        }
        Ok(FrameOut::One(input))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        if self.count > 0 {
            let mean_sq = self.sum_sq / self.count as f64;
            tracing::info!(
                target: "vaco_filter_adynamics::volumedetect",
                "max_volume: {:.1} dB, mean_volume: {:.1} dB",
                db(self.peak),
                db(mean_sq.sqrt()),
            );
            self.count = 0;
        }
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        *self = Self::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(VolumeDetect::default())),
    }
}
