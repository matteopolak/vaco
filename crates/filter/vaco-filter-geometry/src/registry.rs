//! [`T2GeometryRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as `vaco-filter-audio::registry` /
//! `vaco-filter-video-geometry::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "alphaextract",
    "field",
    "fillborders",
    "il",
    "perspective",
    "pixelize",
    "scroll",
    "shuffleframes",
    "shuffleplanes",
    "swaprect",
    "swapuv",
    "tile",
    "untile",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct T2GeometryRegistry;

impl FilterRegistry for T2GeometryRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "alphaextract" => crate::alphaextract::create(req),
            "field" => crate::field::create(req),
            "fillborders" => crate::fillborders::create(req),
            "il" => crate::il::create(req),
            "perspective" => crate::perspective::create(req),
            "pixelize" => crate::pixelize::create(req),
            "scroll" => crate::scroll::create(req),
            "shuffleframes" => crate::shuffleframes::create(req),
            "shuffleplanes" => crate::shuffleplanes::create(req),
            "swaprect" => crate::swaprect::create(req),
            "swapuv" => crate::swapuv::create(req),
            "tile" => crate::tile::create(req),
            "untile" => crate::untile::create(req),
            other => Err(format!("vaco-filter-geometry: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = T2GeometryRegistry;
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
        let registry = T2GeometryRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
