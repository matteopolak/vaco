//! [`DrawVfRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as `vaco-filter-stack::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["drawbox", "drawgrid"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawVfRegistry;

impl FilterRegistry for DrawVfRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "drawbox" => crate::drawbox::create(req),
            "drawgrid" => crate::drawgrid::create(req),
            other => Err(format!("vaco-filter-draw-vf: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = DrawVfRegistry;
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
        let registry = DrawVfRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
