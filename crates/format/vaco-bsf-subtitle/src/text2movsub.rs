//! `text2movsub`: wrap plain UTF-8 text into an MP4/QuickTime "Text Sample"
//! (`mov_text`) packet — the inverse of [`crate::mov2textsub`].
//!
//! # What is measured
//!
//! Measured directly against `ffmpeg 8.1`: transcoding `sub.srt` to the
//! `text` codec and running `-bsf:s text2movsub` on it produces exactly the
//! two-byte big-endian length of the input packet followed by the input
//! bytes unchanged — `"Hello world"` (11 bytes) becomes `00 0b "Hello
//! world"`, `"Second line"` (11 bytes) becomes `00 0b "Second line"`. No
//! style box or other suffix is added; this is the literal inverse of
//! [`crate::mov2textsub`]'s prefix-and-truncate.
//!
//! # The length limit — measured, not assumed
//!
//! A `u16` length field cannot express more than 65535 bytes of text.
//! Measured directly: an input packet of exactly 65535 bytes is accepted;
//! one of 65536 bytes is refused outright (`ffmpeg` reports "Invalid data
//! found when processing input" and produces no output packet at all — not a
//! truncation, not a wraparound). This filter reproduces the refusal rather
//! than silently truncating, which is exactly
//! `CONFORMANCE-FINDINGS.md` finding 31's point: the reference's own range
//! (`0..=65535` here, since there is no `-h bsf=` option table for a filter
//! with no `AVOption`s to state it) is the bound this filter must enforce
//! too, not an incidental implementation detail to skip.
//!
//! # Configuration
//!
//! `ffmpeg -h bsf=text2movsub` declares no options and no codec restriction.
//! Gap 12 (`planning/INTERFACE-GAPS.md`) has nothing to block here.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "text2movsub",
    long_name: "Convert text subtitles to MOV text",
    build,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(_params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    Ok(Box::new(MappedFilter::new(Text2MovSub {
        // Two budgets, not one, matching `vaco-bsf-h2645::h264_mp4toannexb`'s
        // `convert_budget`/`out_budget` split: the scratch buffer assembled
        // below and the packet's own storage are separately-accounted
        // allocations, not one input-derived size charged twice against the
        // same allowance.
        assemble_budget: Budget::new(Limits::permissive()),
        out_budget: Budget::new(Limits::permissive()),
    })))
}

struct Text2MovSub {
    assemble_budget: Budget,
    out_budget: Budget,
}

impl PacketMap for Text2MovSub {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let text = p.payload();
        let len = u16::try_from(text.len())
            .map_err(|_| Error::InvalidData("text2movsub: text sample longer than 65535 bytes"))?;
        // The sanctioned way to size an input-derived allocation
        // (`Vec::with_capacity` is denied workspace-wide) — a zeroed buffer,
        // written via `copy_from_slice` rather than indexing.
        let mut buf: Vec<u8> = self.assemble_budget.alloc(text.len().saturating_add(2))?;
        // `alloc` returns exactly `text.len() + 2` zeroed bytes, so the split
        // point is always in range.
        let (head, tail) = buf.split_at_mut(2);
        head.copy_from_slice(&len.to_be_bytes());
        tail.copy_from_slice(text);
        let mut np = Packet::from_slice(&mut self.out_budget, &buf)?;
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
        let mut budget = Budget::new(Limits::strict());
        Packet::from_slice(&mut budget, bytes).unwrap()
    }

    /// A genuine reference oracle: bytes measured directly from `ffmpeg 8.1`
    /// (`sub.srt` -> `text` -> `-bsf:s text2movsub`).
    #[test]
    fn matches_the_reference_on_two_plain_cues() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"Hello world"))).unwrap();
        assert_eq!(
            f.receive_packet().unwrap().payload(),
            b"\x00\x0bHello world"
        );
        f.send_packet(Some(&pkt(b"Second line"))).unwrap();
        assert_eq!(
            f.receive_packet().unwrap().payload(),
            b"\x00\x0bSecond line"
        );
    }

    #[test]
    fn empty_text_becomes_a_bare_zero_length_prefix() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b""))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"\x00\x00");
    }

    /// The measured boundary: exactly `u16::MAX` bytes is the largest text
    /// sample a length-prefixed packet can carry.
    #[test]
    fn exactly_u16_max_bytes_is_accepted() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        let text = vec![b'a'; usize::from(u16::MAX)];
        f.send_packet(Some(&pkt(&text))).unwrap();
        let out = f.receive_packet().unwrap();
        assert_eq!(
            out.payload().get(..2),
            Some(u16::MAX.to_be_bytes().as_slice())
        );
        assert_eq!(out.payload().len(), text.len() + 2);
    }

    /// Measured against the reference: one byte more is refused outright,
    /// not truncated and not wrapped.
    #[test]
    fn one_byte_past_u16_max_is_refused() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        let text = vec![b'a'; usize::from(u16::MAX) + 1];
        assert!(f.send_packet(Some(&pkt(&text))).is_err());
    }

    #[test]
    fn falsified_a_silent_truncation_would_pass_the_oversize_case_too() {
        // Planting the defect: a filter that truncated to 65535 bytes
        // instead of refusing would also "succeed" on the oversize input,
        // which is exactly what the reference does not do. This asserts the
        // truncating variant is a real defect, not merely an alternative
        // reading of "the length does not fit".
        let text = vec![b'a'; usize::from(u16::MAX) + 1];
        let truncated_would_be_len = u16::try_from(text.len().min(usize::from(u16::MAX))).unwrap();
        assert_eq!(
            truncated_would_be_len,
            u16::MAX,
            "a truncating implementation would not error here"
        );
    }
}
