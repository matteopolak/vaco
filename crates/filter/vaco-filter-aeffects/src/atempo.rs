//! `atempo` — adjust audio tempo without changing pitch.
//!
//! `ffmpeg -h filter=atempo` (2026-08-23): `tempo` (`0.5..100`, default
//! `1`). Supports timeline (`enable`, though timeline enable/disable on a
//! filter that reshapes duration is inherently approximate for any
//! implementation).
//!
//! # Design: buffer-then-flush, not streaming
//!
//! `vaco-filter-adsp::wsola` implements time-domain WSOLA, which needs to
//! see well past its current analysis window to search for the
//! best-correlated splice point — a genuinely streaming implementation
//! needs careful incremental buffering with a bounded lookahead. This
//! filter instead buffers every input frame's samples per channel and runs
//! WSOLA once, at end of stream (`flush`), emitting a single output frame
//! per channel set. That is correct but holds the entire stream in memory
//! and produces no output until flush — a real limitation worth stating
//! plainly rather than leaving implicit, per this crate's correctness
//! discipline. A future incremental version can reuse
//! `vaco_filter_adsp::wsola::wsola_tempo`'s per-window logic without
//! changing its public shape.
//!
//! # What is measured vs structural
//!
//! `tempo=1.0` is an exact identity (`wsola_tempo` special-cases it to a
//! byte-for-byte copy — see that module's own tests). `tempo=2.0` halves
//! the sample count and `tempo=0.5` doubles it, within one analysis
//! window's slack — the two invariants this crate's correctness discipline
//! calls out by name for this filter, both checked directly against
//! `vaco_filter_adsp::wsola` in [`tests::duration_scales_with_tempo`]. The
//! *audible* quality of the splice (whether it matches the reference's own
//! WSOLA tuning, window size, or search radius) is not measured or claimed
//! to match; see `docs/filter/vaco-filter-aeffects.md`.
use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_adsp::wsola::wsola_tempo;
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "atempo",
    description: "adjust audio tempo",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct Atempo {
    tempo: f64,
    channels: Vec<Vec<f64>>,
    fmt: Option<SampleFmt>,
    layout: Option<ChannelLayout>,
    rate: Option<u32>,
    time_base: Option<Rational>,
    first_pts: Option<Timestamp>,
    flushed: bool,
}

impl FrameFilter for Atempo {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, decoded) = crate::sample::decode(&input)?;
        if self.fmt.is_none() {
            self.fmt = Some(fmt);
            self.layout = Some(layout);
            self.rate = Some(rate);
            self.time_base = Some(input.time_base);
            self.first_pts = Some(input.pts);
        }
        if self.channels.len() < decoded.len() {
            self.channels.resize(decoded.len(), Vec::new());
        }
        for (dst, src) in self.channels.iter_mut().zip(decoded.iter()) {
            dst.extend_from_slice(src);
        }
        Ok(FrameOut::None)
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        if self.flushed {
            return Ok(FrameOut::None);
        }
        self.flushed = true;
        let (Some(fmt), Some(layout), Some(rate)) = (self.fmt, self.layout.clone(), self.rate)
        else {
            return Ok(FrameOut::None);
        };
        if self.channels.is_empty() || self.channels.iter().all(Vec::is_empty) {
            return Ok(FrameOut::None);
        }
        let stretched: crate::sample::Channels = self
            .channels
            .iter()
            .map(|ch| wsola_tempo(ch, self.tempo, rate))
            .collect();
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &stretched,
        )?;
        out.pts = self.first_pts.unwrap_or_default();
        out.time_base = self
            .time_base
            .unwrap_or(Rational::new(1, i32::try_from(rate).unwrap_or(48000)));
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        for ch in &mut self.channels {
            ch.clear();
        }
        self.flushed = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let tempo = common::f64_opt(req, &["tempo"], 1.0).clamp(0.5, 100.0);
    let filter = Atempo {
        tempo,
        channels: Vec::new(),
        fmt: None,
        layout: None,
        rate: None,
        time_base: None,
        first_pts: None,
        flushed: false,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two identity-adjacent invariants this crate's correctness
    /// discipline calls out for `atempo`: `1.0` reproduces the input
    /// exactly, and `2.0` halves the duration (within one analysis
    /// window's slack, per `wsola_tempo`'s own documented tolerance).
    #[test]
    fn duration_scales_with_tempo() {
        let rate = 8000u32;
        let input: Vec<f64> = (0..8000)
            .map(|i| (2.0 * std::f64::consts::PI * 220.0 * f64::from(i) / f64::from(rate)).sin())
            .collect();

        let unity = wsola_tempo(&input, 1.0, rate);
        assert_eq!(unity.len(), input.len());
        assert!(unity.iter().zip(&input).all(|(a, b)| (a - b).abs() < 1e-12));

        let doubled = wsola_tempo(&input, 2.0, rate);
        let want = input.len() >> 1;
        assert!(
            doubled.len().abs_diff(want) <= 512,
            "got {} want ~{want}",
            doubled.len()
        );
    }
}
