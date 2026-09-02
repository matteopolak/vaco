//! [`AsourceRegistry`] — the [`FilterRegistry`] this crate's filters
//! answer through. Same shape as `vaco-filter-source::registry` and every
//! other sibling filter crate.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "sine",
    "anoisesrc",
    "aevalsrc",
    "afdelaysrc",
    "sinc",
    "hilbert",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsourceRegistry;

impl FilterRegistry for AsourceRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "sine" => crate::sine::create(req),
            "anoisesrc" => crate::anoisesrc::create(req),
            "aevalsrc" => crate::aevalsrc::create(req),
            "afdelaysrc" => crate::afdelaysrc::create(req),
            "sinc" => crate::sinc::create(req),
            "hilbert" => crate::hilbert::create(req),
            other => Err(format!("vaco-filter-asource: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        // `aevalsrc` is the one exception: the reference itself refuses it
        // with no `exprs` (measured: `aevalsrc` with no arguments errors
        // against ffmpeg 8.1 too), so requiring an explicit expression here
        // matches the reference rather than diverging from it. See
        // `aevalsrc.rs`'s own tests for its creatable-with-arguments case.
        let registry = AsourceRegistry;
        for &name in NAMES {
            if name == "aevalsrc" {
                continue;
            }
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            assert!(registry.create(&req).is_ok(), "failed to create `{name}`");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = AsourceRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
