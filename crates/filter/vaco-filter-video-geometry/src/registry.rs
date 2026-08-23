//! [`GeometryRegistry`] — the [`FilterRegistry`] this crate's six filters
//! answer through. Same shape as `vaco-filter-audio::registry` /
//! `vaco-filter-plumbing::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &["crop", "hflip", "pad", "scale", "transpose", "vflip"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeometryRegistry;

impl FilterRegistry for GeometryRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "crop" => crate::crop::create(req),
            "hflip" => crate::flip::hflip::create(req),
            "pad" => crate::pad::create(req),
            "scale" => crate::scale::create(req),
            "transpose" => crate::transpose::create(req),
            "vflip" => crate::flip::vflip::create(req),
            other => Err(format!(
                "vaco-filter-video-geometry: no filter named `{other}`"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = GeometryRegistry;
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
        let registry = GeometryRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
