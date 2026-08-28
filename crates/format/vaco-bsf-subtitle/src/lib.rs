//! Subtitle bitstream filters.
//!
//! # What this is
//!
//! Issue #354 (B-06)'s subtitle half. Membership was derived from
//! `ffmpeg -bsfs` and `ffmpeg -h bsf=<name>`, not assumed from the issue
//! title: `mov2textsub` and `text2movsub` — the pair that lifts plain text
//! out of, and wraps it back into, an MP4/QuickTime `mov_text` sample. Both
//! are real byte-level transforms, measured against `ffmpeg 8.1` end to end
//! (SRT -> `mov_text` -> MP4 -> filter, and the reverse), not the bare-name
//! identity most of this filter family turns out to be — see each module's
//! docs for the exact measured bytes.
//!
//! # What was measured and left out
//!
//! * **`pgs_frame_merge`** (`Supported codecs: hdmv_pgs_subtitle`) merges
//!   Blu-ray PGS display sets that a producer split across multiple packets
//!   sharing one PTS. This `ffmpeg` build has no PGS *encoder* and no PGS
//!   sample was available in this environment, so there is no fragmented
//!   input to measure the merge rule against — the reference's own `-h
//!   bsf=pgs_frame_merge` states no options and no default to fall back on,
//!   unlike the `*_metadata` family. Left out.
//! * **`eia608_to_smpte436m`** (`Supported codecs: eia_608`) and its inverse
//!   **`smpte436m_to_eia608`** (`Supported codecs: smpte_436m_anc`) convert
//!   CEA-608 closed captions to and from SMPTE 436M VANC ancillary packets.
//!   `eia_608` has a [`vaco_codec_core::CodecId::Eia608`] in this workspace,
//!   but `smpte_436m_anc` has no `CodecId` at all — so `smpte436m_to_eia608`
//!   is unreachable outright, and `eia608_to_smpte436m` would produce a
//!   stream typed as a codec this workspace cannot even name, which is not a
//!   filter output any caller here could consume. Both left out; not a
//!   single-sample judgement call like `pgs_frame_merge`; a structural gap
//!   like `vaco-bsf-audio`'s `ahx_to_mp2`.
//!
//! # How it works
//!
//! Same shape as every other `vaco-bsf-*` crate: one [`vaco_bsf_core::BsfDesc`]
//! per module, built on [`vaco_bsf_core::PacketMap`] wrapped in
//! [`vaco_bsf_core::MappedFilter`]. Neither filter here restricts the codec
//! at construction — `ffmpeg -h bsf=mov2textsub`/`text2movsub` state no
//! `Supported codecs:` line, so none is invented here either.
//!
//! # Configuration
//!
//! Neither filter has an `AVOption` in the reference (`ffmpeg -h
//! bsf=mov2textsub`/`text2movsub` list none), so gap 12
//! (`planning/INTERFACE-GAPS.md`, `BsfProvider::open` carrying no
//! per-instance option string) does not limit anything here — unlike the
//! `*_metadata` family, this crate is not a bare-name substitute for a
//! richer filter. `text2movsub` does enforce one measured bound (a `u16`
//! length prefix cannot exceed 65535 bytes of text, and the reference
//! refuses rather than truncates past it) — see its own module docs and
//! `CONFORMANCE-FINDINGS.md` finding 31.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-core`/`vaco-limits`/`vaco-packet`
//! for the error, budget and packet types every filter needs. No
//! codec-specific parsing crate: both filters here operate on the packet
//! payload directly, per ISO/IEC 14496-17's Text Sample format.

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
