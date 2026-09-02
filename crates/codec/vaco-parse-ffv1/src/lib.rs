//! FFV1 Configuration Record parsing — **no decode**.
//!
//! # Parsing is not decoding
//!
//! Same line every other `vaco-parse-*` crate draws (D5, D7, plan 15 §1.6):
//! this crate reads exactly enough of the Configuration Record (RFC 9043
//! §4.3) to answer "what pixel format does this stream carry", and stops.
//! No slice is read, no quantization table, no context state, no sample of
//! output — everything past `log2_v_chroma_subsample` in the record's field
//! order (RFC 9043 Figure 28) goes unread.
//!
//! # Why this exists
//!
//! RFC 9043 §4 requires width/height to come from "external means" (the
//! container), but colour space and chroma subsampling are stream-wide facts
//! that live *only* in this out-of-band record — nothing in a Matroska or
//! MP4 header states them, and no frame carries them either. Before this
//! crate existed, nothing populated `VideoParameters::format` for an FFV1
//! stream ahead of an actual decode, which is user-visible: `vaco -i`/
//! `vaco-probe -show_streams` reported `pix_fmt=none`/`unknown` where a real
//! decode (or the reference) reports the true value, and worse, callers that
//! *guessed* a format to unblock a pipeline (a converter's fallback, a
//! filtergraph's default) had no way to tell their guess from a fact.
//!
//! # The record is itself range-coded
//!
//! Unlike an `avcC`/`hvcC` (plain fixed-layout bytes), RFC 9043's
//! Configuration Record is `Parameters()` (§4.2) coded through the same
//! binary range coder (§3.8.1) that codes slice data, with its own context
//! array starting at 128. Reading `colorspace_type`/`chroma_planes`/
//! `log2_h_chroma_subsample`/`log2_v_chroma_subsample` correctly therefore
//! means running that entropy decoder from the very first bit — there is no
//! way to skip to a fixed byte offset — including consuming and discarding
//! `state_transition_delta` (256 further symbols) when a custom coder table
//! is signalled, since skipping those symbols instead of decoding them would
//! desynchronise every field read after them.
//!
//! This is a deliberately independent, minimal reimplementation of that
//! range decoder and the record's field layout, not a shared module with
//! `vaco-codec-ffv1` (which needs the *whole* record, including the
//! quantization table sets and per-context initial states this crate never
//! reads, to actually decode a frame). Duplicating the entropy primitive
//! this small a distance was judged lower-risk than threading a new
//! cross-crate dependency through an already-shipping, tested decoder crate
//! under review-light conditions; both copies implement the same published
//! RFC 9043 pseudocode, so they have the same source of truth to stay
//! correct against even though they are not the same source of code.
//!
//! # Specification
//!
//! RFC 9043 §3.8.1 (range coder), §4.2 (`Parameters`, Figure 28's field
//! order), §4.3/§4.3.2 (Configuration Record framing and its CRC).
//!
//! # Dependencies
//!
//! `vaco-codec-core` for the [`Parser`](vaco_codec_core::Parser) trait and
//! [`CodecParameters`], `vaco-pixfmt` for the pixel-format enum, `vaco-limits`
//! for the packetization budget, `vaco-packet` for the emitted packet. No
//! external runtime dependencies.

#![forbid(unsafe_code)]

use vaco_codec_core::{CodecId, CodecParameters, Parser, ParserDesc};
use vaco_core::{MediaType, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

mod crc;
mod header;
mod rangecoder;

use header::Header;

/// The registry descriptor for the FFV1 parser. `vaco-component.toml` names
/// this const, `cargo xtask gen-registry` puts it in `vaco_registry::PARSERS`,
/// and a demuxer/stream-discovery pass reaches it through `ParserProvider`
/// without ever naming this crate (D14.1).
pub const PARSER: ParserDesc = ParserDesc {
    name: "ffv1",
    long_name: "FFmpeg video codec #1 (Configuration Record only)",
    codecs: &[CodecId::Ffv1],
    media_type: MediaType::Video,
    make: |limits| Box::new(Ffv1Parser::new(limits)),
};

/// A [`Parser`] for FFV1: every container that carries FFV1 in this
/// workspace (Matroska, MP4) already delimits one coded frame as one
/// packet — the same "whole input is one already-framed sample" contract
/// `vaco-parse-opus` and `vaco-parse-vpx` document for the identical
/// reason (FFV1 has no elementary byte-stream format of its own to find a
/// boundary in) — so [`Parser::parse`] here never has to search for one.
#[derive(Debug)]
pub struct Ffv1Parser {
    budget: Budget,
    params: Option<CodecParameters>,
}

impl Ffv1Parser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            params: None,
        }
    }
}

impl Parser for Ffv1Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        let packet = Packet::from_slice(&mut self.budget, input)?;
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let Ok(header) = Header::parse(extradata) else {
            // Same contract every other `Parser::set_extradata` in this
            // workspace follows (see the trait's own doc): a malformed
            // record is not a reason to stop reporting what the container
            // itself already knows, so this is not an error.
            return Ok(());
        };
        self.params = Some(CodecParameters {
            video: Some(vaco_codec_core::VideoParameters {
                format: header.pix_fmt(),
                bits_per_raw_sample: Some(header.bits_per_raw_sample as u8),
                ..vaco_codec_core::VideoParameters::default()
            }),
            ..CodecParameters::default()
        });
        Ok(())
    }

    /// `true`: this crate's own module doc already states the contract —
    /// every container carrying FFV1 in this workspace delimits one coded
    /// frame as one packet, so [`Ffv1Parser::parse`] never assembles one
    /// from more than a single call. Declaring it costs this parser nothing
    /// (its parameters resolve from [`Ffv1Parser::set_extradata`] alone,
    /// before a single packet arrives, so an oversized frame was never the
    /// gap here — see `vaco-parse-prores`'s module doc, which names the
    /// concrete containers/resolutions where it was), but keeps a stream
    /// whose frames are individually large — FFV1 is lossless and a big
    /// frame at high resolution or high bit depth is real, not hypothetical
    /// — from failing `ParserDriver::push`'s reassembly cap for no reason at
    /// all: nothing needs that buffer here, so nothing should be bounded by
    /// it either.
    fn whole_sample_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code exercising the parser, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    /// The same real, `ffmpeg 9.0.1`-encoded `yuv420p` Configuration Record
    /// `header`'s own tests parse directly -- exercised here through the
    /// public `Parser` surface instead, so the two layers (record parsing,
    /// `Parser`/`CodecParameters` plumbing) each have their own coverage.
    #[test]
    fn a_real_ffmpeg_record_resolves_through_the_parser_trait() {
        let record: [u8; 42] = [
            0x56, 0x2b, 0x84, 0xd1, 0x9c, 0x05, 0x2f, 0x41, 0x3c, 0x60, 0x26, 0xe9, 0x5c, 0x37,
            0x6f, 0x5d, 0x1b, 0x76, 0x97, 0x9d, 0x3a, 0xc9, 0xc4, 0x20, 0x43, 0x1e, 0x8b, 0x9f,
            0x55, 0x20, 0x51, 0x2f, 0x4e, 0xf8, 0xa1, 0x68, 0x3b, 0x9b, 0x17, 0x13, 0x7c, 0x03,
        ];
        let mut p = Ffv1Parser::new(Limits::permissive());
        p.set_extradata(&record).expect("set_extradata");
        let params = p.parameters().expect("parameters resolved");
        let video = params.video.as_ref().expect("video parameters");
        assert_eq!(video.format, Some(PixFmt::Yuv420p));
        assert_eq!(video.bits_per_raw_sample, Some(8));
    }

    #[test]
    fn a_truncated_record_is_not_an_error_and_reports_nothing() {
        let mut p = Ffv1Parser::new(Limits::permissive());
        p.set_extradata(&[0, 1, 2])
            .expect("truncated record is not an error");
        assert!(p.parameters().is_none());
    }

    #[test]
    fn parse_passes_one_whole_packet_through_unchanged() {
        let mut p = Ffv1Parser::new(Limits::permissive());
        let (packet, used) = p.parse(&[1, 2, 3, 4]).expect("parse");
        assert_eq!(used, 4);
        assert_eq!(packet.expect("packet").payload(), &[1, 2, 3, 4]);
    }

    #[test]
    fn parse_of_empty_input_reports_nothing_consumed() {
        let mut p = Ffv1Parser::new(Limits::permissive());
        let (packet, used) = p.parse(&[]).expect("parse");
        assert!(packet.is_none());
        assert_eq!(used, 0);
    }
}
