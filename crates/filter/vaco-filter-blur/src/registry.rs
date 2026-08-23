//! [`BlurRegistry`] — the [`FilterRegistry`] this crate's fourteen filters
//! answer through. Same shape as `vaco-filter-video-geometry::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "avgblur",
    "boxblur",
    "convolution",
    "dilation",
    "erosion",
    "gblur",
    "kirsch",
    "maskedclamp",
    "median",
    "prewitt",
    "roberts",
    "scharr",
    "sobel",
    "unsharp",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlurRegistry;

impl FilterRegistry for BlurRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "avgblur" => crate::avgblur::create(req),
            "boxblur" => crate::boxblur::create(req),
            "convolution" => crate::convolution::create(req),
            "dilation" => crate::dilation::create(req),
            "erosion" => crate::erosion::create(req),
            "gblur" => crate::gblur::create(req),
            "kirsch" => crate::kirsch::create(req),
            "maskedclamp" => crate::maskedclamp::create(req),
            "median" => crate::median::create(req),
            "prewitt" => crate::prewitt::create(req),
            "roberts" => crate::roberts::create(req),
            "scharr" => crate::scharr::create(req),
            "sobel" => crate::sobel::create(req),
            "unsharp" => crate::unsharp::create(req),
            other => Err(format!("vaco-filter-blur: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = BlurRegistry;
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
        let registry = BlurRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
