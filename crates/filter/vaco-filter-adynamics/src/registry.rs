//! [`DynamicsRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through, mirroring `vaco-filter-audio::registry::AudioRegistry`'s shape.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "acompressor",
    "acrusher",
    "adrc",
    "adynamicequalizer",
    "adynamicsmooth",
    "agate",
    "alimiter",
    "apsyclip",
    "asoftclip",
    "astats",
    "compand",
    "dynaudnorm",
    "loudnorm",
    "mcompand",
    "sidechaincompress",
    "sidechaingate",
    "silencedetect",
    "silenceremove",
    "speechnorm",
    "volumedetect",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct DynamicsRegistry;

impl FilterRegistry for DynamicsRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        Ok(match req.name {
            "acompressor" => crate::acompressor::create(req),
            "acrusher" => crate::acrusher::create(req),
            "adrc" => crate::adrc::create(req),
            "adynamicequalizer" => crate::adynamicequalizer::create(req),
            "adynamicsmooth" => crate::adynamicsmooth::create(req),
            "agate" => crate::agate::create(req),
            "alimiter" => crate::alimiter::create(req),
            "apsyclip" => crate::apsyclip::create(req),
            "asoftclip" => crate::asoftclip::create(req),
            "astats" => crate::astats::create(req),
            "compand" => crate::compand::create(req),
            "dynaudnorm" => crate::dynaudnorm::create(req),
            "loudnorm" => crate::loudnorm::create(req),
            "mcompand" => crate::mcompand::create(req),
            "sidechaincompress" => crate::sidechaincompress::create(req),
            "sidechaingate" => crate::sidechaingate::create(req),
            "silencedetect" => crate::silencedetect::create(req),
            "silenceremove" => crate::silenceremove::create(req),
            "speechnorm" => crate::speechnorm::create(req),
            "volumedetect" => crate::volumedetect::create(req),
            other => {
                return Err(format!("vaco-filter-adynamics: no filter named `{other}`"));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = DynamicsRegistry;
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
        let registry = DynamicsRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
