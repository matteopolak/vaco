//! `chomp`: trim trailing zero padding from a packet.
//!
//! `ffmpeg -h bsf=chomp` declares no options and no codec restriction — the
//! whole filter is "drop `0x00` bytes off the end of the packet", which is
//! unambiguous enough that there is only one reading of it. Exists for
//! producers that pad a sample to a block size (some capture devices, some
//! broken muxers) and a consumer downstream that treats the padding as part
//! of the bitstream.
//!
//! Trims *every* trailing zero byte, not a fixed count: a packet of all
//! zeros becomes empty, and a packet with no trailing zero is untouched.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "chomp",
    long_name: "Remove zero padding at the end of a packet",
    build,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(_params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    Ok(Box::new(MappedFilter::new(Chomp {
        budget: Budget::new(Limits::permissive()),
    })))
}

struct Chomp {
    budget: Budget,
}

impl PacketMap for Chomp {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let payload = p.payload();
        let trimmed_len = payload.len() - payload.iter().rev().take_while(|&&b| b == 0).count();
        let Some(trimmed) = payload.get(..trimmed_len) else {
            out.push_back(p.clone());
            return Ok(());
        };
        if trimmed.len() == payload.len() {
            out.push_back(p.clone());
            return Ok(());
        }
        let mut np = Packet::from_slice(&mut self.budget, trimmed)?;
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

    fn pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut Budget::new(Limits::strict()), bytes).unwrap()
    }

    #[test]
    fn trailing_zeros_are_removed() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"hello\x00\x00\x00"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"hello");
    }

    #[test]
    fn a_packet_with_no_trailing_zero_is_untouched() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"hello"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"hello");
    }

    #[test]
    fn an_interior_zero_is_kept() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"he\x00lo\x00\x00"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"he\x00lo");
    }

    #[test]
    fn an_all_zero_packet_becomes_empty() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(&[0, 0, 0]))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"");
    }

    #[test]
    fn falsified_a_fixed_trim_count_would_leave_padding_or_eat_data() {
        // Planting the defect: trimming a fixed count (say, one byte) rather
        // than "every trailing zero" gets both a longer-than-one-byte pad and
        // a zero-length pad wrong. Checked against the real implementation
        // above via the two tests it already has; this documents why the
        // loop is a `take_while`, not a constant.
        let payload = b"hello\x00\x00\x00";
        let fixed_trim = payload.get(..payload.len() - 1).unwrap_or(&[]);
        assert!(
            fixed_trim.ends_with(&[0, 0]),
            "still padded after a fixed trim"
        );
    }
}
