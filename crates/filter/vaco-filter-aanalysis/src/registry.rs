//! [`AmeasureRegistry`] — the [`FilterRegistry`] this crate's eleven
//! filters answer through. Mirrors `vaco-filter-audio::registry::
//! AudioRegistry`'s shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

use crate::common;

/// Every name this crate answers to, alphabetical as `ffmpeg -filters`
/// lists them. Plan 16 §4.3's `vaco-filter-aanalysis` row names fourteen
/// (`astats`, `aspectralstats`, `ebur128`, `drmeter`, `silencedetect`,
/// `replaygain`, `apsnr`, `asdr`, `asisdr`, `axcorrelate`, `aderivative`,
/// `aintegral`, `ashowinfo`, `aphasemeter`); this crate registers eleven of
/// them. `astats` and `silencedetect` are excluded because
/// `vaco-filter-adynamics` already registers both. `axcorrelate` is
/// excluded because `vaco-filter-achannel` (FT-4.13b, GitHub #482, landed
/// first) already registers it too — a genuine cross-issue overlap (two
/// work packages independently listed the same filter), not a mistake
/// either agent could see coming without cross-referencing the other's
/// fragment. See `docs/filter/vaco-filter-aanalysis.md` for the full
/// reconciliation, including where GitHub #483's own suggested list was
/// wrong in other ways.
const NAMES: &[&str] = &[
    "aderivative",
    "aintegral",
    "aphasemeter",
    "apsnr",
    "asdr",
    "asisdr",
    "aspectralstats",
    "ashowinfo",
    "drmeter",
    "ebur128",
    "replaygain",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AmeasureRegistry;

impl FilterRegistry for AmeasureRegistry {
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
            "aderivative" => crate::aderivative::create(req),
            "aintegral" => crate::aintegral::create(req),
            "aphasemeter" => crate::aphasemeter::create(req),
            "apsnr" => crate::apsnr::create(req),
            "asdr" => crate::asdr::create(req),
            "asisdr" => crate::asisdr::create(req),
            "aspectralstats" => crate::aspectralstats::create(req),
            "ashowinfo" => crate::ashowinfo::create(req),
            "drmeter" => crate::drmeter::create(req),
            "ebur128" => crate::ebur128::create(req),
            "replaygain" => crate::replaygain::create(req),
            other => return Err(format!("vaco-filter-aanalysis: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = AmeasureRegistry;
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
        let registry = AmeasureRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
