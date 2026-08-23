//! `SoX` (Sound eXchange) native format.
//!
//! Published by the `SoX` project itself (`.sox`/`.raw` native-format
//! documentation at sox.sourceforge.net). Fixed-endianness magic, then a
//! header carrying the sample count, rate and channel count, then raw
//! 32-bit signed integer samples — `SoX`'s own internal working format is
//! always 32-bit, so the file format never states a bit depth.
//!
//! # Layout
//!
//! ```text
//! magic:FourCC (".SoX" little-endian host, "XoS." big-endian — the
//!               remaining fields' endianness follows whichever was written)
//! header_size:u32     (bytes from the start of the file to the comment_size
//!                       field below — always 28 when there is no comment)
//! num_samples:u64     (total samples: frames × channels, NOT frames alone)
//! rate:f64
//! channels:u32
//! comment_size:u32
//! comment[comment_size]
//! data: num_samples × 4 bytes, signed 32-bit, same endianness as the header
//! ```
//!
//! **Measured** against `ffmpeg`/`ffprobe` 8.1: `ffmpeg -f sox` always
//! writes `codec_name=pcm_s32le`/`sample_fmt=s32` and a `.SoX` (little-
//! endian) magic; `header_size` is `28` with an empty comment (`comment_size
//! = 0` immediately follows, at offset 28, making the true data offset
//! `32`) — the field's own name is one byte short of describing what it
//! measures, which is why this module computes the data offset from
//! `header_size + 4 + comment_size` rather than from `header_size` alone.
//!
//! Only the little-endian (`".SoX"`) form is exercised here — `ffmpeg`'s
//! `sox` muxer never emits the big-endian one in the build used to write
//! this crate — but both share one code path parameterised on endianness,
//! so the big-endian form is structurally supported, not untested from
//! neglect.

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

const MAGIC_LE: [u8; 4] = *b".SoX";
const MAGIC_BE: [u8; 4] = *b"XoS.";

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// sox` file with no extension is `100`.
pub const SOX_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match data.tag(0) {
        Some(m) if m == MAGIC_LE || m == MAGIC_BE => SOX_SCORE,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "sox",
    long_name: "SoX (Sound eXchange) native",
    extensions: &["sox"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "sox",
    long_name: "SoX (Sound eXchange) native",
    extensions: &["sox"],
    default_video: None,
    // `ffmpeg -h muxer=sox` says "Default audio codec: pcm_s32le." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(CodecId::PcmS32le),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(SoxDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(SoxMuxer::new(sink)?))
}

#[derive(Debug)]
pub struct SoxDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl SoxDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the magic does not match.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let magic = io.tag()?;
        let big_endian = if magic == MAGIC_BE {
            true
        } else if magic == MAGIC_LE {
            false
        } else {
            return Err(Error::InvalidData("sox: missing .SoX signature"));
        };

        let (header_size, num_samples, rate, channels) = if big_endian {
            (
                io.rb32()?,
                io.rb64()?,
                f64::from_bits(io.rb64()?),
                io.rb32()?,
            )
        } else {
            (
                io.rl32()?,
                io.rl64()?,
                f64::from_bits(io.rl64()?),
                io.rl32()?,
            )
        };
        let comment_size = if big_endian { io.rb32()? } else { io.rl32()? };
        let _ = header_size;
        io.skip(u64::from(comment_size))?;
        let data_start = io.pos();

        let channels16 = u16::try_from(channels.min(65535)).unwrap_or(1).max(1);
        let bytes_per_frame = u32::from(channels16) * 4;
        // `num_samples` counts individual samples across all channels, not
        // frames, so the byte length is `num_samples * 4` directly.
        let declared_len = num_samples.checked_mul(4);

        let sample_rate = rate.round().max(1.0) as u32;
        let mut stream = pcm::new_stream(Rational::new(1, i32::try_from(sample_rate).unwrap_or(1)));
        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(sample_rate, channels16, bytes_per_frame),
            Some(CodecId::Pcm),
            Some(SampleFmt::S32),
            Some(32),
            None,
        );
        params.codec_tag = Some(*b"SOX ");
        stream.params = params;

        let inner =
            RawPcmDemuxer::new(io, stream, data_start, declared_len, bytes_per_frame.max(1));
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for SoxDemuxer {
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

/// Writes little-endian (`.SoX`) headers only.
#[derive(Debug)]
pub struct SoxMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    samples_written: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u32,
}

impl SoxMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream: None,
            header_written: false,
            samples_written: 0,
        })
    }
}

impl Muxer for SoxMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("sox: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("sox: not an audio stream"))?;
        if audio.format != Some(SampleFmt::S32) {
            return Err(Error::Unsupported(
                "sox: only 32-bit signed PCM is supported for writing",
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
            .ok_or(Error::InvalidData("sox: no stream added"))?;
        self.out.write(&MAGIC_LE)?;
        self.out.wl32(28)?; // header_size
        self.out.wl64(0)?; // num_samples, patched in write_trailer
        self.out.write(&0f64.to_le_bytes())?; // rate, patched in write_trailer
        self.out.wl32(s.channels)?;
        self.out.wl32(0)?; // comment_size
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("sox: packet written before the header"));
        }
        self.out.write(packet.payload())?;
        self.samples_written = self
            .samples_written
            .saturating_add(pcm::frames_in(packet.payload().len() as u64, 4));
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
        let s = self
            .stream
            .ok_or(Error::InvalidData("sox: no stream added"))?;
        if !self.header_written {
            return Err(Error::InvalidData("sox: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            return self.out.flush();
        }
        let end = self.out.pos();
        self.out.seek(8)?; // num_samples field
        self.out.wl64(self.samples_written)?;
        self.out.write(&f64::from(s.sample_rate).to_le_bytes())?;
        self.out.seek(end)?;
        self.out.flush()
    }
}
