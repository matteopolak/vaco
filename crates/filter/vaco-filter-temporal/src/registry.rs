//! [`TemporalRegistry`] — the [`FilterRegistry`] this crate's sixteen
//! implemented filters answer through. Mirrors
//! `vaco-filter-denoise::registry::DenoiseRegistry`'s shape exactly.
//! `fps` is deliberately absent: see the crate root doc.

use vaco_filter_graph::registry::{FilterRegistry, Instance, Instantiate};

/// Every name this crate registers, alphabetical as `ffmpeg -filters` lists
/// them within this row (all sixteen checked against `ffmpeg -hide_banner
/// -filters`, ffmpeg 8.1, 2026-08-23).
const NAMES: &[&str] = &[
    "decimate",
    "deflicker",
    "dejudder",
    "framestep",
    "freezedetect",
    "freezeframes",
    "fsync",
    "lagfun",
    "mpdecimate",
    "random",
    "tblend",
    "tlut2",
    "tmedian",
    "tmidequalizer",
    "tmix",
    "tpad",
];

/// Implements [`FilterRegistry`] for every filter in this crate.
#[derive(Debug, Clone, Copy, Default)]
pub struct TemporalRegistry;

impl FilterRegistry for TemporalRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String> {
        Ok(match req.name {
            "decimate" => crate::decimate::create(req),
            "deflicker" => crate::deflicker::create(req),
            "dejudder" => crate::dejudder::create(req),
            "framestep" => crate::framestep::create(req),
            "freezedetect" => crate::freezedetect::create(req),
            "freezeframes" => crate::freezeframes::create(req),
            "fsync" => crate::fsync::create(req)?,
            "lagfun" => crate::lagfun::create(req),
            "mpdecimate" => crate::mpdecimate::create(req),
            "random" => crate::random::create(req),
            "tblend" => crate::tblend::create(req)?,
            "tlut2" => crate::tlut2::create(req)?,
            "tmedian" => crate::tmedian::create(req),
            "tmidequalizer" => crate::tmidequalizer::create(req),
            "tmix" => crate::tmix::create(req),
            "tpad" => crate::tpad::create(req),
            other => return Err(format!("vaco-filter-temporal: no filter named `{other}`")),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
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
    fn every_declared_name_is_creatable_with_no_arguments() {
        let registry = TemporalRegistry;
        for &name in NAMES {
            // `fsync` genuinely requires a `file` argument (see its module
            // doc) and is expected to fail cleanly without one.
            if name == "fsync" {
                assert!(registry.create(&req(name)).is_err());
                continue;
            }
            let result = registry.create(&req(name));
            assert!(result.is_ok(), "{name} failed to create with no args: {result:?}");
        }
    }

    #[test]
    fn an_unknown_name_is_a_clean_error_not_a_panic() {
        let registry = TemporalRegistry;
        assert!(registry.create(&req("not-a-real-filter")).is_err());
    }

    #[test]
    fn fps_is_deliberately_not_registered_here() {
        assert!(!TemporalRegistry.contains("fps"));
    }
}
