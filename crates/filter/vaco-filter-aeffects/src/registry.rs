//! [`AeffectsRegistry`] — the [`FilterRegistry`] this crate's seven filters
//! answer through. Mirrors `vaco-filter-aeq::registry::EqRegistry`'s
//! shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

use crate::common;

/// Every name this crate answers to, alphabetical as `ffmpeg -filters` lists
/// them.
const NAMES: &[&str] = &[
    "adelay",
    "aecho",
    "aexciter",
    "aphaser",
    "apulsator",
    "atempo",
    "axcorrelate",
    "chorus",
    "compensationdelay",
    "crossfeed",
    "crystalizer",
    "dcshift",
    "deesser",
    "dialoguenhance",
    "earwax",
    "extrastereo",
    "flanger",
    "haas",
    "stereotools",
    "stereowiden",
    "tremolo",
    "vibrato",
    "virtualbass",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AeffectsRegistry;

impl FilterRegistry for AeffectsRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all for this filter, before dispatching -- see
        // `common::ensure_known_options`'s own doc for what it
        // deliberately still tolerates (a real, if unimplemented,
        // reference option).
        common::ensure_known_options(req)?;
        Ok(match req.name {
            "adelay" => crate::adelay::create(req),
            "aecho" => crate::aecho::create(req),
            "aexciter" => crate::aexciter::create(req),
            "aphaser" => crate::aphaser::create(req),
            "apulsator" => crate::apulsator::create(req),
            "atempo" => crate::atempo::create(req),
            "axcorrelate" => crate::axcorrelate::create(req),
            "chorus" => crate::chorus::create(req),
            "compensationdelay" => crate::compensationdelay::create(req),
            "crossfeed" => crate::crossfeed::create(req),
            "crystalizer" => crate::crystalizer::create(req),
            "dcshift" => crate::dcshift::create(req),
            "deesser" => crate::deesser::create(req),
            "dialoguenhance" => crate::dialoguenhance::create(req),
            "earwax" => crate::earwax::create(req),
            "extrastereo" => crate::extrastereo::create(req),
            "flanger" => crate::flanger::create(req),
            "haas" => crate::haas::create(req),
            "stereotools" => crate::stereotools::create(req),
            "stereowiden" => crate::stereowiden::create(req),
            "tremolo" => crate::tremolo::create(req),
            "vibrato" => crate::vibrato::create(req),
            "virtualbass" => crate::virtualbass::create(req),
            other => return Err(format!("vaco-filter-aeffects: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = AeffectsRegistry;
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
        let registry = AeffectsRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
