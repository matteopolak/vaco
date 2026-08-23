//! [`DenoiseRegistry`] — the [`FilterRegistry`] this crate's eight
//! implemented filters answer through. Mirrors
//! `vaco-filter-audio-eq::registry::EqRegistry`'s shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

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
        Ok(match req.name {
            "atadenoise" => crate::atadenoise::create(req),
            "dctdnoiz" => crate::dctdnoiz::create(req),
            "fftdnoiz" => crate::fftdnoiz::create(req),
            "hqdn3d" => crate::hqdn3d::create(req),
            "nlmeans" => crate::nlmeans::create(req),
            "owdenoise" => crate::owdenoise::create(req),
            "removegrain" => crate::removegrain::create(req),
            "vaguedenoiser" => crate::vaguedenoiser::create(req),
            other => return Err(format!("vaco-filter-denoise: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
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
}
