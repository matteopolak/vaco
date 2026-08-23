//! `av1_metadata`: rewrite AV1 sequence/frame metadata (colour description,
//! the temporal delimiter, tick rate, padding OBUs) under a set of options.
//!
//! # Why this is the identity transform
//!
//! `ffmpeg -h bsf=av1_metadata` lists nine options — `td`, `color_primaries`,
//! `transfer_characteristics`, `matrix_coefficients`, `color_range`,
//! `chroma_sample_position`, `tick_rate`, `num_ticks_per_picture`,
//! `delete_padding` — and **every one of them defaults to "leave it
//! alone"**: `td=pass`, the four colour-description options and
//! `chroma_sample_position` default to `-1` ("unset" — not "set to
//! unspecified"), `tick_rate` defaults to `0/1`, `num_ticks_per_picture`
//! defaults to `-1`, and `delete_padding` defaults to `false`. Measured
//! directly: `ffmpeg -bsf:v av1_metadata` with no option string, run on a
//! real SVT-AV1 stream, reproduced the input **byte for byte**
//! (`cmp` against the un-filtered elementary stream).
//!
//! [`vaco_format_core::mux::BsfProvider::open`] has no per-instance option
//! string (`planning/INTERFACE-GAPS.md` gap 12), so this crate can only ever
//! construct the bare-name behaviour — every option above is permanently
//! unreachable through the seam this workspace has today, not merely
//! unimplemented. If a caller ever needs `av1_metadata` to *do* something,
//! gap 12 is the blocker to close first; widening this filter without it
//! would just be dead code no path can exercise.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "av1_metadata",
    long_name: "Modify metadata embedded in an AV1 stream",
    build,
};

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    match params.codec_id {
        Some(CodecId::Av1) => Ok(Box::new(MappedFilter::new(Av1Metadata))),
        _ => Err(Error::Unsupported("av1_metadata: av1 only")),
    }
}

struct Av1Metadata;

impl PacketMap for Av1Metadata {
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
        let mut f = (DESC.build)(&CodecParameters::video().with_codec(CodecId::Av1)).unwrap();
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, &[1, 2, 3, 4]).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[1, 2, 3, 4]);
    }

    #[test]
    fn a_non_av1_codec_is_refused_at_construction() {
        let params = CodecParameters::video().with_codec(CodecId::H264);
        assert!((DESC.build)(&params).is_err());
    }
}
