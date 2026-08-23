//! `dump_extra`: prepend a stream's extradata onto an access unit's payload.
//!
//! Measured (`ffmpeg 8.1`, an MP4 H.264 stream reframed to Annex B and muxed
//! to MPEG-TS with and without `-bsf:v dump_extra`): the default
//! (`freq=keyframe`, `k`) prepends the stream's `extradata` bytes onto the
//! **first** keyframe's payload and nothing else — the second keyframe of a
//! two-GOP test file was byte-identical with and without the filter, and the
//! packet count did not change (this is a merge into the existing packet, not
//! an extra one). That is a different reading of "keyframe" than
//! [`crate::remove_extra`]'s own `freq=keyframe` default, which *does* apply
//! to every keyframe — the two filters were checked independently rather than
//! assumed symmetric, and they are not.
//!
//! `freq=all`/`e` was also measured and produced a pattern (later packets in
//! the same GOP also grew, later GOPs did not) this crate does not have a
//! confident model for yet; since
//! [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open) carries no
//! option string, `all` is unreachable through the registry seam regardless,
//! so only the default is implemented.
//!
//! Entirely codec-agnostic: the filter never inspects the payload, only
//! [`CodecParameters::extradata`].

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "dump_extra",
    long_name: "Dump extradata into the bitstream",
    build,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    let extradata = params.extradata.clone().unwrap_or_default();
    Ok(Box::new(MappedFilter::new(DumpExtra {
        extradata,
        dumped: false,
        budget: Budget::new(Limits::permissive()),
    })))
}

struct DumpExtra {
    extradata: Vec<u8>,
    /// Set once the first keyframe has had extradata prepended.
    dumped: bool,
    budget: Budget,
}

impl PacketMap for DumpExtra {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        if self.dumped || self.extradata.is_empty() || !p.is_key() {
            out.push_back(p.clone());
            return Ok(());
        }
        let mut merged = self.extradata.clone();
        merged.extend_from_slice(p.payload());
        let mut np = Packet::from_slice(&mut self.budget, &merged)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;
        np.side_data.clone_from(&p.side_data);
        self.dumped = true;
        out.push_back(np);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn pkt(bytes: &[u8], key: bool) -> Packet {
        let mut p = Packet::from_slice(&mut Budget::new(Limits::strict()), bytes).unwrap();
        if key {
            p.flags |= vaco_packet::PacketFlags::KEY;
        }
        p
    }

    fn params_with(extradata: &[u8]) -> CodecParameters {
        CodecParameters {
            extradata: Some(extradata.to_vec()),
            ..CodecParameters::default()
        }
    }

    #[test]
    fn extradata_is_prepended_to_the_first_keyframe_only() {
        let mut f = (DESC.build)(&params_with(b"EXTRA")).unwrap();
        f.send_packet(Some(&pkt(b"one", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"EXTRAone");
        f.send_packet(Some(&pkt(b"two", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"two");
    }

    #[test]
    fn non_keyframes_are_never_touched() {
        let mut f = (DESC.build)(&params_with(b"EXTRA")).unwrap();
        f.send_packet(Some(&pkt(b"p-frame", false))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"p-frame");
    }

    #[test]
    fn no_extradata_means_no_dump() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"one", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"one");
    }
}
