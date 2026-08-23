//! `filter_units`: drop or keep NAL/OBU-style units by type.
//!
//! `ffmpeg -h bsf=filter_units` defaults `pass_types`/`remove_types` to empty
//! and `discard` to `none` — measured directly, `-bsf:v filter_units` with no
//! assignments produces byte-identical output to no filter at all.
//! [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open) carries no
//! option string (`planning/INTERFACE-GAPS.md`), so the type lists and
//! discard policy that make this filter useful are not reachable through the
//! registry seam today; what is implemented is the identity transform every
//! caller through that seam actually gets.
//!
//! `ffmpeg` lists eight supported codecs (`apv av1 h264 hevc vvc lcevc
//! mpeg2video vp8 vp9`); construction is restricted to H.264 and HEVC, the
//! two this workspace has NAL-unit-type vocabulary for, so a caller naming
//! anything else finds out at construction rather than getting a silent,
//! unconditional pass-through under a codec this crate cannot actually
//! inspect.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "filter_units",
    long_name: "Remove units with types in a given set",
    build,
};

struct FilterUnits;

impl PacketMap for FilterUnits {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        if let Some(p) = packet {
            out.push_back(p.clone());
        }
        Ok(())
    }
}

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::H264 | CodecId::Hevc) => Ok(Box::new(MappedFilter::new(FilterUnits))),
        _ => Err(Error::Unsupported(
            "filter_units: this build only recognises units for h264 and hevc",
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn default_options_are_the_identity_transform() {
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[0, 0, 0, 1, 0x67, 0x42]).unwrap();
        let params = CodecParameters::video().with_codec(CodecId::H264);
        let mut f = (DESC.build)(&params).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), pkt.payload());
    }

    #[test]
    fn an_unrecognised_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Av1);
        assert!((DESC.build)(&params).is_err());
    }
}
