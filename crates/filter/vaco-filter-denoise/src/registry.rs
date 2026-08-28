//! [`DenoiseRegistry`] — the [`FilterRegistry`] this crate's eight
//! implemented filters answer through. Mirrors
//! `vaco-filter-audio-eq::registry::EqRegistry`'s shape exactly.

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
        "atadenoise",
        &[
            "0a", "0b", "1a", "1b", "2a", "2b", "s", "p", "a", "0s", "1s", "2s",
        ],
    ),
    ("dctdnoiz", &["sigma", "s", "overlap", "expr", "e", "n"]),
    (
        "fftdnoiz",
        &[
            "sigma", "amount", "block", "overlap", "method", "prev", "next", "planes", "window",
        ],
    ),
    (
        "hqdn3d",
        &["luma_spatial", "chroma_spatial", "luma_tmp", "chroma_tmp"],
    ),
    ("nlmeans", &["s", "p", "pc", "r", "rc"]),
    (
        "owdenoise",
        &["depth", "luma_strength", "ls", "chroma_strength", "cs"],
    ),
    ("removegrain", &["m0", "m1", "m2", "m3"]),
    (
        "vaguedenoiser",
        &["threshold", "method", "nsteps", "percent", "planes", "type"],
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
/// them. Nine names in the reference's own denoise group were checked
/// (`ffmpeg -hide_banner -filters`, ffmpeg 8.1); `bm3d` is not registered —
/// see [`crate::bm3d`] for why.
const NAMES: &[&str] = &[
    "atadenoise",
    "dctdnoiz",
    "fftdnoiz",
    "hqdn3d",
    "nlmeans",
    "owdenoise",
    "removegrain",
    "vaguedenoiser",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenoiseRegistry;

impl FilterRegistry for DenoiseRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        match req.name {
            "atadenoise" => Ok(crate::atadenoise::create(req)),
            "dctdnoiz" => Ok(crate::dctdnoiz::create(req)),
            "fftdnoiz" => Ok(crate::fftdnoiz::create(req)),
            "hqdn3d" => Ok(crate::hqdn3d::create(req)),
            "nlmeans" => Ok(crate::nlmeans::create(req)),
            "owdenoise" => Ok(crate::owdenoise::create(req)),
            // Fallible: `m0`..`m3` in `8..=24` are named-rejected rather
            // than silently running mode 7's clip -- see removegrain.rs.
            "removegrain" => crate::removegrain::create(req),
            "vaguedenoiser" => Ok(crate::vaguedenoiser::create(req)),
            other => Err(format!("vaco-filter-denoise: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = DenoiseRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            let result = registry.create(&req);
            assert!(
                result.is_ok(),
                "{name} failed to create with no args: {result:?}"
            );
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = DenoiseRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }

    #[test]
    fn bm3d_is_deliberately_not_registered() {
        assert!(!DenoiseRegistry.contains("bm3d"));
    }

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = DenoiseRegistry;
        let src = "hqdn3d=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "hqdn3d",
            instance: "hqdn3d",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }

    /// `hqdn3d`'s real `luma_spatial` option -- unaffected by the fix.
    #[test]
    fn hqdn3d_real_option_still_creates() {
        let registry = DenoiseRegistry;
        let src = "hqdn3d=luma_spatial=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "hqdn3d",
            instance: "hqdn3d",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_ok());
    }
}
