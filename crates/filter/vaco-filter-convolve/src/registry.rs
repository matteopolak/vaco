//! [`ConvolveRegistry`] — the [`FilterRegistry`] this crate's twelve
//! filters answer through. Same shape as `vaco-filter-blur::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "convolution",
    "deflate",
    "dilation",
    "erosion",
    "inflate",
    "kirsch",
    "median",
    "morpho",
    "prewitt",
    "roberts",
    "scharr",
    "sobel",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConvolveRegistry;

impl FilterRegistry for ConvolveRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "convolution" => crate::convolution::create(req),
            "deflate" => crate::deflate::create(req),
            "dilation" => crate::dilation::create(req),
            "erosion" => crate::erosion::create(req),
            "inflate" => crate::inflate::create(req),
            "kirsch" => crate::kirsch::create(req),
            "median" => crate::median::create(req),
            "morpho" => crate::morpho::create(req),
            "prewitt" => crate::prewitt::create(req),
            "roberts" => crate::roberts::create(req),
            "scharr" => crate::scharr::create(req),
            "sobel" => crate::sobel::create(req),
            other => Err(format!("vaco-filter-convolve: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = ConvolveRegistry;
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
        let registry = ConvolveRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
