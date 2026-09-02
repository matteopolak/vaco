//! `hevc_metadata`: rewrite SPS/VUI/VPS-level metadata (colour description,
//! AUD insertion/removal, cropping, size-after-crop, tick rate, level) in an
//! HEVC stream.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=hevc_metadata` lists eighteen options, and **every one of
//! them defaults to "leave whatever the bitstream already says alone"**:
//! `aud=pass` (0), the nine `-1`-default fields (`video_format`,
//! `video_full_range_flag`, `colour_primaries`, `transfer_characteristics`,
//! `matrix_coefficients`, `chroma_sample_loc_type`, `num_ticks_poc_diff_one`,
//! `crop_left/right/top/bottom`, `width`, `height`), `sample_aspect_ratio`
//! defaults to `0/1` (unset), `tick_rate` to `0/1`, and `level` to `-2`
//! ("unset").
//!
//! Measured directly against `ffmpeg 8.1`, the same five adversarial inputs
//! used for [`crate::h264_metadata`] re-encoded with `libx265` — plain,
//! AUD-already-present, a non-16-multiple crop dimension, explicit
//! `-level 5.1` with forced `bt709` colour description, and a longer
//! B-frame-bearing clip: `-bsf:v hevc_metadata` with no option string
//! reproduced every one of those five **byte for byte**.
//!
//! # That claim does not generalise — corrected, not just the code left alone
//!
//! Once `-bsf` made this filter reachable at all (before that, nothing in
//! this tree ever called it — see the bsf reachability sweep), a sixth,
//! independently-generated real encode falsified the "byte for byte"
//! claim above as a *general* statement about `ffmpeg 9.0.1`'s
//! `hevc_metadata`, though it remains true of the original five fixtures.
//! Measured: a fresh `libx265` `testsrc` clip (`-tag:v hvc1`, five access
//! units, sizes 2100/472/151/88/201 bytes) run through real ffmpeg's own
//! `-bsf:v hevc_metadata` with no option string kept every packet's exact
//! byte *size* unchanged but changed the CRC32 of **all five**, including
//! ones that carry no parameter set at all — so this is not confined to a
//! VPS/SPS rewrite; something about the filter's default pass reserialises
//! (or re-frames) the whole Annex-B stream, at least for this input, not
//! only when a field value actually needs to change. What exactly changes
//! byte-for-byte was not characterised further (`planning/INTERFACE-GAPS.md`
//! gap 12's own tracking issue, not reproduced here) — the important fact
//! for this doc to state plainly is that "measured byte-identical" was a
//! claim about five specific fixtures, not a property of the reference
//! filter in general, and a caller should not read it as one. This crate's
//! own implementation below remains a true no-op regardless (see the next
//! section for why: there is no HEVC CBS write path to make it do anything
//! else), so it silently does not reproduce whatever real ffmpeg's default
//! pass does here — a known, narrower gap than "wrong output", recorded
//! rather than fixed blind.
//!
//! # Why this crate does not also carry the CBS write path
//!
//! Same wall as [`crate::h264_metadata`], one layer more concrete here
//! because HEVC actually has a `CbsCodec`: `vaco_parse_hevc::cbs::HevcCbs`
//! implements `read_unit` for a typed `Sps`/`Vps`/`Pps`/`Sei`, but its
//! `write_unit` returns `Error::Unsupported` for every one of those variants —
//! only `HevcContent::Raw` (an undecoded unit's bytes, unchanged) writes back
//! out. Its own module docs call this out directly: "writing an SPS means
//! writing `profile_tier_level()`, every reference picture set and the whole
//! VUI back out bit-exactly, and a writer that is not bit-exact silently
//! corrupts a stream rather than failing" — unstarted work, tracked
//! separately (plan 15 §D-19).
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string either (`planning/INTERFACE-GAPS.md` gap 12), so even a complete
//! SPS writer would have no caller in this workspace that could hand it a
//! non-default value to apply. Two missing pieces, and closing only one would
//! not make this filter do anything it does not already do. Left as the
//! measured identity transform, like `vaco-bsf-av1::metadata` and
//! `vaco-bsf-vpx::metadata` before it.
//!
//! No numeric option is read here, so `CONFORMANCE-FINDINGS.md` finding 31
//! (unenforced option ranges) has nothing to apply to.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "hevc_metadata",
    long_name: "Modify metadata embedded in an HEVC stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Hevc) => Ok(Box::new(MappedFilter::new(HevcMetadata))),
        _ => Err(Error::Unsupported("hevc_metadata: hevc only")),
    }
}

struct HevcMetadata;

impl PacketMap for HevcMetadata {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        if let Some(p) = packet {
            out.push_back(p.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn bare_invocation_is_byte_identical() {
        let mut f = (DESC.build)(&CodecParameters::video().with_codec(CodecId::Hevc)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[9, 8, 7, 6]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[9, 8, 7, 6]);
    }

    #[test]
    fn a_non_hevc_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::H264);
        assert!((DESC.build)(&params).is_err());
    }
}
