//! Audio bitstream filters: `aac_adtstoasc`, `opus_metadata`, `pcm_rechunk`.
//!
//! Membership was derived from `ffmpeg -bsfs` and `ffmpeg -h bsf=<name>`.
//! Two exclusions are not obvious:
//!
//! * **`dts2pts` is not an audio filter.** `ffmpeg -h bsf=dts2pts` reports
//!   `Supported codecs: h264 hevc` — "dts" is *decode timestamp* here, not
//!   the DTS codec.
//! * **`ahx_to_mp2`** is an audio filter, but AHX has no
//!   [`vaco_codec_core::CodecId`] variant in this workspace, so no
//!   `CodecParameters` it could claim can even be constructed.
//!
//! `dca_core`, `eac3_core` and `truehd_core` are also absent. Every encoder
//! available here produces core-only DTS/E-AC-3/TrueHD, with no extension or
//! dependent substream to strip, so the only samples this environment can
//! make these three look like the identity transform whether or not they are.
//! Implementing the stripping logic with nothing to falsify it against would
//! present a guess as a measurement; left for whoever has real
//! DTS-HD/Atmos/JOC material.
//!
//! # How it works
//!
//! One [`vaco_bsf_core::BsfDesc`] per module, built on
//! [`vaco_bsf_core::PacketMap`] wrapped in [`vaco_bsf_core::MappedFilter`].
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] takes no
//! per-instance option string. `opus_metadata`'s `gain` defaults to `0`
//! (identity, measured); `pcm_rechunk`'s three options all default to the
//! shape this crate implements (`1024`-sample chunks, zero-padded).
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-pool` for the
//! [`vaco_packet::PacketSideData::NewExtradata`] payload type
//! `aac_adtstoasc` attaches.

#![forbid(unsafe_code)]

pub mod aac_adtstoasc;
pub mod opus_metadata;
pub mod pcm_rechunk;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[aac_adtstoasc::DESC, opus_metadata::DESC, pcm_rechunk::DESC]
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
