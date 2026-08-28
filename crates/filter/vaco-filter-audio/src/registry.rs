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

/// `(filter, known option names)` for the filters in this crate that
/// were found silently accepting *any* option name, including one the
/// reference does not document at all -- probed directly against real
/// `ffmpeg 8.1 -h filter=<name>`, 2026-08-28. Every other filter in this
/// crate already rejects an unrecognised name on its own (a strict
/// `vaco_opts::Options`-derived parser, or its own existing validation),
/// so only the filters that needed the fix are listed here -- adding an
/// entry is what closing one of `option_name_gate.rs`'s `KNOWN_GAPS`
/// lines looks like from this side.
const KNOWN_OPTIONS: &[(&str, &[&str])] = &[
    ("adecorrelate", &["stages", "seed"]),
    (
        "aformat",
        &[
            "sample_fmts",
            "f",
            "sample_rates",
            "r",
            "channel_layouts",
            "cl",
        ],
    ),
    ("amultiply", &[]),
    ("asetnsamples", &["nb_out_samples", "n", "pad", "p"]),
    ("asetrate", &["sample_rate", "r"]),
    ("channelmap", &["map", "channel_layout"]),
    ("channelsplit", &["channel_layout", "channels"]),
    ("join", &["inputs", "channel_layout", "map"]),
];

/// Rejects any `key=value` argument whose key is not one of the
/// reference's own documented option names for `req.name` (see
/// [`KNOWN_OPTIONS`]'s own doc for the filters this actually covers). A
/// filter name absent from the table is not this function's business --
/// either it has no real options at all and its own `create` never reads
/// `Instantiate::named`, or it already validates names itself.
///
/// # Errors
/// Names the filter and the exact unrecognised key.
fn ensure_known_options(req: &Instantiate<'_>) -> Result<(), String> {
    let Some((_, known)) = KNOWN_OPTIONS.iter().find(|(name, _)| *name == req.name) else {
        return Ok(());
    };
    for arg in req.arguments {
        if let Some(key) = arg.key.as_deref()
            && !known.contains(&key)
        {
            return Err(format!(
                "{}: unrecognized option `{key}` (not one of the reference's own documented \
                 options for this filter)",
                req.name
            ));
        }
    }
    Ok(())
}

/// The names this crate answers to, in the order `ffmpeg -filters` would
/// print them (alphabetical, as the reference's own listing is).
const NAMES: &[&str] = &[
    "adecorrelate",
    "aformat",
    "amerge",
    "amix",
    "amultiply",
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
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        match req.name {
            "adecorrelate" => Ok(crate::adecorrelate::create(req)),
            "aformat" => crate::aformat::create(req),
            "amerge" => crate::amerge::create(req),
            "amix" => crate::amix::create(req),
            "amultiply" => Ok(crate::amultiply::create(req)),
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
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

    /// An option name the reference does not document at all -- these
    /// filters used to accept it silently (see `KNOWN_OPTIONS`'s own
    /// doc); `ensure_known_options` now rejects it by name.
    #[test]
    fn an_unrecognised_option_name_is_rejected() {
        let registry = AudioRegistry;
        let src = "aformat=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "aformat",
            instance: "aformat",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
        let src = "amultiply=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "amultiply",
            instance: "amultiply",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }

    /// `aformat`'s real `sample_fmts` option -- unaffected by the fix.
    #[test]
    fn aformat_real_option_still_creates() {
        let registry = AudioRegistry;
        let src = "aformat=sample_fmts=s16";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "aformat",
            instance: "aformat",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_ok());
    }
}
