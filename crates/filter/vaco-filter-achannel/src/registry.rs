//! [`AchannelRegistry`] — the [`FilterRegistry`] this crate's seven filters
//! answer through. Mirrors `vaco-filter-audio-eq::registry::EqRegistry`'s
//! shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// Every name this crate answers to, alphabetical as `ffmpeg -filters` lists
/// them.
const NAMES: &[&str] = &[
    "axcorrelate",
    "crossfeed",
    "earwax",
    "extrastereo",
    "haas",
    "stereotools",
    "stereowiden",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AchannelRegistry;

impl FilterRegistry for AchannelRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        Ok(match req.name {
            "axcorrelate" => crate::axcorrelate::create(req),
            "crossfeed" => crate::crossfeed::create(req),
            "earwax" => crate::earwax::create(req),
            "extrastereo" => crate::extrastereo::create(req),
            "haas" => crate::haas::create(req),
            "stereotools" => crate::stereotools::create(req),
            "stereowiden" => crate::stereowiden::create(req),
            other => return Err(format!("vaco-filter-achannel: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = AchannelRegistry;
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
        let registry = AchannelRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
