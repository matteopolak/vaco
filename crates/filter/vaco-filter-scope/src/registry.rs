//! [`ScopeRegistry`] — the [`FilterRegistry`] this crate's filters answer
//! through. Same shape as `vaco-filter-artistic::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &[
    "histogram",
    "waveform",
    "datascope",
    "thistogram",
    "graphmonitor",
    "agraphmonitor",
    "pixscope",
    "drawgraph",
    "adrawgraph",
    "vectorscope",
    "oscilloscope",
    "ciescope",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScopeRegistry;

impl FilterRegistry for ScopeRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "histogram" => crate::histogram::create(req),
            "waveform" => crate::waveform::create(req),
            "datascope" => crate::datascope::create(req),
            "thistogram" => crate::thistogram::create(req),
            "graphmonitor" => crate::graphmonitor::create(req),
            "agraphmonitor" => crate::graphmonitor::create_audio(req),
            "pixscope" => crate::pixscope::create(req),
            "drawgraph" => crate::drawgraph::create(req),
            "adrawgraph" => crate::drawgraph::create_audio(req),
            "vectorscope" => crate::vectorscope::create(req),
            "oscilloscope" => crate::oscilloscope::create(req),
            "ciescope" => crate::ciescope::create(req),
            other => Err(format!("vaco-filter-scope: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = ScopeRegistry;
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
        let registry = ScopeRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
