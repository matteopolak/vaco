//! `prores_metadata`: rewrite colour-description fields (primaries, transfer
//! characteristic, matrix/colourspace) in an Apple `ProRes` stream.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=prores_metadata` lists three options — `color_primaries`,
//! `color_trc`, `colorspace` — and all three default to `auto` (`-1`,
//! documented literally as "keep the same" for each one).
//!
//! Measured directly against `ffmpeg 8.1`: a `prores_ks`-encoded (profile 3,
//! "HQ") `QuickTime` file's video stream, run through `-bsf:v prores_metadata`
//! with no option string, produced `framemd5`-identical output to the
//! unfiltered stream — the same per-packet payload, checked independently of
//! container overhead so a `mov` remux side effect could not be mistaken for
//! a filter effect.
//!
//! # Configuration
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string (`planning/INTERFACE-GAPS.md` gap 12 — see this crate's top-level
//! docs for why it is not closed here), so all three options above are
//! permanently unreachable, not merely unimplemented. No numeric option is
//! read here, so `CONFORMANCE-FINDINGS.md` finding 31 (unenforced option
//! ranges) has nothing to apply to.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "prores_metadata",
    long_name: "Modify color property metadata embedded in a ProRes stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Prores) => Ok(Box::new(MappedFilter::new(ProresMetadata))),
        _ => Err(Error::Unsupported("prores_metadata: prores only")),
    }
}

struct ProresMetadata;

impl PacketMap for ProresMetadata {
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
        let mut f = (DESC.build)(&CodecParameters::video().with_codec(CodecId::Prores)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[0xde, 0xad, 0xbe, 0xef]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(
            f.receive_packet().unwrap().payload(),
            &[0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn a_non_prores_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::Dnxhd);
        assert!((DESC.build)(&params).is_err());
    }
}
