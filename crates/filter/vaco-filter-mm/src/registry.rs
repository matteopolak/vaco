//! [`MmRegistry`] — the [`FilterRegistry`] this crate's filters
//! answer through. See `vaco-filter-audio::registry` for why this pattern
//! (one dispatching `FilterRegistry` per leaf crate) is what stands in for
//! an aggregator that does not exist yet.

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
    ("acopy", &[]),
    ("alatency", &[]),
    ("anull", &[]),
    ("anullsink", &[]),
    ("areverse", &[]),
    ("copy", &[]),
    ("latency", &[]),
    ("null", &[]),
    ("nullsink", &[]),
    ("reverse", &[]),
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

const NAMES: &[&str] = &[
    "acopy",
    "ainterleave",
    "aloop",
    "ametadata",
    "anull",
    "anullsink",
    "anullsrc",
    "areverse",
    "abench",
    "acue",
    "alatency",
    "aperms",
    "arealtime",
    "asegment",
    "aselect",
    "asendcmd",
    "asidedata",
    "astreamselect",
    "asettb",
    "asetpts",
    "asplit",
    "atrim",
    "color",
    "concat",
    "copy",
    "interleave",
    "loop",
    "metadata",
    "null",
    "nullsink",
    "nullsrc",
    "reverse",
    "bench",
    "cue",
    "latency",
    "perms",
    "realtime",
    "segment",
    "select",
    "sendcmd",
    "sidedata",
    "streamselect",
    "settb",
    "setpts",
    "split",
    "trim",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct MmRegistry;

impl FilterRegistry for MmRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        // Rejects an option name the reference does not document at
        // all, for the filters in this crate found accepting one --
        // see `KNOWN_OPTIONS`'s own doc for which and why.
        ensure_known_options(req)?;
        match req.name {
            "acopy" => crate::passthrough::acopy::create(req),
            "ainterleave" => crate::interleave::audio::create(req),
            "aloop" => crate::looping::audio::create(req),
            "ametadata" => crate::metadata::audio::create(req),
            "anull" => crate::passthrough::anull::create(req),
            "anullsink" => crate::nullsink::audio::create(req),
            "anullsrc" => crate::nullsrc::audio::create(req),
            "areverse" => crate::reverse::audio::create(req),
            "abench" => crate::misc::bench::audio::create(req),
            "acue" => crate::misc::cue::audio::create(req),
            "alatency" => crate::misc::latency::audio::create(req),
            "aperms" => crate::misc::perms::audio::create(req),
            "arealtime" => crate::misc::realtime::audio::create(req),
            "asegment" => crate::segment::audio::create(req),
            "aselect" => crate::select::audio::create(req),
            "asendcmd" => crate::sendcmd::audio::create(req),
            "asidedata" => crate::misc::sidedata::audio::create(req),
            "astreamselect" => crate::streamselect::audio::create(req),
            "asettb" => crate::settb::audio::create(req),
            "asetpts" => crate::setpts::audio::create(req),
            "asplit" => crate::split::audio::create(req),
            "atrim" => crate::trim::audio::create(req),
            "color" => crate::color::create(req),
            "concat" => crate::concat::create(req),
            "copy" => crate::passthrough::copy::create(req),
            "interleave" => crate::interleave::video::create(req),
            "loop" => crate::looping::video::create(req),
            "metadata" => crate::metadata::video::create(req),
            "null" => crate::passthrough::null::create(req),
            "nullsink" => crate::nullsink::video::create(req),
            "nullsrc" => crate::nullsrc::video::create(req),
            "reverse" => crate::reverse::video::create(req),
            "bench" => crate::misc::bench::video::create(req),
            "cue" => crate::misc::cue::video::create(req),
            "latency" => crate::misc::latency::video::create(req),
            "perms" => crate::misc::perms::video::create(req),
            "realtime" => crate::misc::realtime::video::create(req),
            "segment" => crate::segment::video::create(req),
            "select" => crate::select::video::create(req),
            "sendcmd" => crate::sendcmd::video::create(req),
            "sidedata" => crate::misc::sidedata::video::create(req),
            "streamselect" => crate::streamselect::video::create(req),
            "settb" => crate::settb::video::create(req),
            "setpts" => crate::setpts::video::create(req),
            "split" => crate::split::video::create(req),
            "trim" => crate::trim::video::create(req),
            other => Err(format!("vaco-filter-mm: no filter named `{other}`")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = MmRegistry;
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
        let registry = MmRegistry;
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
        let registry = MmRegistry;
        let src = "null=zzz_totally_invented_option_name_xyz=1";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "null",
            instance: "null",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_err());
    }
}
