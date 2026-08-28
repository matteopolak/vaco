//! `mov2textsub`: strip an MP4/QuickTime "Text Sample" (`mov_text`) packet
//! down to its plain UTF-8 text.
//!
//! # What is measured
//!
//! An MP4 `mov_text` sample (ISO/IEC 14496-17's Text Sample format, the same
//! layout `tx3g` uses) is a big-endian `u16` length followed by that many
//! bytes of UTF-8 text, optionally followed by style/box atoms (a `styl` box,
//! for text with in-line formatting) the text renderer may use. Measured
//! directly against `ffmpeg 8.1`: muxing an `.srt` to MP4 with `-c:s
//! mov_text`, then running `-bsf:s mov2textsub` on the extracted stream —
//!
//! * a plain two-cue file produces packets `00 0b "Hello world"` and
//!   `00 0b "Second line"`; the filter's output is exactly `"Hello
//!   world"`/`"Second line"` — the two-byte length prefix removed.
//! * a cue with an in-line `<b>` tag produces a packet carrying the text
//!   *and* a trailing `styl` box (`padding to 0x0009 "Bold text" 00 00 00 16
//!   73 74 79 6c ...`); the filter's output is exactly `"Bold text"` — the
//!   length prefix **and** the trailing style box are both dropped, not just
//!   the prefix. Confirms this truncates to the declared length rather than
//!   passing everything after the first two bytes through.
//!
//! A packet shorter than its own declared length (truncated/malformed input)
//! is not something this environment could produce through the reference to
//! measure directly; this filter takes what bytes are actually available
//! rather than indexing past the end, which is a safety requirement
//! (`indexing_slicing` is denied workspace-wide) rather than a measured
//! reference behaviour — disclosed here rather than presented as verified.
//!
//! # Configuration
//!
//! `ffmpeg -h bsf=mov2textsub` declares no options and no codec restriction,
//! so there is nothing gap 12 (`planning/INTERFACE-GAPS.md`) could be
//! blocking here, and no numeric option for `CONFORMANCE-FINDINGS.md` finding
//! 31 to apply to.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "mov2textsub",
    long_name: "Convert MOV text to text subtitles",
    build,
};

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(_params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    Ok(Box::new(MappedFilter::new(Mov2TextSub {
        budget: Budget::new(Limits::permissive()),
    })))
}

struct Mov2TextSub {
    budget: Budget,
}

/// The text bytes a `mov_text` sample declares, bounded to what is actually
/// present. A packet too short to hold even the two-byte length prefix
/// yields no text at all, rather than reading past the end.
fn declared_text(payload: &[u8]) -> &[u8] {
    let Some(&hi) = payload.first() else {
        return &[];
    };
    let Some(&lo) = payload.get(1) else {
        return &[];
    };
    let len = usize::from(u16::from_be_bytes([hi, lo]));
    let end = len.saturating_add(2).min(payload.len());
    payload.get(2..end).unwrap_or(&[])
}

impl PacketMap for Mov2TextSub {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        let text = declared_text(p.payload());
        let mut np = Packet::from_slice(&mut self.budget, text)?;
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
    /// (`sub.srt` -> `mov_text` -> MP4 -> `-bsf:s mov2textsub`).
    #[test]
    fn matches_the_reference_on_a_plain_cue() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        let input = b"\x00\x0bHello world";
        f.send_packet(Some(&pkt(input))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"Hello world");
    }

    /// Same oracle, on a cue whose `mov_text` packet carries a trailing
    /// `styl` style box: the reference's own output drops it along with the
    /// length prefix, not just the prefix.
    #[test]
    fn a_trailing_style_box_is_dropped_with_the_prefix() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        // The exact 33 bytes `ffprobe -show_data` reports for a `<b>Bold
        // text</b>` cue muxed as `mov_text`: the two-byte length, the text,
        // then a `styl` box the reference's own `mov2textsub` discards.
        let input = [
            0x00, 0x09, 0x42, 0x6f, 0x6c, 0x64, 0x20, 0x74, 0x65, 0x78, 0x74, 0x00, 0x00, 0x00,
            0x16, 0x73, 0x74, 0x79, 0x6c, 0x00, 0x01, 0x00, 0x00, 0x00, 0x09, 0x00, 0x01, 0x01,
            0x10, 0xff, 0xff, 0xff, 0xff,
        ];
        f.send_packet(Some(&pkt(&input))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"Bold text");
    }

    #[test]
    fn a_declared_length_past_the_end_is_bounded_not_indexed_out_of_range() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        // Declares 100 bytes of text but only carries 3.
        f.send_packet(Some(&pkt(b"\x00\x64abc"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"abc");
    }

    #[test]
    fn a_packet_too_short_for_the_length_prefix_yields_empty_text() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"\x00"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"");
    }
}
