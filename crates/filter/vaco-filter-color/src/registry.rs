//! [`ColorRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as the sibling filter crates.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] =
    &["colorchannelmixer", "colorlevels", "lut", "lut2", "lutrgb", "lutyuv", "pseudocolor"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorRegistry;

impl FilterRegistry for ColorRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "colorchannelmixer" => crate::colorchannelmixer::create(req),
            "colorlevels" => crate::colorlevels::create(req),
            "lut" => crate::lut::lut::create(req),
            "lutrgb" => crate::lut::lutrgb::create(req),
            "lutyuv" => crate::lut::lutyuv::create(req),
            "lut2" => crate::lut2::create(req),
            "pseudocolor" => crate::pseudocolor::create(req),
            other => Err(format!("vaco-filter-color: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = ColorRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
