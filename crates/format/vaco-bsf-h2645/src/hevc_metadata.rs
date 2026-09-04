//! HEVC metadata filtering is currently a byte-preserving pass-through.
//!
//! The reference's eighteen options default to retaining existing fields.
//! With ffmpeg 8.1, five libx265 fixtures were byte-identical after the bare
//! filter: plain video, existing AUDs, non-macroblock crop dimensions,
//! explicit level and BT.709 VUI values, and a longer B-picture stream.
//!
//! That observation does not generalize. A separate ffmpeg 9.0.1 `hvc1`
//! fixture had five access units of 2100/472/151/88/201 bytes. The bare
//! reference filter preserved every packet size but changed every CRC32,
//! including packets without parameter sets. The exact reserialization was
//! not characterized, so this implementation remains an explicit no-op rather
//! than guessing at a transformation.
//!
//! `vaco_parse_hevc::cbs::HevcCbs` can read typed SPS/VPS/PPS/SEI units but
//! refuses to write them; only raw undecoded units round-trip. A non-bit-exact
//! parameter-set writer would silently corrupt streams. In addition,
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string, so callers cannot supply a non-default value. Until both boundaries
//! exist, every metadata option remains pass-through.

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
