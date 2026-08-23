//! [`LutRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as the sibling filter crates.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &["haldclut", "haldclutsrc", "lut1d", "lut3d"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct LutRegistry;

impl FilterRegistry for LutRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "haldclut" => crate::haldclut::create(req),
            "haldclutsrc" => crate::haldclutsrc::create(req),
            "lut1d" => crate::lut1d::create(req),
            "lut3d" => crate::lut3d::create(req),
            other => Err(format!("vaco-filter-lut: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = LutRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
