//! [`OverlayRegistry`] — the [`FilterRegistry`] this crate's filters
//! answer through. Same shape as `vaco-filter-stack::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "blend", "multiply", "mix", "xmedian", "xfade", "displace", "remap",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayRegistry;

impl FilterRegistry for OverlayRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "blend" => crate::blend::create(req),
            "multiply" => crate::multiply::create(req),
            "mix" => crate::mix::create(req),
            "xmedian" => crate::xmedian::create(req),
            "xfade" => crate::xfade::create(req),
            "displace" => crate::displace::create(req),
            "remap" => crate::remap::create(req),
            other => Err(format!("vaco-filter-overlay: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = OverlayRegistry;
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
        let registry = OverlayRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
