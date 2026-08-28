//! [`AnalysisRegistry`] — the [`FilterRegistry`] this crate's eight
//! implemented filters answer through. Mirrors
//! `vaco-filter-temporal::registry::TemporalRegistry`'s shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// `(filter, known option names)` for the filters in this crate that
/// were found silently accepting *any* option name, including one the
/// reference does not document at all -- probed directly against real
/// `ffmpeg 8.1 -h filter=<name>`, 2026-08-28. Every other filter in this
/// crate already rejects an unrecognised name on its own (a strict
/// `vaco_opts::Options`-derived parser, or its own existing validation),
/// so only the filters that needed the fix are listed here -- adding an
/// entry is what closing one of `option_name_gate.rs`'s `KNOWN_GAPS`
/// lines looks like from this side.
const KNOWN_OPTIONS: &[(&str, &[&str])] = &[
    ("bbox", &["min_val"]),
    (
        "blackdetect",
        &[
            "d",
            "black_min_duration",
            "picture_black_ratio_th",
            "pic_th",
            "pixel_black_th",
            "pix_th",
            "alpha",
        ],
    ),
    ("blackframe", &["amount", "threshold", "thresh"]),
    (
        "cropdetect",
        &[
            "limit",
            "round",
            "reset",
            "skip",
            "reset_count",
            "max_outliers",
            "mode",
            "high",
            "low",
            "mv_threshold",
        ],
    ),
    ("entropy", &["mode"]),
    ("identity", &[]),
    ("msad", &[]),
    ("psnr", &["stats_file", "f", "stats_version", "output_max"]),
    ("scdet", &["threshold", "t", "sc_pass", "s"]),
    ("showinfo", &["checksum", "udu_sei_as_ascii"]),
    ("signalstats", &["stat", "out", "c", "color"]),
    ("ssim", &["stats_file", "f"]),
];

/// Rejects any `key=value` argument whose key is not one of the
/// reference's own documented option names for `req.name` (see
/// [`KNOWN_OPTIONS`]'s own doc for the filters this actually covers). A
/// filter name absent from the table is not this function's business --
/// either it has no real options at all and its own `create` never reads
/// `Instantiate::named`, or it already validates names itself.
///
/// # Errors
/// Names the filter and the exact unrecognised key.
fn ensure_known_options(req: &Instantiate<'_>) -> Result<(), String> {
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

/// Every name this crate registers. See this crate's root doc for which of
/// plan 16 SS4.3's row landed and which did not.
const NAMES: &[&str] = &[
    "bbox",
    "blackdetect",
    "blackframe",
    "cropdetect",
    "entropy",
    "identity",
    "msad",
    "psnr",
    "scdet",
    "showinfo",
    "signalstats",
    "ssim",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisRegistry;

impl FilterRegistry for AnalysisRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        Ok(match req.name {
            "bbox" => crate::bbox::create(req),
            "blackdetect" => crate::blackdetect::create(req),
            "blackframe" => crate::blackframe::create(req),
            "cropdetect" => crate::cropdetect::create(req),
            "entropy" => crate::entropy::create(req),
            "identity" => crate::identity::create(req),
            "msad" => crate::msad::create(req),
            "psnr" => crate::psnr::create(req),
            "scdet" => crate::scdet::create(req),
            "showinfo" => crate::showinfo::create(req),
            "signalstats" => crate::signalstats::create(req),
            "ssim" => crate::ssim::create(req),
            other => return Err(format!("vaco-filter-analysis: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn req(name: &'static str) -> Instantiate<'static> {
        Instantiate {
            name,
            instance: name,
            args: None,
            arguments: &[],
        }
    }

    #[test]
    fn every_registered_name_creates_without_error() {
        let registry = AnalysisRegistry;
        for &name in NAMES {
            let instance = registry.create(&req(name));
            assert!(instance.is_ok(), "{name} failed to create: {instance:?}");
        }
    }

    #[test]
    fn names_matches_what_create_accepts() {
        let registry = AnalysisRegistry;
        assert_eq!(registry.names(), NAMES.to_vec());
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = AnalysisRegistry;
        let src = "cropdetect=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "cropdetect",
            instance: "cropdetect",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
        let src = "identity=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "identity",
            instance: "identity",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }

    /// `cropdetect`'s real `limit` option -- unaffected by the fix.
    #[test]
    fn cropdetect_real_option_still_creates() {
        let registry = AnalysisRegistry;
        let src = "cropdetect=limit=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "cropdetect",
            instance: "cropdetect",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_ok());
    }
}
