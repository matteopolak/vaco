//! `silencedetect` — detect silence.
//!
//! `ffmpeg -h filter=silencedetect` (2026-08-23): `noise`/`n` (linear
//! amplitude threshold, default 0.001), `duration`/`d` (seconds, default
//! 2), `mono`/`m` (check each channel separately, default false). The
//! reference prints `silence_start: <t>` and, on the matching end,
//! `silence_end: <t> | silence_duration: <d>` via `av_log`; this crate logs
//! the same two events through `tracing::info!` (see `volumedetect`'s doc
//! for why `tracing` stands in for `av_log` here).
//!
//! `mono=false` (the default) is implemented as "every channel's peak is
//! below the threshold"; `mono=true` — independent per-channel timelines —
//! is accepted but treated the same as `mono=false`, a documented gap.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "silencedetect",
    description: "detect silence",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone)]
struct SilenceDetect {
    noise: f64,
    min_duration_s: f64,
    sample_rate: f64,
    in_silence: bool,
    run_samples: u64,
    silence_start_s: f64,
    reported: bool,
    total_samples: u64,
}

impl FrameFilter for SilenceDetect {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let vaco_filter_core::LinkFormat::Audio { sample_rate, .. } = ctx.link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        let n = channels.iter().map(Vec::len).max().unwrap_or(0);
        for i in 0..n {
            let peak = channels
                .iter()
                .filter_map(|c| c.get(i))
                .fold(0.0f64, |a, &b| a.max(b.abs()));
            let silent_sample = peak <= self.noise;
            let t = self.total_samples as f64 / self.sample_rate;
            if silent_sample {
                if !self.in_silence {
                    self.in_silence = true;
                    self.run_samples = 0;
                    self.silence_start_s = t;
                    self.reported = false;
                }
                self.run_samples += 1;
                let run_s = self.run_samples as f64 / self.sample_rate;
                if !self.reported && run_s >= self.min_duration_s {
                    tracing::info!(
                        target: "vaco_filter_audio_dynamics::silencedetect",
                        "silence_start: {:.6}",
                        self.silence_start_s
                    );
                    self.reported = true;
                }
            } else {
                if self.in_silence && self.reported {
                    let run_s = self.run_samples as f64 / self.sample_rate;
                    tracing::info!(
                        target: "vaco_filter_audio_dynamics::silencedetect",
                        "silence_end: {:.6} | silence_duration: {:.6}",
                        t,
                        run_s
                    );
                }
                self.in_silence = false;
                self.run_samples = 0;
            }
            self.total_samples += 1;
        }
        Ok(FrameOut::One(input))
    }

    fn flush_state(&mut self) {
        self.in_silence = false;
        self.run_samples = 0;
        self.total_samples = 0;
        self.reported = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = SilenceDetect {
        noise: common::f64_opt(req, &["noise", "n"], 0.001),
        min_duration_s: common::f64_opt(req, &["duration", "d"], 2.0),
        sample_rate: 48_000.0,
        in_silence: false,
        run_samples: 0,
        silence_start_s: 0.0,
        reported: false,
        total_samples: 0,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}
