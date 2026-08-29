//! [`SubtitleRegistry`] — the [`FilterRegistry`] this crate's filters
//! answer through. Same shape as `vaco-filter-draw-vf::registry::DrawVfRegistry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

const NAMES: &[&str] = &["ass", "subtitles"];

#[derive(Debug, Clone, Copy, Default)]
pub struct SubtitleRegistry;

impl FilterRegistry for SubtitleRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "ass" => crate::ass_filter::create(req),
            "subtitles" => crate::subtitles::create(req),
            other => Err(format!("vaco-filter-subtitle: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = SubtitleRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }

    #[test]
    fn names_lists_both_filters() {
        let registry = SubtitleRegistry;
        assert_eq!(registry.names(), vec!["ass", "subtitles"]);
    }
}
