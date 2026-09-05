//! [`BlurRegistry`] — the [`FilterRegistry`] this crate's nine filters
//! answer through. Same shape as `vaco-filter-convolve::registry`.
//!
//! `planning/16-filters.md` §4.2 assigns eleven names to this crate:
//! `unsharp, cas, avgblur, gblur, dblur, varblur, yaepblur, guided,
//! boxblur, smartblur, sab`. Ten are implemented — every name except
//! `sab` — and registered below. `sab` remains a follow-up rather than a
//! name with no `create` function behind it.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "avgblur",
    "boxblur",
    "cas",
    "dblur",
    "gblur",
    "guided",
    "smartblur",
    "unsharp",
    "varblur",
    "yaepblur",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlurRegistry;

impl FilterRegistry for BlurRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "avgblur" => crate::avgblur::create(req),
            "boxblur" => crate::boxblur::create(req),
            "cas" => crate::cas::create(req),
            "dblur" => crate::dblur::create(req),
            "gblur" => crate::gblur::create(req),
            "guided" => crate::guided::create(req),
            "smartblur" => crate::smartblur::create(req),
            "unsharp" => crate::unsharp::create(req),
            "varblur" => crate::varblur::create(req),
            "yaepblur" => crate::yaepblur::create(req),
            other => Err(format!("vaco-filter-blur: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = BlurRegistry;
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
        let registry = BlurRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
