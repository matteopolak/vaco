//! `aspectralstats` — show frequency domain statistics about audio frames.
//!
//! `ffmpeg -h filter=aspectralstats` (2026-08-23): `win_size` (32–65536,
//! default 2048), `win_func` (21 named windows, default `hann`), `overlap`
//! (0–1, default 0.5), `measure` (a flag set selecting which of thirteen
//! named statistics to report, default `all`). Only the `hann` window is
//! implemented; any other `win_func` value falls back to it — a documented
//! simplification, not a different window for a different setting. Every
//! one of the thirteen named measures is computed regardless of the
//! `measure` flag selection (the flag is accepted, not applied).
//!
//! # The thirteen measures
//!
//! Standard audio spectral features (Peeters, *"A large set of audio
//! features for sound description"*, IRCAM 2004 — published,
//! ffmpeg-independent definitions), computed in [`engine::measures`] from a
//! magnitude spectrum and its bin frequencies: `mean`, `variance`,
//! `centroid`, `spread`, `skewness`, `kurtosis`, `entropy`, `flatness`,
//! `crest`, `flux` (needs the previous frame — zero on the first), `slope`
//! (least-squares fit of magnitude against frequency), `decrease` and
//! `rolloff` (the frequency below which 95% of spectral energy sits — the
//! reference does not document its own percentage; 95% is this crate's
//! documented choice, drawn from the same literature).
//!
//! **Oracle.** [`engine::measures`] is checked against synthetic magnitude
//! spectra with known shapes, not against a second FFT-based
//! implementation: a spectrum with all its energy in one bin has that bin's
//! frequency as its `centroid` and `spread == 0`; a perfectly flat spectrum
//! has `flatness == 1` (geometric mean equals arithmetic mean only when
//! every value is equal) and the *lowest possible* crest factor for a
//! nonzero spectrum; a symmetric spectrum around its centroid has
//! `skewness == 0`.

mod engine;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;
use vaco_tx::{Plan, Tx};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aspectralstats",
    description: "show frequency domain statistics about audio frames",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// One channel's sliding analysis window plus its previous-frame spectrum
/// (for `flux`).
#[derive(Debug, Default)]
struct ChannelWindow {
    history: Vec<f64>,
    prev_mag: Vec<f64>,
}

struct SpectralStats {
    win_size: usize,
    hop: usize,
    sample_rate: f64,
    window: Vec<f64>,
    tx: Option<Tx<f64>>,
    channels: Vec<ChannelWindow>,
}

impl std::fmt::Debug for SpectralStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpectralStats")
            .field("win_size", &self.win_size)
            .field("hop", &self.hop)
            .field("channels", &self.channels.len())
            .finish_non_exhaustive()
    }
}

/// A periodic Hann window, the reference's default and the only one this
/// crate implements (see the module doc).
fn hann(n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![1.0; n];
    }
    (0..n)
        .map(|i| {
            0.5 - 0.5
                * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()
        })
        .collect()
}

impl SpectralStats {
    fn ensure_tx(&mut self) {
        if self.tx.is_none()
            && let Ok(plan) = Plan::<f64>::fft(self.win_size, false)
        {
            self.tx = Some(Tx::new(plan));
        }
    }

    fn process_channel(&mut self, index: usize) {
        self.ensure_tx();
        let win_size = self.win_size;
        let hop = self.hop.max(1);
        let sample_rate = self.sample_rate;
        let window = self.window.clone();
        let Some(tx) = self.tx.as_mut() else { return };
        let Some(chan) = self.channels.get_mut(index) else {
            return;
        };
        while chan.history.len() >= win_size {
            let mut input = vec![0.0f64; 2 * win_size];
            for (i, w) in window.iter().enumerate() {
                let Some(&s) = chan.history.get(i) else { break };
                if let Some(slot) = input.get_mut(2 * i) {
                    *slot = s * w;
                }
            }
            let mut output = vec![0.0f64; 2 * win_size];
            tx.execute(&mut output, &input);

            // The RDFT bin count for a real-input FFT of `win_size`: the
            // truncating half is the definition, not a precision loss —
            // `vaco-tx`'s own `Plan::new` allows the identical lint for the
            // identical reason.
            #[allow(
                clippy::integer_division,
                reason = "win_size/2 is the Nyquist bin index, the definition of an RDFT bin count"
            )]
            let bins = win_size / 2 + 1;
            let mut mag = Vec::new();
            let mut freqs = Vec::new();
            for k in 0..bins {
                let re = output.get(2 * k).copied().unwrap_or(0.0);
                let im = output.get(2 * k + 1).copied().unwrap_or(0.0);
                mag.push((re * re + im * im).sqrt());
                freqs.push(k as f64 * sample_rate / win_size.max(1) as f64);
            }

            let prev = if chan.prev_mag.len() == mag.len() {
                Some(chan.prev_mag.as_slice())
            } else {
                None
            };
            let m = engine::measures(&mag, &freqs, prev);
            tracing::info!(
                target: "vaco_filter_ameasure::aspectralstats",
                channel = index,
                mean = m.mean,
                variance = m.variance,
                centroid = m.centroid,
                spread = m.spread,
                skewness = m.skewness,
                kurtosis = m.kurtosis,
                entropy = m.entropy,
                flatness = m.flatness,
                crest = m.crest,
                flux = m.flux,
                slope = m.slope,
                decrease = m.decrease,
                rolloff = m.rolloff,
                "aspectralstats",
            );
            chan.prev_mag = mag;

            let drain = hop.min(chan.history.len());
            chan.history.drain(0..drain);
        }
    }
}

impl FrameFilter for SpectralStats {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        if self.channels.len() != channels.len() {
            self.channels.resize_with(channels.len(), ChannelWindow::default);
        }
        for (i, ch) in channels.iter().enumerate() {
            if let Some(slot) = self.channels.get_mut(i) {
                slot.history.extend_from_slice(ch);
            }
            self.process_channel(i);
        }
        Ok(FrameOut::One(input))
    }

    fn flush_state(&mut self) {
        self.channels.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let win_size = common::usize_opt(req, &["win_size"], 2048).clamp(32, 65536);
    let overlap = common::f64_opt(req, &["overlap"], 0.5).clamp(0.0, 0.99);
    let hop = ((win_size as f64) * (1.0 - overlap)).round().max(1.0) as usize;
    let filter = SpectralStats {
        win_size,
        hop,
        sample_rate: 48_000.0,
        window: hann(win_size),
        tx: None,
        channels: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}
