//! `astats` — show time domain statistics about audio frames.
//!
//! `ffmpeg -h filter=astats` (2026-08-23) can measure 26 named parameters
//! (`measure_perchannel`/`measure_overall` flag sets), per channel and
//! overall, printed via `av_log` (or injected as frame metadata when
//! `metadata=true`). This crate implements nine of the twenty-six —
//! `DC_offset`, `Min_level`, `Max_level`, `Peak_level` (dB), `RMS_level`
//! (dB), `Crest_factor`, `Number_of_samples`, `Zero_crossings`,
//! `Zero_crossings_rate` — computed cumulatively (matching `reset=0`, the
//! default: one report at end of stream) and logged with `tracing::info!`
//! rather than `av_log`. `length` (the windowed short-term stats),
//! `measure_perchannel`/`measure_overall`'s flag selection, `reset` (a
//! non-zero periodic reset), and `metadata` (frame side-data injection) are
//! accepted and not applied — always full-stream cumulative stats, always
//! logged, never injected as metadata.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db};

pub const DESC: FilterDesc = FilterDesc {
    name: "astats",
    description: "show time domain statistics about audio frames",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Default)]
struct ChannelStats {
    sum: f64,
    sum_sq: f64,
    min: f64,
    max: f64,
    peak: f64,
    zero_crossings: u64,
    last_sign: Option<bool>,
    n: u64,
}

impl ChannelStats {
    fn observe(&mut self, x: f64) {
        self.sum += x;
        self.sum_sq += x * x;
        self.min = if self.n == 0 { x } else { self.min.min(x) };
        self.max = if self.n == 0 { x } else { self.max.max(x) };
        self.peak = self.peak.max(x.abs());
        let sign = x >= 0.0;
        if let Some(last) = self.last_sign
            && last != sign
        {
            self.zero_crossings += 1;
        }
        self.last_sign = Some(sign);
        self.n += 1;
    }

    fn report(&self, sample_rate: f64, index: usize) {
        if self.n == 0 {
            return;
        }
        let n = self.n as f64;
        let mean = self.sum / n;
        let rms = (self.sum_sq / n).sqrt();
        let crest = if rms > 1e-12 { self.peak / rms } else { 0.0 };
        let zc_rate = if sample_rate > 0.0 {
            self.zero_crossings as f64 * sample_rate / n
        } else {
            0.0
        };
        tracing::info!(
            target: "vaco_filter_adynamics::astats",
            "Channel {index}: DC_offset={mean:.6} Min_level={:.6} Max_level={:.6} \
             Peak_level={:.3}dB RMS_level={:.3}dB Crest_factor={crest:.6} \
             Number_of_samples={} Zero_crossings={} Zero_crossings_rate={zc_rate:.6}",
            self.min,
            self.max,
            db(self.peak),
            db(rms),
            self.n,
            self.zero_crossings,
        );
    }
}

#[derive(Debug, Clone, Default)]
struct Astats {
    channels: Vec<ChannelStats>,
    sample_rate: f64,
}

impl FrameFilter for Astats {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let vaco_filter_core::LinkFormat::Audio { sample_rate, .. } = ctx.link(0) {
            self.sample_rate = f64::from(*sample_rate);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        if self.channels.len() != channels.len() {
            self.channels = vec![ChannelStats::default(); channels.len()];
        }
        for (stats, ch) in self.channels.iter_mut().zip(channels.iter()) {
            for &s in ch {
                stats.observe(s);
            }
        }
        Ok(FrameOut::One(input))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        for (i, stats) in self.channels.iter().enumerate() {
            stats.report(self.sample_rate, i);
        }
        self.channels.clear();
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.channels.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Astats::default())),
    }
}
