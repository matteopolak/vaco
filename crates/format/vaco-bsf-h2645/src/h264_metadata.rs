//! `h264_metadata`: rewrite SPS/VUI-level metadata (colour description, AUD
//! insertion/removal, cropping, level, SEI) in an H.264 stream.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=h264_metadata` lists twenty options, and **every one of
//! them defaults to "leave whatever the bitstream already says alone"**:
//! `aud=pass` (0), the eleven `-1`-default VUI/crop fields (`overscan_appropriate_flag`,
//! `video_format`, `video_full_range_flag`, `colour_primaries`,
//! `transfer_characteristics`, `matrix_coefficients`, `chroma_sample_loc_type`,
//! `fixed_frame_rate_flag`, `crop_left/right/top/bottom`), `sample_aspect_ratio`
//! defaults to `0/1` (unset), `tick_rate` to `0/1`, `zero_new_constraint_set_flags`
//! to `false`, `delete_filler` to `0` (off), `display_orientation=pass` (0),
//! `rotate` to `nan` (unset), `flip` to no bits set, and `level` to `-2`
//! ("unset" — not even `-1`'s "guess from stream").
//!
//! Measured directly against `ffmpeg 8.1`: `-bsf:v h264_metadata` with no
//! option string, run on real `libx264` elementary streams, reproduced the
//! input **byte for byte** (`cmp`) across five inputs chosen to be adversarial
//! about it, not just the easy case —
//!
//! * a plain 176x144 stream (baseline case),
//! * a stream with `access_unit_delimiter`s already present
//!   (`x264-params aud=1`), which `aud=pass` must leave alone on *both* ends —
//!   neither inserting nor removing,
//! * a 178x146 stream, whose dimensions are not multiples of 16 and therefore
//!   carries a non-trivial SPS conformance-window crop — the exact field
//!   `crop_left/right/top/bottom=-1` claims not to touch,
//! * a stream with an explicit `-level 5.1` and forced VUI colour description
//!   (`bt709`/`bt709`/`bt709`) — the fields `level=-2` and the four `-1`-default
//!   colour options claim not to touch, and
//! * a 320x240, 60-frame, B-frame-bearing encode combining the above, to rule
//!   out any per-frame or per-slice-type divergence a single short clip could
//!   hide.
//!
//! All five reproduced the input exactly.
//!
//! # Why this crate does not also carry the CBS write path
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string (`planning/INTERFACE-GAPS.md` gap 12) and this crate's owner does
//! not have standing to add one silently (see this crate's own top-level
//! docs). Every option above is therefore permanently unreachable through the
//! seam this workspace has today — not merely unimplemented — so there is no
//! way to *drive* a real field-level rewrite even if one existed.
//!
//! One exists in outline: `vaco_codec_cbs::CbsCodec` already has the
//! `read_unit`/`write_unit`/`assemble` shape a filter like this would use, but
//! `vaco-parse-hevc`'s implementation of it (`cbs::HevcCbs`, the only
//! `CbsCodec` for an H.26x codec in this tree) can `write_unit` a raw,
//! undecoded unit back out but returns `Error::Unsupported` for a typed SPS —
//! writing one back out bit-exactly (`profile_tier_level`, every VUI field,
//! `rbsp_trailing_bits` padding) is real, unstarted work, and `vaco-parse-h264`
//! has no `CbsCodec` implementation at all yet. Building an H.264 SPS writer
//! now, with no path in this workspace that could ever pass it a non-default
//! option to exercise, would be dead code no test could honestly claim to
//! cover — exactly the trap `vaco-bsf-av1::metadata`'s docs already name.
//! Left out for the same reason, and flagged here again because two
//! independent crates hitting the identical wall is a fact worth keeping in
//! one place.
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
    name: "h264_metadata",
    long_name: "Modify metadata embedded in an H.264 stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::H264) => Ok(Box::new(MappedFilter::new(H264Metadata))),
        _ => Err(Error::Unsupported("h264_metadata: h264 only")),
    }
}

struct H264Metadata;

impl PacketMap for H264Metadata {
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
        let mut f = (DESC.build)(&CodecParameters::video().with_codec(CodecId::H264)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[1, 2, 3, 4, 5]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_non_h264_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Hevc);
        assert!((DESC.build)(&params).is_err());
    }
}
