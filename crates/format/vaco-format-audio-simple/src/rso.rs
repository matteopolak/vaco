//! Lego Mindstorms RSO.
//!
//! A tiny, de facto format with no independent public specification —
//! probed directly by black-box observation of `ffmpeg`/`ffprobe` 8.1
//! (D6/D7: recording a shipped binary's observed behaviour is not copying
//! its expression), since no other public source describes it. Eight-byte
//! big-endian header, then raw 8-bit unsigned mono PCM to EOF.
//!
//! # Layout, as measured
//!
//! ```text
//! offset 0  u16be   0x0100 in every file produced; meaning not established
//!                    (see below), written verbatim and not validated on read
//! offset 2  u16be   sample count — wraps/truncates past 65535 and is not
//!                    trusted for framing (see below)
//! offset 4  u16be   sample rate
//! offset 6  u16be   0x0000 in every file produced; meaning not established
//! offset 8  ...     raw 8-bit unsigned mono PCM to EOF
//! ```
//!
//! **Measured**: `ffmpeg -f rso` refuses every codec but `pcm_u8`, and
//! refuses stereo — `ffmpeg -h muxer=rso` states the default (and, in
//! practice, only working) audio codec is `pcm_u8`. The sample-count field
//! was disambiguated from the sample-rate field by encoding a duration and
//! rate that give the two fields different values (`8000 Hz` for `0.01 s` of
//! audio: field at offset 2 reads `80`, the field at offset 4 reads `8000`)
//! — see `docs/format/vaco-format-audio-simple.md` for the exact
//! transcript. The two constant fields' purpose could not be determined
//! this way (they never vary across any input tried) and are recorded as
//! unknown rather than guessed at.
//!
//! Because the sample-count field is only 16 bits, it cannot state the true
//! length of anything longer than 65 535 samples (about 8 seconds at 8 kHz);
//! this module does not use it for framing at all, reading to EOF instead,
//! which is correct for every length and not just the ones the field can
//! represent.
//!
//! No probe signature exists — every field on offer is a plausible-looking
//! number, not a magic string, so [`probe`] never claims more than
//! [`ProbeScore::NONE`] from content alone; this format is reached by
//! extension or explicit `-f rso`, exactly as the reference reaches it.

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

const HEADER_LEN: u64 = 8;

/// Never claims content-based confidence (module docs); present only so
/// `vaco-registry`'s probe table has a uniform entry for every demuxer.
#[must_use]
pub const fn probe(_data: &ProbeData<'_>) -> ProbeScore {
    ProbeScore::NONE
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "rso",
    long_name: "Lego Mindstorms RSO",
    extensions: &["rso"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "rso",
    long_name: "Lego Mindstorms RSO",
    extensions: &["rso"],
    default_video: None,
    default_audio: Some(CodecId::Pcm),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(RsoDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(RsoMuxer::new(sink)?))
}

#[derive(Debug)]
pub struct RsoDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl RsoDemuxer {
    /// # Errors
    /// [`Error::UnexpectedEof`] if the source is shorter than the 8-byte
    /// header.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let _unknown0 = io.rb16()?;
        let _sample_count = io.rb16()?;
        let sample_rate = u32::from(io.rb16()?.max(1));
        let _unknown1 = io.rb16()?;
        let data_start = HEADER_LEN;

        let mut stream = pcm::new_stream(Rational::new(1, i32::try_from(sample_rate).unwrap_or(1)));
        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(sample_rate, 1, 1),
            Some(CodecId::Pcm),
            Some(SampleFmt::U8),
            Some(8),
            None,
        );
        params.codec_tag = Some(*b"RSO ");
        stream.params = params;

        let inner = RawPcmDemuxer::new(io, stream, data_start, None, 1);
        Ok(Self {
            inner,
            budget: Budget::new(vaco_limits::Limits::permissive()),
        })
    }
}

impl Demuxer for RsoDemuxer {
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

/// Writes 8-bit unsigned mono PCM, matching the only combination the
/// reference muxer accepts.
#[derive(Debug)]
pub struct RsoMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    samples_written: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
}

impl RsoMuxer {
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

impl Muxer for RsoMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("rso: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("rso: not an audio stream"))?;
        if audio.format != Some(SampleFmt::U8) {
            return Err(Error::Unsupported(
                "rso: only 8-bit unsigned mono PCM is supported for writing",
            ));
        }
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels);
        if channels != 1 {
            return Err(Error::Unsupported(
                "rso: only mono is supported for writing",
            ));
        }
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("rso: no stream added"))?;
        self.out.wb16(0x0100)?;
        self.out.wb16(0)?; // sample count, patched in write_trailer where possible
        self.out
            .wb16(u16::try_from(s.sample_rate.min(u32::from(u16::MAX))).unwrap_or(u16::MAX))?;
        self.out.wb16(0)?;
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("rso: packet written before the header"));
        }
        self.out.write(packet.payload())?;
        self.samples_written = self
            .samples_written
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
            return Err(Error::InvalidData("rso: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            return self.out.flush();
        }
        let end = self.out.pos();
        self.out.seek(2)?;
        // The field is only 16 bits and this format has no larger one to
        // fall back to (module docs); clamp rather than wrap, so a long
        // file's header states its cap instead of a wrapped-around lie.
        self.out.wb16(
            u16::try_from(self.samples_written.min(u64::from(u16::MAX))).unwrap_or(u16::MAX),
        )?;
        self.out.seek(end)?;
        self.out.flush()
    }
}
