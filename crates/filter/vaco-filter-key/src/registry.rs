//! [`KeyRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as the sibling filter crates.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &["maskedmerge", "premultiply", "unpremultiply"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyRegistry;

impl FilterRegistry for KeyRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "maskedmerge" => crate::maskedmerge::create(req),
            "premultiply" => crate::premultiply::premultiply::create(req),
            "unpremultiply" => crate::premultiply::unpremultiply::create(req),
            other => Err(format!("vaco-filter-key: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = KeyRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
