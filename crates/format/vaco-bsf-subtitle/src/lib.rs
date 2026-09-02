//! Subtitle bitstream filters.
//!
//! # What this is
//!
//! `mov2textsub` and `text2movsub` — the pair that lifts plain text out of,
//! and wraps it back into, an MP4/QuickTime `mov_text` sample. Both are real
//! byte-level transforms, measured against `ffmpeg 8.1` end to end (SRT ->
//! `mov_text` -> MP4 -> filter, and the reverse), not the bare-name identity
//! most of this filter family turns out to be — see each module's docs for
//! the exact measured bytes.
//!
//! # What was measured and left out
//!
//! `pgs_frame_merge` (Blu-ray PGS display-set merging) has no PGS encoder or
//! sample available here to measure the merge rule against. `eia608_to_smpte436m`
//! and its inverse (CEA-608 <-> SMPTE 436M VANC) are a structural gap:
//! `smpte_436m_anc` has no `CodecId` in this workspace, so the inverse is
//! unreachable and the forward direction would produce a stream typed as a
//! codec nothing here can consume. All three left out.
//!
//! # How it works
//!
//! One [`vaco_bsf_core::BsfDesc`] per module, built on
//! [`vaco_bsf_core::PacketMap`]. Neither filter restricts the codec at
//! construction, matching the reference stating no `Supported codecs:` line
//! for either.
//!
//! # Configuration
//!
//! Neither filter has an `AVOption` in the reference, so the crate is not a
//! bare-name substitute for a richer filter the way `*_metadata` is.
//! `text2movsub` does enforce one measured bound — a `u16` length prefix
//! cannot exceed 65535 bytes of text, and the reference refuses rather than
//! truncates past it — see its own module docs.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-core`/`vaco-limits`/`vaco-packet`
//! for the error, budget and packet types every filter needs. Both operate
//! on the packet payload directly, per ISO/IEC 14496-17's Text Sample format.

#![forbid(unsafe_code)]

pub mod mov2textsub;
pub mod text2movsub;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[mov2textsub::DESC, text2movsub::DESC]
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
