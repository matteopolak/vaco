//! [`MmRegistry`] — the [`FilterRegistry`] this crate's filters
//! answer through. See `vaco-filter-audio::registry` for why this pattern
//! (one dispatching `FilterRegistry` per leaf crate) is what stands in for
//! an aggregator that does not exist yet.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "acopy",
    "aloop",
    "ametadata",
    "anull",
    "anullsink",
    "anullsrc",
    "aselect",
    "asettb",
    "asetpts",
    "asplit",
    "atrim",
    "color",
    "concat",
    "copy",
    "loop",
    "metadata",
    "null",
    "nullsink",
    "nullsrc",
    "select",
    "settb",
    "setpts",
    "split",
    "trim",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct MmRegistry;

impl FilterRegistry for MmRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "acopy" => crate::passthrough::acopy::create(req),
            "aloop" => crate::looping::audio::create(req),
            "ametadata" => crate::metadata::audio::create(req),
            "anull" => crate::passthrough::anull::create(req),
            "anullsink" => crate::nullsink::audio::create(req),
            "anullsrc" => crate::nullsrc::audio::create(req),
            "aselect" => crate::select::audio::create(req),
            "asettb" => crate::settb::audio::create(req),
            "asetpts" => crate::setpts::audio::create(req),
            "asplit" => crate::split::audio::create(req),
            "atrim" => crate::trim::audio::create(req),
            "color" => crate::color::create(req),
            "concat" => crate::concat::create(req),
            "copy" => crate::passthrough::copy::create(req),
            "loop" => crate::looping::video::create(req),
            "metadata" => crate::metadata::video::create(req),
            "null" => crate::passthrough::null::create(req),
            "nullsink" => crate::nullsink::video::create(req),
            "nullsrc" => crate::nullsrc::video::create(req),
            "select" => crate::select::video::create(req),
            "settb" => crate::settb::video::create(req),
            "setpts" => crate::setpts::video::create(req),
            "split" => crate::split::video::create(req),
            "trim" => crate::trim::video::create(req),
            other => Err(format!("vaco-filter-mm: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = MmRegistry;
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
        let registry = MmRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
