//! Sony Wave64 (`.w64`).
//!
//! Sony's Wave64 specification: RIFF/WAVE's own grammar, restated with
//! 128-bit GUIDs in place of four-character chunk codes and 64-bit chunk
//! sizes in place of 32-bit ones — built for exactly the >4 GiB files
//! plain WAV needs the `RF64` extension for. `fmt ` itself is byte-identical
//! to plain `WAVEFORMATEX`, so this module reuses
//! [`vaco_format_riff::wave::WaveFormatEx`] and
//! [`vaco_format_riff::wave_tags`] verbatim for it, the same way [`crate::wav`]
//! does — only the outer chunk grammar differs, so only the outer chunk
//! grammar is reimplemented here.
//!
//! # Layout, measured byte-for-byte against `ffmpeg` 8.1
//!
//! ```text
//! RIFF_GUID(16)  riff_size:u64le (counts the WHOLE file, header included —
//!                                 unlike RIFF's own size, which excludes
//!                                 its 8-byte tag+size header)
//! WAVE_GUID(16)
//!
//! chunk, repeated:
//!   guid:128 bits   chunk_size:u64le (counts this 24-byte header too)
//!   payload[chunk_size - 24]
//!   pad to the next 8-byte boundary
//! ```
//!
//! Every GUID this module recognises is `ASCII-tag(4 bytes) ++ suffix(12
//! bytes)`, but — measured, not assumed — **the suffix is not the same for
//! every tag**: the outer `RIFF` GUID's suffix
//! (`2E91CF11A5D628DB04C10000`) differs from the one `WAVE`/`fmt `/`data`
//! share (`F3ACD3118CD100C04F8EDB8A`). A parser that reused one suffix for
//! both would silently fail to recognise `fmt `/`data`, which is exactly the
//! kind of "plausible-looking pattern, wrong in a way only measurement
//! catches" plan 13 §1b warns about, so this module matches each GUID as a
//! fixed 16-byte constant rather than a decoded tag-plus-shared-suffix.

use vaco_codec_core::CodecParameters;
use vaco_core::{Error, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, FormatOptions, Muxer, MuxerDesc, ParserProvider, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use vaco_format_riff::wave::WaveFormatEx;
use vaco_format_riff::wave_tags;

use crate::pcm::{self, PcmLayout, RawPcmDemuxer};

const RIFF_GUID: [u8; 16] = *b"riff\x2e\x91\xcf\x11\xa5\xd6\x28\xdb\x04\xc1\x00\x00";
const WAVE_GUID: [u8; 16] = *b"wave\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";
const FMT_GUID: [u8; 16] = *b"fmt \xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";
const DATA_GUID: [u8; 16] = *b"data\xf3\xac\xd3\x11\x8c\xd1\x00\xc0\x4f\x8e\xdb\x8a";

/// Bytes in a chunk's GUID + 64-bit size header.
const CHUNK_HEADER_LEN: u64 = 24;

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// w64` file with no extension is `100`.
pub const W64_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.matches_at(0, &RIFF_GUID) && data.matches_at(24, &WAVE_GUID) {
        W64_SCORE
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "w64",
    long_name: "Sony Wave64",
    extensions: &["w64"],
    mime_types: &[],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "w64",
    long_name: "Sony Wave64",
    extensions: &["w64"],
    default_video: None,
    // `ffmpeg -h muxer=w64` says "Default audio codec: pcm_s16le." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(vaco_codec_core::CodecId::PcmS16le),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(W64Demuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(W64Muxer::new(sink)?))
}

fn read_guid(io: &mut IoContext) -> Result<[u8; 16]> {
    let mut g = [0u8; 16];
    io.read_exact(&mut g)?;
    Ok(g)
}

/// Round `n` up to the next multiple of 8, per Wave64's own chunk-alignment
/// rule (the 8-byte-granularity analogue of RIFF's 2-byte pad).
const fn pad8(n: u64) -> u64 {
    n.saturating_add(7) & !7
}

#[derive(Debug)]
pub struct W64Demuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl W64Demuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the `riff`/`wave` GUIDs or the `fmt `/`data`
    /// chunks do not parse.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if read_guid(&mut io)? != RIFF_GUID {
            return Err(Error::InvalidData("w64: missing riff GUID"));
        }
        let _riff_size = io.rl64()?;
        if read_guid(&mut io)? != WAVE_GUID {
            return Err(Error::InvalidData("w64: form GUID is not wave"));
        }

        let mut budget = Budget::new(Limits::permissive());
        let mut fmt: Option<WaveFormatEx> = None;
        let mut data_start = 0u64;
        let mut data_declared: Option<u64> = None;

        while let Ok(guid) = read_guid(&mut io) {
            let chunk_size = io.rl64()?;
            let payload_len = chunk_size.saturating_sub(CHUNK_HEADER_LEN);
            if guid == FMT_GUID {
                let take = usize::try_from(payload_len).unwrap_or(0).min(4096);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                fmt = Some(WaveFormatEx::parse(&buf, &mut budget)?);
                let consumed = CHUNK_HEADER_LEN.saturating_add(take as u64);
                let padded = pad8(chunk_size);
                io.skip(padded.saturating_sub(consumed))?;
            } else if guid == DATA_GUID {
                data_start = io.pos();
                data_declared = Some(payload_len);
                break;
            } else {
                io.skip(pad8(chunk_size).saturating_sub(CHUNK_HEADER_LEN))?;
            }
        }

        let fmt = fmt.ok_or(Error::InvalidData("w64: no fmt chunk"))?;
        let Some(declared_len) = data_declared else {
            return Err(Error::InvalidData("w64: no data chunk"));
        };

        let channels = fmt.channels;
        let codec_id = wave_tags::codec_id(&fmt);
        let pcm_fmt = codec_id.and_then(pcm::sample_fmt_of);
        let (format, bits_raw) = match pcm_fmt {
            Some((sf, raw)) => (Some(sf), raw),
            None => (None, None),
        };
        let bits_coded = if pcm_fmt.is_some() {
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
            bits_coded,
            bits_raw,
        );
        // `fmt.format_tag` alone is `WAVE_FORMAT_EXTENSIBLE` (0xFFFE) for
        // anything past plain 16-bit PCM/float — measured on `-c:a
        // pcm_s24le`, which `ffmpeg`'s own `w64` muxer writes as
        // `WAVEFORMATEXTENSIBLE` the same way it does for WAV. The reference
        // reports the real, unwrapped tag (`0x0001` `WAVE_FORMAT_PCM`), not
        // the extensible sentinel — `wave_tags::codec_id` already resolves
        // this same `SubFormat` for the *codec*, just not for this field.
        let effective_tag = fmt
            .extensible()
            .and_then(|e| e.sub_format_tag())
            .unwrap_or(fmt.format_tag);
        params.codec_tag = Some([
            (effective_tag & 0xff) as u8,
            (effective_tag >> 8) as u8,
            0,
            0,
        ]);

        let mut stream =
            pcm::new_stream(Rational::new(1, fmt.samples_per_sec.max(1).cast_signed()));
        stream.params = params;

        let mut inner = RawPcmDemuxer::new(
            io,
            stream,
            data_start,
            Some(declared_len),
            bytes_per_frame.max(1),
        );
        inner.forget_frame_count();
        inner.size_packets_in_frames();
        Ok(Self {
            inner,
            budget: Budget::new(Limits::permissive()),
        })
    }
}

impl Demuxer for W64Demuxer {
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

/// Writes plain `WAVEFORMATEX` PCM, the same restriction [`crate::wav::WavMuxer`]
/// documents.
#[derive(Debug)]
pub struct W64Muxer {
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

impl W64Muxer {
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

impl Muxer for W64Muxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("w64: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("w64: not an audio stream"))?;
        let format = audio
            .format
            .ok_or(Error::Unsupported("w64: sample format must be known"))?;
        if format.is_planar() {
            return Err(Error::Unsupported(
                "w64: planar sample formats are not supported",
            ));
        }
        // The coded width, not the decoded format's.
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("w64: the codec must be known"))?;
        let coded_bits = pcm::coded_bits(codec).ok_or(Error::Unsupported(
            "w64: only PCM-shaped codecs are supported",
        ))?;
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1) as u16;
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
            bits_per_sample: u16::from(coded_bits),
            format_tag: match codec {
                vaco_codec_core::CodecId::PcmAlaw => 6,
                vaco_codec_core::CodecId::PcmMulaw => 7,
                _ if pcm::is_float(codec) => 3,
                _ => 1,
            },
            bytes_per_frame: u32::from(channels).saturating_mul(u32::from(coded_bits.div_ceil(8))),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("w64: no stream added"))?;
        self.out.write(&RIFF_GUID)?;
        self.out.wl64(0)?; // patched in write_trailer
        self.out.write(&WAVE_GUID)?;

        self.out.write(&FMT_GUID)?;
        self.out.wl64(CHUNK_HEADER_LEN + 16)?;
        let tag: u16 = s.format_tag;
        self.out.wl16(tag)?;
        self.out.wl16(s.channels)?;
        self.out.wl32(s.sample_rate)?;
        self.out
            .wl32(s.sample_rate.saturating_mul(s.bytes_per_frame))?;
        self.out.wl16(s.bytes_per_frame as u16)?;
        self.out.wl16(s.bits_per_sample)?;

        self.out.write(&DATA_GUID)?;
        self.out.wl64(0)?; // patched in write_trailer
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("w64: packet written before the header"));
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
            return Err(Error::InvalidData("w64: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            return self.out.flush();
        }
        let end = self.out.pos();
        let fmt_chunk_total = CHUNK_HEADER_LEN + 16;
        let data_chunk_total = CHUNK_HEADER_LEN + self.data_bytes;
        let riff_total = CHUNK_HEADER_LEN + 16 + fmt_chunk_total + data_chunk_total;

        self.out.seek(16)?;
        self.out.wl64(riff_total)?;

        let data_size_pos = CHUNK_HEADER_LEN + 16 + fmt_chunk_total + 16;
        self.out.seek(data_size_pos)?;
        self.out.wl64(data_chunk_total)?;

        self.out.seek(end)?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    /// A minimal, hand-built `.w64` file whose `fmt ` chunk is
    /// `WAVEFORMATEXTENSIBLE` (`format_tag = 0xFFFE`) wrapping plain PCM —
    /// measured shape: `ffmpeg -f w64 -c:a pcm_s24le` writes exactly this,
    /// the same way it writes 24-bit WAV as extensible (`vaco-format-riff`'s
    /// own `wave_tags` tests build the identical `SubFormat` GUID for WAV).
    fn extensible_pcm24_mono_w64() -> Vec<u8> {
        let mut fmt_payload = Vec::new();
        fmt_payload.extend_from_slice(&0xFFFEu16.to_le_bytes()); // format_tag
        fmt_payload.extend_from_slice(&1u16.to_le_bytes()); // channels
        fmt_payload.extend_from_slice(&44_100u32.to_le_bytes()); // samples_per_sec
        fmt_payload.extend_from_slice(&(44_100u32 * 3).to_le_bytes()); // avg_bytes_per_sec
        fmt_payload.extend_from_slice(&3u16.to_le_bytes()); // block_align
        fmt_payload.extend_from_slice(&24u16.to_le_bytes()); // bits_per_sample
        fmt_payload.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        fmt_payload.extend_from_slice(&24u16.to_le_bytes()); // valid_bits_per_sample
        fmt_payload.extend_from_slice(&4u32.to_le_bytes()); // channel_mask
        fmt_payload.extend_from_slice(&[
            // KSDATAFORMAT_SUBTYPE_PCM
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ]);
        assert_eq!(fmt_payload.len(), 40);

        let data_payload = [0u8; 12];

        let mut out = Vec::new();
        out.extend_from_slice(&RIFF_GUID);
        out.extend_from_slice(&0u64.to_le_bytes()); // riff_size, unchecked by `open`
        out.extend_from_slice(&WAVE_GUID);

        out.extend_from_slice(&FMT_GUID);
        out.extend_from_slice(&(CHUNK_HEADER_LEN + fmt_payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&fmt_payload);

        out.extend_from_slice(&DATA_GUID);
        out.extend_from_slice(&(CHUNK_HEADER_LEN + data_payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&data_payload);
        out
    }

    /// Regression: `codec_tag` used to be `fmt.format_tag` verbatim, so an
    /// extensible-PCM file stated `WAVE_FORMAT_EXTENSIBLE` (`0xfffe`) as its
    /// `codec_tag` where the reference resolves the wrapped `SubFormat` and
    /// states plain PCM (`0x0001`) — `wave_tags::codec_id` already did this
    /// resolution for the *codec*, just not for this field.
    #[test]
    fn codec_tag_resolves_the_extensible_sub_format_not_the_wrapper_tag() {
        let src = Box::new(MemorySource::new(extensible_pcm24_mono_w64()));
        let demux = W64Demuxer::open(src, &FormatOptions::default()).expect("open");
        assert_eq!(
            demux.streams()[0].params.codec_tag,
            Some([0x01, 0x00, 0x00, 0x00]),
            "codec_tag should be the unwrapped WAVE_FORMAT_PCM, not 0xfffe"
        );
    }
}
