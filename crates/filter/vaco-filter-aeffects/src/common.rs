//! Shared option parsing for this crate's seven filters.
//!
//! As in `vaco-filter-aeq::common` and `vaco-filter-adynamics::common`,
//! options are read straight off [`Instantiate::named`] rather than through a
//! strict `vaco_opts::Options`-derived parser. That loose parsing exists so
//! a real reference command line setting an option this crate has not
//! wired up internally (several biquad-family-style refinement knobs
//! across this crate's filters) still runs, rather than hard-failing the
//! way a strict `set_from_string` would on any undeclared field. But
//! `Instantiate::named` alone cannot tell "a real option this crate has
//! not implemented" from "not a real option at all" — a typo silently ran
//! with defaults and said nothing. [`ensure_known_options`] closes that:
//! it accepts every name the reference actually documents for a filter
//! (implemented or not, preserving the original intent) and rejects
//! anything else by name.

use vaco_core::MediaType;
use vaco_filter_core::Pad;

use vaco_filter_graph::registry::Instantiate;

pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `axcorrelate`'s two named input pads, matching `ffmpeg -h filter=axcorrelate`
/// (`axcorrelate0`, `axcorrelate1`) rather than the generic `main`/`sidechain`
/// naming `vaco-filter-adynamics` uses for its own dual-input filters.
pub(crate) const AXCORRELATE_PADS: &[Pad] = &[
    Pad {
        name: "axcorrelate0",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "axcorrelate1",
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

pub(crate) fn usize_opt(req: &Instantiate<'_>, keys: &[&str], default: usize) -> usize {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(n) = v.trim().parse::<usize>()
        {
            return n;
        }
    }
    default
}

/// A linearly-interpolated delay line: writes one sample per call and reads
/// back a value from `delay_samples` (which may be fractional) earlier,
/// interpolating between the two nearest whole-sample history entries.
///
/// Shared by `chorus`, `flanger` and `vibrato` — every LFO-modulated delay
/// filter in this crate needs exactly this building block, so it lives here
/// once (D19) rather than being re-derived per filter.
pub(crate) struct InterpDelay {
    hist: std::collections::VecDeque<f64>,
    max_len: usize,
}

impl InterpDelay {
    pub(crate) fn new(max_len_samples: usize) -> Self {
        let max_len = max_len_samples.max(1);
        let mut hist = std::collections::VecDeque::new();
        hist.resize(max_len, 0.0);
        Self { hist, max_len }
    }

    /// Push `x` into the line and return the interpolated value
    /// `delay_samples` behind the sample just pushed (`0.0` returns `x`
    /// itself).
    pub(crate) fn process(&mut self, x: f64, delay_samples: f64) -> f64 {
        self.hist.push_back(x);
        if self.hist.len() > self.max_len {
            self.hist.pop_front();
        }
        let len = self.hist.len();
        if len == 0 {
            return x;
        }
        let max_delay = (len - 1) as f64;
        let d = delay_samples.clamp(0.0, max_delay);
        let read_pos = max_delay - d; // 0 = oldest, max_delay = newest (just pushed)
        let i0 = read_pos.floor().max(0.0) as usize;
        let frac = read_pos - (i0 as f64);
        let i1 = (i0 + 1).min(len.saturating_sub(1));
        let s0 = self.hist.get(i0).copied().unwrap_or(0.0);
        let s1 = self.hist.get(i1).copied().unwrap_or(0.0);
        s0 + (s1 - s0) * frac
    }

    pub(crate) fn flush(&mut self) {
        self.hist.clear();
        self.hist.resize(self.max_len, 0.0);
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
    ("adelay", &["delays", "all"]),
    ("aecho", &["in_gain", "out_gain", "delays", "decays"]),
    (
        "aexciter",
        &[
            "level_in",
            "level_out",
            "amount",
            "drive",
            "blend",
            "freq",
            "ceil",
            "listen",
        ],
    ),
    (
        "aphaser",
        &["in_gain", "out_gain", "delay", "decay", "speed", "type"],
    ),
    (
        "apulsator",
        &[
            "level_in",
            "level_out",
            "mode",
            "amount",
            "offset_l",
            "offset_r",
            "width",
            "timing",
            "bpm",
            "ms",
            "hz",
        ],
    ),
    ("atempo", &["tempo"]),
    ("axcorrelate", &["size", "algo"]),
    (
        "chorus",
        &[
            "in_gain", "out_gain", "delays", "decays", "speeds", "depths",
        ],
    ),
    (
        "compensationdelay",
        &["mm", "cm", "m", "dry", "wet", "temp"],
    ),
    (
        "crossfeed",
        &[
            "strength",
            "range",
            "slope",
            "level_in",
            "level_out",
            "block_size",
        ],
    ),
    ("crystalizer", &["i", "c"]),
    ("dcshift", &["shift", "limitergain"]),
    ("deesser", &["i", "m", "f", "s"]),
    ("dialoguenhance", &["original", "enhance", "voice"]),
    ("earwax", &[]),
    ("extrastereo", &["m", "c"]),
    (
        "flanger",
        &[
            "delay", "depth", "regen", "width", "speed", "shape", "phase", "interp",
        ],
    ),
    (
        "haas",
        &[
            "level_in",
            "level_out",
            "side_gain",
            "middle_source",
            "middle_phase",
            "left_delay",
            "left_balance",
            "left_gain",
            "left_phase",
            "right_delay",
            "right_balance",
            "right_gain",
            "right_phase",
        ],
    ),
    (
        "stereotools",
        &[
            "level_in",
            "level_out",
            "balance_in",
            "balance_out",
            "softclip",
            "mutel",
            "muter",
            "phasel",
            "phaser",
            "mode",
            "slev",
            "sbal",
            "mlev",
            "mpan",
            "base",
            "delay",
            "sclevel",
            "phase",
            "bmode_in",
            "bmode_out",
        ],
    ),
    ("stereowiden", &["delay", "feedback", "crossfeed", "drymix"]),
    ("tremolo", &["f", "d"]),
    ("vibrato", &["f", "d"]),
    ("virtualbass", &["cutoff", "strength"]),
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

    /// A name the reference does not document at all for `axcorrelate` --
    /// exactly the case `Instantiate::named` alone could not distinguish
    /// from a real-but-unimplemented option before this fix.
    #[test]
    fn an_unrecognised_option_name_is_a_named_error() {
        let arguments = [arg("not_a_real_option", "1")];
        let err =
            ensure_known_options(&req("axcorrelate", Some("not_a_real_option=1"), &arguments))
                .unwrap_err();
        assert!(
            err.contains("axcorrelate") && err.contains("not_a_real_option"),
            "unexpected error text: {err}"
        );
    }

    /// A real, implemented option -- unaffected.
    #[test]
    fn an_implemented_option_is_accepted() {
        let arguments = [arg("size", "1")];
        assert!(ensure_known_options(&req("axcorrelate", Some("size=1"), &arguments)).is_ok());
    }

    /// A real reference option for `axcorrelate` this crate has not wired up
    /// internally -- the case loose parsing exists to keep working.
    #[test]
    fn a_real_but_unimplemented_option_is_still_accepted() {
        let arguments = [arg("algo", "1024")];
        assert!(ensure_known_options(&req("axcorrelate", Some("algo=1024"), &arguments)).is_ok());
    }

    /// A filter name not in `KNOWN_OPTIONS` at all is not this function's
    /// business -- the registry's own dispatch handles that.
    #[test]
    fn an_unregistered_filter_name_is_not_this_functions_business() {
        assert!(ensure_known_options(&req("not-a-real-filter", None, &[])).is_ok());
    }
}
