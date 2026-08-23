//! AV1 bitstream filters.
//!
//! # What this is
//!
//! Issue #351 (B-03)'s AV1 half. `ffmpeg -bsfs` lists three filters whose
//! `-h bsf=<name>` reports `Supported codecs: av1` and nothing else:
//! `av1_frame_merge`, `av1_frame_split`, `av1_metadata`. (`dovi_rpu` also
//! names `av1` in its supported-codec line, alongside `hevc` — it is *not*
//! here; see `frame_split`'s module docs for why a dual-codec Dolby Vision
//! filter does not fit a single-codec crate and was left out rather than
//! guessed at.)
//!
//! # How it works
//!
//! Same shape as `vaco-bsf-generic`/`vaco-bsf-h2645`: each module exports one
//! [`vaco_bsf_core::BsfDesc`], built on [`vaco_bsf_core::PacketMap`] wrapped
//! in [`vaco_bsf_core::MappedFilter`]. `frame_split`/`frame_merge` are the
//! pair that matters — see their own docs for the OBU-grouping rule, measured
//! against `ffmpeg 8.1` rather than assumed. `metadata` is the reference's
//! `av1_metadata`, which this crate implements as the identity transform: see
//! its own docs for why that is a measurement, not a shortcut.
//!
//! # How to change it
//!
//! Add a module, implement [`vaco_bsf_core::PacketMap`], export a `DESC`, add
//! it to [`filters`], and register it with a `[[component]]` table in
//! `vaco-component.toml`.
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] has no
//! per-instance option string (`planning/INTERFACE-GAPS.md` gap 12), so every
//! filter here is the reference's bare-name (default-option) behaviour only.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-parse-av1` for OBU framing
//! (`obu::units`, `ObuHeader`, `ObuType`) — the same crate `vaco-parse-av1`'s
//! owner already built and the one place OBU-layout knowledge lives (D19).

#![forbid(unsafe_code)]

pub mod frame_merge;
pub mod frame_split;
pub mod metadata;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[frame_split::DESC, frame_merge::DESC, metadata::DESC]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_has_a_unique_name() {
        let names: Vec<&str> = filters().iter().map(|d| d.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "{names:?}");
    }
}
