//! [`V360Registry`] — the `FilterRegistry` this crate answers through,
//! same shape as every other single-filter-family crate in this tree.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["v360"];

#[derive(Debug, Clone, Copy, Default)]
pub struct V360Registry;

impl FilterRegistry for V360Registry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "v360" => crate::v360::create(req),
            other => Err(format!("vaco-filter-v360: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = V360Registry;
        for &name in NAMES {
            let req = Instantiate { name, instance: name, args: None, arguments: &[] };
            assert!(registry.create(&req).is_ok(), "{name} should be creatable with defaults");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = V360Registry;
        let req = Instantiate { name: "not-a-real-filter", instance: "not-a-real-filter", args: None, arguments: &[] };
        assert!(registry.create(&req).is_err());
    }
}
