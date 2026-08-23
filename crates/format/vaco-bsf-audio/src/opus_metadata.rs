//! `opus_metadata`: apply an output-gain adjustment to Opus packets.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=opus_metadata` lists exactly one option, `gain`, default
//! `0` — and the reference's own description of it is `"actual amplification
//! is pow(10, gain/(20.0*256))"`, which is `1.0` at `gain=0`. Measured
//! directly: `-bsf:a opus_metadata` with no option string, run on a real
//! `libopus`-encoded stream, reproduced the input **byte for byte**
//! (`framecrc` agreement against the unfiltered stream).
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string (`planning/INTERFACE-GAPS.md` gap 12), so this crate can only ever
//! construct the bare-name (`gain=0`) behaviour, which is measured identity.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "opus_metadata",
    long_name: "Modify metadata embedded in an Opus stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Opus) => Ok(Box::new(MappedFilter::new(OpusMetadata))),
        _ => Err(Error::Unsupported("opus_metadata: opus only")),
    }
}

struct OpusMetadata;

impl PacketMap for OpusMetadata {
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
        let mut f = (DESC.build)(&CodecParameters::audio().with_codec(CodecId::Opus)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[1, 2, 3]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[1, 2, 3]);
    }

    #[test]
    fn a_non_opus_codec_is_refused_at_construction() {
        let params = CodecParameters::audio().with_codec(CodecId::Aac);
        assert!((DESC.build)(&params).is_err());
    }
}
