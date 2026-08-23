//! H.264/HEVC bitstream filters.
//!
//! # What this is
//!
//! `h264_mp4toannexb` and `hevc_mp4toannexb` — the two filters
//! `vaco-mux-avi` and `vaco-mux-mpegts` are waiting on (their own inline
//! length-prefix-to-Annex-B converters do the framing half but never splice
//! parameter sets back in front of a keyframe, which the reference does; see
//! each module's docs for the measurement). `h264_redundant_pps` is not
//! implemented — see its section below for why.
//!
//! # How it works
//!
//! Framing (length-prefixed ↔ Annex B) is
//! `vaco_format_nalu::convert::length_prefixed_to_annexb`, not reimplemented
//! here (that crate's own module docs name this crate as the place its
//! "everything else" — parameter-set splicing — belongs). Parameter sets come
//! from `vaco_parse_h264::AvcDecoderConfigurationRecord` /
//! `vaco_parse_hevc::HevcDecoderConfigurationRecord`, parsed once at
//! construction from `CodecParameters::extradata`. Splicing is a byte-level
//! NAL-unit insertion, using `vaco_format_nalu::units` to find the insertion
//! point rather than scanning start codes by hand.
//!
//! # `h264_redundant_pps` — measured, not implemented
//!
//! Measured against `ffmpeg 8.1` on an x264 stream with `repeat-headers=1`
//! (which emits two PPS occurrences per keyframe): the filter's effect is not
//! "delete the second PPS NAL unit" at the byte level. A `SequenceMatcher`
//! diff of the filtered and unfiltered elementary streams shows the edit
//! starts *inside* the surviving PPS's own RBSP (a handful of bits shorter),
//! and small, recurring, non-byte-aligned differences continue through the
//! following slice's CABAC-coded data before resetting at the next NAL
//! boundary — the signature of a bit width changing mid-stream (most likely
//! `pic_parameter_set_id`'s `ue(v)` encoding, if the surviving PPS's id
//! differs from the one a slice used to reference) rather than of a clean
//! unit deletion.
//!
//! Reproducing that needs a CABAC-safe, bit-precise PPS rewrite and slice
//! header renumbering — the same class of problem
//! `vaco_parse_hevc::cbs::HevcCbs`'s own docs call out as *not yet
//! supported*, for the identical reason: "writing an SPS means writing
//! ... bit-exactly, and a writer that is not bit-exact silently corrupts a
//! stream rather than failing." `vaco-parse-h264` has no bit-writer layer at
//! all (unlike HEVC's `cbs` module), so there is nowhere to build this
//! correctly today. Shipping a naive byte-level unit removal without the
//! renumbering would produce a stream real decoders reject or misdecode,
//! which is worse than not registering the filter — left out rather than
//! landed wrong.
//!
//! # How to change it
//!
//! Add a module, implement [`vaco_bsf_core::PacketMap`], export a `DESC`, add
//! it to `filters()`, and register it with a `[[component]]` table in
//! `vaco-component.toml`.
//!
//! # Configuration
//!
//! None — see `vaco-bsf-generic`'s crate docs for why (`BsfProvider::open`
//! carries no option string).
//!
//! # Dependencies
//!
//! `vaco-bsf-core` for the driver; `vaco-format-nalu` for framing and NAL
//! headers; `vaco-parse-h264`/`vaco-parse-hevc` for decoder configuration
//! record parsing.

#![forbid(unsafe_code)]

pub mod h264_mp4toannexb;
pub mod hevc_mp4toannexb;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[h264_mp4toannexb::DESC, hevc_mp4toannexb::DESC]
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
