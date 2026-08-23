//! [`AnalysisRegistry`] — the [`FilterRegistry`] this crate's eight
//! implemented filters answer through. Mirrors
//! `vaco-filter-temporal::registry::TemporalRegistry`'s shape exactly.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// Every name this crate registers. See this crate's root doc for which of
/// plan 16 SS4.3's row landed and which did not.
const NAMES: &[&str] = &[
    "bbox",
    "blackdetect",
    "blackframe",
    "identity",
    "msad",
    "psnr",
    "signalstats",
    "ssim",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisRegistry;

impl FilterRegistry for AnalysisRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        Ok(match req.name {
            "bbox" => crate::bbox::create(req),
            "blackdetect" => crate::blackdetect::create(req),
            "blackframe" => crate::blackframe::create(req),
            "identity" => crate::identity::create(req),
            "msad" => crate::msad::create(req),
            "psnr" => crate::psnr::create(req),
            "signalstats" => crate::signalstats::create(req),
            "ssim" => crate::ssim::create(req),
            other => return Err(format!("vaco-filter-analysis: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn req(name: &'static str) -> Instantiate<'static> {
        Instantiate {
            name,
            instance: name,
            args: None,
            arguments: &[],
        }
    }

    #[test]
    fn every_registered_name_creates_without_error() {
        let registry = AnalysisRegistry;
        for &name in NAMES {
            let instance = registry.create(&req(name));
            assert!(instance.is_ok(), "{name} failed to create: {instance:?}");
        }
    }

    #[test]
    fn names_matches_what_create_accepts() {
        let registry = AnalysisRegistry;
        assert_eq!(registry.names(), NAMES.to_vec());
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }
}
