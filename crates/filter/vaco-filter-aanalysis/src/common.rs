//! Shared option parsing and pad descriptors.
//!
//! Mirrors `vaco-filter-adynamics::common`: options are read straight off
//! [`Instantiate::named`] rather than through a strict
//! `vaco_opts::Options`-derived parser. That loose parsing exists so a
//! real reference command line setting an option this crate has not wired
//! up internally still runs, rather than hard-failing the way a strict
//! `set_from_string` would on any undeclared field. But `Instantiate::named`
//! alone cannot tell "a real option this crate has not implemented" from
//! "not a real option at all" — a typo silently ran with defaults and said
//! nothing. [`ensure_known_options`] closes that: it accepts every name the
//! reference actually documents for a filter (implemented or not,
//! preserving the original intent) and rejects anything else by name.

use vaco_core::MediaType;
use vaco_filter_core::Pad;

use vaco_filter_graph::registry::Instantiate;

/// The single named audio pad every 1-in/1-out filter in this crate uses.
pub(crate) const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// `apsnr`/`asdr`/`asisdr` all name their two inputs `input0`/`input1`
/// (`ffmpeg -h filter=apsnr`, 2026-08-23) — distinct from `axcorrelate`'s
/// own `axcorrelate0`/`axcorrelate1`, so not folded into one constant.
pub(crate) const INPUT01_PADS: &[Pad] = &[
    Pad {
        name: "input0",
        media_type: MediaType::Audio,
    },
    Pad {
        name: "input1",
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

/// Linear amplitude to dB, clamped rather than producing `-inf` for silence —
/// same convention and same floor as
/// `vaco-filter-adynamics::common::db`.
pub(crate) fn db(linear: f64) -> f64 {
    if linear.is_finite() && linear > 1e-12 {
        20.0 * linear.abs().log10()
    } else {
        -240.0
    }
}

/// Cumulative pairwise statistics between a reference signal and a signal
/// under test — the one accumulator `apsnr`, `asdr` and `asisdr` are each a
/// different closed-form reduction of. Shared because it is pure
/// bookkeeping (D19: one definition per concept); the three metrics it
/// feeds are still three distinct, independently-checked formulas.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PairStats {
    pub(crate) sum_ref_sq: f64,
    pub(crate) sum_est_sq: f64,
    pub(crate) sum_diff_sq: f64,
    pub(crate) sum_cross: f64,
    pub(crate) count: u64,
}

impl PairStats {
    pub(crate) fn observe(&mut self, reference: f64, estimate: f64) {
        let diff = reference - estimate;
        self.sum_ref_sq += reference * reference;
        self.sum_est_sq += estimate * estimate;
        self.sum_diff_sq += diff * diff;
        self.sum_cross += reference * estimate;
        self.count += 1;
    }

    /// `10*log10(peak^2 / MSE)`, `peak == 1.0` for the normalized `[-1, 1]`
    /// domain every filter in this crate decodes samples into.
    pub(crate) fn psnr_db(&self) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let mse = self.sum_diff_sq / self.count as f64;
        if mse <= 0.0 {
            return Some(f64::INFINITY);
        }
        Some(10.0 * (1.0 / mse).log10())
    }

    /// `10*log10(||reference||^2 / ||reference - estimate||^2)`.
    pub(crate) fn sdr_db(&self) -> Option<f64> {
        if self.count == 0 || self.sum_ref_sq <= 0.0 {
            return None;
        }
        if self.sum_diff_sq <= 0.0 {
            return Some(f64::INFINITY);
        }
        Some(10.0 * (self.sum_ref_sq / self.sum_diff_sq).log10())
    }

    /// Scale-invariant SDR (Le Roux et al. 2019): project the estimate onto
    /// the reference's scale first, so a pure gain change scores infinite
    /// rather than being penalised as distortion.
    pub(crate) fn si_sdr_db(&self) -> Option<f64> {
        if self.count == 0 || self.sum_ref_sq <= 0.0 {
            return None;
        }
        let alpha = self.sum_cross / self.sum_ref_sq;
        let target_energy = alpha * alpha * self.sum_ref_sq;
        let noise_energy =
            self.sum_est_sq - 2.0 * alpha * self.sum_cross + alpha * alpha * self.sum_ref_sq;
        if noise_energy <= 0.0 {
            return Some(f64::INFINITY);
        }
        if target_energy <= 0.0 {
            return None;
        }
        Some(10.0 * (target_energy / noise_energy).log10())
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
    ("aderivative", &[]),
    ("aintegral", &[]),
    (
        "aphasemeter",
        &[
            "rate",
            "r",
            "size",
            "s",
            "rc",
            "gc",
            "bc",
            "mpc",
            "video",
            "phasing",
            "tolerance",
            "t",
            "angle",
            "a",
            "duration",
            "d",
        ],
    ),
    ("apsnr", &[]),
    ("asdr", &[]),
    ("ashowinfo", &[]),
    ("asisdr", &[]),
    (
        "aspectralstats",
        &["win_size", "win_func", "overlap", "measure"],
    ),
    ("drmeter", &["length"]),
    (
        "ebur128",
        &[
            "video",
            "size",
            "meter",
            "framelog",
            "metadata",
            "peak",
            "dualmono",
            "panlaw",
            "target",
            "gauge",
            "scale",
            "integrated",
            "range",
            "lra_low",
            "lra_high",
            "sample_peak",
            "true_peak",
        ],
    ),
    ("replaygain", &["track_gain", "track_peak"]),
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

    /// A name the reference does not document at all for `drmeter` --
    /// exactly the case `Instantiate::named` alone could not distinguish
    /// from a real-but-unimplemented option before this fix.
    #[test]
    fn an_unrecognised_option_name_is_a_named_error() {
        let arguments = [arg("not_a_real_option", "1")];
        let err = ensure_known_options(&req("drmeter", Some("not_a_real_option=1"), &arguments))
            .unwrap_err();
        assert!(
            err.contains("drmeter") && err.contains("not_a_real_option"),
            "unexpected error text: {err}"
        );
    }

    /// A real, implemented option -- unaffected.
    #[test]
    fn an_implemented_option_is_accepted() {
        let arguments = [arg("length", "1")];
        assert!(ensure_known_options(&req("drmeter", Some("length=1"), &arguments)).is_ok());
    }

    /// A filter name not in `KNOWN_OPTIONS` at all is not this function's
    /// business -- the registry's own dispatch handles that.
    #[test]
    fn an_unregistered_filter_name_is_not_this_functions_business() {
        assert!(ensure_known_options(&req("not-a-real-filter", None, &[])).is_ok());
    }
}
