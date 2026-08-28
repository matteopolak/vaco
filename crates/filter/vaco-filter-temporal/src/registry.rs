//! [`TemporalRegistry`] — the [`FilterRegistry`] this crate's sixteen
//! implemented filters answer through. Mirrors
//! `vaco-filter-denoise::registry::DenoiseRegistry`'s shape exactly.
//! `fps` is deliberately absent: see the crate root doc.

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
    (
        "decimate",
        &[
            "cycle",
            "dupthresh",
            "scthresh",
            "blockx",
            "blocky",
            "ppsrc",
            "chroma",
            "mixed",
        ],
    ),
    ("deflicker", &["size", "s", "mode", "m", "bypass"]),
    ("dejudder", &["cycle"]),
    ("framestep", &["step"]),
    ("freezedetect", &["n", "noise", "d", "duration"]),
    ("freezeframes", &["first", "last", "replace"]),
    ("lagfun", &["decay", "planes"]),
    ("mpdecimate", &["max", "keep", "hi", "lo", "frac"]),
    ("random", &["frames", "seed"]),
    (
        "tblend",
        &[
            "c0_mode",
            "c1_mode",
            "c2_mode",
            "c3_mode",
            "all_mode",
            "c0_expr",
            "c1_expr",
            "c2_expr",
            "c3_expr",
            "all_expr",
            "c0_opacity",
            "c1_opacity",
            "c2_opacity",
            "c3_opacity",
            "all_opacity",
        ],
    ),
    ("tlut2", &["c0", "c1", "c2", "c3"]),
    ("tmedian", &["radius", "planes", "percentile"]),
    ("tmidequalizer", &["radius", "sigma", "planes"]),
    ("tmix", &["frames", "weights", "scale", "planes"]),
    (
        "tpad",
        &[
            "start",
            "stop",
            "start_mode",
            "stop_mode",
            "start_duration",
            "stop_duration",
            "color",
        ],
    ),
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

/// Every name this crate registers, alphabetical as `ffmpeg -filters` lists
/// them within this row (all sixteen checked against `ffmpeg -hide_banner
/// -filters`, ffmpeg 8.1, 2026-08-23).
const NAMES: &[&str] = &[
    "decimate",
    "deflicker",
    "dejudder",
    "framestep",
    "freezedetect",
    "freezeframes",
    "fsync",
    "lagfun",
    "mpdecimate",
    "random",
    "tblend",
    "tlut2",
    "tmedian",
    "tmidequalizer",
    "tmix",
    "tpad",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct TemporalRegistry;

impl FilterRegistry for TemporalRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        Ok(match req.name {
            "decimate" => crate::decimate::create(req),
            "deflicker" => crate::deflicker::create(req),
            "dejudder" => crate::dejudder::create(req),
            "framestep" => crate::framestep::create(req),
            "freezedetect" => crate::freezedetect::create(req),
            "freezeframes" => crate::freezeframes::create(req),
            "fsync" => crate::fsync::create(req)?,
            "lagfun" => crate::lagfun::create(req),
            "mpdecimate" => crate::mpdecimate::create(req),
            "random" => crate::random::create(req),
            "tblend" => crate::tblend::create(req)?,
            "tlut2" => crate::tlut2::create(req)?,
            "tmedian" => crate::tmedian::create(req),
            "tmidequalizer" => crate::tmidequalizer::create(req),
            "tmix" => crate::tmix::create(req),
            "tpad" => crate::tpad::create(req),
            other => return Err(format!("vaco-filter-temporal: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
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
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = TemporalRegistry;
        for &name in NAMES {
            // `fsync` genuinely requires a `file` argument (see its module
            // doc) and is expected to fail cleanly without one.
            if name == "fsync" {
                assert!(registry.create(&req(name)).is_err());
                continue;
            }
            let result = registry.create(&req(name));
            assert!(
                result.is_ok(),
                "{name} failed to create with no args: {result:?}"
            );
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = TemporalRegistry;
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }

    #[test]
    fn fps_is_deliberately_not_registered_here() {
        assert!(!TemporalRegistry.contains("fps"));
    }

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = TemporalRegistry;
        let src = "tpad=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "tpad",
            instance: "tpad",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }

    /// `tpad`'s real `start` option -- unaffected by the fix.
    #[test]
    fn tpad_real_option_still_creates() {
        let registry = TemporalRegistry;
        let src = "tpad=start=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "tpad",
            instance: "tpad",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_ok());
    }
}
