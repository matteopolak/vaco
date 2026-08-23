//! Opus header parsing (no decode).
//!
//! Opus is **GREEN** under D9 — royalty-free and shippable — so unlike AAC there
//! is no licensing reason to stop at parsing. There is a *scoping* reason: v0.1
//! is `ffprobe` on modern containers (D5), which needs stream properties and
//! packet boundaries and nothing else. Nothing here decodes.
//!
//! # What is in here
//!
//! | Module | Syntax | Specification |
//! |---|---|---|
//! | [`head`] | `OpusHead`, and the MP4 `dOps` box | RFC 7845 §5.1, RFC 8486 §3, Opus-in-ISOBMFF |
//! | [`comment`] | `OpusTags` | RFC 7845 §5.2 |
//! | [`packet`] | the TOC byte and frame packing codes 0..=3 | RFC 6716 §3 |
//!
//! # What the caller must have done first
//!
//! **Opus packets are framed by the container, never by themselves.** There is
//! no sync word, no length prefix and no way to find a packet boundary by
//! looking at the bytes. [`OpusPacket::parse`] therefore takes a slice that is
//! *exactly* one packet — an Ogg packet reassembled from its segments, one
//! Matroska block frame, one MP4 sample, or one RTP payload — and treats its
//! length as authoritative.
//!
//! [`OpusParser`] exists so that a demuxer can reach this crate through
//! [`vaco_codec_core::Parser`] like any other, and it inherits that
//! requirement: **one `push` per packet**. It is not a resynchronising
//! byte-stream splitter, because there is nothing to resynchronise to. Pushing
//! two packets before draining produces one nonsense packet rather than two
//! good ones, which is why the contract is stated here rather than left to be
//! discovered.
//!
//! The one place Opus *is* self-delimiting is inside a multi-stream packet,
//! where every stream but the last codes its own length —
//! [`OpusPacket::parse_self_delimited`], RFC 6716 Appendix B.
//!
//! # Sample rate is always 48 kHz
//!
//! `input_sample_rate` in the identification header describes the material
//! *before* encoding and has no effect on anything: Opus decodes at 48 kHz
//! always. Probed against `ffprobe 8.1` — a header declaring 8000 still reports
//! `sample_rate=48000`. See [`head::OUTPUT_SAMPLE_RATE`].
//!
//! # Example
//!
//! ```
//! use vaco_parse_opus::{IdentificationHeader, OpusPacket};
//!
//! let head = IdentificationHeader::parse(b"OpusHead\x01\x02\x38\x01\x80\xbb\0\0\0\0\0")?;
//! assert_eq!(head.channel_count, 2);
//! assert_eq!(head.pre_skip, 312);
//!
//! // A 20 ms stereo CELT packet: config 31, stereo, code 0.
//! let packet = OpusPacket::parse(&[0xfc, 0x01, 0x02, 0x03])?;
//! assert_eq!(packet.samples(), 960);
//! assert_eq!(packet.frames.len(), 1);
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod comment;
pub mod head;
pub mod packet;

use vaco_codec_core::{CodecParameters, Parser};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

pub use comment::{CommentHeader, CommentIter};
pub use head::{IdentificationHeader, MappingFamily, OUTPUT_SAMPLE_RATE, ambisonic_order};
pub use packet::{Bandwidth, MAX_FRAME_BYTES, MAX_FRAMES, Mode, OpusPacket, Toc};

/// Validates already-framed Opus packets and reports stream parameters.
///
/// See the crate documentation: **each `parse` call's input must be exactly one
/// packet.** The parser consumes the whole slice, validates its framing, and
/// hands it back as a [`Packet`].
///
/// Parameters come from the identification header, which the container supplies
/// out of band — [`OpusParser::set_identification_header`] or
/// [`OpusParser::with_extradata`]. A parser that has never seen one still
/// validates packets; it just cannot describe the stream.
#[derive(Debug)]
pub struct OpusParser {
    head: Option<IdentificationHeader>,
    params: Option<CodecParameters>,
    budget: Budget,
    packets: u64,
    samples: u64,
}

impl OpusParser {
    /// A parser with no identification header yet.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            head: None,
            params: None,
            budget: Budget::new(limits),
            packets: 0,
            samples: 0,
        }
    }

    /// A parser configured from a stream's `extradata`, which for Opus is the
    /// `OpusHead` packet — the shape Ogg, Matroska and (after conversion from
    /// `dOps`) MP4 all present.
    ///
    /// # Errors
    ///
    /// Whatever [`IdentificationHeader::parse`] returns.
    pub fn with_extradata(limits: Limits, extradata: &[u8]) -> Result<Self> {
        let mut parser = Self::new(limits);
        parser.set_identification_header(IdentificationHeader::parse(extradata)?);
        Ok(parser)
    }

    /// Supply the identification header out of band.
    pub fn set_identification_header(&mut self, head: IdentificationHeader) {
        self.params = Some(head.to_codec_parameters());
        self.head = Some(head);
    }

    /// The identification header, if one has been supplied.
    #[must_use]
    pub const fn identification_header(&self) -> Option<&IdentificationHeader> {
        self.head.as_ref()
    }

    /// Packets validated so far.
    #[must_use]
    pub const fn packets(&self) -> u64 {
        self.packets
    }

    /// Total duration of those packets in 48 kHz samples, `pre_skip` included.
    ///
    /// A demuxer needs this to turn a granule position into a packet duration.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// Split a multi-stream packet into its per-stream sub-packets.
    ///
    /// RFC 6716 Appendix B: every stream but the last uses the self-delimiting
    /// framing, so the boundaries are recoverable. The identification header
    /// says how many streams there are; without one this returns `None`, since
    /// guessing the count is exactly the mistake that turns a malformed packet
    /// into a parser that walks off the end.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] when a sub-packet's framing does not
    /// fit the bytes available.
    #[must_use]
    pub fn split_streams<'a>(&self, data: &'a [u8]) -> Option<Result<Vec<OpusPacket<'a>>>> {
        let streams = usize::from(self.head.as_ref()?.stream_count);
        Some(split_streams(data, streams))
    }
}

/// Split `data` into `streams` sub-packets: `streams - 1` self-delimited, then
/// one that takes the remainder.
///
/// # Errors
///
/// [`vaco_core::Error::InvalidData`] when a sub-packet does not fit.
pub fn split_streams(data: &[u8], streams: usize) -> Result<Vec<OpusPacket<'_>>> {
    let mut out = Vec::new();
    let mut rest = data;
    for index in 0..streams {
        let last = index + 1 == streams;
        let packet = if last {
            OpusPacket::parse(rest)?
        } else {
            OpusPacket::parse_self_delimited(rest)?
        };
        let consumed = packet.len;
        out.push(packet);
        rest = rest
            .get(consumed..)
            .ok_or(vaco_core::Error::InvalidData("Opus sub-packet overruns"))?;
    }
    Ok(out)
}

impl Parser for OpusParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            // End of stream. Nothing is buffered here — a packet is whole or it
            // is not a packet — so there is never a final unit to flush.
            return Ok((None, 0));
        }
        let parsed = OpusPacket::parse(input)?;
        let mut packet = Packet::from_slice(&mut self.budget, input)?;
        // Every Opus packet is independently decodable.
        packet.flags = PacketFlags::KEY;
        self.packets = self.packets.saturating_add(1);
        self.samples = self.samples.saturating_add(u64::from(parsed.samples()));
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// Read an `OpusHead`.
    ///
    /// Opus has **no in-band configuration at all** — the channel count, the
    /// pre-skip and the mapping live only in the identification header the
    /// container carries — so for this codec `set_extradata` is not an
    /// optimisation, it is the only way a parser can describe the stream.
    /// Measured: `sample_fmt`, `channels`, `channel_layout` and
    /// `initial_padding` all arrive here and none of them arrives from a
    /// packet.
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        if extradata.is_empty() {
            return Ok(());
        }
        self.set_identification_header(IdentificationHeader::parse(extradata)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// The registry descriptor for this parser.
///
/// `vaco-component.toml` names this const, `cargo xtask gen-registry` puts it
/// in `vaco_registry::PARSERS`, and a demuxer reaches it through
/// `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER: ::vaco_codec_core::ParserDesc = ::vaco_codec_core::ParserDesc {
    name: "opus",
    long_name: "Opus (Opus Interactive Audio Codec)",
    codecs: &[::vaco_codec_core::CodecId::Opus],
    media_type: ::vaco_core::MediaType::Audio,
    make: |limits| ::std::boxed::Box::new(OpusParser::new(limits)),
};
