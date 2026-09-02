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
//! mandatory leading `ds64` chunk. `LIST/INFO` is read as far as its `ISFT`
//! sub-chunk ([`vaco_format_riff::info::list_info_tags`]), exposed through
//! [`Demuxer::metadata`] as `encoder` — measured against the reference,
//! which states it and this crate silently did not. Every other chunk
//! (`fact`, `bext`, `cue `, `JUNK`, an ID3 tag, and every other `LIST/INFO`
//! sub-chunk) is still skipped, not decoded — the rest of the metadata and
//! marker surfaces plan `18-formats.md` §3.4.6 lists for this format are
//! **deferred**, consistent with the brief's "structurally present but
//! untested" allowance; only the audio itself and this one tag are exposed
//! today.
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
use vaco_format_riff::info::list_info_tags;
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
    tags: Vec<(String, String)>,
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
        let mut tags: Vec<(String, String)> = Vec::new();
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
            } else if id == ids::LIST.as_bytes() {
                // `LIST/INFO/ISFT` is the one sub-chunk measured against the
                // reference; `format.tags.encoder` was silently dropped
                // before. Bounded the same way `fmt `/`ds64` are above.
                let take = usize::try_from(size).unwrap_or(0).min(1 << 16);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                tags.extend(list_info_tags(&buf));
                if size % 2 == 1 {
                    io.skip(1)?;
                }
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
            tags,
        })
    }
}

impl Demuxer for WavDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.tags
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
/// `docs/format/vaco-format-audio-simple.md`. One compressed codec is also
/// written undecoded — see [`MuxStream::extradata`] and `add_stream`'s own
/// doc for why AAC needed its own path rather than reusing PCM's checks.
#[derive(Debug)]
pub struct WavMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    data_bytes: u64,
    /// Where `data`'s own four-byte size field landed, so `write_trailer`
    /// can patch it regardless of how long the `fmt ` chunk (and an
    /// optional `fact` chunk ahead of it) turned out to be — a fixed offset
    /// only worked while every `fmt ` chunk this muxer wrote was the same
    /// 16-byte, `fact`-free shape.
    data_size_pos: u64,
    /// `Some(position)` of a `fact` chunk's `dwSampleLength` field, for a
    /// compressed stream only — PCM's own sample count is fully implied by
    /// `data_bytes`/`block_align`, which is exactly why the reference does
    /// not write a `fact` chunk for it either (OBSERVED, `ffmpeg 9.0.1`).
    fact_size_pos: Option<u64>,
    /// The furthest `pts + duration` (in samples — this muxer's own
    /// declared `stream_time_base`) seen on the compressed stream, tracked
    /// because a compressed codec's total sample count cannot be derived
    /// from `data_bytes` the way PCM's can (a coded AAC frame's byte size
    /// is not a fixed function of its sample count). Written to the `fact`
    /// chunk `write_trailer` patches in.
    sample_extent: i64,
}

#[derive(Debug, Clone)]
struct MuxStream {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    /// `wFormatTag`: 1 `WAVE_FORMAT_PCM`, 3 `WAVE_FORMAT_IEEE_FLOAT`,
    /// 6 `WAVE_FORMAT_ALAW`, 7 `WAVE_FORMAT_MULAW`, 0x00FF `WAVE_FORMAT_AAC`.
    ///
    /// Derived from the codec. It used to be `if is_float { 3 } else { 1 }`,
    /// and A-law decodes to `s16` — so A-law data went out tagged as 16-bit
    /// linear PCM, two bytes per sample over one-byte samples, and the
    /// reference could not read back what we wrote.
    format_tag: u16,
    /// `nBlockAlign`. PCM: real bytes per interleaved frame
    /// (`channels * bytes_per_sample`), exact by construction. AAC: a
    /// nominal `768 * channels` — see `add_stream`'s doc for where that
    /// number came from, since it is not derivable from anything else a
    /// stream-copy has on hand.
    block_align: u32,
    /// `nAvgBytesPerSec`. PCM: `sample_rate * block_align`, exact. AAC: the
    /// container's own declared `bit_rate / 8` — the only real byte-rate
    /// figure a compressed, undecoded stream has, and the one field here
    /// that legitimately varies encode to encode rather than following a
    /// fixed formula.
    avg_bytes_per_sec: u32,
    /// The out-of-band configuration record's raw bytes, copied verbatim
    /// into `cbSize`/the extended `fmt ` chunk tail — empty for every PCM
    /// tag, which is what distinguishes the two `write_header`/
    /// `write_trailer` shapes below (a real `WAVEFORMATEX` never carries
    /// one; only the compressed path does). Non-empty only for AAC today:
    /// `add_stream` copies `CodecParameters::extradata` straight through,
    /// unmodified, the same `AudioSpecificConfig` the MP4 `esds` box that
    /// carried it already held — OBSERVED, `ffmpeg 9.0.1`'s own AAC-in-WAV
    /// `-c copy` output carries the identical bytes.
    extradata: Vec<u8>,
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
            data_size_pos: 0,
            fact_size_pos: None,
            sample_extent: 0,
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
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("wav: the codec must be known"))?;
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1) as u16;
        let sample_rate = audio.sample_rate.max(1);

        // AAC copied through undecoded — RIFF/WAVE's compressed-audio
        // convention, not this muxer's PCM path. Checked ahead of the
        // planar-format and "PCM-shaped" refusals just below because
        // neither applies to it: both guard PCM's own byte layout (a fixed
        // function of sample count, channels and bit depth), and a
        // compressed AAC frame does not have one — `format.is_planar()`
        // describes what *decoding* this stream would produce, not
        // anything about the bytes actually being copied, so refusing on
        // it here was refusing a real `ffmpeg 9.0.1`-accepted `-c copy`
        // for a property that was never true of the bytes on the wire.
        if codec == vaco_codec_core::CodecId::Aac {
            let extradata = params
                .extradata
                .clone()
                .filter(|e| !e.is_empty())
                .ok_or(Error::Unsupported(
                    "wav: this AAC stream has no AudioSpecificConfig to copy \
                     (needs one from the source container, or a decode)",
                ))?;
            // `nBlockAlign`: measured against two real `ffmpeg 9.0.1`
            // AAC-in-MP4 -> WAV encodes at different channel counts and bit
            // rates (mono/70 kb/s, stereo/192 kb/s) — both produced exactly
            // `768 * channels`, unrelated to either the real (and,
            // per-frame, variable) coded size or the bit rate. Not derived
            // from anything a stream-copy has on hand, because nothing
            // available states it: no frame has been decoded, so no real
            // per-packet sample count is known before the fact, and RIFF's
            // own convention for a compressed tag does not require
            // `nBlockAlign` to be exact the way it must be for PCM.
            let block_align = 768u32.saturating_mul(u32::from(channels));
            #[allow(
                clippy::integer_division,
                reason = "bits to bytes is an exact unit change, and the floor this truncates \
                          towards is the measured behaviour: a real 191223 bps AAC encode's own \
                          WAV output states 23902, not 23903"
            )]
            let avg_bytes_per_sec =
                u32::try_from(params.bit_rate.unwrap_or(0) / 8).unwrap_or(u32::MAX);
            self.stream = Some(MuxStream {
                sample_rate,
                channels,
                bits_per_sample: 16, // nominal; WAVE_FORMAT_AAC states no real bit depth.
                format_tag: vaco_format_riff::wave::WAVE_FORMAT_AAC,
                block_align,
                avg_bytes_per_sec,
                extradata,
            });
            return Ok(0);
        }

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
        let coded_bits = pcm::coded_bits(codec)
            .ok_or(Error::Unsupported("wav: only PCM-shaped codecs are supported"))?;
        let block_align =
            u32::from(channels).saturating_mul(u32::from(coded_bits.div_ceil(8)));
        self.stream = Some(MuxStream {
            sample_rate,
            channels,
            bits_per_sample: u16::from(coded_bits),
            format_tag: match codec {
                vaco_codec_core::CodecId::PcmAlaw => 6,
                vaco_codec_core::CodecId::PcmMulaw => 7,
                _ if pcm::is_float(codec) => 3,
                _ => 1,
            },
            block_align,
            avg_bytes_per_sec: sample_rate.saturating_mul(block_align),
            extradata: Vec::new(),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .clone()
            .ok_or(Error::InvalidData("wav: no stream added"))?;
        let compressed = !s.extradata.is_empty();
        self.out.write(&ids::RIFF.as_bytes())?;
        self.out.wl32(0)?; // patched in write_trailer
        self.out.write(&ids::WAVE.as_bytes())?;

        self.out.write(&ids::FMT.as_bytes())?;
        // 16 bytes: plain `WAVEFORMATEX`, no `cbSize` at all — the shape
        // every PCM reader expects and the reference itself writes for PCM.
        // 18 + extradata: `cbSize` present and non-zero, `WAVEFORMATEX`'s
        // own documented way to carry a codec-specific tail.
        // OBSERVED, `ffmpeg 9.0.1`: an odd `extradata` length (AAC's own
        // `AudioSpecificConfig` commonly is — 5 bytes for LC/44.1 kHz/mono)
        // gets one zero pad byte, and the declared `fmt ` chunk size counts
        // it — 18 + 5 rounds up to 24, not RIFF's usual convention of
        // padding *between* chunks without it counting toward either
        // chunk's own declared size. Reproduced exactly rather than
        // following the general RIFF rule, since this is what the
        // reference's own bytes state.
        let fmt_payload_len = if compressed { 18 + s.extradata.len() } else { 16 };
        let fmt_len = fmt_payload_len + fmt_payload_len % 2;
        self.out.wl32(u32::try_from(fmt_len).unwrap_or(u32::MAX))?;
        self.out.wl16(s.format_tag)?;
        self.out.wl16(s.channels)?;
        self.out.wl32(s.sample_rate)?;
        self.out.wl32(s.avg_bytes_per_sec)?;
        self.out
            .wl16(u16::try_from(s.block_align).unwrap_or(u16::MAX))?;
        self.out.wl16(s.bits_per_sample)?;
        if compressed {
            self.out
                .wl16(u16::try_from(s.extradata.len()).unwrap_or(u16::MAX))?;
            self.out.write(&s.extradata)?;
            if s.extradata.len() % 2 == 1 {
                self.out.write(&[0])?;
            }

            // `fact`: required for any non-PCM `wFormatTag` — OBSERVED,
            // `ffmpeg 9.0.1` writes one for AAC-in-WAV and does not for
            // PCM. `dwSampleLength` is patched in `write_trailer`, once
            // every packet's extent has been seen.
            self.out.write(&ids::FACT.as_bytes())?;
            self.out.wl32(4)?;
            self.fact_size_pos = Some(self.out.pos());
            self.out.wl32(0)?; // patched in write_trailer
        }

        self.out.write(&ids::DATA.as_bytes())?;
        self.data_size_pos = self.out.pos();
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
        // Only meaningful for the compressed path's `fact` chunk — PCM's
        // sample count is `data_bytes / block_align`, needing nothing
        // tracked per packet.
        if self.fact_size_pos.is_some()
            && let Some(s) = &self.stream
            && let Some(pts) = packet.pts.ticks()
        {
            let base = Rational::new(1, s.sample_rate.cast_signed());
            let end = pts.saturating_add(packet.duration.to_ticks(base).unwrap_or(0));
            self.sample_extent = self.sample_extent.max(end);
        }
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        if stream_index != 0 {
            return None;
        }
        self.stream
            .as_ref()
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
        // RIFF's ordinary padding rule, unlike `fmt `'s (see `write_header`):
        // an odd-length `data` chunk gets one trailing zero byte that counts
        // toward the file's overall size but *not* toward `data`'s own
        // declared size — OBSERVED, `ffmpeg 9.0.1`'s `data` size field
        // states the true (odd) byte count while the file itself is one
        // byte longer than that count would otherwise imply.
        if self.data_bytes % 2 == 1 {
            self.out.write(&[0])?;
        }
        let end = self.out.pos();
        // The actual end position minus the 8-byte `RIFF`/size header is
        // `riff_size` by RIFF's own definition, for any chunk layout this
        // muxer ever writes — simpler, and more robust to a new chunk
        // appearing later, than a formula that has to be kept in sync with
        // every chunk this function writes.
        self.out.seek(4)?;
        self.out
            .wl32(u32::try_from(end.saturating_sub(8)).unwrap_or(u32::MAX))?;
        if let Some(pos) = self.fact_size_pos {
            self.out.seek(pos)?;
            self.out
                .wl32(u32::try_from(self.sample_extent.max(0)).unwrap_or(u32::MAX))?;
        }
        self.out.seek(self.data_size_pos)?;
        self.out
            .wl32(u32::try_from(self.data_bytes).unwrap_or(u32::MAX))?;
        self.out.seek(end)?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::WavDemuxer;
    use vaco_format_core::{Demuxer, FormatOptions};
    use vaco_io::MemorySource;

    /// Builds a minimal WAV file with a `fmt `, a `LIST/INFO/ISFT` and a
    /// `data` chunk, in that order.
    fn wav_with_isft(software: &[u8]) -> Vec<u8> {
        let mut fmt_payload = vec![1, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0];
        let mut info = b"INFO".to_vec();
        info.extend_from_slice(b"ISFT");
        info.extend_from_slice(&(software.len() as u32).to_le_bytes());
        info.extend_from_slice(software);
        if software.len() % 2 == 1 {
            info.push(0);
        }

        let mut body = Vec::new();
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt_payload.len() as u32).to_le_bytes());
        body.append(&mut fmt_payload);
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(&info);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&[0; 4]);

        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&body);
        file
    }

    /// Finding 55's `format.tags.encoder` gap: the reference states it from
    /// `LIST/INFO/ISFT`, and this demuxer used to skip the whole chunk.
    #[test]
    fn list_info_isft_surfaces_as_an_encoder_tag() {
        let bytes = wav_with_isft(b"Lavf62.12.100\0");
        let demux =
            WavDemuxer::open(Box::new(MemorySource::new(bytes)), &FormatOptions::default())
                .unwrap();
        assert_eq!(
            demux.metadata(),
            &[("encoder".to_owned(), "Lavf62.12.100".to_owned())]
        );
    }

    /// A `LIST` chunk whose form is not `INFO` (e.g. `adtl`, cue labels) must
    /// not be misread as one — and must not desync the chunk walk that finds
    /// `data` right after it.
    #[test]
    fn a_wav_with_no_list_chunk_has_no_tags() {
        let mut fmt_payload = vec![1, 0, 1, 0, 0x44, 0xac, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0];
        let mut body = Vec::new();
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt_payload.len() as u32).to_le_bytes());
        body.append(&mut fmt_payload);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&body);

        let demux =
            WavDemuxer::open(Box::new(MemorySource::new(file)), &FormatOptions::default()).unwrap();
        assert!(demux.metadata().is_empty());
    }
}
