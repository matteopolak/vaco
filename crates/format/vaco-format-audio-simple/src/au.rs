//! Sun/NeXT `.au` ("SND").
//!
//! De facto format, no formal publisher, but stable and documented for
//! decades in the Sun `audio(4)`/multimedia manual pages and every audio
//! library that reads it — a fixed 24-byte header plus raw big-endian audio
//! data.
//!
//! # Layout
//!
//! ```text
//! magic:FourCC(".snd")   data_offset:be32   data_size:be32 (0xFFFFFFFF = unknown)
//! encoding:be32          sample_rate:be32   channels:be32
//! [ annotation text, padded to fill up to data_offset ]
//! ```
//!
//! # Measured: the encoding table, by round-tripping through `ffmpeg`/`ffprobe` 8.1
//!
//! `ffmpeg -c:a <codec> -f au`, then reading the `encoding` field back with
//! `xxd`/`python struct`:
//!
//! | `encoding` | codec | `sample_fmt` | notes |
//! |---|---|---|---|
//! | 1 | `pcm_mulaw` | `s16` | 8-bit container |
//! | 2 | `pcm_s8` | `u8` | the one genuine surprise: signed-8-bit *codec*, unsigned-8-bit *working format* — reproduced exactly, see [`crate::pcm::sample_fmt_for`] |
//! | 3 | `pcm_s16be` | `s16` | |
//! | 4 | `pcm_s24be` | `s32` | `bits_per_raw_sample=24` |
//! | 5 | `pcm_s32be` | `s32` | |
//! | 6 | `pcm_f32be` | `flt` | |
//! | 7 | `pcm_f64be` | `dbl` | |
//! | 27 | `pcm_alaw` | `s16` | 8-bit container — **not 28**, which a
//! plausible-looking guess (mirroring `WAVE_FORMAT_ALAW`'s registered value
//! landing one below µ-law's) would have given; this is exactly the kind of
//! value plan 13 §1b warns against recalling rather than measuring |
//!
//! `pcm_u8` (unsigned 8-bit) refused to mux to `au` at all in the `ffmpeg`
//! 8.1 build used here — `.au`'s "8-bit linear" is signed, and there is no
//! encoding value for unsigned.
//!
//! # What is not read
//!
//! The annotation text between the fixed header and `data_offset` is
//! skipped, not decoded into metadata — deferred.

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

const MAGIC: [u8; 4] = *b".snd";
const HEADER_LEN: u32 = 24;

const ENC_MULAW8: u32 = 1;
const ENC_PCM8: u32 = 2;
const ENC_PCM16: u32 = 3;
const ENC_PCM24: u32 = 4;
const ENC_PCM32: u32 = 5;
const ENC_FLOAT32: u32 = 6;
const ENC_FLOAT64: u32 = 7;
const ENC_ALAW8: u32 = 27;

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// au` file with no extension is `100`.
pub const AU_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) == Some(MAGIC) {
        AU_SCORE
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "au",
    long_name: "Sun AU",
    extensions: &["au", "snd"],
    mime_types: &["audio/basic"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "au",
    long_name: "Sun AU",
    extensions: &["au"],
    default_video: None,
    // `ffmpeg -h muxer=au` says "Default audio codec: pcm_s16be." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(CodecId::PcmS16be),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(AuDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(AuMuxer::new(sink)?))
}

/// `(sample_fmt, bits_per_coded_sample)` for one of the encoding values this
/// module recognises, or `None` for anything else (ADPCM/G.72x variants,
/// fixed-point DSP formats — present in the `.au` registry but not mapped).
fn encoding_to_format(encoding: u32) -> Option<(SampleFmt, u8)> {
    match encoding {
        ENC_MULAW8 | ENC_ALAW8 => Some((SampleFmt::S16, 8)),
        ENC_PCM8 => Some((SampleFmt::U8, 8)),
        ENC_PCM16 => Some((SampleFmt::S16, 16)),
        ENC_PCM24 => Some((SampleFmt::S32, 24)),
        ENC_PCM32 => Some((SampleFmt::S32, 32)),
        ENC_FLOAT32 => Some((SampleFmt::F32, 32)),
        ENC_FLOAT64 => Some((SampleFmt::F64, 64)),
        _ => None,
    }
}

/// The codec the reference names for one `.au` encoding value.
///
/// `.au` is big-endian throughout, and its "8-bit linear" encoding is
/// *signed*: the reference's `au` muxer accepts `-c:a pcm_s8` and rejects
/// `-c:a pcm_u8`, and a written file probes back as `pcm_s8` with
/// `sample_fmt=u8` — the decoded format, which has no signed 8-bit spelling.
fn encoding_to_codec(encoding: u32) -> Option<CodecId> {
    match encoding {
        ENC_MULAW8 => Some(CodecId::PcmMulaw),
        ENC_ALAW8 => Some(CodecId::PcmAlaw),
        ENC_PCM8 => Some(CodecId::PcmS8),
        ENC_PCM16 => Some(CodecId::PcmS16be),
        ENC_PCM24 => Some(CodecId::PcmS24be),
        ENC_PCM32 => Some(CodecId::PcmS32be),
        ENC_FLOAT32 => Some(CodecId::PcmF32be),
        ENC_FLOAT64 => Some(CodecId::PcmF64be),
        _ => None,
    }
}

#[derive(Debug)]
pub struct AuDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl AuDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the `.snd` signature does not parse.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != MAGIC {
            return Err(Error::InvalidData("au: missing .snd signature"));
        }
        let data_offset = io.rb32()?;
        let data_size = io.rb32()?;
        let encoding = io.rb32()?;
        let sample_rate = io.rb32()?;
        let channels = io.rb32()?;

        let header_end = data_offset.max(HEADER_LEN);
        io.seek(u64::from(header_end))?;
        let data_start = io.pos();
        let declared_len = if data_size == u32::MAX {
            None
        } else {
            Some(u64::from(data_size))
        };

        let (format, bits_coded) =
            encoding_to_format(encoding).map_or((None, Some(0)), |(f, b)| (Some(f), Some(b)));
        let codec_id = encoding_to_codec(encoding);
        let bits_raw = if encoding == ENC_PCM24 {
            Some(24)
        } else {
            None
        };

        // Anything not explicitly one of the wider widths — including the
        // three genuinely 8-bit-wide encodings (mu-law, A-law, signed 8-bit
        // linear) — is one byte per sample.
        let bytes_per_sample = match encoding {
            ENC_PCM16 => 2,
            ENC_PCM24 => 3,
            ENC_PCM32 | ENC_FLOAT32 => 4,
            ENC_FLOAT64 => 8,
            _ => 1,
        };
        let bytes_per_frame = channels.max(1).saturating_mul(bytes_per_sample);

        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(
                sample_rate.max(1),
                u16::try_from(channels.min(65535)).unwrap_or(1),
                bytes_per_frame,
            ),
            codec_id,
            format,
            bits_coded,
            bits_raw,
        );
        params.codec_tag = Some(encoding.to_be_bytes());

        let mut stream = pcm::new_stream(Rational::new(1, sample_rate.max(1).cast_signed()));
        stream.params = params;

        let inner =
            RawPcmDemuxer::new(io, stream, data_start, declared_len, bytes_per_frame.max(1));
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for AuDemuxer {
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

/// Writes signed integer and float PCM, plus A-law/µ-law. Encoding is chosen
/// from the stream's [`SampleFmt`] and `bits_per_coded_sample`; unsigned
/// 8-bit has no encoding value (module docs) and is refused, matching the
/// reference muxer's own behaviour.
#[derive(Debug)]
pub struct AuMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    data_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u32,
    encoding: u32,
}

impl AuMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream: None,
            header_written: false,
            data_bytes: 0,
        })
    }
}

impl Muxer for AuMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("au: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("au: not an audio stream"))?;
        let format = audio
            .format
            .ok_or(Error::Unsupported("au: sample format must be known"))?;
        if format.is_planar() {
            return Err(Error::Unsupported(
                "au: planar sample formats are not supported",
            ));
        }
        let encoding = match format {
            SampleFmt::U8 => ENC_PCM8,
            SampleFmt::S16 => ENC_PCM16,
            SampleFmt::S32 if audio.bits_per_coded_sample == Some(24) => ENC_PCM24,
            SampleFmt::S32 => ENC_PCM32,
            SampleFmt::F32 => ENC_FLOAT32,
            SampleFmt::F64 => ENC_FLOAT64,
            _ => return Err(Error::Unsupported("au: sample format has no .au encoding")),
        };
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1);
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
            encoding,
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("au: no stream added"))?;
        self.out.write(&MAGIC)?;
        self.out.wb32(HEADER_LEN)?;
        self.out.wb32(u32::MAX)?; // data_size: unknown until write_trailer
        self.out.wb32(s.encoding)?;
        self.out.wb32(s.sample_rate)?;
        self.out.wb32(s.channels)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("au: packet written before the header"));
        }
        self.out.write(packet.payload())?;
        self.data_bytes = self
            .data_bytes
            .saturating_add(packet.payload().len() as u64);
        Ok(())
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
            return Err(Error::InvalidData("au: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            // `0xFFFFFFFF` (already written) is `.au`'s own "unknown size"
            // convention, so this is a fully valid file even unpatched.
            return self.out.flush();
        }
        let end = self.out.pos();
        self.out.seek(8)?; // data_size field
        self.out
            .wb32(u32::try_from(self.data_bytes).unwrap_or(u32::MAX))?;
        self.out.seek(end)?;
        self.out.flush()
    }
}
