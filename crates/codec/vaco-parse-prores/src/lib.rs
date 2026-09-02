//! `ProRes` `frame_header()` parsing — **no decode**.
//!
//! # Parsing is not decoding
//!
//! Same line every other `vaco-parse-*` crate draws (D5, D7, plan 15 §1.6):
//! this crate reads exactly enough of `frame_header()` (RDD 36 §5.1.1) to
//! answer "what pixel format does this stream carry", and stops. No slice is
//! read, no quantization matrix, no coefficient, no sample of output.
//!
//! # Why this exists
//!
//! The same gap `vaco-parse-ffv1` closed for FFV1, for a codec whose format
//! lives somewhere else entirely: RDD 36 states no stream-wide configuration
//! record at all — `chroma_format` and `alpha_channel_type` are fields of
//! *every frame's own* `frame_header()`, RDD 36 §5.1.1, never of anything
//! MOV/MP4 carry out of band. Before this crate existed, nothing populated
//! `VideoParameters::format` for a `ProRes` stream ahead of an actual decode:
//! `vaco -i`/`vaco-probe -show_streams` reported `pix_fmt=none` where a real
//! decode (or the reference, which decodes ahead for exactly this reason)
//! reports the true value, and a caller that guessed one to unblock a
//! pipeline had no way to tell its guess from a fact — see `vaco-parse-ffv1`'s
//! module doc for the concrete failure this caused (a converter silently
//! producing greyscale output from colour video).
//!
//! # A frame header, not a configuration record — a different shape of parser
//!
//! FFV1's Configuration Record is one stream-wide record delivered once,
//! through [`Parser::set_extradata`], before a single packet arrives. `ProRes`
//! has no such record, so this parser instead reads [`Parser::parse`]'s own
//! `input` — the same whole coded frame `vaco-codec-prores`'s decoder is
//! handed — the same shape `vaco-parse-vpx`'s `Vp9Parser` already uses for
//! VP9's per-frame `uncompressed_header()`. `frame_header()` is also not
//! entropy-coded (unlike FFV1's range-coded record), so no decoder state
//! needs to be carried between calls: [`header::parse`] is a pure function
//! of one sample's leading ~18 bytes, tried again on every packet handed to
//! [`Parser::parse`] until one succeeds (a truncated or malformed first
//! sample is not a reason to give up on a later, intact one — the same
//! tolerance `Vp9Parser::parse` extends to a frame its own header parse
//! fails on).
//!
//! # A different, smaller demand on `Discovery`'s bound than FFV1's
//!
//! `vaco-format-core::Discovery`'s `refine` hands a parser's [`Parser::parse`]
//! call the packet's *whole* payload — not a bounded prefix — after pushing
//! that whole payload through [`vaco_codec_core::ParserDriver`]'s reassembly
//! buffer (`DEFAULT_MAX_PENDING`, 2 MiB). [`header::parse`] itself only reads
//! the sample's leading ~18 bytes and ignores the rest, but the *push*
//! happens before parsing gets a say, so a single `ProRes` sample larger than
//! 2 MiB is refused by the reassembly buffer before this crate ever sees it.
//! FFV1's parser made no such demand at all — its record arrives once, out of
//! band, sized in the tens of bytes, never through this buffer.
//!
//! At Apple's published data rates a 2 MiB frame is not exotic for this
//! codec: 1920×1080 4444 XQ (~500 Mbit/s) averages roughly 2.1 MB per frame
//! at 24 fps, and every profile at 3840×2160 (4x the pixels) is comfortably
//! over 2 MiB per frame. A stream whose first several samples all exceed the
//! cap therefore falls back to exactly today's pre-fix behaviour —
//! `pix_fmt=none`/`unknown`, not a crash and not a wrong guess — the same
//! "malformed record told this parser nothing" fallback [`header::parse`]
//! already returns for a genuinely broken sample. This is not new to `ProRes`:
//! `vaco-parse-vpx`'s identical "whole input is one already-framed sample"
//! `Vp9Parser` is exactly as exposed on a large VP9 key frame, undocumented
//! there before this paragraph named it here.
//!
//! Raising `DEFAULT_MAX_PENDING` (or giving `Discovery::build_parser` a way
//! to ask a parser for a bigger one) is a shared, workspace-wide change to
//! `vaco-codec-core`/`vaco-format-core` that would move the bound for every
//! parser using this contract at once, on the read path, over
//! attacker-controlled input — out of scope for adding one parser, and not
//! done quietly here. Named instead: **a `ProRes` stream needs its reassembly
//! bound sized to its largest sample, not a fixed 2 MiB, if every profile at
//! every resolution is to resolve its pixel format during discovery** — the
//! concrete number for a given deployment is `bit_rate / (8 * frame_rate)`
//! for the largest profile it expects to see, comfortably over 2 MiB at 4K.
//!
//! # Specification
//!
//! SMPTE RDD 36-2022 §5.1.1 (`frame_header()`, Table 4's field order) and
//! Table 1/Table 7 (`chroma_format`/`alpha_channel_type` code points) — the
//! same freely-published document `vaco-codec-prores` decodes against (see
//! `provenance/vaco-codec-prores.toml`, source `smpte-rdd36-2022`). `ProRes`
//! *decode* is GREEN in the default distributable build per
//! `planning/research/07-legal-patents-licensing.md` §5.1; this crate reads
//! frame-header syntax only, the same scope note that already covers
//! `vaco-codec-prores` itself.
//!
//! # Dependencies
//!
//! `vaco-bitstream` for the sticky-overrun bit reader, `vaco-codec-core` for
//! the [`Parser`] trait and [`CodecParameters`], `vaco-pixfmt` for the
//! pixel-format enum, `vaco-limits` for the packetization budget,
//! `vaco-packet` for the emitted packet. No external runtime dependencies.

#![forbid(unsafe_code)]

use vaco_codec_core::{CodecId, CodecParameters, Parser, ParserDesc, VideoParameters};
use vaco_core::{MediaType, Result};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

mod header;

/// The registry descriptor for the `ProRes` parser. `vaco-component.toml`
/// names this const, `cargo xtask gen-registry` puts it in
/// `vaco_registry::PARSERS`, and a demuxer/stream-discovery pass reaches it
/// through `ParserProvider` without ever naming this crate (D14.1).
pub const PARSER: ParserDesc = ParserDesc {
    name: "prores",
    long_name: "Apple ProRes (frame header only)",
    codecs: &[CodecId::Prores],
    media_type: MediaType::Video,
    make: |limits| Box::new(ProresParser::new(limits)),
};

/// A [`Parser`] for `ProRes`: every container that carries it in this
/// workspace (MOV, MP4) already delimits one coded frame as one packet, so
/// [`Parser::parse`] never has to search for a boundary — it only reads the
/// leading bytes of whatever whole sample it is handed.
#[derive(Debug)]
pub struct ProresParser {
    budget: Budget,
    params: Option<CodecParameters>,
}

impl ProresParser {
    /// A parser bounded by `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits),
            params: None,
        }
    }
}

impl Parser for ProresParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            return Ok((None, 0));
        }
        let packet = Packet::from_slice(&mut self.budget, input)?;
        // Tried on every sample, not just the first: a truncated or
        // malformed leading sample is not a reason to give up on a later,
        // intact one arriving within the same bounded discovery pass — see
        // this crate's module doc.
        if let Some(info) = header::parse(input) {
            let found = CodecParameters {
                media_type: Some(MediaType::Video),
                video: Some(VideoParameters {
                    format: Some(info.format),
                    bits_per_raw_sample: Some(info.bits_per_raw_sample),
                    ..VideoParameters::default()
                }),
                ..CodecParameters::default()
            };
            if let Some(existing) = &mut self.params {
                existing.fill_from(&found);
            } else {
                self.params = Some(found);
            }
        }
        Ok((Some(packet), input.len()))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code exercising the parser, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    /// The real leading 26 bytes of the first `mdat` sample in this
    /// workspace's own `vaco-codec-prores` oracle fixture
    /// `tests/fixtures/422hq.mov` — a real `ffmpeg 9.0.1 -c:v prores_ks
    /// -profile:v 3` 4:2:2 HQ encode, hand-walked out of the file's
    /// `stsz`/`stco` boxes (`ffprobe` confirms `pix_fmt=yuv422p10le`,
    /// `bits_per_raw_sample=10` on this file) rather than synthesised, the
    /// same way `vaco-parse-ffv1`'s test embeds real extracted bytes for its
    /// record. `encoder_identifier` reads `"Lavc"` (libavcodec);
    /// `bitstream_version` is 0 here, which is legal only paired with 4:2:2
    /// and no alpha — exactly what this sample carries, so it doubles as
    /// coverage for that cross-field rule on real, not synthetic, input.
    #[test]
    fn a_real_ffmpeg_422hq_sample_resolves_through_the_parser_trait() {
        let sample: [u8; 26] = [
            0x00, 0x00, 0x08, 0xe7, 0x69, 0x63, 0x70, 0x66, 0x00, 0x94, 0x00, 0x00, 0x4c, 0x61,
            0x76, 0x63, 0x00, 0x40, 0x00, 0x40, 0x80, 0x00, 0x02, 0x02, 0x02, 0x00,
        ];
        let mut p = ProresParser::new(Limits::permissive());
        let (packet, used) = p.parse(&sample).expect("parse");
        assert_eq!(used, sample.len());
        assert!(packet.is_some());
        let params = p.parameters().expect("parameters resolved");
        let video = params.video.as_ref().expect("video parameters");
        assert_eq!(video.format, Some(PixFmt::Yuv422p10le));
        assert_eq!(video.bits_per_raw_sample, Some(10));
    }

    /// Same provenance as the 4:2:2 test above, from
    /// `tests/fixtures/444.mov` (`ffmpeg -c:v prores_ks -profile:v 4`,
    /// `ffprobe` confirms `pix_fmt=yuv444p12le`, no alpha).
    #[test]
    fn a_real_ffmpeg_444_sample_resolves_12_bit_444_with_no_alpha() {
        let sample: [u8; 26] = [
            0x00, 0x00, 0x12, 0x53, 0x69, 0x63, 0x70, 0x66, 0x00, 0x94, 0x00, 0x01, 0x4c, 0x61,
            0x76, 0x63, 0x00, 0x40, 0x00, 0x40, 0xc0, 0x00, 0x02, 0x02, 0x02, 0x00,
        ];
        let mut p = ProresParser::new(Limits::permissive());
        p.parse(&sample).expect("parse");
        let params = p.parameters().expect("parameters resolved");
        let video = params.video.as_ref().expect("video parameters");
        assert_eq!(video.format, Some(PixFmt::Yuv444p12le));
        assert_eq!(video.bits_per_raw_sample, Some(12));
    }

    /// Same provenance again, from `tests/fixtures/alpha444.mov`
    /// (`ffmpeg -c:v prores_ks -profile:v 4444`, `ffprobe` confirms
    /// `pix_fmt=yuva444p12le`). This fixture's `stsz` box states a single
    /// uniform sample size with no per-sample entries array (ISO/IEC
    /// 14496-12 §8.7.3.2: entries are only present when the box's own
    /// `sample_size` field is `0`) — the one box shape this crate's own
    /// mp4-box-walking to extract the byte array below had to get right to
    /// find the true sample start, since assuming entries are always
    /// present silently reads 4 bytes into the next box instead.
    #[test]
    fn a_real_ffmpeg_alpha444_sample_resolves_4444_with_alpha() {
        let sample: [u8; 26] = [
            0x00, 0x00, 0x24, 0x89, 0x69, 0x63, 0x70, 0x66, 0x00, 0x94, 0x00, 0x01, 0x4c, 0x61,
            0x76, 0x63, 0x00, 0x40, 0x00, 0x40, 0xc0, 0x00, 0x02, 0x02, 0x02, 0x02,
        ];
        let mut p = ProresParser::new(Limits::permissive());
        p.parse(&sample).expect("parse");
        let params = p.parameters().expect("parameters resolved");
        let video = params.video.as_ref().expect("video parameters");
        assert_eq!(video.format, Some(PixFmt::Yuva444p12le));
        assert_eq!(video.bits_per_raw_sample, Some(12));
    }

    #[test]
    fn a_malformed_first_sample_does_not_stop_a_later_good_one_resolving() {
        let mut p = ProresParser::new(Limits::permissive());
        let (packet, used) = p.parse(&[1, 2, 3, 4]).expect("parse of junk");
        assert!(packet.is_some());
        assert_eq!(used, 4);
        assert!(p.parameters().is_none());

        let mut sample = vec![0u8; 4];
        sample.extend_from_slice(b"icpf");
        let mut header = vec![0u8; 20];
        header[0..2].copy_from_slice(&20u16.to_be_bytes());
        header[3] = 1;
        header[12] = 0b1000_0000;
        sample.extend_from_slice(&header);
        p.parse(&sample).expect("parse of a real sample");
        assert!(p.parameters().is_some());
    }

    #[test]
    fn parse_of_empty_input_reports_nothing_consumed() {
        let mut p = ProresParser::new(Limits::permissive());
        let (packet, used) = p.parse(&[]).expect("parse");
        assert!(packet.is_none());
        assert_eq!(used, 0);
    }

    #[test]
    fn parameters_are_none_before_any_sample_is_seen() {
        let p = ProresParser::new(Limits::permissive());
        assert!(p.parameters().is_none());
    }
}
