//! `remove_extra`: strip a leading copy of extradata from keyframe payloads.
//!
//! Measured (`ffmpeg 8.1`, `mpeg4` in an AVI container, 3 keyframes at
//! `-g 10` over 25 frames): the default (`freq=keyframe`) shrinks **every**
//! keyframe packet by exactly `extradata_size` bytes — 47 bytes off each of
//! the 3 keyframes, 141 total — and every keyframe's payload after removal
//! begins with a different start code than before, confirming the removed
//! bytes are a leading copy of the stream's own extradata (some encoders,
//! `mpeg4`/`msmpeg4` among them, repeat the sequence header in front of every
//! keyframe; this is the filter that un-repeats it). This is the opposite of
//! [`crate::dump_extra`]'s own `freq=keyframe` default, which touches only
//! the *first* keyframe — checked independently, not assumed symmetric.
//!
//! Implemented generically, as an exact byte-prefix match against
//! [`CodecParameters::extradata`], rather than per-codec parsing: that is
//! what the measured behaviour actually tests for, and it costs nothing on a
//! stream whose keyframes do not happen to repeat it (H.264/HEVC keyframes
//! carry their SPS/PPS, not a byte-for-byte copy of `extradata`, so the
//! prefix never matches and this filter is a no-op for them — exactly the
//! reference's own division of labour between this filter and
//! `extract_extradata`).

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "remove_extra",
    long_name: "Remove extradata repeated in the bitstream",
    build,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    let extradata = params.extradata.clone().unwrap_or_default();
    Ok(Box::new(MappedFilter::new(RemoveExtra {
        extradata,
        budget: Budget::new(Limits::permissive()),
    })))
}

struct RemoveExtra {
    extradata: Vec<u8>,
    budget: Budget,
}

impl PacketMap for RemoveExtra {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        if self.extradata.is_empty() || !p.is_key() || !p.payload().starts_with(&self.extradata) {
            out.push_back(p.clone());
            return Ok(());
        }
        let Some(rest) = p.payload().get(self.extradata.len()..) else {
            out.push_back(p.clone());
            return Ok(());
        };
        let mut np = Packet::from_slice(&mut self.budget, rest)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;
        np.side_data.clone_from(&p.side_data);
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
    fn a_repeated_prefix_is_stripped_from_every_keyframe() {
        let mut f = (DESC.build)(&params_with(b"HDR")).unwrap();
        f.send_packet(Some(&pkt(b"HDRone", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"one");
        f.send_packet(Some(&pkt(b"HDRtwo", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"two");
    }

    #[test]
    fn a_keyframe_without_the_prefix_is_untouched() {
        let mut f = (DESC.build)(&params_with(b"HDR")).unwrap();
        f.send_packet(Some(&pkt(b"one", true))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"one");
    }

    #[test]
    fn non_keyframes_are_never_touched_even_with_the_prefix() {
        let mut f = (DESC.build)(&params_with(b"HDR")).unwrap();
        f.send_packet(Some(&pkt(b"HDRone", false))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"HDRone");
    }
}
