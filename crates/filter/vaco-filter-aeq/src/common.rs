//! Shared option parsing and per-channel application for the biquad family.
//!
//! Every filter in [`vaco_filter_adsp::biquad`]'s family documents more options than this
//! crate implements (`transform`, `precision`, `blocksize`, and their `a`/`r`
//! aliases pick among numerically-different realisations of the *same*
//! transfer function or an execution-speed knob — see `biquad::State`'s doc).
//! Rather than declare them on a [`vaco_opts::Options`] struct and reject a
//! filtergraph string that sets one — which is what a strict
//! `set_from_string` would do — every filter here reads only the options it
//! implements straight off [`Instantiate::named`]. But that alone cannot
//! tell "a real biquad option this crate has not implemented" from "not a
//! real option at all" — a typo silently ran with defaults and said
//! nothing. [`ensure_known_options`] closes that: it accepts every name the
//! reference actually documents for a filter (implemented or not,
//! preserving the original intent) and rejects anything else by name.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut};
use vaco_filter_core::{FilterContext, LinkFormat, Pad};
use vaco_frame::Frame;

use vaco_filter_graph::registry::Instantiate;

use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};

pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// Read a named option as `f64`, trying each alias in order.
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

/// Read a named option as `bool` (`ffmpeg`'s boolean spellings: `1`/`0`,
/// `true`/`false`).
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

/// Read a named option as `u8`.
pub(crate) fn u8_opt(req: &Instantiate<'_>, keys: &[&str], default: u8) -> u8 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(n) = v.trim().parse::<u8>()
        {
            return n;
        }
    }
    default
}

/// `width_type`/`t`: a name (`h`/`q`/`o`/`s`/`k`) or the reference's numeric
/// encoding (`1..=5` in that same order, probed via `ffmpeg -h`).
pub(crate) fn width_type_opt(req: &Instantiate<'_>) -> WidthType {
    for k in ["width_type", "t"] {
        if let Some(v) = req.named(k) {
            let v = v.trim();
            if let Some(wt) = WidthType::parse(v) {
                return wt;
            }
            match v {
                "1" => return WidthType::Hz,
                "2" => return WidthType::Octave,
                "3" => return WidthType::QFactor,
                "4" => return WidthType::Slope,
                "5" => return WidthType::KHz,
                _ => {}
            }
        }
    }
    WidthType::QFactor
}

/// `channels`/`c`: `"all"` (the default) or a whitespace/`|`-separated list
/// of channel indices. Reference names ("FL", "FR", ...) are not resolved —
/// index selection covers the common `channels=0` / `channels=0 1` cases and
/// is a documented structural gap.
#[derive(Debug, Clone)]
pub(crate) enum ChannelSelect {
    All,
    Indices(Vec<usize>),
}

impl ChannelSelect {
    pub(crate) fn parse(req: &Instantiate<'_>) -> Self {
        let raw = req.named("channels").or_else(|| req.named("c"));
        let Some(raw) = raw else {
            return Self::All;
        };
        let raw = raw.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
            return Self::All;
        }
        let idx: Vec<usize> = raw
            .split(|c: char| c.is_whitespace() || c == '|' || c == ',')
            .filter_map(|t| t.trim().parse::<usize>().ok())
            .collect();
        if idx.is_empty() {
            Self::All
        } else {
            Self::Indices(idx)
        }
    }

    pub(crate) fn selects(&self, index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Indices(v) => v.contains(&index),
        }
    }
}

/// What coefficients to build, and from what parameters.
///
/// Coefficients cannot be built at `create()` time: the cookbook formulas
/// need the sample rate, which is only known once link negotiation has run.
/// So a filter module builds a `Design` from its options immediately, and
/// [`Biquad::configure`] calls [`Design::build`] once the real rate is
/// available — recomputing it every time `configure` runs, which also covers
/// a format change mid-graph.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Design {
    Lowpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        poles: u8,
    },
    Highpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        poles: u8,
    },
    Bandpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        csg: bool,
    },
    Bandreject {
        f0: f64,
        wt: WidthType,
        width: f64,
    },
    Allpass {
        f0: f64,
        wt: WidthType,
        width: f64,
        order: u8,
    },
    Peaking {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Lowshelf {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Highshelf {
        f0: f64,
        wt: WidthType,
        width: f64,
        gain_db: f64,
    },
    Raw(Coeffs),
}

impl Design {
    pub(crate) fn build(self, fs: f64) -> Coeffs {
        match self {
            Self::Lowpass {
                f0,
                wt,
                width,
                poles,
            } => {
                if poles == 1 {
                    biquad::lowpass_one_pole(fs, f0)
                } else {
                    biquad::lowpass(fs, f0, wt, width)
                }
            }
            Self::Highpass {
                f0,
                wt,
                width,
                poles,
            } => {
                if poles == 1 {
                    biquad::highpass_one_pole(fs, f0)
                } else {
                    biquad::highpass(fs, f0, wt, width)
                }
            }
            Self::Bandpass { f0, wt, width, csg } => biquad::bandpass(fs, f0, wt, width, csg),
            Self::Bandreject { f0, wt, width } => biquad::bandreject(fs, f0, wt, width),
            Self::Allpass {
                f0,
                wt,
                width,
                order,
            } => biquad::allpass(fs, f0, wt, width, order),
            Self::Peaking {
                f0,
                wt,
                width,
                gain_db,
            } => biquad::peaking(fs, f0, wt, width, gain_db),
            Self::Lowshelf {
                f0,
                wt,
                width,
                gain_db,
            } => biquad::lowshelf(fs, f0, wt, width, gain_db),
            Self::Highshelf {
                f0,
                wt,
                width,
                gain_db,
            } => biquad::highshelf(fs, f0, wt, width, gain_db),
            Self::Raw(c) => c,
        }
    }
}

/// A `FrameFilter` that runs one biquad section over every selected channel,
/// wet/dry-mixed by `mix`. This is the whole body of every filter in
/// [`vaco_filter_adsp::biquad`]'s family that is not `tiltshelf` (which cascades two of
/// these).
#[derive(Debug, Clone)]
pub(crate) struct Biquad {
    design: Design,
    coeffs: Coeffs,
    pub mix: f64,
    pub select: ChannelSelect,
    states: Vec<State>,
}

impl Biquad {
    pub(crate) fn new(design: Design, mix: f64, select: ChannelSelect) -> Self {
        Self {
            design,
            coeffs: Coeffs::identity(),
            mix,
            select,
            states: Vec::new(),
        }
    }
}

impl FrameFilter for Biquad {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.coeffs = self.design.build(f64::from(*sample_rate));
            self.states = vec![State::default(); layout.channels.max(1) as usize];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.states.len() != channels.len() {
            self.states = vec![State::default(); channels.len()];
        }
        for (i, ch) in channels.iter_mut().enumerate() {
            if !self.select.selects(i) {
                continue;
            }
            let Some(state) = self.states.get_mut(i) else {
                continue;
            };
            for s in ch.iter_mut() {
                let dry = *s;
                let wet = state.process(&self.coeffs, dry);
                *s = self.mix.mul_add(wet - dry, dry);
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

/// `frequency`/`f`, read with a filter-specific default (the reference's
/// default frequency differs per filter — 0 Hz for `equalizer`, 3000 Hz for
/// `highpass`/`bandpass`/`bandreject`/`allpass`/`treble`, 100 Hz for `bass`,
/// 500 Hz for `lowpass` — all probed via `ffmpeg -h filter=<name>`).
pub(crate) fn frequency_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["frequency", "f"], default)
}

pub(crate) fn width_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["width", "w"], default)
}

pub(crate) fn gain_opt(req: &Instantiate<'_>, default: f64) -> f64 {
    f64_opt(req, &["gain", "g"], default)
}

pub(crate) fn mix_opt(req: &Instantiate<'_>) -> f64 {
    f64_opt(req, &["mix", "m"], 1.0)
}

pub(crate) fn poles_opt(req: &Instantiate<'_>) -> u8 {
    u8_opt(req, &["poles", "p"], 2)
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
    ("aemphasis", &["level_in", "level_out", "mode", "type"]),
    (
        "allpass",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "order",
            "o",
            "transform",
            "a",
            "precision",
            "r",
        ],
    ),
    (
        "anequalizer",
        &["params", "curves", "size", "mgain", "fscale", "colors"],
    ),
    ("atilt", &["freq", "slope", "width", "order", "level"]),
    (
        "bandpass",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "csg",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "bandreject",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "bass",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "biquad",
        &[
            "a0",
            "a1",
            "a2",
            "b0",
            "b1",
            "b2",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "equalizer",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "firequalizer",
        &[
            "gain",
            "gain_entry",
            "delay",
            "accuracy",
            "wfunc",
            "fixed",
            "multi",
            "zero_phase",
            "scale",
            "dumpfile",
            "dumpscale",
            "fft2",
            "min_phase",
        ],
    ),
    (
        "highpass",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "highshelf",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "lowpass",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "lowshelf",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "superequalizer",
        &[
            "1b", "2b", "3b", "4b", "5b", "6b", "7b", "8b", "9b", "10b", "11b", "12b", "13b",
            "14b", "15b", "16b", "17b", "18b",
        ],
    ),
    (
        "tiltshelf",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
    (
        "treble",
        &[
            "frequency",
            "f",
            "width_type",
            "t",
            "width",
            "w",
            "gain",
            "g",
            "poles",
            "p",
            "mix",
            "m",
            "channels",
            "c",
            "normalize",
            "n",
            "transform",
            "a",
            "precision",
            "r",
            "blocksize",
            "b",
        ],
    ),
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

    /// A name the reference does not document at all for `highpass` --
    /// not a typo of a real option, a value this crate has not
    /// implemented, or an alias; just not a real option. This is exactly
    /// the case the crate's `Instantiate::named` policy could not
    /// distinguish from a real-but-unimplemented option before this fix.
    #[test]
    fn an_unrecognised_option_name_is_a_named_error() {
        let arguments = [arg("not_a_real_option", "1")];
        let err = ensure_known_options(&req("highpass", Some("not_a_real_option=1"), &arguments))
            .unwrap_err();
        assert!(
            err.contains("highpass") && err.contains("not_a_real_option"),
            "unexpected error text: {err}"
        );
    }

    /// `transform`/`precision`/`blocksize` are real reference options for
    /// `highpass` this crate has not wired up internally -- the exact
    /// case loose parsing exists to keep working. `ensure_known_options`
    /// must still accept them, preserving the original intent.
    #[test]
    fn a_real_but_unimplemented_option_is_still_accepted() {
        let arguments = [
            arg("transform", "di"),
            arg("precision", "auto"),
            arg("blocksize", "0"),
        ];
        assert!(
            ensure_known_options(&req(
                "highpass",
                Some("transform=di:precision=auto:blocksize=0"),
                &arguments
            ))
            .is_ok()
        );
    }

    /// A real, implemented option -- unaffected.
    #[test]
    fn an_implemented_option_is_accepted() {
        let arguments = [arg("frequency", "200")];
        assert!(ensure_known_options(&req("highpass", Some("frequency=200"), &arguments)).is_ok());
    }

    /// A filter name not in `KNOWN_OPTIONS` at all (an unregistered name)
    /// is not this function's business -- the registry's own dispatch
    /// handles that.
    #[test]
    fn an_unregistered_filter_name_is_not_this_functions_business() {
        assert!(ensure_known_options(&req("not-a-real-filter", None, &[])).is_ok());
    }
}
