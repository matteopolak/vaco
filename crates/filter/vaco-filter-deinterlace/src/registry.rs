//! [`DeinterlaceRegistry`] — the [`FilterRegistry`] this crate's twenty
//! filters answer through. Mirrors `vaco-filter-temporal::registry::
//! TemporalRegistry`'s shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

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
            other => return Err(format!("vaco-filter-deinterlace: no filter named `{other}`")),
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
            assert!(result.is_ok(), "{name} failed to create with no args: {result:?}");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = DeinterlaceRegistry;
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }
}
