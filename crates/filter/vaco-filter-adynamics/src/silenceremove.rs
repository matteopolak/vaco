//! `silenceremove` — remove silence from the start and/or end of a stream.
//!
//! `ffmpeg -h filter=silenceremove` (2026-08-23) has a large option surface
//! (`start_periods`, `start_duration`, `start_threshold`, `start_silence`,
//! `start_mode`, and the mirrored `stop_*` set, plus `detection` and
//! `window`); this crate implements the **single-period** case for both
//! ends, which is what `start_periods=1`/`stop_periods=1` (by far the most
//! common invocation) means. `start_periods`/`stop_periods` values other
//! than 0 or 1 are treated as 1 — a documented gap, not a probed
//! equivalence. `start_duration`/`start_silence` are not applied (leading
//! silence is dropped in full rather than retaining a configurable amount).
//! `detection` implements `peak`, `rms` and `avg`; `median`, `ptp` and `dev`
//! fall back to `rms`. `timestamp` (full rewrite vs. copy) is not
//! implemented: timestamps pass through unchanged, which matches neither
//! documented mode exactly.
//!
//! Silence is judged per fixed-size `window` (default 20 ms), not per
//! sample — `start_mode`/`stop_mode` (`any`/`all`) decide whether one loud
//! channel in a window is enough to call the whole window non-silent.

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::{Frame, FramePool};
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, ChannelMode};

pub const DESC: FilterDesc = FilterDesc {
    name: "silenceremove",
    description: "remove silence",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detect {
    Peak,
    Rms,
    Avg,
}

fn detect_opt(req: &Instantiate<'_>, key: &str) -> Detect {
    match req.named(key).as_deref() {
        Some("peak" | "2") => Detect::Peak,
        Some("avg" | "0") => Detect::Avg,
        _ => Detect::Rms,
    }
}

fn window_value(samples: &[f64], mode: Detect) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let n = samples.len() as f64;
    match mode {
        Detect::Peak => samples.iter().fold(0.0f64, |a, &b| a.max(b.abs())),
        Detect::Avg => samples.iter().map(|s| s.abs()).sum::<f64>() / n,
        Detect::Rms => (samples.iter().map(|s| s * s).sum::<f64>() / n).sqrt(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Dropping leading silence, waiting for the first non-silent window.
    Start,
    /// Emitting normally.
    Passthrough,
    /// A silent run at (what might be) the tail: buffered, not yet emitted.
    Buffering,
}

type Block = Vec<Vec<f64>>;

struct SilenceRemove {
    start_enabled: bool,
    start_threshold: f64,
    start_mode: ChannelMode,
    stop_enabled: bool,
    stop_threshold: f64,
    stop_mode: ChannelMode,
    stop_silence_s: f64,
    detection: Detect,
    window_s: f64,
    sample_rate: f64,
    window_samples: usize,
    state: State,
    pending: Block,
    buffered: VecDeque<Block>,
    format: Option<(SampleFmt, ChannelLayout, u32)>,
}

impl SilenceRemove {
    /// `Any`: the window counts as silent if *any* channel is below
    /// threshold. `All`: only if *every* channel is. This is the natural
    /// reading of the option names, not a probed equivalence to the
    /// reference's exact per-channel trigger semantics.
    fn silent(&self, window: &[Vec<f64>], threshold: f64, mode: ChannelMode) -> bool {
        let mut below = window
            .iter()
            .map(|ch| window_value(ch, self.detection) <= threshold);
        match mode {
            ChannelMode::Any => below.any(|b| b),
            ChannelMode::All => below.all(|b| b),
        }
    }

    fn take_window(&mut self) -> Option<Block> {
        let ready = self
            .pending
            .first()
            .is_some_and(|c| c.len() >= self.window_samples);
        if !ready {
            return None;
        }
        let mut window = Vec::new();
        for ch in &mut self.pending {
            let tail = ch.split_off(self.window_samples.min(ch.len()));
            window.push(std::mem::replace(ch, tail));
        }
        Some(window)
    }

    fn push_samples(&mut self, channels: &[Vec<f64>]) {
        if self.pending.len() != channels.len() {
            self.pending = vec![Vec::new(); channels.len()];
        }
        for (dst, src) in self.pending.iter_mut().zip(channels.iter()) {
            dst.extend_from_slice(src);
        }
    }

    fn append(out: &mut Block, block: Block) {
        if out.len() != block.len() {
            out.resize_with(block.len(), Vec::new);
        }
        for (o, c) in out.iter_mut().zip(block) {
            o.extend(c);
        }
    }

    fn flatten(&mut self) -> Block {
        let channels = self.buffered.front().map_or(0, Vec::len);
        let mut out = vec![Vec::new(); channels];
        for w in self.buffered.drain(..) {
            Self::append(&mut out, w);
        }
        out
    }

    fn drain_ready(&mut self, out: &mut Block) {
        while let Some(window) = self.take_window() {
            let is_start_silent =
                self.start_enabled && self.silent(&window, self.start_threshold, self.start_mode);
            let is_stop_silent =
                self.stop_enabled && self.silent(&window, self.stop_threshold, self.stop_mode);
            match self.state {
                State::Start => {
                    if !is_start_silent {
                        self.state = State::Passthrough;
                        Self::append(out, window);
                    }
                }
                State::Passthrough => {
                    if is_stop_silent {
                        self.state = State::Buffering;
                        self.buffered.push_back(window);
                    } else {
                        Self::append(out, window);
                    }
                }
                State::Buffering => {
                    if is_stop_silent {
                        self.buffered.push_back(window);
                        let keep = ((self.stop_silence_s / self.window_s.max(1e-6))
                            .ceil()
                            .max(1.0)) as usize;
                        while self.buffered.len() > keep {
                            self.buffered.pop_front();
                        }
                    } else {
                        self.state = State::Passthrough;
                        let flushed = self.flatten();
                        Self::append(out, flushed);
                        Self::append(out, window);
                    }
                }
            }
        }
    }

    fn encode(&self, block: Block) -> Result<Frame> {
        let (fmt, layout, rate) = self.format.clone().unwrap_or((
            SampleFmt::F64,
            ChannelLayout::unspecified(2),
            self.sample_rate as u32,
        ));
        crate::sample::encode(&FramePool::default(), fmt, layout, rate, &block.into())
    }
}

impl FrameFilter for SilenceRemove {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let LinkFormat::Audio { sample_rate, .. } = ctx.link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
            self.window_samples = ((self.window_s * self.sample_rate).round() as usize).max(1);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, channels) = crate::sample::decode(&input)?;
        self.format = Some((fmt, layout, rate));
        self.push_samples(&channels);
        let mut out: Block = vec![Vec::new(); channels.len()];
        self.drain_ready(&mut out);
        if out.iter().all(Vec::is_empty) {
            return Ok(FrameOut::None);
        }
        let mut frame = self.encode(out)?;
        frame.pts = input.pts;
        frame.time_base = input.time_base;
        Ok(FrameOut::One(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        // A final short window (less than `window_samples`) at end of
        // stream is treated as non-silent: there is nothing after it to
        // measure a silent run against.
        if self.pending.iter().any(|c| !c.is_empty()) {
            let tail = std::mem::take(&mut self.pending);
            let mut out = if self.state == State::Buffering {
                self.flatten()
            } else {
                Vec::new()
            };
            self.state = State::Passthrough;
            Self::append(&mut out, tail);
            if out.iter().any(|c| !c.is_empty()) {
                return self.encode(out).map(FrameOut::One);
            }
            return Ok(FrameOut::None);
        }
        if self.state == State::Buffering && !self.buffered.is_empty() {
            // True tail silence: drop it, keeping only the retained window
            // count `drain_ready` already trimmed `buffered` down to.
            let out = self.flatten();
            if out.iter().any(|c| !c.is_empty()) {
                return self.encode(out).map(FrameOut::One);
            }
        }
        Ok(FrameOut::None)
    }

    fn flush_state(&mut self) {
        self.state = if self.start_enabled {
            State::Start
        } else {
            State::Passthrough
        };
        self.pending.clear();
        self.buffered.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let start_periods = common::f64_opt(req, &["start_periods"], 0.0);
    let stop_periods = common::f64_opt(req, &["stop_periods"], 0.0);
    let start_enabled = start_periods > 0.0;
    let filter = SilenceRemove {
        start_enabled,
        start_threshold: common::f64_opt(req, &["start_threshold"], 0.0),
        start_mode: if req.named("start_mode").as_deref() == Some("all") {
            ChannelMode::All
        } else {
            ChannelMode::Any
        },
        stop_enabled: stop_periods != 0.0,
        stop_threshold: common::f64_opt(req, &["stop_threshold"], 0.0),
        stop_mode: if req.named("stop_mode").as_deref() == Some("any") {
            ChannelMode::Any
        } else {
            ChannelMode::All
        },
        stop_silence_s: common::f64_opt(req, &["stop_silence"], 0.0),
        detection: detect_opt(req, "detection"),
        window_s: common::f64_opt(req, &["window"], 0.02).max(0.001),
        sample_rate: 48_000.0,
        window_samples: 1,
        state: if start_enabled {
            State::Start
        } else {
            State::Passthrough
        },
        pending: Vec::new(),
        buffered: VecDeque::new(),
        format: None,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_value_peak_and_rms_agree_on_a_constant_signal() {
        let samples = vec![0.5f64; 100];
        assert!((window_value(&samples, Detect::Peak) - 0.5).abs() < 1e-9);
        assert!((window_value(&samples, Detect::Rms) - 0.5).abs() < 1e-9);
        assert!((window_value(&samples, Detect::Avg) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_window_is_not_silent_by_division_error() {
        // An empty slice must read as zero, not NaN from a `0.0/0.0`.
        assert!(window_value(&[], Detect::Rms).abs() < 1e-12);
    }

    #[test]
    fn disabled_start_and_stop_never_buffer() {
        // The default (`start_periods=0`, `stop_periods=0`) must be a pure
        // passthrough: nothing about this filter's own state machine should
        // ever enter `Buffering` when both ends are disabled.
        let mut sr = SilenceRemove {
            start_enabled: false,
            start_threshold: 0.0,
            start_mode: ChannelMode::Any,
            stop_enabled: false,
            stop_threshold: 0.0,
            stop_mode: ChannelMode::All,
            stop_silence_s: 0.0,
            detection: Detect::Rms,
            window_s: 0.02,
            sample_rate: 48_000.0,
            window_samples: 4,
            state: State::Passthrough,
            pending: Vec::new(),
            buffered: VecDeque::new(),
            format: None,
        };
        sr.push_samples(&[vec![0.0; 16]]);
        let mut out: Block = vec![Vec::new()];
        sr.drain_ready(&mut out);
        assert_eq!(sr.state, State::Passthrough);
        assert!(sr.buffered.is_empty());
        assert_eq!(out.first().map(Vec::len), Some(16));
    }
}
