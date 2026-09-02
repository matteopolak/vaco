//! [`PaletteRegistry`] — the `FilterRegistry` this crate's filters answer
//! through, same shape as `vaco-filter-artistic::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["palettegen", "paletteuse", "elbg"];

#[derive(Debug, Clone, Copy, Default)]
pub struct PaletteRegistry;

impl FilterRegistry for PaletteRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "palettegen" => crate::palettegen::create(req),
            "paletteuse" => crate::paletteuse::create(req),
            "elbg" => crate::elbg::create(req),
            other => Err(format!("vaco-filter-palette: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = PaletteRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            assert!(
                registry.create(&req).is_ok(),
                "{name} should be creatable with defaults"
            );
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = PaletteRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
