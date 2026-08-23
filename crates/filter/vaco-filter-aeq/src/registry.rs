//! [`EqRegistry`] — the [`FilterRegistry`] this crate's fifteen filters
//! answer through. Mirrors `vaco-filter-audio::registry::AudioRegistry`'s
//! shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// Every name this crate answers to, alphabetical as `ffmpeg -filters` lists
/// them. Fifteen: the twelve biquad-family filters from `af_biquads.c`
/// (`equalizer`, `bass`, `lowshelf`, `treble`, `highshelf`, `tiltshelf`,
/// `highpass`, `lowpass`, `bandpass`, `bandreject`, `allpass`, `biquad`) plus
/// `anequalizer`, `firequalizer`, `superequalizer` — GitHub #471's actual
/// scope, which is thirteen names wider than this work package's brief
/// (missing `tiltshelf` and `firequalizer`); see
/// `docs/filter/vaco-filter-aeq.md` for the reconciliation.
const NAMES: &[&str] = &[
    "allpass",
    "anequalizer",
    "bandpass",
    "bandreject",
    "bass",
    "biquad",
    "equalizer",
    "firequalizer",
    "highpass",
    "highshelf",
    "lowpass",
    "lowshelf",
    "superequalizer",
    "tiltshelf",
    "treble",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct EqRegistry;

impl FilterRegistry for EqRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        Ok(match req.name {
            "allpass" => crate::allpass::create(req),
            "anequalizer" => crate::anequalizer::create(req),
            "bandpass" => crate::bandpass::create(req),
            "bandreject" => crate::bandreject::create(req),
            "bass" => crate::bass::create(req),
            "biquad" => crate::biquad::create(req),
            "equalizer" => crate::equalizer::create(req),
            "firequalizer" => crate::firequalizer::create(req),
            "highpass" => crate::highpass::create(req),
            "highshelf" => crate::highshelf::create(req),
            "lowpass" => crate::lowpass::create(req),
            "lowshelf" => crate::lowshelf::create(req),
            "superequalizer" => crate::superequalizer::create(req),
            "tiltshelf" => crate::tiltshelf::create(req),
            "treble" => crate::treble::create(req),
            other => return Err(format!("vaco-filter-aeq: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = EqRegistry;
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
        let registry = EqRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
