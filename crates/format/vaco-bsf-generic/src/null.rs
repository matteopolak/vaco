//! `null`: the identity bitstream filter.
//!
//! Measured: `ffmpeg -h bsf=null` declares no options and no codec
//! restriction, and `-bsf:v null` produces byte-identical output to no filter
//! at all on every input tried. There is nothing to get wrong here; it exists
//! so a chain can name "do nothing" explicitly (`vaco_format_core::mux`'s own
//! tests do exactly that with a hand-written `PassThrough`).

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "null",
    long_name: "Null bitstream filter",
    build,
};

struct Null;

impl PacketMap for Null {
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
    Ok(Box::new(MappedFilter::new(Null)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    #[test]
    fn a_packet_passes_through_unchanged() {
        let mut budget = Budget::new(Limits::strict());
        let pkt = Packet::from_slice(&mut budget, b"hello").unwrap();
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt)).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(out.payload(), b"hello");
        f.send_packet(None).unwrap();
        assert!(f.receive_packet().is_err());
    }
}
