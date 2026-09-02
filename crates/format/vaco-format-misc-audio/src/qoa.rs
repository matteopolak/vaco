//! QOA (Quite OK Audio), `.qoa` file-level framing.
//!
//! Implemented directly from "The Quite OK Audio Format", Specification
//! Version 1.0, 2023.04.24 (<https://qoaformat.org/qoa-specification.pdf>).
//! `vaco_codec_simple_audio::qoa` decodes one *frame*'s bytes (frame header,
//! LMS state, then slices) per packet and deliberately does not parse the
//! 8-byte file header or the frame boundaries itself -- see that module's
//! own doc ("No `.qoa` file-level framing ... is handled here -- that is a
//! container concern for whatever demuxer reads `.qoa` files"). Until this
//! module existed, nothing did: the decoder was registered and listed by
//! `-decoders` but no demuxer could ever open a `.qoa` file, so it was
//! unreachable from the CLI on any real input.
//!
//! # Layout (big-endian throughout, spec's own `struct qoa_file_t`)
//!
//! ```text
//! file_header (8 bytes):
//!   magic:FourCC("qoaf")  samples:be32  // samples per channel in the file,
//!                                       // 0 means "streaming" (unknown)
//!
//! frame_header (8 bytes), once per frame:
//!   num_channels:u8  sample_rate:be24  fsamples:be16  fsize:be16
//!   // fsize is this frame's total byte size, INCLUDING this 8-byte header
//!   // -- so one packet is exactly the next `fsize` bytes starting at the
//!   // frame header, which is exactly what the codec crate's `decode`
//!   // expects as its packet payload.
//! ```
//!
//! Per the spec, every frame in a non-streaming file shares one channel
//! count and sample rate, so the first frame's header is authoritative for
//! the whole stream; a "streaming" file (`samples == 0`) may vary them
//! frame to frame; this demuxer reports the first frame's values and does
//! not re-detect a change mid-stream.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

const MAGIC: [u8; 4] = *b"qoaf";
const FRAME_HEADER_BYTES: usize = 8;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(&MAGIC) {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "qoa",
    long_name: "QOA (Quite OK Audio)",
    extensions: &["qoa"],
    mime_types: &["audio/x-qoa"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(QoaDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct QoaDemuxer {
    io: IoContext,
    stream: Stream,
    /// From the file header; 0 means "streaming", total length unknown.
    total_samples: u64,
    sample_rate: u32,
    samples_emitted: u64,
    budget: Budget,
    eof: bool,
}

impl QoaDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic is missing, the file has no
    /// frames, or the first frame declares zero channels or sample rate.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != MAGIC {
            return Err(Error::InvalidData("qoa: missing qoaf signature"));
        }
        let total_samples = u64::from(io.rb32()?);

        // Peek (not consume) the first frame's header: CodecParameters
        // needs channels/sample_rate up front, and read_packet must still
        // see these same 8 bytes as the start of the first packet.
        let (num_channels, sample_rate) = {
            let header = io.peek(FRAME_HEADER_BYTES)?;
            if header.len() < FRAME_HEADER_BYTES {
                return Err(Error::InvalidData("qoa: file has no frames"));
            }
            let byte = |i: usize| header.get(i).copied().unwrap_or(0);
            let num_channels = u32::from(byte(0));
            let sample_rate = (u32::from(byte(1)) << 16) | (u32::from(byte(2)) << 8) | u32::from(byte(3));
            (num_channels, sample_rate)
        };
        if num_channels == 0 || sample_rate == 0 {
            return Err(Error::InvalidData(
                "qoa: first frame declares zero channels or sample rate",
            ));
        }

        let mut stream = Stream::new(
            0,
            MediaType::Audio,
            Rational::new(1, sample_rate.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        params.codec_id = Some(CodecId::Qoa);
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = sample_rate;
            // Measured: vaco_codec_simple_audio::qoa::decode always produces
            // S16 (the spec's own output range is the signed 16-bit clamp).
            audio.format = Some(SampleFmt::S16);
            audio.layout = ChannelLayout::default_for(num_channels);
        }
        stream.params = params;
        if total_samples > 0 {
            stream.duration_ts = i64::try_from(total_samples).ok();
        }

        Ok(Self {
            io,
            stream,
            total_samples,
            sample_rate,
            samples_emitted: 0,
            budget: Budget::new(Limits::permissive()),
            eof: false,
        })
    }
}

impl Demuxer for QoaDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let (fsamples, fsize) = {
            let header = self.io.peek(FRAME_HEADER_BYTES)?;
            if header.len() < FRAME_HEADER_BYTES {
                self.eof = true;
                return Err(Error::Eof);
            }
            let byte = |i: usize| header.get(i).copied().unwrap_or(0);
            let fsamples = u32::from(u16::from_be_bytes([byte(4), byte(5)]));
            let fsize = usize::from(u16::from_be_bytes([byte(6), byte(7)]));
            (fsamples, fsize)
        };
        if fsize < FRAME_HEADER_BYTES {
            return Err(Error::InvalidData(
                "qoa: frame size shorter than its own header",
            ));
        }

        let mut pkt = Packet::alloc(&mut self.budget, fsize)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::try_from(self.samples_emitted).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        let micros = u64::from(fsamples)
            .saturating_mul(1_000_000)
            .checked_div(u64::from(self.sample_rate.max(1)))
            .unwrap_or(0);
        pkt.duration = Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX));

        self.samples_emitted = self.samples_emitted.saturating_add(u64::from(fsamples));
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        if self.total_samples == 0 {
            return None;
        }
        let micros = self
            .total_samples
            .checked_mul(1_000_000)?
            .checked_div(u64::from(self.sample_rate.max(1)))?;
        Some(Duration::from_micros(i64::try_from(micros).unwrap_or(i64::MAX)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// Hand-built per the spec's own `qoa_file_t`/frame header layout: an
    /// 8-byte file header, one frame header declaring a payload it does not
    /// actually carry (this demuxer only needs to hand the codec `fsize`
    /// bytes -- what is inside them is that crate's concern, exercised by
    /// its own tests).
    fn build_file(total_samples: u32, channels: u8, sample_rate: u32, frames: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&total_samples.to_be_bytes());
        for frame in frames {
            v.extend_from_slice(frame);
            let _ = (channels, sample_rate);
        }
        v
    }

    fn frame_header(channels: u8, sample_rate: u32, fsamples: u16, fsize: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.push(channels);
        v.extend_from_slice(&sample_rate.to_be_bytes()[1..4]);
        v.extend_from_slice(&fsamples.to_be_bytes());
        v.extend_from_slice(&fsize.to_be_bytes());
        v
    }

    #[test]
    fn opens_and_reads_two_frames_by_their_own_declared_size() {
        let mut frame0 = frame_header(2, 44_100, 20, 8 + 4);
        frame0.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let mut frame1 = frame_header(2, 44_100, 20, 8 + 2);
        frame1.extend_from_slice(&[0xEE, 0xFF]);

        let data = build_file(40, 2, 44_100, &[&frame0, &frame1]);
        let src = Box::new(MemorySource::new(data));
        let mut d = QoaDemuxer::open(src).unwrap();
        let audio = d.streams()[0].params.audio.clone().unwrap();
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.layout, ChannelLayout::default_for(2));

        let p0 = d.read_packet().unwrap();
        assert_eq!(p0.payload(), frame0.as_slice());
        assert_eq!(p0.pts.ticks(), Some(0));

        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.payload(), frame1.as_slice());
        assert_eq!(p1.pts.ticks(), Some(20));

        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn zero_samples_in_the_file_header_means_streaming_and_reports_no_duration() {
        let mut frame0 = frame_header(1, 8_000, 5, 8 + 1);
        frame0.push(0x00);
        let data = build_file(0, 1, 8_000, &[&frame0]);
        let src = Box::new(MemorySource::new(data));
        let d = QoaDemuxer::open(src).unwrap();
        assert_eq!(d.duration(), None);
    }

    #[test]
    fn probe_matches_only_the_signature() {
        let ok = ProbeData::new(b"qoaf\x00\x00\x00\x00garbage");
        assert_eq!(probe(&ok), ProbeScore::MAGIC);
        let bad = ProbeData::new(b"not qoa at all");
        assert_eq!(probe(&bad), ProbeScore::NONE);
    }

    #[test]
    fn a_file_with_no_frames_is_rejected() {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&0u32.to_be_bytes());
        let src = Box::new(MemorySource::new(v));
        assert!(QoaDemuxer::open(src).is_err());
    }
}
