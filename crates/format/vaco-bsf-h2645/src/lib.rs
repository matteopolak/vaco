//! H.264/HEVC bitstream filters.
//!
//! `h264_mp4toannexb`/`hevc_mp4toannexb` splice parameter sets in front of a
//! keyframe during length-prefixed-to-Annex-B conversion
//! (`vaco_format_nalu::convert::length_prefixed_to_annexb` does the framing;
//! parameter sets come from the AVC/HEVC decoder configuration record,
//! spliced in via `vaco_format_nalu::units`). `h264_metadata`/`hevc_metadata`
//! are measured identity transforms except for `aud` (below).
//! `h264_redundant_pps` and `dts2pts` are not implemented.
//!
//! # The CBS write path — scaffolded, not built
//!
//! `vaco_parse_hevc::cbs::HevcCbs` can only write a raw undecoded unit back
//! out (every typed variant returns `Error::Unsupported`, since a
//! non-bit-exact writer would corrupt a stream), and `vaco-parse-h264` has
//! no `CbsCodec` at all — no bit-exact SPS/PPS serialiser exists yet.
//! `h264_metadata`'s `aud` is wired through `BitstreamFilter::set_option`
//! instead: `insert`/`remove` splice a whole AUD NAL in or out, a structural
//! edit needing no CBS write path. Every other option on both filters
//! defaults to "leave the bitstream alone" and is unreachable anyway
//! (`BsfProvider::open` takes no option string). `hevc_metadata`'s own `aud`
//! is deliberately not wired yet, rather than assuming its two-byte AUD
//! header matches H.264's one-byte header without checking.
//!
//! # `h264_redundant_pps` — measured, not implemented
//!
//! On an x264 stream with `repeat-headers=1`, the reference's edit starts
//! inside the surviving PPS's RBSP and continues through the next slice's
//! CABAC data — a bit width changing mid-stream (likely
//! `pic_parameter_set_id`'s `ue(v)`), not a clean NAL deletion. That needs a
//! CABAC-safe, bit-precise PPS rewrite and slice renumbering that H.264 has
//! no bit-writer layer to do. Left out rather than landed wrong.
//!
//! # `dts2pts` — measured, not implemented
//!
//! The reference supports `h264 hevc`, not audio — "dts" is *decode
//! timestamp*, touching only `Packet::pts`. Measured: it assigns `pts` by a
//! real picture-order-count computation over a hierarchical B-frame
//! structure (H.264 §8.2.1, HEVC §8.3.1), not a constant reorder-delay
//! shift — which needs decoding slice-header POC fields and buffering a
//! reorder window, decoder-adjacent work left unimplemented.
//!
//! # How to change it
//!
//! Add a module implementing [`vaco_bsf_core::PacketMap`], export a `DESC`,
//! add it to `filters()`, and register it in `vaco-component.toml`.

#![forbid(unsafe_code)]

pub mod h264_metadata;
pub mod h264_mp4toannexb;
pub mod hevc_metadata;
pub mod hevc_mp4toannexb;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[
        h264_metadata::DESC,
        h264_mp4toannexb::DESC,
        hevc_metadata::DESC,
        hevc_mp4toannexb::DESC,
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
