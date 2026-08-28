//! [`MotionRegistry`] — the `FilterRegistry` this crate's filters answer
//! through, same shape as `vaco-filter-artistic::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["framerate", "deshake"];

#[derive(Debug, Clone, Copy, Default)]
pub struct MotionRegistry;

impl FilterRegistry for MotionRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "framerate" => crate::framerate::create(req),
            "deshake" => crate::deshake::create(req),
            other => Err(format!("vaco-filter-motion: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = MotionRegistry;
        for &name in NAMES {
            let req = Instantiate { name, instance: name, args: None, arguments: &[] };
            assert!(registry.create(&req).is_ok(), "{name} should be creatable with defaults");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = MotionRegistry;
        let req = Instantiate { name: "not-a-real-filter", instance: "not-a-real-filter", args: None, arguments: &[] };
        assert!(registry.create(&req).is_err());
    }
}
