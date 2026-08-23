//! Berkeley/IRCAM/CARL Sound Format (BICSF), a.k.a. `.sf`.
//!
//! De facto format from the 1980s CARL software distribution (UC San Diego
//! Computer Audio Research Lab) and IRCAM, documented in that distribution's
//! `sfheader` utility and widely implemented since. A small typed header
//! padded to a **fixed 1024-byte block**, then raw samples — the padding is
//! this format's one genuinely unusual property; every other format in this
//! crate sizes its header from what it actually declares.
//!
//! # Layout
//!
//! ```text
//! magic:le32     one of four byte-order/host variants (this module only
//!                writes and fully trusts 0x0001_A364, the "VAX/little-
//!                endian" magic, which is what every current writer uses)
//! sample_rate:f32le
//! channels:le32
//! encoding:le32  see below
//! [ ... reserved / optional info blocks ... ]
//! data: starts at byte 1024, to EOF
//! ```
//!
//! # Measured: the `encoding` field is two packed 16-bit halves
//!
//! `ffmpeg -c:a <codec> -f ircam`, then reading `encoding` back as `le32`,
//! for every codec `ircam` accepted (`pcm_u8` was refused outright — this
//! format has no unsigned-8-bit encoding):
//!
//! | codec | `encoding` | decoded as `(mode << 16) \| width` |
//! |---|---|---|
//! | `pcm_s8` | 1 | mode 0 (linear), width 1 (8-bit) |
//! | `pcm_s16le` | 2 | mode 0, width 2 (16-bit) |
//! | `pcm_s24le` | 3 | mode 0, width 3 (24-bit) |
//! | `pcm_f32le` | 4 | mode 0, width 4 (32-bit) — **float**, not int |
//! | `pcm_s32le` | 262148 (`0x00040004`) | mode 4 (forces integer), width 4 |
//! | `pcm_f64le` | 8 | mode 0, width 8 |
//! | `pcm_alaw` | 65537 (`0x00010001`) | mode 1 (A-law), width 1 |
//! | `pcm_mulaw` | 131073 (`0x00020001`) | mode 2 (µ-law), width 1 |
//!
//! so width 4 is ambiguous between float and 32-bit int and is
//! disambiguated by mode — a plain `4` is `pcm_f32le`; `0x00040004` is
//! `pcm_s32le`. This module recognises exactly these eight measured values
//! (module docs; [`decode_encoding`]) and reports `None` for anything else
//! rather than guess at the rest of the scheme's plausible-looking encoding
//! space, per plan 13 §1b.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, FormatOptions, Muxer, MuxerDesc, ParserProvider, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

use crate::pcm::{self, PcmLayout, RawPcmDemuxer};

const MAGIC: u32 = 0x0001_A364;
const HEADER_LEN: u64 = 1024;

const ENC_S8: u32 = 1;
const ENC_S16: u32 = 2;
const ENC_S24: u32 = 3;
const ENC_F32: u32 = 4;
const ENC_F64: u32 = 8;
const ENC_S32: u32 = 262_148;
const ENC_ALAW: u32 = 65_537;
const ENC_MULAW: u32 = 131_073;

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// ircam` file with no extension is `75` — `ProbeScore::CONTENT`, not the
/// maximum, which this crate's other eight formats all score. `docs/format/
/// vaco-format-audio-simple.md` records the exact command; no explanation
/// for the lower confidence was found beyond the reference's own scoring
/// table, so it is reproduced rather than rationalised.
pub const IRCAM_SCORE: ProbeScore = ProbeScore::CONTENT;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match data.rl32(0) {
        Some(m) if m == MAGIC => IRCAM_SCORE,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ircam",
    long_name: "Berkeley/IRCAM/CARL Sound Format",
    extensions: &["sf"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "ircam",
    long_name: "Berkeley/IRCAM/CARL Sound Format",
    extensions: &["sf"],
    default_video: None,
    // `ffmpeg -h muxer=ircam` says "Default audio codec: pcm_s16le." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(CodecId::PcmS16le),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(IrcamDemuxer::open(
        src,
        &FormatOptions::default(),
    )?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(IrcamMuxer::new(sink)?))
}

fn decode_encoding(encoding: u32) -> Option<(SampleFmt, u8)> {
    match encoding {
        ENC_S8 => Some((SampleFmt::U8, 8)), // container is signed; see crate::pcm::sample_fmt_for's u8 precedent
        ENC_S16 => Some((SampleFmt::S16, 16)),
        ENC_S24 => Some((SampleFmt::S32, 24)),
        ENC_F32 => Some((SampleFmt::F32, 32)),
        ENC_S32 => Some((SampleFmt::S32, 32)),
        ENC_F64 => Some((SampleFmt::F64, 64)),
        ENC_ALAW | ENC_MULAW => Some((SampleFmt::S16, 8)),
        _ => None,
    }
}

/// Anything not explicitly one of the wider widths — including the three
/// genuinely 8-bit-wide encodings (`ENC_S8`, A-law, µ-law) — is one byte per
/// sample.
fn bytes_per_sample(encoding: u32) -> u32 {
    match encoding {
        ENC_S16 => 2,
        ENC_S24 => 3,
        ENC_F32 | ENC_S32 => 4,
        ENC_F64 => 8,
        _ => 1,
    }
}

#[derive(Debug)]
pub struct IrcamDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl IrcamDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic does not match.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.rl32()? != MAGIC {
            return Err(Error::InvalidData("ircam: missing BICSF signature"));
        }
        let sample_rate = io.rl32()?; // read as bits, reinterpreted below
        let sample_rate = f32::from_bits(sample_rate).max(1.0).round() as u32;
        let channels = io.rl32()?;
        let encoding = io.rl32()?;
        io.seek(HEADER_LEN)?;
        let data_start = io.pos();

        let (format, bits_coded) =
            decode_encoding(encoding).map_or((None, Some(0)), |(f, b)| (Some(f), Some(b)));
        let codec_id = if format.is_some() {
            Some(CodecId::Pcm)
        } else {
            None
        };
        let bits_raw = if encoding == ENC_S24 { Some(24) } else { None };
        let bytes_per_frame = channels.max(1).saturating_mul(bytes_per_sample(encoding));

        let mut stream = pcm::new_stream(Rational::new(1, i32::try_from(sample_rate).unwrap_or(1)));
        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(
                sample_rate,
                u16::try_from(channels.min(65535)).unwrap_or(1),
                bytes_per_frame,
            ),
            codec_id,
            format,
            bits_coded,
            bits_raw,
        );
        params.codec_tag = Some(encoding.to_le_bytes());
        stream.params = params;

        let inner = RawPcmDemuxer::new(io, stream, data_start, None, bytes_per_frame.max(1));
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for IrcamDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }
    fn read_packet(&mut self) -> Result<Packet> {
        self.inner.read_packet(&mut self.budget)
    }
    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        self.inner.seek(target, flags)
    }
    fn duration(&self) -> Option<vaco_core::Duration> {
        self.inner.duration()
    }
}

/// Writes 16-bit signed PCM (`encoding = 2`).
#[derive(Debug)]
pub struct IrcamMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u32,
}

impl IrcamMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream: None,
            header_written: false,
        })
    }
}

impl Muxer for IrcamMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("ircam: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("ircam: not an audio stream"))?;
        if audio.format != Some(SampleFmt::S16) {
            return Err(Error::Unsupported(
                "ircam: only 16-bit signed PCM is supported for writing",
            ));
        }
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1);
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("ircam: no stream added"))?;
        self.out.wl32(MAGIC)?;
        self.out.write(&(s.sample_rate as f32).to_le_bytes())?;
        self.out.wl32(s.channels)?;
        self.out.wl32(ENC_S16)?;
        let written = 4 + 4 + 4 + 4u64;
        self.out
            .write(&vec![0u8; (HEADER_LEN - written) as usize])?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "ircam: packet written before the header",
            ));
        }
        self.out.write(packet.payload())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index != 0 {
            return None;
        }
        self.stream
            .map(|s| Rational::new(1, s.sample_rate.cast_signed()))
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData(
                "ircam: trailer written before the header",
            ));
        }
        // No length field anywhere in this format's header to patch (module
        // docs); the whole file's own length is the answer.
        self.out.flush()
    }
}
