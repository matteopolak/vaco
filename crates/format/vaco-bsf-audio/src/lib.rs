//! Audio bitstream filters.
//!
//! # What this is
//!
//! Issue #352 (B-04), named `vaco-bsf-audio`. Membership was derived from
//! `ffmpeg -bsfs` and `ffmpeg -h bsf=<name>`, not assumed from the issue
//! title: `aac_adtstoasc` (priority — "the one a real MP4-from-TS remux
//! needs"), `opus_metadata`, `pcm_rechunk`.
//!
//! Two names that *sound* like they belong here do not, measured directly:
//!
//! * **`dts2pts` is not an audio filter.** Its name suggests DTS audio, but
//!   `ffmpeg -h bsf=dts2pts` reports `Supported codecs: h264 hevc` — "dts"
//!   here is *decode timestamp*, not the DTS codec. It belongs nowhere in
//!   this crate.
//! * **`ahx_to_mp2`** genuinely is an audio filter (`Supported codecs: ahx`)
//!   but AHX (a Sega/Yamaha ADPCM-family codec) has no [`vaco_codec_core::CodecId`]
//!   variant in this workspace at all — there is no way to even construct a
//!   `CodecParameters` this filter could claim, so it is not merely
//!   unimplemented, it is unreachable. Left out rather than adding a codec
//!   id with nothing behind it.
//!
//! `dca_core`, `eac3_core` and `truehd_core` — the DTS/E-AC-3/TrueHD
//! "extract the backward-compatible core" filters — are also **not** here.
//! Every encoder available in this environment produces core-only DTS/E-AC-3/
//! `TrueHD` (no extension or dependent substream to strip), so the only
//! samples this environment can produce make these three filters look like
//! the identity transform whether or not that is actually true in general —
//! exactly the "one matching sample is not a passing test" trap. Implementing
//! their real stripping logic from the format description alone, with no
//! extension-bearing sample to falsify it against, would be presenting a
//! guess as a measurement. Left out and flagged for whoever has real
//! DTS-HD/Atmos/JOC material to test against, rather than shipped wrong with
//! confidence.
//!
//! # How it works
//!
//! Same shape as every other `vaco-bsf-*` crate: one
//! [`vaco_bsf_core::BsfDesc`] per module, built on
//! [`vaco_bsf_core::PacketMap`] wrapped in [`vaco_bsf_core::MappedFilter`].
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] has no
//! per-instance option string (`planning/INTERFACE-GAPS.md` gap 12).
//! `opus_metadata`'s one option (`gain`) defaults to `0` (identity,
//! measured); `pcm_rechunk`'s three options all default to the shape this
//! crate implements (`1024`-sample chunks, zero-padded — see its own module
//! docs).
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
