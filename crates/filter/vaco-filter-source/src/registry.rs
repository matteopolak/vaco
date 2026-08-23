//! [`GeneratorRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as the sibling filter crates (see
//! `vaco-filter-plumbing::registry` for why one dispatching registry per
//! leaf crate, rather than an aggregator, is the standing pattern).

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "allrgb",
    "allyuv",
    "cellauto",
    "colorchart",
    "colorspectrum",
    "gradients",
    "life",
    "mandelbrot",
    "perlin",
    "rgbtestsrc",
    "sierpinski",
    "smptebars",
    "smptehdbars",
    "yuvtestsrc",
    "zoneplate",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeneratorRegistry;

impl FilterRegistry for GeneratorRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "allrgb" => crate::allrgb::create(req),
            "allyuv" => crate::allyuv::create(req),
            "cellauto" => crate::cellauto::create(req),
            "colorchart" => crate::colorchart::create(req),
            "colorspectrum" => crate::colorspectrum::create(req),
            "gradients" => crate::gradients::create(req),
            "life" => crate::life::create(req),
            "mandelbrot" => crate::mandelbrot::create(req),
            "perlin" => crate::perlin::create(req),
            "rgbtestsrc" => crate::rgbtestsrc::create(req),
            "sierpinski" => crate::sierpinski::create(req),
            "smptebars" => crate::bars::sd::create(req),
            "smptehdbars" => crate::bars::hd::create(req),
            "yuvtestsrc" => crate::yuvtestsrc::create(req),
            "zoneplate" => crate::zoneplate::create(req),
            other => Err(format!("vaco-filter-source: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = GeneratorRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            assert!(registry.create(&req).is_ok(), "failed to create `{name}`");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = GeneratorRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
