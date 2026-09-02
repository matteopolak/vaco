//! `mpeg2_metadata`: rewrite sequence/sequence-display metadata (display
//! aspect ratio, frame rate, colour description) in an MPEG-2 video stream.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=mpeg2_metadata` lists six options, and every one defaults
//! to "leave whatever the bitstream already says alone": `display_aspect_ratio`
//! and `frame_rate` both default to `0/1` (unset), and `video_format`,
//! `colour_primaries`, `transfer_characteristics`, `matrix_coefficients` all
//! default to `-1`.
//!
//! Measured directly against `ffmpeg 8.1`: a native `mpeg2video`-encoded
//! elementary stream run through `-bsf:v mpeg2_metadata` with no option
//! string reproduced the input byte for byte (`cmp`).
//!
//! # Configuration
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string, so every option above is permanently unreachable, not merely
//! unimplemented. No numeric option is read here, so unenforced option-range
//! checks have nothing to apply to either.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "mpeg2_metadata",
    long_name: "Modify metadata embedded in an MPEG-2 stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Mpeg2video) => Ok(Box::new(MappedFilter::new(Mpeg2Metadata))),
        _ => Err(Error::Unsupported("mpeg2_metadata: mpeg2video only")),
    }
}

struct Mpeg2Metadata;

impl PacketMap for Mpeg2Metadata {
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
        let mut f =
            (DESC.build)(&CodecParameters::video().with_codec(CodecId::Mpeg2video)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[0, 0, 1, 0xb3]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[0, 0, 1, 0xb3]);
    }

    #[test]
    fn a_non_mpeg2_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Mpeg4);
        assert!((DESC.build)(&params).is_err());
    }
}
