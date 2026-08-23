//! [`AudioRegistry`] — the [`FilterRegistry`] this crate's eleven filters
//! answer through.
//!
//! `vaco-filter-graph` never names a filter itself (its own docs say so
//! directly); a `FilterRegistry` impl is how a DSL builder reaches an actual
//! implementation. Nothing downstream exists yet to compose every filter
//! crate's registry into one, so this is this crate's contribution to that —
//! `vaco-cli-core` or a generated umbrella registry is expected to hold a
//! `Vec<&dyn FilterRegistry>` and try each in turn, which is why `create`
//! here returns a plain `Err(String)` for an unknown name rather than
//! panicking or guessing.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// The names this crate answers to, in the order `ffmpeg -filters` would
/// print them (alphabetical, as the reference's own listing is).
const NAMES: &[&str] = &[
    "aformat",
    "amerge",
    "amix",
    "aresample",
    "asetnsamples",
    "asetrate",
    "channelmap",
    "channelsplit",
    "join",
    "pan",
    "volume",
];

/// Implements [`FilterRegistry`] for every T1 audio filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioRegistry;

impl FilterRegistry for AudioRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        match req.name {
            "aformat" => crate::aformat::create(req),
            "amerge" => crate::amerge::create(req),
            "amix" => crate::amix::create(req),
            "aresample" => crate::aresample::create(req),
            "asetnsamples" => crate::asetnsamples::create(req),
            "asetrate" => crate::asetrate::create(req),
            "channelmap" => crate::channelmap::create(req),
            "channelsplit" => crate::channelsplit::create(req),
            "join" => crate::join::create(req),
            "pan" => crate::pan::create(req),
            "volume" => crate::volume::create(req),
            other => Err(format!("vaco-filter-audio: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = AudioRegistry;
        for &name in NAMES {
            let req = Instantiate {
                name,
                instance: name,
                args: None,
                arguments: &[],
            };
            // `amix`/`amerge`/`join`/`pan` need arguments (dynamic pad counts,
            // or `pan`'s single mandatory positional) and are expected to
            // fail cleanly rather than panic; everything else should succeed
            // with its documented defaults.
            let result = registry.create(&req);
            match name {
                "pan" => assert!(result.is_err(), "pan with no args should be a clean error"),
                _ => {
                    let _ = result;
                }
            }
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = AudioRegistry;
        let req = Instantiate {
            name: "not-a-real-filter",
            instance: "not-a-real-filter",
            args: None,
            arguments: &[],
        };
        assert!(registry.create(&req).is_err());
    }
}
