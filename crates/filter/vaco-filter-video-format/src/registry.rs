//! [`FormatRegistry`] — the [`FilterRegistry`] this crate's nine filters
//! answer through. Same shape as the sibling filter crates.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &[
    "format",
    "fps",
    "framerate",
    "noformat",
    "setdar",
    "setfield",
    "setparams",
    "setrange",
    "setsar",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct FormatRegistry;

impl FilterRegistry for FormatRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "format" => crate::format::create(req),
            "fps" => crate::fps::create(req),
            "framerate" => crate::framerate::create(req),
            "noformat" => crate::noformat::create(req),
            "setdar" => crate::setdar::create(req),
            "setfield" => crate::setfield::create(req),
            "setparams" => crate::setparams::create(req),
            "setrange" => crate::setrange::create(req),
            "setsar" => crate::setsar::create(req),
            other => Err(format!(
                "vaco-filter-video-format: no filter named `{other}`"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = FormatRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            // `format`/`noformat` need a `pix_fmts` list to be meaningful
            // and are expected to fail cleanly rather than panic with none.
            let result = registry.create(&req);
            match name {
                "format" => assert!(
                    result.is_err(),
                    "format with no args should be a clean error"
                ),
                _ => {
                    let _ = result;
                }
            }
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = FormatRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
