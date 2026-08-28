//! Shared option parsing and the single-input compressor/gate `FrameFilter`.
//!
//! As in `vaco-filter-aeq::common`, options are read straight off
//! [`Instantiate::named`] rather than through a strict
//! [`vaco_opts::Options`]-derived parser. That loose parsing exists so a
//! real reference command line setting an option this crate has not wired
//! up internally still runs, rather than hard-failing the way a strict
//! `set_from_string` would on any undeclared field. But `Instantiate::named`
//! alone cannot tell "a real option this crate has not implemented" from
//! "not a real option at all" — a typo silently ran with defaults and said
//! nothing. [`ensure_known_options`] closes that: it accepts every name the
//! reference actually documents for a filter (implemented or not,
//! preserving the original intent) and rejects anything else by name.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut};
use vaco_filter_core::{FilterContext, LinkFormat, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::Instantiate;

use crate::engine::{Curve, Detection, Envelope, Link, Mode};

pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `sidechaincompress`/`sidechaingate`: `main` (the signal being gain-
/// adjusted) and `sidechain` (the signal that drives the envelope).
pub(crate) const DUAL_AUDIO_PADS: &[Pad] = &[
    Pad {
        name: "main",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "sidechain",
        media_type: MediaType::Audio,
    },
];

pub(crate) fn f64_opt(req: &Instantiate<'_>, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f64>()
        {
            return f;
        }
    }
    default
}

pub(crate) fn bool_opt(req: &Instantiate<'_>, keys: &[&str], default: bool) -> bool {
    for k in keys {
        if let Some(v) = req.named(k) {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                return true;
            }
            if v.eq_ignore_ascii_case("false") || v == "0" {
                return false;
            }
        }
    }
    default
}

pub(crate) fn detection_opt(req: &Instantiate<'_>) -> Detection {
    match req.named("detection").as_deref() {
        Some("peak" | "0") => Detection::Peak,
        _ => Detection::Rms,
    }
}

pub(crate) fn link_opt(req: &Instantiate<'_>) -> Link {
    match req.named("link").as_deref() {
        Some("maximum" | "1") => Link::Maximum,
        _ => Link::Average,
    }
}

/// `start_mode`/`stop_mode` (`silenceremove`): whether one channel or every
/// channel must agree for a window to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelMode {
    Any,
    All,
}

pub(crate) fn mode_opt(req: &Instantiate<'_>) -> Mode {
    match req.named("mode").as_deref() {
        Some("upward" | "1") => Mode::Upward,
        _ => Mode::Downward,
    }
}

/// `20 * log10(linear)`, floored so a zero or negative input reads as a very
/// quiet level rather than `-inf`/`NaN`.
pub(crate) fn db(linear: f64) -> f64 {
    if linear.is_finite() && linear > 1e-12 {
        20.0 * linear.abs().log10()
    } else {
        -240.0
    }
}

pub(crate) fn from_db(db: f64) -> f64 {
    if db.is_finite() {
        10f64.powf(db / 20.0)
    } else {
        1.0
    }
}

/// The dynamics processor every one of `acompressor`/`agate`/
/// `sidechaincompress`/`sidechaingate` reduces to: an envelope follower per
/// channel, linked across channels, run through [`Curve`], with makeup gain
/// and wet/dry mix.
///
/// `sidechaincompress`/`sidechaingate` differ only in *what* feeds the
/// envelope (the second input's samples instead of the first's) — see
/// `sidechaincompress.rs`/`sidechaingate.rs`, which drive this same type
/// through `vaco-filter-framesync` instead of `Simple`.
#[derive(Debug, Clone)]
pub(crate) struct Dynamics {
    pub level_in: f64,
    pub curve: Curve,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub makeup: f64,
    pub range: f64,
    pub link: Link,
    pub detection: Detection,
    pub level_sc: f64,
    pub mix: f64,
    envelopes: Vec<Envelope>,
    sample_rate: f64,
}

impl Dynamics {
    pub(crate) fn new(
        level_in: f64,
        curve: Curve,
        attack_ms: f64,
        release_ms: f64,
        makeup: f64,
        range: f64,
        link: Link,
        detection: Detection,
        level_sc: f64,
        mix: f64,
    ) -> Self {
        Self {
            level_in,
            curve,
            attack_ms,
            release_ms,
            makeup,
            range,
            link,
            detection,
            level_sc,
            mix,
            envelopes: Vec::new(),
            sample_rate: 48_000.0,
        }
    }

    /// Set the sample rate the envelope's attack/release coefficients are
    /// computed against. Called from `FrameFilter::configure` for the
    /// single-input filters and from `FrameSyncFilter::configure` for the
    /// sidechain pair, since the two adapters reach `configure` differently.
    pub(crate) fn set_sample_rate(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate.max(1.0);
    }

    /// Detector magnitude for one sample: peak is `|x|`; RMS uses the same
    /// per-sample envelope follower on `x^2`, then a `sqrt` at read time —
    /// approximated here as `sqrt` of the smoothed squared value computed
    /// inline, since a true windowed RMS needs a second buffer this crate
    /// does not keep. Structural: this is an RMS-shaped detector, not
    /// windowed RMS.
    fn magnitude(x: f64, detection: Detection) -> f64 {
        match detection {
            Detection::Peak => x.abs(),
            Detection::Rms => x * x,
        }
    }

    /// Process one channel-aligned block of samples (`main`, the signal
    /// being gain-adjusted) driven by `detect` (the sidechain signal — the
    /// same slice as `main` for `acompressor`/`agate`).
    pub(crate) fn process(&mut self, main: &mut [Vec<f64>], detect: &[Vec<f64>]) {
        let n = main.len();
        if self.envelopes.len() != n {
            self.envelopes = vec![Envelope::default(); n];
        }
        let attack = Envelope::coeff(self.attack_ms, self.sample_rate);
        let release = Envelope::coeff(self.release_ms, self.sample_rate);
        let samples = main.iter().map(Vec::len).min().unwrap_or(0);
        for i in 0..samples {
            // Detector reading, per channel, linked.
            let mut linked = match self.link {
                Link::Average => 0.0,
                Link::Maximum => f64::MIN,
            };
            let mut count = 0.0;
            for (ch, env) in detect.iter().zip(self.envelopes.iter_mut()) {
                let Some(&s) = ch.get(i) else { continue };
                let mag =
                    Self::magnitude(s * self.level_sc, self.detection) * self.level_in.max(0.0);
                let raw = if self.detection == Detection::Rms {
                    mag.sqrt()
                } else {
                    mag
                };
                let smoothed = env.step(raw, attack, release);
                match self.link {
                    Link::Average => linked += smoothed,
                    Link::Maximum => linked = linked.max(smoothed),
                }
                count += 1.0;
            }
            if self.link == Link::Average && count > 0.0 {
                linked /= count;
            }
            let level_db = db(linked);
            let mut gain_db = self.curve.gain_db(level_db);
            // `range`: the maximum attenuation a gate may apply (linear,
            // `ffmpeg -h filter=agate`'s `range` option). Not meaningful for
            // a compressor (`range` defaults to `1.0`, i.e. no floor there).
            let floor_db = db(self.range.clamp(0.0, 1.0).max(1e-6));
            if self.curve.mode == crate::engine::Mode::Downward {
                gain_db = gain_db.max(floor_db.min(0.0));
            }
            let gain = from_db(gain_db) * self.makeup.max(0.0);
            for ch in main.iter_mut() {
                let Some(s) = ch.get_mut(i) else { continue };
                let wet = *s * gain;
                *s = self.mix.mul_add(wet - *s, *s);
            }
        }
    }
}

impl FrameFilter for Dynamics {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.set_sample_rate(f64::from(*sample_rate));
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let detect = channels.clone();
        self.process(&mut channels, &detect);
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
        for e in &mut self.envelopes {
            *e = Envelope::default();
        }
    }
}

/// Rejects any `key=value` argument whose key is not one of the
/// reference's own documented option names for `req.name` (see
/// [`KNOWN_OPTIONS`] and this module's own doc for what this deliberately
/// still tolerates). A filter name absent from the table is not this
/// function's business — the registry's own dispatch already rejects an
/// unregistered filter name before this ever runs.
///
/// # Errors
/// Names the filter and the exact unrecognised key.
pub(crate) fn ensure_known_options(req: &Instantiate<'_>) -> Result<(), String> {
    let Some((_, known)) = KNOWN_OPTIONS.iter().find(|(name, _)| *name == req.name) else {
        return Ok(());
    };
    for arg in req.arguments {
        if let Some(key) = arg.key.as_deref()
            && !known.contains(&key)
        {
            return Err(format!(
                "{}: unrecognized option `{key}` (not one of the reference's own documented \
                 options for this filter)",
                req.name
            ));
        }
    }
    Ok(())
}

/// Every option name (canonical and every alias) the reference documents
/// for this crate's filters -- probed directly against real `ffmpeg 8.1
/// -h filter=<name>`, 2026-08-28. Keyed by the registered filter name.
///
/// [`ensure_known_options`] is the only thing that reads this: an option
/// name the reference does not document at all (a typo, or something that
/// was never a real option) is rejected; a real reference option this
/// crate has not wired up internally is still accepted and silently has no
/// effect, preserving this crate's established `Instantiate::named` policy
/// for options it has not implemented -- see the module doc.
const KNOWN_OPTIONS: &[(&str, &[&str])] = &[
    (
        "acompressor",
        &[
            "level_in",
            "mode",
            "threshold",
            "ratio",
            "attack",
            "release",
            "makeup",
            "knee",
            "link",
            "detection",
            "level_sc",
            "mix",
        ],
    ),
    (
        "acrusher",
        &[
            "level_in",
            "level_out",
            "bits",
            "mix",
            "mode",
            "dc",
            "aa",
            "samples",
            "lfo",
            "lforange",
            "lforate",
        ],
    ),
    ("adrc", &["transfer", "attack", "release", "channels"]),
    (
        "adynamicequalizer",
        &[
            "threshold",
            "dfrequency",
            "dqfactor",
            "tfrequency",
            "tqfactor",
            "attack",
            "release",
            "ratio",
            "makeup",
            "range",
            "mode",
            "dftype",
            "tftype",
            "auto",
            "precision",
        ],
    ),
    ("adynamicsmooth", &["sensitivity", "basefreq"]),
    (
        "agate",
        &[
            "level_in",
            "mode",
            "range",
            "threshold",
            "ratio",
            "attack",
            "release",
            "makeup",
            "knee",
            "detection",
            "link",
            "level_sc",
        ],
    ),
    (
        "alimiter",
        &[
            "level_in",
            "level_out",
            "limit",
            "attack",
            "release",
            "asc",
            "asc_level",
            "level",
            "latency",
        ],
    ),
    (
        "apsyclip",
        &[
            "level_in",
            "level_out",
            "clip",
            "diff",
            "adaptive",
            "iterations",
            "level",
        ],
    ),
    (
        "asoftclip",
        &["type", "threshold", "output", "param", "oversample"],
    ),
    (
        "astats",
        &[
            "length",
            "metadata",
            "reset",
            "measure_perchannel",
            "measure_overall",
        ],
    ),
    (
        "compand",
        &[
            "attacks",
            "decays",
            "points",
            "soft-knee",
            "gain",
            "volume",
            "delay",
        ],
    ),
    (
        "dynaudnorm",
        &[
            "framelen",
            "f",
            "gausssize",
            "g",
            "peak",
            "p",
            "maxgain",
            "m",
            "targetrms",
            "r",
            "coupling",
            "n",
            "correctdc",
            "c",
            "altboundary",
            "b",
            "compress",
            "s",
            "threshold",
            "t",
            "channels",
            "h",
            "overlap",
            "o",
            "curve",
            "v",
        ],
    ),
    (
        "loudnorm",
        &[
            "I",
            "i",
            "LRA",
            "lra",
            "TP",
            "tp",
            "measured_I",
            "measured_i",
            "measured_LRA",
            "measured_lra",
            "measured_TP",
            "measured_tp",
            "measured_thresh",
            "offset",
            "linear",
            "dual_mono",
            "print_format",
            "stats_file",
        ],
    ),
    ("mcompand", &["args"]),
    (
        "sidechaincompress",
        &[
            "level_in",
            "mode",
            "threshold",
            "ratio",
            "attack",
            "release",
            "makeup",
            "knee",
            "link",
            "detection",
            "level_sc",
            "mix",
        ],
    ),
    (
        "sidechaingate",
        &[
            "level_in",
            "mode",
            "range",
            "threshold",
            "ratio",
            "attack",
            "release",
            "makeup",
            "knee",
            "detection",
            "link",
            "level_sc",
        ],
    ),
    (
        "silencedetect",
        &["n", "noise", "d", "duration", "mono", "m"],
    ),
    (
        "silenceremove",
        &[
            "start_periods",
            "start_duration",
            "start_threshold",
            "start_silence",
            "start_mode",
            "stop_periods",
            "stop_duration",
            "stop_threshold",
            "stop_silence",
            "stop_mode",
            "detection",
            "window",
            "timestamp",
        ],
    ),
    (
        "speechnorm",
        &[
            "peak",
            "p",
            "expansion",
            "e",
            "compression",
            "c",
            "threshold",
            "t",
            "raise",
            "r",
            "fall",
            "f",
            "channels",
            "h",
            "invert",
            "i",
            "link",
            "l",
            "rms",
            "m",
        ],
    ),
    ("volumedetect", &[]),
];

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn req<'a>(
        name: &'a str,
        args: Option<&'a str>,
        arguments: &'a [vaco_filter_graph::ast::Arg],
    ) -> Instantiate<'a> {
        Instantiate {
            name,
            instance: name,
            args,
            arguments,
        }
    }

    fn arg(key: &str, value: &str) -> vaco_filter_graph::ast::Arg {
        vaco_filter_graph::ast::Arg {
            key: Some(key.to_owned()),
            raw_value: value.to_owned(),
            span: vaco_filter_graph::span::Span::default(),
        }
    }

    /// A name the reference does not document at all for `alimiter` --
    /// exactly the case `Instantiate::named` alone could not distinguish
    /// from a real-but-unimplemented option before this fix.
    #[test]
    fn an_unrecognised_option_name_is_a_named_error() {
        let arguments = [arg("not_a_real_option", "1")];
        let err = ensure_known_options(&req("alimiter", Some("not_a_real_option=1"), &arguments))
            .unwrap_err();
        assert!(
            err.contains("alimiter") && err.contains("not_a_real_option"),
            "unexpected error text: {err}"
        );
    }

    /// A real, implemented option -- unaffected.
    #[test]
    fn an_implemented_option_is_accepted() {
        let arguments = [arg("level_in", "1")];
        assert!(ensure_known_options(&req("alimiter", Some("level_in=1"), &arguments)).is_ok());
    }

    /// A real reference option for `alimiter` this crate has not wired up
    /// internally -- the case loose parsing exists to keep working.
    #[test]
    fn a_real_but_unimplemented_option_is_still_accepted() {
        let arguments = [arg("asc", "1")];
        assert!(ensure_known_options(&req("alimiter", Some("asc=1"), &arguments)).is_ok());
    }

    /// A filter name not in `KNOWN_OPTIONS` at all is not this function's
    /// business -- the registry's own dispatch handles that.
    #[test]
    fn an_unregistered_filter_name_is_not_this_functions_business() {
        assert!(ensure_known_options(&req("not-a-real-filter", None, &[])).is_ok());
    }
}
