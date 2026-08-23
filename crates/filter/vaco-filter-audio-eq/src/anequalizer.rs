//! `anequalizer` — high-order parametric multi-band equalizer.
//!
//! `ffmpeg -h filter=anequalizer` (2026-08-23) shows only six options —
//! `params`, `curves`, `size`, `mgain`, `fscale`, `colors` — and `params` is
//! an unstructured string; its per-band grammar (`c<chan> f=<f> w=<w> g=<g>
//! t=<type>`, bands separated by `|`) is documented user-facing syntax in the
//! reference's own texi manual, which D7 treats as a specification (an
//! interface fact, not source) — not measured against a running filter here,
//! so treat the grammar itself as the medium-confidence part of this module.
//!
//! Each band becomes one [`vaco_filter_adsp::biquad::peaking`] section on its declared
//! channel, cascaded in declaration order; `w` is documented as a bandwidth
//! in Hz, so it is read through [`WidthType::Hz`]. `t` (filter type —
//! Butterworth/Chebyshev variants in the reference) is accepted and ignored:
//! every band is a cookbook peaking section regardless, a structural gap
//! recorded here rather than guessed at. `curves`/`size`/`mgain`/`fscale`/
//! `colors` (the video response-curve output) are accepted and ignored —
//! this crate produces the audio output only.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};

pub const DESC: FilterDesc = FilterDesc {
    name: "anequalizer",
    description: "apply high-order audio parametric multi band equalizer",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
struct Band {
    channel: usize,
    f0: f64,
    width_hz: f64,
    gain_db: f64,
}

/// Parse `c<chan> f=<f> w=<w> g=<g> t=<type>|...`. A band whose tokens do not
/// parse is dropped rather than failing the whole filter — one malformed
/// entry should not silently disable every other band, and a parse error
/// here has no way to reach the user besides the filtergraph's own.
fn parse_params(raw: &str) -> Vec<Band> {
    let mut bands = Vec::new();
    for entry in raw.split('|') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut channel = 0usize;
        let mut f0 = None;
        let mut width_hz = None;
        let mut gain_db = None;
        for tok in entry.split_whitespace() {
            if let Some(c) = tok.strip_prefix('c') {
                if let Ok(n) = c.parse::<usize>() {
                    channel = n;
                }
                continue;
            }
            if let Some((k, v)) = tok.split_once('=') {
                match k {
                    "f" => f0 = v.parse::<f64>().ok(),
                    "w" => width_hz = v.parse::<f64>().ok(),
                    "g" => gain_db = v.parse::<f64>().ok(),
                    _ => {}
                }
            }
        }
        if let (Some(f0), Some(width_hz), Some(gain_db)) = (f0, width_hz, gain_db) {
            bands.push(Band {
                channel,
                f0,
                width_hz,
                gain_db,
            });
        }
    }
    bands
}

#[derive(Debug, Clone)]
struct AnEqualizer {
    bands: Vec<Band>,
    stages: Vec<Vec<(Coeffs, State)>>,
}

impl FrameFilter for AnEqualizer {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let fs = f64::from(*sample_rate);
            let n = layout.channels.max(1) as usize;
            self.stages = vec![Vec::new(); n];
            for band in &self.bands {
                let Some(stage) = self.stages.get_mut(band.channel) else {
                    continue;
                };
                let coeffs =
                    biquad::peaking(fs, band.f0, WidthType::Hz, band.width_hz, band.gain_db);
                stage.push((coeffs, State::default()));
            }
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.stages.len() != channels.len() {
            self.stages.resize(channels.len(), Vec::new());
        }
        for (i, ch) in channels.iter_mut().enumerate() {
            let Some(stage) = self.stages.get_mut(i) else {
                continue;
            };
            for s in ch.iter_mut() {
                let mut v = *s;
                for (coeffs, state) in stage.iter_mut() {
                    v = state.process(coeffs, v);
                }
                *s = v;
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

    fn flush_state(&mut self) {
        for stage in &mut self.stages {
            for (_, state) in stage.iter_mut() {
                *state = State::default();
            }
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let raw = req.named("params").unwrap_or_default();
    let filter = AnEqualizer {
        bands: parse_params(&raw),
        stages: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}
