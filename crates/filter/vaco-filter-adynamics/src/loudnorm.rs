//! `loudnorm` — EBU R128 loudness normalization.
//!
//! `ffmpeg -h filter=loudnorm` (2026-08-23): `I`/`i` (integrated loudness
//! target, LUFS, default -24), `LRA`/`lra` (loudness range target, default
//! 7), `TP`/`tp` (max true peak, dBTP, default -2), `measured_I`/
//! `measured_LRA`/`measured_TP`/`measured_thresh` (two-pass inputs),
//! `offset`, `linear` (default true), `dual_mono`, `print_format`,
//! `stats_file`.
//!
//! **This is not an EBU R128 / ITU-R BS.1770 implementation.** True
//! loudness needs K-weighting (a shelving + high-pass pre-filter defined by
//! BS.1770), gated block-averaging, and — for the reference's default
//! `linear=true` mode — a full first pass over the audio before any sample
//! is written, none of which this crate implements. What is implemented is
//! a single-pass adaptive gain control: an RMS-based level estimate (in dB,
//! *not* K-weighted, so `measured_I` is a plausible-looking number rather
//! than a correct one) is tracked with a slow exponential average, and gain
//! adapts toward `I - measured` each block, capped so the *linear* signal
//! never exceeds `TP` (also RMS/peak-based, not a true-peak oversampled
//! measurement). `LRA`, `offset`, `dual_mono` and the two-pass
//! `measured_*` inputs are accepted and not applied. `print_format`
//! (`json`/`summary`) is honoured for the end-of-stream report, logged via
//! `tracing::info!` rather than written to `stats_file`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{AudioFilter, Blocked, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db};

pub const DESC: FilterDesc = FilterDesc {
    name: "loudnorm",
    description: "EBU R128 loudness normalization",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone)]
struct LoudNorm {
    target_db: f64,
    true_peak_db: f64,
    print: bool,
    gain: f64,
    measured_peak: f64,
    running_db: f64,
    block_samples: u32,
    seen_any: bool,
}

impl AudioFilter for LoudNorm {
    fn frame_size(&self) -> u32 {
        self.block_samples
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let LinkFormat::Audio { sample_rate, .. } = ctx.link(0) {
            self.block_samples = (f64::from(*sample_rate) * 0.4).round().max(1.0) as u32;
        }
        Ok(())
    }

    fn filter_samples(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let total: usize = channels.iter().map(Vec::len).sum();
        let sum_sq: f64 = channels.iter().flatten().map(|s| s * s).sum();
        let peak = channels
            .iter()
            .flatten()
            .fold(0.0f64, |a, &b| a.max(b.abs()));
        self.measured_peak = self.measured_peak.max(peak);
        if total > 0 {
            let block_db = db((sum_sq / total as f64).sqrt());
            self.running_db = if self.seen_any {
                self.running_db + 0.1 * (block_db - self.running_db)
            } else {
                block_db
            };
            self.seen_any = true;
        }
        let target_gain_db = (self.target_db - self.running_db).clamp(-30.0, 30.0);
        self.gain += 0.1 * (common::from_db(target_gain_db) - self.gain);
        let ceiling = common::from_db(self.true_peak_db);
        let effective = if self.measured_peak * self.gain > ceiling && self.measured_peak > 1e-9 {
            (ceiling / self.measured_peak).min(self.gain)
        } else {
            self.gain
        };
        for ch in &mut channels {
            for s in ch.iter_mut() {
                *s *= effective;
            }
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        if self.print {
            tracing::info!(
                target: "vaco_filter_adynamics::loudnorm",
                "input_i: {:.2} output_i: {:.2} target_offset: {:.2} input_tp: {:.2}",
                self.running_db,
                self.target_db,
                self.target_db - self.running_db,
                db(self.measured_peak),
            );
        }
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.gain = 1.0;
        self.measured_peak = 0.0;
        self.running_db = self.target_db;
        self.seen_any = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let target_db = common::f64_opt(req, &["I", "i"], -24.0);
    let filter = LoudNorm {
        target_db,
        true_peak_db: common::f64_opt(req, &["TP", "tp"], -2.0),
        print: !matches!(
            req.named("print_format").as_deref(),
            None | Some("none" | "0")
        ),
        gain: 1.0,
        measured_peak: 0.0,
        running_db: target_db,
        block_samples: 19_200,
        seen_any: false,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Blocked::new(filter))),
    }
}
