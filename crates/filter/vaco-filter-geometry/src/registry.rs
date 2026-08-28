//! [`T2GeometryRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as `vaco-filter-audio::registry` /
//! `vaco-filter-video-geometry::registry`.

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
const KNOWN_OPTIONS: &[(&str, &[&str])] =
    &[("alphaextract", &[]), ("alphamerge", &[]), ("swapuv", &[])];

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

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "alphaextract",
    "alphamerge",
    "extractplanes",
    "field",
    "fillborders",
    "framepack",
    "il",
    "mergeplanes",
    "perspective",
    "pixelize",
    "scroll",
    "shuffleframes",
    "shuffleplanes",
    "swaprect",
    "swapuv",
    "tile",
    "untile",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct T2GeometryRegistry;

impl FilterRegistry for T2GeometryRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        match req.name {
            "alphaextract" => crate::alphaextract::create(req),
            "alphamerge" => crate::alphamerge::create(req),
            "extractplanes" => crate::extractplanes::create(req),
            "field" => crate::field::create(req),
            "fillborders" => crate::fillborders::create(req),
            "framepack" => crate::framepack::create(req),
            "il" => crate::il::create(req),
            "mergeplanes" => crate::mergeplanes::create(req),
            "perspective" => crate::perspective::create(req),
            "pixelize" => crate::pixelize::create(req),
            "scroll" => crate::scroll::create(req),
            "shuffleframes" => crate::shuffleframes::create(req),
            "shuffleplanes" => crate::shuffleplanes::create(req),
            "swaprect" => crate::swaprect::create(req),
            "swapuv" => crate::swapuv::create(req),
            "tile" => crate::tile::create(req),
            "untile" => crate::untile::create(req),
            other => Err(format!("vaco-filter-geometry: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = T2GeometryRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            let _ = registry.create(&req);
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = T2GeometryRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = T2GeometryRegistry;
        let src = "swapuv=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "swapuv",
            instance: "swapuv",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }
}
