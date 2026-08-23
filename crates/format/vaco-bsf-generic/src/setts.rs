//! `setts`: rewrite packet timestamps by expression.
//!
//! `ffmpeg -h bsf=setts` defaults every expression to identity — `-ts`
//! defaults to the literal expression `"TS"` (itself), `-duration` to
//! `"DURATION"`, `-time_base` to `0/1` (meaning "keep the input time base"),
//! `-prescale` to `false`. Measured directly: `-bsf:v setts` with no
//! assignments at all produces a byte-identical elementary stream to no
//! filter, on both timestamps and payload.
//!
//! [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open) carries no
//! option string, so a non-default expression is not reachable through the
//! registry seam today (`planning/INTERFACE-GAPS.md` records this once for
//! every filter it affects, not per filter). What is implemented here is
//! exactly what that leaves reachable: the identity transform, faithfully.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "setts",
    long_name: "Set packet timestamps",
    build,
};

struct Setts;

impl PacketMap for Setts {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        if let Some(p) = packet {
            out.push_back(p.clone());
        }
        Ok(())
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(_params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    Ok(Box::new(MappedFilter::new(Setts)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn default_options_are_the_identity_transform() {
        let mut budget = Budget::new(Limits::strict());
        let mut pkt = Packet::from_slice(&mut budget, b"payload").unwrap();
        pkt.pts = vaco_core::Timestamp::new(1234);
        pkt.dts = vaco_core::Timestamp::new(1230);
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(out.pts, pkt.pts);
        assert_eq!(out.dts, pkt.dts);
        assert_eq!(out.payload(), pkt.payload());
    }
}
