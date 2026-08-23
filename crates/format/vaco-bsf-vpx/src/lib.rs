//! `VPx` bitstream filters.
//!
//! # What this is, and what "`VPx`" turns out to mean
//!
//! Issue #351 (B-03)'s `VPx` half, named `vaco-bsf-vpx`. `ffmpeg -bsfs` has
//! exactly four filters whose `-h bsf=<name>` reports a VP-family codec —
//! `vp9_metadata`, `vp9_raw_reorder`, `vp9_superframe`,
//! `vp9_superframe_split` — and every one of them is `Supported codecs: vp9`.
//! **There is no VP8 bitstream filter in the reference at all.** "`VPx`"
//! plural, in the issue title, overstates it: this crate is VP9-only because
//! that is all the reference has, not because VP8 was skipped.
//!
//! # Coverage
//!
//! `vp9_metadata` and `vp9_superframe_split`/`vp9_superframe` are here,
//! measured against `ffmpeg 8.1`. `vp9_raw_reorder` is **not**: every VP9
//! stream this environment could produce that contains anything worth
//! reordering (alt-ref/hidden frames) packs them as superframes, and the
//! reference's own `vp9_raw_reorder` refuses superframe input outright
//! (`Input in superframes is not supported.`, measured directly). That
//! leaves no oracle input in this environment that both exercises the filter
//! and does not just re-confirm identity-on-ordinary-frames — implementing
//! its reordering rule from description alone, with nothing to falsify it
//! against, is exactly the false-confidence trap this project's other
//! findings warn about. Left out and flagged rather than guessed.
//!
//! # How it works
//!
//! Same shape as the other `vaco-bsf-*` crates: one [`vaco_bsf_core::BsfDesc`]
//! per module, built on [`vaco_bsf_core::PacketMap`] wrapped in
//! [`vaco_bsf_core::MappedFilter`].
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] has no
//! per-instance option string (`planning/INTERFACE-GAPS.md` gap 12).
//! `vp9_metadata`'s two options (`color_space`, `color_range`) both default
//! to `-1` ("leave alone") — measured bare-name byte-identity against a real
//! `libvpx-vp9` stream — so this crate implements it as the identity
//! transform for the same reason `vaco-bsf-av1::metadata` does.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-bitstream` for the raw bit reads
//! `vp9_superframe`'s grouping decision needs (VP9 has no NAL-level parser
//! crate in this workspace to depend on instead — the handful of header bits
//! read here do not warrant one).

#![forbid(unsafe_code)]

pub mod metadata;
pub mod superframe;
pub mod superframe_split;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[metadata::DESC, superframe::DESC, superframe_split::DESC]
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
