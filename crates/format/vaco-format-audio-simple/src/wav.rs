//! WAV / WAVE, and its `RF64` 64-bit-size extension.
//!
//! Thin over `vaco-format-riff`: the RIFF chunk grammar
//! ([`vaco_format_riff::chunk`]), `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE`
//! ([`vaco_format_riff::wave`]), the `wFormatTag` → codec table
//! ([`vaco_format_riff::wave_tags`]) and the `ds64` 64-bit-size extension
//! ([`vaco_format_riff::rf64`]) are all reused verbatim; this module is the
//! chunk walk that finds `fmt `/`ds64`/`data` and the [`Demuxer`]/[`Muxer`]
//! glue around them.
//!
//! Specification: Microsoft/IBM RIFF/WAVE, plus EBU Tech 3306 (`RF64`) for
//! files over 4 GiB.
//!
//! # What is read
//!
//! `fmt ` (via [`vaco_format_riff::wave::WaveFormatEx`]) and `data`. A
//! `RIFF` container walks chunks with 32-bit sizes; an `RF64` container's
//! outer size and `data` size are `0xFFFFFFFF` placeholders overridden by a
//! mandatory leading `ds64` chunk. Any other chunk (`LIST/INFO`, `fact`,
//! `bext`, `cue `, `JUNK`, an ID3 tag) is skipped, not decoded — the metadata
//! and marker surfaces plan `18-formats.md` §3.4.6 lists for this format are
//! **deferred**, consistent with the brief's "structurally present but
//! untested" allowance; only the audio itself is exposed today.
//!
//! # Deviation: the "unknown length" convention
//!
//! A streaming writer that does not know the final size up front commonly
//! writes `0xFFFFFFFF` for `data`'s declared size, meaning "everything that
//! follows". [`vaco_format_riff::chunk::ChunkIter`] already treats an
//! oversized declared length this way for chunks it walks in memory; this
//! module's own sequential walk applies the same rule directly to `data`,
//! by handing [`crate::pcm::RawPcmDemuxer`] `None` for the declared length
//! whenever the field reads `0` or `0xFFFF_FFFF` so it reads to true EOF
//! instead.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, FormatFlags, FormatOptions, ParserProvider, SeekFlags, SeekTarget, Stream,
};
use vaco_format_core::{DemuxerDesc, Muxer, MuxerDesc};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use vaco_format_riff::chunk::ids;
use vaco_format_riff::rf64::Ds64;
use vaco_format_riff::wave::WaveFormatEx;
use vaco_format_riff::wave_tags;

use crate::pcm::{self, PcmLayout, RawPcmDemuxer};

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// wav` file with no extension is `99`, not `100` — the one format in this
/// crate whose reference score is not the maximum. `docs/format/
/// vaco-format-audio-simple.md` records the exact command.
pub const WAV_SCORE_FULL: ProbeScore = ProbeScore(99);

/// Signature present but not further validated (no confirmed `fmt `/`data`
/// chunk within the scanned prefix) — `vaco-format-core`'s own convention for
/// "unambiguous magic, nothing further checked".
pub const WAV_SCORE_MAGIC: ProbeScore = ProbeScore::MAGIC;

/// Bytes of the probe buffer this module is willing to scan looking for
/// `fmt `, to keep probing O(1) in file size.
const PROBE_SCAN: usize = 4096;

/// Longest `fmt ` chunk payload this module will read, generously above any
/// real `WAVEFORMATEXTENSIBLE` (44 bytes) so a hostile declared size cannot
/// force a large allocation before the real cap in [`open`] applies.
const MAX_FMT_LEN: usize = 4096;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let Some(container) = data.tag(0) else {
        return ProbeScore::NONE;
    };
    if container != ids::RIFF.as_bytes() && container != ids::RF64.as_bytes() {
        return ProbeScore::NONE;
    }
    if data.tag(8) != Some(ids::WAVE.as_bytes()) {
        return ProbeScore::NONE;
    }
    // Confirm a `fmt ` chunk id appears somewhere in the scanned prefix
    // before declaring full confidence; this is a cheap sanity check, not a
    // full parse.
    let scan = data.len().min(PROBE_SCAN);
    let mut at: usize = 12;
    while at.saturating_add(8) <= scan {
        let Some(id) = data.tag(at) else { break };
        if id == ids::FMT.as_bytes() {
            return WAV_SCORE_FULL;
        }
        let Some(size) = data.rl32(at + 4) else { break };
        let step = 8usize
            .saturating_add(usize::try_from(size).unwrap_or(usize::MAX))
            .saturating_add(usize::from(size % 2 == 1));
        if step == 0 {
            break;
        }
        at = at.saturating_add(step);
    }
    WAV_SCORE_MAGIC
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "wav",
    long_name: "WAV / WAVE (Waveform Audio)",
    extensions: &["wav"],
    mime_types: &["audio/x-wav", "audio/wav"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "wav",
    long_name: "WAV / WAVE (Waveform Audio)",
    extensions: &["wav"],
    default_video: None,
    // `ffmpeg -h muxer=wav` says "Default audio codec: pcm_s16le." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(vaco_codec_core::CodecId::PcmS16le),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(WavDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(WavMuxer::new(sink)?))
}

#[derive(Debug)]
pub struct WavDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl WavDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the RIFF/WAVE signature or `fmt `/`data`
    /// chunks do not parse.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let container = io.tag()?;
        let is_rf64 = container == ids::RF64.as_bytes();
        if !is_rf64 && container != ids::RIFF.as_bytes() {
            return Err(Error::InvalidData("wav: missing RIFF/RF64 signature"));
        }
        let _declared_size = io.rl32()?;
        let form = io.tag()?;
        if form != ids::WAVE.as_bytes() {
            return Err(Error::InvalidData("wav: form type is not WAVE"));
        }

        let mut budget = Budget::new(Limits::permissive());
        let mut fmt: Option<WaveFormatEx> = None;
        let mut ds64: Option<Ds64> = None;
        let mut data_declared: Option<u64> = None;
        let mut data_start = 0u64;

        while let Ok(id) = io.tag() {
            let size = io.rl32()?;
            if id == ids::DS64.as_bytes() {
                let take = usize::try_from(size).unwrap_or(0).min(1 << 20);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                ds64 = Ds64::parse(&buf, &mut budget).ok();
                if size % 2 == 1 {
                    io.skip(1)?;
                }
            } else if id == ids::FMT.as_bytes() {
                let take = usize::try_from(size).unwrap_or(0).min(MAX_FMT_LEN);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                fmt = Some(WaveFormatEx::parse(&buf, &mut budget)?);
                if size % 2 == 1 {
                    io.skip(1)?;
                }
            } else if id == ids::DATA.as_bytes() {
                data_start = io.pos();
                data_declared = Some(if size == 0 || size == u32::MAX {
                    ds64.as_ref().map_or(u64::MAX, |d| d.data_size)
                } else {
                    u64::from(size)
                });
                break;
            } else {
                io.skip(u64::from(size).saturating_add(u64::from(size % 2)))?;
            }
        }

        let fmt = fmt.ok_or(Error::InvalidData("wav: no fmt chunk"))?;
        if data_declared.is_none() {
            return Err(Error::InvalidData("wav: no data chunk"));
        }
        let declared_len = data_declared.filter(|&n| n != u64::MAX);

        let channels = fmt.channels;
        let codec_id = wave_tags::codec_id(&fmt);
        // `wave_tags::codec_id` returns the specific variant, never the
        // generic `CodecId::Pcm`.
        let pcm_fmt = codec_id.and_then(pcm::sample_fmt_of);
        let (format, bits_per_raw) = match pcm_fmt {
            Some((sf, raw)) => (Some(sf), raw),
            None => (None, None),
        };
        let bits_per_coded = if pcm_fmt.is_some() {
            Some(fmt.bits_per_sample.min(255) as u8)
        } else {
            Some(0)
        };
        let bytes_per_frame = if fmt.block_align > 0 {
            u32::from(fmt.block_align)
        } else {
            u32::from(channels).saturating_mul(u32::from(fmt.bits_per_sample.div_ceil(8)))
        };

        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(fmt.samples_per_sec, channels, bytes_per_frame),
            codec_id,
            format,
            bits_per_coded,
            bits_per_raw,
        );
        params.codec_tag = Some([
            (fmt.format_tag & 0xff) as u8,
            (fmt.format_tag >> 8) as u8,
            0,
            0,
        ]);

        let mut stream =
            pcm::new_stream(Rational::new(1, fmt.samples_per_sec.max(1).cast_signed()));
        stream.params = params;

        let mut inner =
            RawPcmDemuxer::new(io, stream, data_start, declared_len, bytes_per_frame.max(1));
        inner.forget_frame_count();
        inner.size_packets_in_frames();
        Ok(Self {
            inner,
            budget: Budget::new(Limits::permissive()),
        })
    }
}

impl Demuxer for WavDemuxer {
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

/// Writes plain `WAVEFORMATEX` PCM (16-bit-and-under integer, or the
/// `WAVE_FORMAT_IEEE_FLOAT` tag for float): the common case every reader
/// handles. Wider integer PCM would need `WAVEFORMATEXTENSIBLE` to name its
/// sub-format unambiguously, which this muxer does not yet emit — see
/// `docs/format/vaco-format-audio-simple.md`.
#[derive(Debug)]
pub struct WavMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    data_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    /// `wFormatTag`: 1 `WAVE_FORMAT_PCM`, 3 `WAVE_FORMAT_IEEE_FLOAT`,
    /// 6 `WAVE_FORMAT_ALAW`, 7 `WAVE_FORMAT_MULAW`.
    ///
    /// Derived from the codec. It used to be `if is_float { 3 } else { 1 }`,
    /// and A-law decodes to `s16` — so A-law data went out tagged as 16-bit
    /// linear PCM, two bytes per sample over one-byte samples, and the
    /// reference could not read back what we wrote.
    format_tag: u16,
    bytes_per_frame: u32,
}

impl WavMuxer {
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

impl Muxer for WavMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("wav: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("wav: not an audio stream"))?;
        let format = audio
            .format
            .ok_or(Error::Unsupported("wav: sample format must be known"))?;
        if format.is_planar() {
            return Err(Error::Unsupported(
                "wav: planar sample formats are not supported",
            ));
        }
        // The coded width, not the decoded format's: `pcm_s24le` decodes to
        // `s32` and would be written as 32-bit.
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("wav: the codec must be known"))?;
        let coded_bits = pcm::coded_bits(codec)
            .ok_or(Error::Unsupported("wav: only PCM-shaped codecs are supported"))?;
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1) as u16;
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
            bits_per_sample: u16::from(coded_bits),
            format_tag: match codec {
                vaco_codec_core::CodecId::PcmAlaw => 6,
                vaco_codec_core::CodecId::PcmMulaw => 7,
                _ if format.is_float() => 3,
                _ => 1,
            },
            bytes_per_frame: u32::from(channels)
                .saturating_mul(u32::from(coded_bits.div_ceil(8))),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("wav: no stream added"))?;
        self.out.write(&ids::RIFF.as_bytes())?;
        self.out.wl32(0)?; // patched in write_trailer
        self.out.write(&ids::WAVE.as_bytes())?;

        self.out.write(&ids::FMT.as_bytes())?;
        self.out.wl32(16)?;
        let tag: u16 = s.format_tag;
        self.out.wl16(tag)?;
        self.out.wl16(s.channels)?;
        self.out.wl32(s.sample_rate)?;
        let byte_rate = s.sample_rate.saturating_mul(s.bytes_per_frame);
        self.out.wl32(byte_rate)?;
        self.out.wl16(s.bytes_per_frame as u16)?;
        self.out.wl16(s.bits_per_sample)?;

        self.out.write(&ids::DATA.as_bytes())?;
        self.out.wl32(0)?; // patched in write_trailer
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("wav: packet written before the header"));
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
            return Err(Error::InvalidData("wav: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            // No convention this muxer emits supports an unknown-length
            // RIFF/data size on write, so a non-seekable sink gets a WAV
            // whose size fields under-report — a known, documented
            // divergence rather than a silent one.
            return self.out.flush();
        }
        let riff_size = 4 + (8 + 16) + (8 + self.data_bytes);
        let end = self.out.pos();
        self.out.seek(4)?;
        self.out
            .wl32(u32::try_from(riff_size).unwrap_or(u32::MAX))?;
        self.out.seek(4 + 4 + 4 + 8 + 16 + 4)?;
        self.out
            .wl32(u32::try_from(self.data_bytes).unwrap_or(u32::MAX))?;
        self.out.seek(end)?;
        self.out.flush()
    }
}
