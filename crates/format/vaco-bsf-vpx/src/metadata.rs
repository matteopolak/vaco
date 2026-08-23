//! `vp9_metadata`: rewrite the colour-space/colour-range fields VP9's
//! `color_config()` states.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=vp9_metadata` lists two options, `color_space` and
//! `color_range`, and both default to `-1` — "leave whatever the bitstream
//! already says alone". Measured directly: `-bsf:v vp9_metadata` with no
//! option string, run on a real `libvpx-vp9` elementary stream, reproduced
//! the input **byte for byte** (`cmp` against the unfiltered stream).
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string (`planning/INTERFACE-GAPS.md` gap 12), so — exactly as with
//! `vaco-bsf-av1::metadata` — this crate can only ever construct the
//! bare-name behaviour, and the bare-name behaviour is measured identity.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "vp9_metadata",
    long_name: "Modify metadata embedded in a VP9 stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Vp9) => Ok(Box::new(MappedFilter::new(Vp9Metadata))),
        _ => Err(Error::Unsupported("vp9_metadata: vp9 only")),
    }
}

struct Vp9Metadata;

impl PacketMap for Vp9Metadata {
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
        let mut f = (DESC.build)(&CodecParameters::video().with_codec(CodecId::Vp9)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[9, 8, 7]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[9, 8, 7]);
    }

    #[test]
    fn a_non_vp9_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Vp8);
        assert!((DESC.build)(&params).is_err());
    }
}
