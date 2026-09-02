//! Legacy and professional-format bitstream filters.
//!
//! The "legacy" filters, plus the two `*_metadata` filters left over once
//! the H.264/HEVC/AV1/VP9/Opus metadata filters were homed in their own
//! per-codec crates: `mpeg2_metadata` and `prores_metadata`, both measured
//! identity transforms — see each module's docs.
//!
//! # What was measured and left out
//!
//! `ffmpeg -bsfs` names several more filters this crate's mandate would
//! plausibly cover; each was checked against `ffmpeg 8.1` and left
//! unregistered for lack of a reliable way to verify correct behaviour:
//!
//! * `mjpeg2jpeg`/`mjpegadump`: one measured sample each, rewriting bytes
//!   with no way to vary them independently — not enough to tell "always
//!   these constants" from "derived from input".
//! * `imxdump`: targets Sony XDCAM IMX/D-10 specifically; diverges on an
//!   ordinary `mpeg2video` stream, but there is no real D-10 sample to
//!   measure the correct behaviour against.
//! * `dovi_rpu`, `dv_error_marker`, `evc_frame_merge`: each needs input this
//!   environment cannot produce (Dolby Vision RPU; damaged DV capture; an
//!   EVC encoder, which this `ffmpeg` build lacks).
//! * `hapqa_extract`, `media100_to_mjpegb`, `apv_metadata`, `lcevc_metadata`:
//!   no [`vaco_codec_core::CodecId`] exists for these — unreachable.
//! * `vvc_metadata`/`vvc_mp4toannexb`: has a `CodecId`, but no VVC encoder
//!   or sample was available to measure its option table against.
//!
//! `h264_redundant_pps`'s exclusion is `vaco-bsf-h2645`'s call.
//!
//! # How it works
//!
//! One [`vaco_bsf_core::BsfDesc`] per module, built on
//! [`vaco_bsf_core::PacketMap`]; both filters are pure identity, gated only
//! on `codec_id`.
//!
//! # Configuration
//!
//! None reachable: [`vaco_format_core::mux::BsfProvider::open`] takes no
//! per-instance option string. That is still correct: every option either
//! filter exposes defaults to leaving the bitstream alone.

#![forbid(unsafe_code)]

pub mod mpeg2_metadata;
pub mod prores_metadata;

/// Every filter this crate registers.
#[must_use]
pub fn filters() -> &'static [vaco_bsf_core::BsfDesc] {
    &[mpeg2_metadata::DESC, prores_metadata::DESC]
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
