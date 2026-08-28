//! [`DeinterlaceRegistry`] — the [`FilterRegistry`] this crate's twenty
//! filters answer through. Mirrors `vaco-filter-temporal::registry::
//! TemporalRegistry`'s shape exactly.

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
    ("repeatfields", &[]),
    ("separatefields", &[]),
    ("vfrdet", &[]),
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

/// Every name this crate registers, alphabetical (all twenty checked
/// against `ffmpeg -hide_banner -filters`, ffmpeg 8.1, 2026-08-23).
const NAMES: &[&str] = &[
    "bwdif",
    "detelecine",
    "doubleweave",
    "estdif",
    "fieldhint",
    "fieldmatch",
    "fieldorder",
    "idet",
    "interlace",
    "kerndeint",
    "phase",
    "pullup",
    "repeatfields",
    "separatefields",
    "telecine",
    "tinterlace",
    "vfrdet",
    "w3fdif",
    "weave",
    "yadif",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeinterlaceRegistry;

impl FilterRegistry for DeinterlaceRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        Ok(match req.name {
            "bwdif" => crate::bwdif::create(req)?,
            "detelecine" => crate::detelecine::create(req)?,
            "doubleweave" => crate::weave::create_doubleweave(req)?,
            "estdif" => crate::estdif::create(req)?,
            "fieldhint" => crate::fieldhint::create(req)?,
            "fieldmatch" => crate::fieldmatch::create(req)?,
            "fieldorder" => crate::fieldorder::create(req)?,
            "idet" => crate::idet::create(req)?,
            "interlace" => crate::interlace::create(req)?,
            "kerndeint" => crate::kerndeint::create(req)?,
            "phase" => crate::phase::create(req)?,
            "pullup" => crate::pullup::create(req)?,
            "repeatfields" => crate::repeatfields::create(req),
            "separatefields" => crate::separatefields::create(req),
            "telecine" => crate::telecine::create(req)?,
            "tinterlace" => crate::tinterlace::create(req)?,
            "vfrdet" => crate::vfrdet::create(req),
            "w3fdif" => crate::w3fdif::create(req)?,
            "weave" => crate::weave::create_weave(req)?,
            "yadif" => crate::yadif::create(req)?,
            other => {
                return Err(format!(
                    "vaco-filter-deinterlace: no filter named `{other}`"
                ));
            }
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
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
        let registry = DeinterlaceRegistry;
        for &name in NAMES {
            // `fieldhint` genuinely requires a `hint` file (see its module
            // doc) and is expected to fail cleanly without one.
            if name == "fieldhint" {
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
        let registry = DeinterlaceRegistry;
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = DeinterlaceRegistry;
        let src = "vfrdet=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "vfrdet",
            instance: "vfrdet",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }
}
