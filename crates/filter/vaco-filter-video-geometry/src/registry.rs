//! [`GeometryRegistry`] — the [`FilterRegistry`] this crate's six filters
//! answer through. Same shape as `vaco-filter-audio::registry` /
//! `vaco-filter-plumbing::registry`.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// `(filter, known option names)` for the filters in this crate that were
/// found silently accepting *any* option name, including one the
/// reference does not document at all -- probed directly against real
/// `ffmpeg 8.1 -h filter=<name>`, 2026-08-28. `hflip`/`vflip` document no
/// options at all in the reference and never read `Instantiate::named`
/// themselves, so any key at all used to pass through unexamined. `scale`
/// documents a much larger set than this crate implements (`w`/`width`,
/// `h`/`height`, `size`/`s`); every other name here is real but not
/// implemented, which is a separate, already-tracked gap, not this one --
/// this table is only about rejecting a name the reference never
/// documented under `scale` at all. `crop`/`pad`/`transpose` already
/// validate names themselves and are not listed here.
const KNOWN_OPTIONS: &[(&str, &[&str])] = &[
    ("hflip", &[]),
    ("vflip", &[]),
    (
        "scale",
        &[
            "w",
            "width",
            "h",
            "height",
            "flags",
            "interl",
            "size",
            "s",
            "in_color_matrix",
            "out_color_matrix",
            "in_range",
            "out_range",
            "in_chroma_loc",
            "out_chroma_loc",
            "in_primaries",
            "out_primaries",
            "in_transfer",
            "out_transfer",
            "in_v_chr_pos",
            "in_h_chr_pos",
            "out_v_chr_pos",
            "out_h_chr_pos",
            "force_original_aspect_ratio",
            "force_divisible_by",
            "reset_sar",
            "param0",
            "param1",
            "eval",
        ],
    ),
];

/// Rejects any `key=value` argument whose key is not one of the
/// reference's own documented option names for `req.name` (see
/// [`KNOWN_OPTIONS`]'s own doc for the filters this actually covers). A
/// filter name absent from the table is not this function's business --
/// it already validates names itself.
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

/// The names this crate answers to, alphabetical (as `ffmpeg -filters`
/// prints them).
const NAMES: &[&str] = &["crop", "hflip", "pad", "scale", "transpose", "vflip"];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeometryRegistry;

impl FilterRegistry for GeometryRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        ensure_known_options(req)?;
        match req.name {
            "crop" => crate::crop::create(req),
            "hflip" => crate::flip::hflip::create(req),
            "pad" => crate::pad::create(req),
            "scale" => crate::scale::create(req),
            "transpose" => crate::transpose::create(req),
            "vflip" => crate::flip::vflip::create(req),
            other => Err(format!(
                "vaco-filter-video-geometry: no filter named `{other}`"
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = GeometryRegistry;
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
        let registry = GeometryRegistry;
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
        let registry = GeometryRegistry;
        for src in [
            "hflip=zzz_totally_invented_option_name_xyz=1",
            "vflip=zzz_totally_invented_option_name_xyz=1",
            "scale=zzz_totally_invented_option_name_xyz=1",
        ] {
            let parsed = vaco_filter_graph::ast::parse(src).unwrap();
            let spec = &parsed.chains[0].filters[0];
            let arguments = spec.arguments().unwrap();
            let req = Instantiate {
                name: spec.name.as_str(),
                instance: spec.name.as_str(),
                args: spec.args.as_deref(),
                arguments: &arguments,
            };
            assert!(registry.create(&req).is_err(), "{src}");
        }
    }

    #[test]
    fn scale_real_option_still_creates() {
        let registry = GeometryRegistry;
        let src = "scale=w=320:h=240";
        let parsed = vaco_filter_graph::ast::parse(src).unwrap();
        let spec = &parsed.chains[0].filters[0];
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: "scale",
            instance: "scale",
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(registry.create(&req).is_ok());
    }
}
