//! Codec-agnostic bitstream filters.
//!
//! # What this is
//!
//! Every filter `ffmpeg -bsfs` lists that does not need per-codec NAL
//! vocabulary: `null`, `extract_extradata`, `noise`, `remove_extra`, `setts`,
//! `chomp`, `dump_extra`, and `filter_units`. The H.264/HEVC-specific family
//! (`h264_mp4toannexb`, `hevc_mp4toannexb`, `h264_redundant_pps`, ...) is
//! `vaco-bsf-h2645`, one layer up from this one for exactly the reason
//! `extract_extradata` is *here* rather than there: it dispatches on codec but
//! its own logic — scan for parameter sets, compare, attach side data — is the
//! same shape for every codec it supports, unlike `*_mp4toannexb`'s splicing
//! rules, which are genuinely per-codec.
//!
//! # How it works
//!
//! Every filter here is a [`vaco_bsf_core::PacketMap`] wrapped in
//! [`vaco_bsf_core::MappedFilter`], which is what actually implements
//! [`vaco_codec_core::BitstreamFilter`]'s push/pull protocol. Each module
//! exports one `pub const DESC: vaco_bsf_core::BsfDesc`, which is what a
//! `vaco-component.toml` fragment's `ctor` names and what
//! `vaco-registry`'s `BsfProvider` implementation matches on by name — see
//! that crate's docs for why matching by name rather than a generated typed
//! table is this wave's deliberate, scoped answer to
//! `vaco_registry::Kind::has_table`'s "no descriptor type yet" gap for
//! `bitstream_filter`.
//!
//! # How to change it
//!
//! Add a module, implement [`vaco_bsf_core::PacketMap`], export a `DESC`, add
//! it to `filters()` below, and register it with a `[[component]]` table in
//! `vaco-component.toml` (`kind = "bitstream_filter"`). `cargo xtask
//! gen-registry` picks it up from there.
//!
//! # Configuration
//!
//! None of these filters take options today: [`vaco_format_core::mux::BsfProvider::open`]
//! has no options-string parameter, so every filter here implements the
//! reference's *bare-name* (all-default-options) behaviour only. Recorded as
//! a gap in `planning/INTERFACE-GAPS.md` rather than worked around by
//! widening a frozen trait.
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-format-nalu` for NAL framing and
//! header layout; `vaco-parse-h264`/`vaco-parse-hevc` for the *meaning* of a
//! NAL type number (`extract_extradata` only); `vaco-pool` for the
//! [`vaco_packet::PacketSideData::NewExtradata`] payload type.

#![forbid(unsafe_code)]

pub mod chomp;
pub mod dump_extra;
pub mod extract_extradata;
pub mod filter_units;
pub mod noise;
pub mod null;
pub mod remove_extra;
pub mod setts;
pub mod trace_headers;

/// Every filter this crate registers, for anything that wants the whole list
/// without depending on the registry (a test, or a future `-bsfs` listing
/// that runs before `gen-registry` has assembled one).
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[
        null::DESC,
        extract_extradata::DESC,
        noise::DESC,
        remove_extra::DESC,
        setts::DESC,
        chomp::DESC,
        dump_extra::DESC,
        filter_units::DESC,
        trace_headers::DESC,
    ]
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
