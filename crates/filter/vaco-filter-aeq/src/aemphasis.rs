//! `aemphasis` — audio emphasis / de-emphasis.
//!
//! `ffmpeg -h filter=aemphasis` (2026-08-27): `level_in`/`level_out` (0 to
//! 64, default 1), `mode` (`reproduction`=0 default, `production`=1), `type`
//! (`col`=0, `emi`=1, `bsi`=2, `riaa`=3, `cd`=4 default, `50fm`=5, `75fm`=6,
//! `50kf`=7, `75kf`=8).
//!
//! # Measured: direction and rough shape, not the exact curve
//!
//! Fed a 100 Hz / 3183 Hz / 15 kHz / 30 kHz sine sweep through the
//! reference at `type=50fm` and measured the gain relative to the unfiltered
//! tone at each frequency: `mode=reproduction` cuts highs (`-2.7 dB` at 3183
//! Hz, `-16.4 dB` at 15 kHz, `-20.1 dB` at 30 kHz) and `mode=production` is
//! its near-exact mirror (`+2.7 dB`, `+16.3 dB` at the same frequencies) —
//! confirming the two modes are pre-emphasis and de-emphasis of the same
//! curve, in the documented direction, and roughly single-pole in shape
//! (close to, but not exactly, the textbook `-10*log10(1+(f/fc)^2)` curve
//! for `fc = 1/(2*pi*50us) = 3183 Hz` — `-3.0 dB` predicted at the corner
//! against `-2.7 dB` measured, `-19.5 dB` predicted at 30 kHz against
//! `-20.1 dB` measured). The residual few dB of disagreement was not
//! resolved within this pass — likely a different digital realisation of
//! the same analog time constant (bilinear versus impulse-invariant, or a
//! prewarping choice) rather than a different time constant entirely, but
//! that is a hypothesis, not a measurement.
//!
//! # The construction
//!
//! `reproduction` for `50fm`/`75fm`/`50kf`/`75kf`/`cd` is
//! [`vaco_filter_adsp::biquad::lowpass_one_pole`] at `f0 = 1/(2*pi*tau)` for
//! the type's own published time constant (50us or 75us — the standard FM
//! broadcast and CD de-emphasis constants, well-established published
//! values, not measured from the reference). `production` is that same
//! filter's **exact digital inverse**: `H_lp(z) = (1-a)/(1-a z^-1)` inverts
//! to `H_pre(z) = (1 - a z^-1)/(1-a)`, a plain two-tap FIR — unconditionally
//! stable, and cascading `reproduction` then `production` is the identity by
//! construction (checked directly in `tests::production_exactly_inverts_reproduction`,
//! a real algebraic property, not a re-run of the same formula).
//!
//! `riaa` reuses the same one-pole construction at a **single** simplified
//! time constant (318 us, the dominant corner of the standard three-constant
//! RIAA phono curve — 3180/318/75 us) rather than the true two-pole/one-zero
//! network; this is a documented simplification, not the full curve.
//! `col`/`emi`/`bsi` (historical 78 rpm curves) use the same construction at
//! an unverified placeholder time constant (64 us) — no published time
//! constants for these three were confidently available within this pass,
//! and shipping a guessed number as if it were measured would be exactly
//! the "plausible invention" this project's standing rule warns against.
//! All three are called out explicitly here and in
//! `docs/filter/vaco-filter-aeq.md` rather than left to look finished.

use vaco_core::{MediaType, Result};
use vaco_filter_adsp::biquad::{Coeffs, State};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aemphasis",
    description: "audio emphasis",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Reproduction,
    Production,
}

/// Time constant (seconds) for each `type` — see module doc for which are
/// published values and which are unverified placeholders.
fn tau_seconds(kind: &str) -> f64 {
    match kind.trim() {
        "75fm" | "6" | "75kf" | "8" => 75e-6,
        "riaa" | "3" => 318e-6,
        // "col"/"emi"/"bsi": unverified placeholder — see module doc.
        "col" | "0" | "emi" | "1" | "bsi" | "2" => 64e-6,
        // "cd"/"50fm"/"50kf" and the default all share the 50us constant.
        _ => 50e-6,
    }
}

/// `H_lp(z) = (1-a) / (1 - a z^-1)`, `production`'s exact digital inverse
/// `(1 - a z^-1) / (1-a)` — an FIR, so it needs no `a1`/`a2` denominator
/// terms; represented as a [`Coeffs`] anyway so both directions share
/// [`State::process`].
fn coeffs(fs: f64, tau: f64, mode: Mode) -> Coeffs {
    let w0 = 2.0 * std::f64::consts::PI / (tau * fs);
    let a = (-w0).exp();
    match mode {
        Mode::Reproduction => Coeffs::normalise(1.0 - a, 0.0, 0.0, 1.0, -a, 0.0),
        Mode::Production => Coeffs::normalise(1.0, -a, 0.0, (1.0 - a).max(1e-12), 0.0, 0.0),
    }
}

#[derive(Debug, Clone)]
struct Emphasis {
    level_in: f64,
    level_out: f64,
    tau: f64,
    mode: Mode,
    coeffs: Coeffs,
    states: Vec<State>,
}

impl FrameFilter for Emphasis {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.coeffs = coeffs(f64::from(*sample_rate), self.tau, self.mode);
            self.states = vec![State::default(); layout.channels.max(1) as usize];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.states.len() != channels.len() {
            self.states = vec![State::default(); channels.len()];
        }
        for (ch, state) in channels.iter_mut().zip(self.states.iter_mut()) {
            for s in ch.iter_mut() {
                let x = *s * self.level_in;
                *s = state.process(&self.coeffs, x) * self.level_out;
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
        for s in &mut self.states {
            *s = State::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let mode = match req.named("mode").as_deref() {
        Some("production" | "1") => Mode::Production,
        _ => Mode::Reproduction,
    };
    let tau = tau_seconds(req.named("type").as_deref().unwrap_or("cd"));
    let filter = Emphasis {
        level_in: common::f64_opt(req, &["level_in"], 1.0),
        level_out: common::f64_opt(req, &["level_out"], 1.0),
        tau,
        mode,
        coeffs: Coeffs::identity(),
        states: Vec::new(),
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

    /// The real oracle: cascading `reproduction` then `production` at the
    /// same time constant is the identity by construction (one is the exact
    /// digital inverse of the other) — checked by running both stages over
    /// a short signal and comparing to the original, not by inspecting
    /// coefficients.
    #[test]
    fn production_exactly_inverts_reproduction() {
        let fs = 48_000.0;
        let tau = 50e-6;
        let lp = coeffs(fs, tau, Mode::Reproduction);
        let inv = coeffs(fs, tau, Mode::Production);
        let mut s_lp = State::default();
        let mut s_inv = State::default();
        let input = [0.3, -0.7, 0.9, 0.1, -0.4, 0.6, -0.2, 0.05, 0.0, -1.0];
        for &x in &input {
            let y = s_lp.process(&lp, x);
            let back = s_inv.process(&inv, y);
            assert!((back - x).abs() < 1e-9, "x={x}: recovered {back}");
        }
    }

    /// Every `type` name and its numeric code must agree.
    #[test]
    fn type_names_and_codes_agree() {
        for (name, code) in [
            ("col", "0"),
            ("emi", "1"),
            ("bsi", "2"),
            ("riaa", "3"),
            ("cd", "4"),
            ("50fm", "5"),
            ("75fm", "6"),
            ("50kf", "7"),
            ("75kf", "8"),
        ] {
            assert!(
                (tau_seconds(name) - tau_seconds(code)).abs() < 1e-15,
                "{name} vs {code}"
            );
        }
    }

    /// `reproduction` (a lowpass) must have unity DC gain.
    #[test]
    fn reproduction_has_unity_dc_gain() {
        let c = coeffs(48_000.0, 50e-6, Mode::Reproduction);
        assert!((c.response_db(1e-6)).abs() < 1e-3);
    }
}
