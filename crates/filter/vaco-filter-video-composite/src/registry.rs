//! [`CompositeRegistry`] — the [`FilterRegistry`] this crate's two filters
//! answer through. Same shape as `vaco-filter-video-geometry::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to.
const NAMES: &[&str] = &["overlay", "rotate"];

/// Implements [`FilterRegistry`] for `overlay` and `rotate`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompositeRegistry;

impl FilterRegistry for CompositeRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "overlay" => crate::overlay::create(req),
            "rotate" => crate::rotate::create(req),
            other => Err(format!(
                "vaco-filter-video-composite: no filter named `{other}`"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = CompositeRegistry;
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
        let registry = CompositeRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
