//! Codec-agnostic bitstream filters.
//!
//! This crate contains filters that do not need per-codec NAL vocabulary:
//! `null`, `extract_extradata`, `noise`, `remove_extra`, `setts`, `chomp`,
//! `dump_extra`, `filter_units`, and `showinfo`. The reference restricts
//! `showinfo` to no codec, and its only effect is a stderr diagnostic.
//!
//! The H.264/HEVC-specific family
//! (`h264_mp4toannexb`, `hevc_mp4toannexb`, `h264_redundant_pps`, ...) is
//! in `vaco-bsf-h2645`. `extract_extradata` remains here because its operation
//! is generic across supported codecs: scan parameter sets, compare them, and
//! attach side data. Annex-B conversion has codec-specific splicing rules.
//!
//! # How it works
//!
//! Every filter here is a [`vaco_bsf_core::PacketMap`] wrapped in
//! [`vaco_bsf_core::MappedFilter`], which is what actually implements
//! [`vaco_codec_core::BitstreamFilter`]'s push/pull protocol. Each module
//! exports a [`vaco_bsf_core::BsfDesc`] named by `vaco-component.toml` and
//! matched by the registry provider.
//!
//! # How to change it
//!
//! Add a module, implement [`vaco_bsf_core::PacketMap`], export a `DESC`, add
//! it to `filters()` below, and register it with a `[[component]]` table in
//! `vaco-component.toml` (`kind = "bitstream_filter"`). `cargo xtask
//! gen-registry` picks it up from there.
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no option-string parameter,
//! so filters implement only their bare-name, default-option behavior. The
//! driver comes from `vaco-bsf-core`; NAL framing and parameter-set vocabulary
//! come from `vaco-format-nalu`, `vaco-parse-h264`, and `vaco-parse-hevc`.

#![forbid(unsafe_code)]

pub mod chomp;
pub mod dump_extra;
pub mod extract_extradata;
pub mod filter_units;
pub mod noise;
pub mod null;
pub mod remove_extra;
pub mod setts;
pub mod showinfo;
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
        showinfo::DESC,
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
