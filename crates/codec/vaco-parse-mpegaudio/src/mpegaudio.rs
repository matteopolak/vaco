//! MPEG-1/2/2.5 Layer I/II/III as a resynchronising byte-stream
//! [`Parser`](vaco_codec_core::Parser).
//!
//! The framing shape mirrors `vaco-parse-aac`'s `AdtsParser`: a twelve-bit
//! sync word (here, the header's own eleven `1` bits plus the two version
//! bits this crate does not further constrain) occurs by chance often enough
//! in random data that a candidate frame is only accepted once a second sync
//! word is found exactly `frame_len()` bytes later — until the stream is
//! known to be in sync, after which frames are taken as they come so the
//! last frame of a file can still be emitted.
//!
//! Free-format frames (`bitrate_index == 0`) state no `frame_len()` at all —
//! measuring one requires finding the *next* sync word, which this resync
//! loop cannot do without first assuming a length. They are therefore
//! treated as a sync failure and skipped, which is an honest, named cut
//! rather than a guess: no fixture reachable here exercises free-format MP3.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_format_mpegaudio::{Layer, MpegAudioHeader};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// The codec identity a frame's layer field names.
const fn codec_for_layer(layer: Layer) -> CodecId {
    match layer {
        Layer::I => CodecId::Mp1,
        Layer::II => CodecId::Mp2,
        Layer::III => CodecId::Mp3,
    }
}

/// Fold an already-validated header into the parameters a container reports.
///
/// `sample_fmt` is `fltp` — the decoder's planar-float output, not anything
/// the bitstream states — measured with `ffprobe 8.1` against
/// `ffmpeg -c:a libmp3lame` output, the same convention `vaco-parse-aac` and
/// `vaco-parse-opus` already document for their own codecs.
fn to_codec_parameters(header: MpegAudioHeader) -> CodecParameters {
    let mut params = CodecParameters::audio().with_codec(codec_for_layer(header.layer));
    params.bit_rate = header
        .bitrate_kbps()
        .map(|kbps| u64::from(kbps).saturating_mul(1000));
    params.audio = Some(AudioParameters {
        sample_rate: header.sample_rate_hz(),
        format: Some(vaco_sampfmt::SampleFmt::F32P),
        layout: ChannelLayout::default_for(u32::from(header.channels()))
            .or_else(|| Some(ChannelLayout::unspecified(u32::from(header.channels())))),
        bits_per_coded_sample: None,
        bits_per_raw_sample: None,
        initial_padding: 0,
    });
    params
}

/// Whether two bytes could open a header: byte 0 is `0xFF` and the top three
/// bits of byte 1 (the rest of the eleven-bit sync) are set.
///
/// Used only to check the *next* candidate frame's start cheaply, the way
/// [`vaco_parse_aac`](https://docs.rs/vaco-parse-aac)'s `AdtsHeader::looks_like_sync`
/// does for ADTS.
fn looks_like_sync(a: u8, b: u8) -> bool {
    a == 0xff && (b & 0xe0) == 0xe0
}

/// Splits an MPEG audio byte stream into frames.
#[derive(Debug)]
pub struct MpegAudioParser {
    header: Option<MpegAudioHeader>,
    params: Option<CodecParameters>,
    budget: Budget,
    /// A candidate final frame, held until end of stream or a rejection.
    /// Bounded by the largest legal `frame_len()`, which for Layer II at the
    /// lowest sample rate and highest bit rate is under 3 KiB — this cannot
    /// grow with the input.
    deferred: Vec<u8>,
    synced: bool,
    frames: u64,
    resyncs: u64,
}

impl MpegAudioParser {
    /// A parser that allocates packets against `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            header: None,
            params: None,
            budget: Budget::new(limits),
            deferred: Vec::new(),
            synced: false,
            frames: 0,
            resyncs: 0,
        }
    }

    /// The most recently accepted header.
    #[must_use]
    pub const fn header(&self) -> Option<&MpegAudioHeader> {
        self.header.as_ref()
    }

    /// Frames emitted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many times the parser lost sync and had to scan for a new header.
    #[must_use]
    pub const fn resyncs(&self) -> u64 {
        self.resyncs
    }

    fn accept(&mut self, header: MpegAudioHeader) {
        if !self.synced {
            self.resyncs = self.resyncs.saturating_add(1);
        }
        if self.header != Some(header) {
            self.params = Some(to_codec_parameters(header));
        }
        self.header = Some(header);
        self.synced = true;
        self.frames = self.frames.saturating_add(1);
    }
}

impl Parser for MpegAudioParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            if self.deferred.is_empty() {
                return Ok((None, 0));
            }
            let Some(header) = MpegAudioHeader::parse_bytes(&self.deferred) else {
                self.deferred.clear();
                return Ok((None, 0));
            };
            let mut packet = Packet::from_slice(&mut self.budget, &self.deferred)?;
            packet.flags = PacketFlags::KEY;
            self.deferred.clear();
            self.accept(header);
            return Ok((Some(packet), 0));
        }
        self.deferred.clear();

        let mut i = 0usize;
        while let Some(rest) = input.get(i..) {
            if rest.len() < MpegAudioHeader::LEN {
                break;
            }
            let Some(header) = MpegAudioHeader::parse_bytes(rest) else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            // Free-format (`bitrate_index == 0`) states no `frame_len()` at
            // all — see the module docs for why that is a named cut rather
            // than a guess.
            let Some(frame_len) = header.frame_len() else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            let Ok(frame_len) = usize::try_from(frame_len) else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            if frame_len < MpegAudioHeader::LEN {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            }
            if rest.len() < frame_len {
                return Ok((None, i));
            }
            if !self.synced {
                let next = rest.get(frame_len..).unwrap_or_default();
                match next {
                    [a, b, ..] if !looks_like_sync(*a, *b) => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [a] if *a != 0xff => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [] | [_] => {
                        if let Some(frame) = rest.get(..frame_len) {
                            self.deferred.extend_from_slice(frame);
                        }
                        return Ok((None, i));
                    }
                    _ => {}
                }
            }

            let Some(frame) = rest.get(..frame_len) else {
                return Err(Error::InvalidData("MPEG audio frame slice out of range"));
            };
            let mut packet = Packet::from_slice(&mut self.budget, frame)?;
            packet.flags = PacketFlags::KEY;
            self.accept(header);
            return Ok((Some(packet), i + frame_len));
        }
        Ok((None, i.min(input.len())))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// One frame's worth of samples over the header's own sample rate.
    ///
    /// There is no out-of-band configuration for this codec family — every
    /// container carries the frame header in-band — so this always reads the
    /// packet itself rather than a stored record, unlike
    /// `vaco-parse-aac::AdtsParser::packet_duration`'s configured path.
    fn packet_duration(&self, packet: &[u8]) -> Option<vaco_core::Rational> {
        let header = MpegAudioHeader::parse_bytes(packet)?;
        let samples = i32::try_from(header.samples_per_frame()).ok()?;
        let rate = i32::try_from(header.sample_rate_hz()).ok()?;
        if samples <= 0 || rate <= 0 {
            return None;
        }
        Some(vaco_core::Rational::new(samples, rate))
    }
}

/// The next offset at or after `from` that could begin a sync word.
fn advance_to_sync(input: &[u8], from: usize) -> usize {
    match input.get(from..) {
        Some(rest) => from + rest.iter().position(|&b| b == 0xff).unwrap_or(rest.len()),
        None => input.len(),
    }
}
