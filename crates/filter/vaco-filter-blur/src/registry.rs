//! [`BlurRegistry`] — the [`FilterRegistry`] this crate's four filters
//! answer through. Same shape as `vaco-filter-convolve::registry`.
//!
//! `planning/16-filters.md` §4.2 assigns eleven names to this crate:
//! `unsharp, cas, avgblur, gblur, dblur, varblur, yaepblur, guided,
//! boxblur, smartblur, sab`. Four are implemented — `avgblur`, `boxblur`,
//! `gblur`, `unsharp` — and registered below. `cas`, `dblur`, `guided`,
//! `sab`, `smartblur`, `varblur`, `yaepblur` are left for a follow-up (see
//! the crate's own top-level doc and `docs/filter/vaco-filter-blur.md`);
//! nothing this project's dup-check/registry tooling can see would be
//! satisfied by registering a name with no `create` function behind it, so
//! they are simply absent rather than stubbed.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &["avgblur", "boxblur", "gblur", "unsharp"];

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
            "gblur" => crate::gblur::create(req),
            "unsharp" => crate::unsharp::create(req),
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
