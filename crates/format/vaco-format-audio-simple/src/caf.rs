//! Apple CAF (Core Audio Format).
//!
//! Apple's *Core Audio Format Specification 1.0* (published). Unlike RIFF/
//! IFF, CAF chunk sizes are 64-bit and signed, with `-1` meaning "unknown,
//! read to EOF" (a native unknown-length convention, the same idea
//! `vaco-format-riff::chunk` documents for RIFF's `0xFFFFFFFF`), and there is
//! no word-alignment padding at all.
//!
//! # Layout
//!
//! ```text
//! file header:  "caff"  mFileVersion:be16  mFileFlags:be16
//! chunk:        mChunkType:FourCC(4)  mChunkSize:be64 (signed)  data[mChunkSize]
//!
//! 'desc' (Audio Description, always first, exactly 32 bytes):
//!   mSampleRate:f64be  mFormatID:FourCC  mFormatFlags:be32
//!   mBytesPerPacket:be32  mFramesPerPacket:be32  mChannelsPerFrame:be32
//!   mBitsPerChannel:be32
//!
//! 'data' (Audio Data):
//!   mEditCount:be32  audioData[...]
//! ```
//!
//! # Measured
//!
//! `ffmpeg -c:a pcm_s16be -f caf`'s `desc` chunk: `mFormatID = "lpcm"`,
//! `mFormatFlags = 0` (neither `kCAFLinearPCMFormatFlagIsFloat` nor
//! `kCAFLinearPCMFormatFlagIsLittleEndian` set, matching big-endian
//! integer), `mBytesPerPacket = 4`, `mFramesPerPacket = 1`,
//! `mChannelsPerFrame = 2`, `mBitsPerChannel = 16` — i.e. every uncompressed
//! PCM `desc` states one frame per packet and `bytesPerPacket = channels ×
//! bytesPerSample`, which is what this module uses to derive
//! `bytes_per_frame` rather than trusting a second, redundant field.
//!
//! # `info`: measured, not assumed
//!
//! Apple's spec calls this "similar to" Vorbis comments, which is loose
//! enough to be wrong: measured directly (`ffmpeg -c:a pcm_s16be -f caf`,
//! hexdump), the real layout is `mNumEntries:be32` followed by that many
//! **NUL-terminated** C-string pairs (key, then value) — no length prefix on
//! either string, unlike an actual Vorbis comment. `("encoder",
//! "Lavf62.12.100")` round-trips through this exact shape on a real fixture
//! (`fuzz/seeds/diff/caf/pcm16-mono-8k.caf`, offset `0x49`).
//!
//! # What is not read
//!
//! `chan` (channel layout), `pakt` (the variable packet table compressed
//! formats need), `kuki` (codec magic cookie) and `free` are skipped, not
//! decoded — deferred, and the reason compressed CAF streams (anything whose
//! `mFormatID` is not `lpcm`/`ulaw`/`alaw`) get `codec_id: None`: without
//! `pakt`, packet boundaries for a variable-bitrate codec cannot be
//! recovered.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, FormatOptions, Muxer, MuxerDesc, ParserProvider, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::pcm::{self, PcmLayout, RawPcmDemuxer};

const CAFF: [u8; 4] = *b"caff";
const DESC: [u8; 4] = *b"desc";
const DATA: [u8; 4] = *b"data";
const INFO: [u8; 4] = *b"info";
const LPCM: [u8; 4] = *b"lpcm";

const FLAG_FLOAT: u32 = 1 << 0;
/// `kCAFLinearPCMFormatFlagIsLittleEndian`. Measured by writing each PCM
/// flavour and reading `mFormatFlags` at offset 32 of the file:
///
/// ```text
/// pcm_s16be -> 0   pcm_s16le -> 2   pcm_f32be -> 1   pcm_f32le -> 3
/// ```
///
/// `pcm_s8` writes 0 (8-bit LPCM is signed here), and the reference's `caf`
/// muxer refuses `pcm_u8` outright.
const FLAG_LITTLE_ENDIAN: u32 = 1 << 1;

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// caf` file with no extension is `100`.
pub const CAF_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) == Some(CAFF) {
        CAF_SCORE
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "caf",
    long_name: "Apple CAF (Core Audio Format)",
    extensions: &["caf"],
    mime_types: &["audio/x-caf"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "caf",
    long_name: "Apple CAF (Core Audio Format)",
    extensions: &["caf"],
    default_video: None,
    // `ffmpeg -h muxer=caf` says "Default audio codec: pcm_s16be." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(CodecId::PcmS16be),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(CafDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(CafMuxer::new(sink)?))
}

/// Parse an `info` chunk payload (`mNumEntries:be32` then that many
/// NUL-terminated `(key, value)` C-string pairs — see the module docs for
/// why this is not actually Vorbis-comment-shaped despite Apple's wording).
///
/// Never panics or over-reads: a truncated or malformed payload yields
/// whatever complete pairs were read before the truncation, not an error —
/// the same "a container's own metadata is advisory" treatment
/// `vaco_format_riff::info::list_info_tags` gives `LIST/INFO`.
fn parse_info_chunk(payload: &[u8]) -> Vec<(String, String)> {
    let mut r = vaco_bitstream::ByteReader::new(payload);
    if r.remaining() < 4 {
        return Vec::new();
    }
    let count = r.be32();
    let mut tags = Vec::new();
    for _ in 0..count {
        let Some(key) = read_cstring(&mut r) else {
            break;
        };
        let Some(value) = read_cstring(&mut r) else {
            break;
        };
        if !key.is_empty() {
            tags.push((key, value));
        }
    }
    tags
}

/// One NUL-terminated string, or `None` if the reader ran out of bytes
/// before finding the terminator (a truncated file, not a well-formed empty
/// string — an empty string is still `Some(String::new())`).
fn read_cstring(r: &mut vaco_bitstream::ByteReader<'_>) -> Option<String> {
    let rest = r.rest();
    let nul_at = rest.iter().position(|&b| b == 0)?;
    let (text, _) = rest.split_at(nul_at);
    let s = String::from_utf8_lossy(text).into_owned();
    r.skip(nul_at + 1);
    Some(s)
}

#[derive(Debug)]
pub struct CafDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
    metadata: Vec<(String, String)>,
}

impl CafDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the `caff` signature, `desc` or `data`
    /// chunks do not parse.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != CAFF {
            return Err(Error::InvalidData("caf: missing caff signature"));
        }
        let _version = io.rb16()?;
        let _flags = io.rb16()?;

        let mut budget = Budget::new(Limits::permissive());
        let mut sample_rate = 0u32;
        let mut format_id = LPCM;
        let mut format_flags = 0u32;
        let mut bytes_per_packet = 0u32;
        let mut channels = 0u32;
        let mut bits_per_channel = 0u32;
        let mut have_desc = false;
        let mut have_data = false;
        let mut data_start = 0u64;
        let mut data_declared: Option<u64> = None;
        let mut metadata: Vec<(String, String)> = Vec::new();

        while let Ok(id) = io.tag() {
            let size = io.rb64()?.cast_signed();
            if id == DESC {
                let take = usize::try_from(size).unwrap_or(0).min(64);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                let mut r = vaco_bitstream::ByteReader::new(&buf);
                sample_rate = r.f64_be().round() as u32;
                format_id = <[u8; 4]>::try_from(r.bytes(4)).unwrap_or(LPCM);
                format_flags = r.be32();
                bytes_per_packet = r.be32();
                let _frames_per_packet = r.be32();
                channels = r.be32();
                bits_per_channel = r.be32();
                have_desc = true;
            } else if id == DATA {
                let _edit_count = io.rb32()?;
                data_start = io.pos();
                data_declared = if size < 0 {
                    None
                } else {
                    Some(u64::try_from(size).unwrap_or(0).saturating_sub(4))
                };
                have_data = true;
                break;
            } else if id == INFO && size >= 0 {
                let take = usize::try_from(size).unwrap_or(0);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                metadata = parse_info_chunk(&buf);
            } else if size >= 0 {
                io.skip(u64::try_from(size).unwrap_or(0))?;
            } else {
                // A negative size on anything but `data` is not a
                // recognised convention; nothing safe follows it.
                return Err(Error::InvalidData("caf: unexpected unbounded chunk"));
            }
        }

        if !have_desc {
            return Err(Error::InvalidData("caf: no desc chunk"));
        }

        let is_float = format_flags & FLAG_FLOAT != 0;
        let bits_u8 = u8::try_from(bits_per_channel.min(255)).unwrap_or(255);
        let big_endian = format_flags & FLAG_LITTLE_ENDIAN == 0;
        let (codec_id, format, bits_coded, bits_raw) = if format_id == LPCM {
            let (fmt, raw) = pcm::sample_fmt_for(bits_u8, is_float);
            let id = pcm::codec_id_for(bits_u8, is_float, big_endian, true);
            (Some(id), fmt, Some(bits_u8), raw)
        } else if &format_id == b"ulaw" {
            (
                Some(CodecId::PcmMulaw),
                Some(vaco_sampfmt::SampleFmt::S16),
                Some(8),
                None,
            )
        } else if &format_id == b"alaw" {
            (
                Some(CodecId::PcmAlaw),
                Some(vaco_sampfmt::SampleFmt::S16),
                Some(8),
                None,
            )
        } else {
            (None, None, Some(0), None)
        };

        // `bytesPerPacket` already states the per-frame byte width for
        // uncompressed PCM (module docs); fall back to deriving it only if
        // the container left it at zero.
        let bytes_per_frame = if bytes_per_packet > 0 {
            bytes_per_packet
        } else {
            channels
                .max(1)
                .saturating_mul(u32::from(bits_u8.div_ceil(8).max(1)))
        };

        if !have_data {
            return Err(Error::InvalidData("caf: no data chunk"));
        }
        let declared_len = data_declared;

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
        params.codec_tag = Some(format_id);

        let mut stream = pcm::new_stream(Rational::new(1, sample_rate.max(1).cast_signed()));
        stream.params = params;

        let inner =
            RawPcmDemuxer::new(io, stream, data_start, declared_len, bytes_per_frame.max(1));
        Ok(Self {
            inner,
            budget: Budget::new(Limits::permissive()),
            metadata,
        })
    }
}

impl Demuxer for CafDemuxer {
    fn streams(&self) -> &[Stream] {
        self.inner.streams()
    }
    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
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

#[derive(Debug)]
pub struct CafMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    data_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u32,
    bits_per_channel: u32,
    /// `mFormatID`: `lpcm` for linear PCM, `alaw`/`ulaw` for the companded
    /// pair. Writing `lpcm` for A-law data mislabels one byte per sample as
    /// two and corrupts the stream.
    format_id: [u8; 4],
    /// `mFormatFlags`, bit 0 `kCAFLinearPCMFormatFlagIsFloat` and bit 1
    /// `kCAFLinearPCMFormatFlagIsLittleEndian`. Measured across nine codecs:
    /// `pcm_s16be` 0, `pcm_s16le` 2, `pcm_f32be` 1, `pcm_f32le` 3,
    /// `alaw`/`ulaw` 0.
    format_flags: u32,
    bytes_per_frame: u32,
}

impl CafMuxer {
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

impl Muxer for CafMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("caf: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("caf: not an audio stream"))?;
        let format = audio
            .format
            .ok_or(Error::Unsupported("caf: sample format must be known"))?;
        if format.is_planar() {
            return Err(Error::Unsupported(
                "caf: planar sample formats are not supported",
            ));
        }
        // From the codec, not the decoded sample format: endianness and
        // companding are not in the sample format at all.
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("caf: the codec must be known"))?;
        let coded_bits = pcm::coded_bits(codec).ok_or(Error::Unsupported(
            "caf: only PCM-shaped codecs are supported",
        ))?;
        let (format_id, format_flags) = match codec {
            vaco_codec_core::CodecId::PcmAlaw => (*b"alaw", 0),
            vaco_codec_core::CodecId::PcmMulaw => (*b"ulaw", 0),
            // `lpcm` flags state float and endianness, never signedness, so
            // unsigned 8-bit has no representation. The reference refuses it.
            vaco_codec_core::CodecId::PcmU8 => {
                return Err(Error::Unsupported(
                    "caf: unsigned 8-bit PCM has no representation in a CAF                      `lpcm` description",
                ));
            }
            _ => {
                let little_endian = pcm::is_little_endian(codec).unwrap_or(false);
                (
                    *b"lpcm",
                    u32::from(pcm::is_float(codec)) | (u32::from(little_endian) << 1),
                )
            }
        };
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1);
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
            bits_per_channel: u32::from(coded_bits),
            format_id,
            format_flags,
            bytes_per_frame: channels.saturating_mul(u32::from(coded_bits.div_ceil(8))),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("caf: no stream added"))?;
        self.out.write(&CAFF)?;
        self.out.wb16(1)?; // mFileVersion
        self.out.wb16(0)?; // mFileFlags

        self.out.write(&DESC)?;
        self.out.wb64(32)?;
        self.out.write(&f64::from(s.sample_rate).to_be_bytes())?;
        self.out.write(&s.format_id)?;
        self.out.wb32(s.format_flags)?;
        self.out.wb32(s.bytes_per_frame)?;
        self.out.wb32(1)?; // mFramesPerPacket
        self.out.wb32(s.channels)?;
        self.out.wb32(s.bits_per_channel)?;

        // 0x640001 is mono, 0x650002 stereo; anything else needs a bitmap
        // this muxer does not build, so it falls back to no descriptions.
        self.out.write(b"chan")?;
        self.out.wb64(12)?;
        self.out.wb32(match s.channels {
            1 => 0x0064_0001,
            2 => 0x0065_0002,
            _ => 0,
        })?;
        self.out.wb32(0)?; // mChannelBitmap
        self.out.wb32(0)?; // mNumberChannelDescriptions

        self.out.write(&DATA)?;
        self.out.wb64((-1i64).cast_unsigned())?; // unknown length; patched if seekable
        self.out.wb32(0)?; // mEditCount
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("caf: packet written before the header"));
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
            return Err(Error::InvalidData("caf: trailer written before the header"));
        }
        if !self.out.is_seekable() {
            // `-1` (unknown length) is CAF's own native convention for
            // exactly this case, so a non-seekable sink still produces a
            // fully valid file — unlike this crate's other muxers, no
            // divergence to document here.
            return self.out.flush();
        }
        let end = self.out.pos();
        // CAFF header(8) + desc(4+8+32) + chan(4+8+12) + DATA tag(4) lands
        // on the DATA size field. Inserting a chunk moves this.
        self.out.seek(8 + (4 + 8 + 32) + (4 + 8 + 12) + 4)?;
        self.out.wb64(self.data_bytes + 4)?;
        self.out.seek(end)?;
        self.out.flush()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod info_tests {
    use super::{CafDemuxer, parse_info_chunk};
    use vaco_format_core::{Demuxer, FormatOptions};
    use vaco_io::MemorySource;

    /// The exact `info` chunk payload measured on a real `ffmpeg -c:a
    /// pcm_s16be -f caf` file (`fuzz/seeds/diff/caf/pcm16-mono-8k.caf`,
    /// offset `0x49`): `mNumEntries=1`, then the two NUL-terminated strings
    /// `"encoder\0"` and `"Lavf62.12.100\0"` — not length-prefixed, despite
    /// the spec calling this "similar to" a Vorbis comment.
    fn measured_info_payload() -> Vec<u8> {
        let mut v = 1u32.to_be_bytes().to_vec();
        v.extend_from_slice(b"encoder\0");
        v.extend_from_slice(b"Lavf62.12.100\0");
        v
    }

    #[test]
    fn parse_info_chunk_matches_the_measured_shape() {
        assert_eq!(
            parse_info_chunk(&measured_info_payload()),
            vec![("encoder".to_owned(), "Lavf62.12.100".to_owned())]
        );
    }

    #[test]
    fn parse_info_chunk_reads_more_than_one_entry() {
        let mut v = 2u32.to_be_bytes().to_vec();
        v.extend_from_slice(b"encoder\0Lavf62.12.100\0");
        v.extend_from_slice(b"title\0a caf file\0");
        assert_eq!(
            parse_info_chunk(&v),
            vec![
                ("encoder".to_owned(), "Lavf62.12.100".to_owned()),
                ("title".to_owned(), "a caf file".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_info_chunk_stops_cleanly_on_truncation_rather_than_panicking() {
        let full = measured_info_payload();
        for n in 0..full.len() {
            let _ = parse_info_chunk(&full[..n]);
        }
        // A payload cut mid-value yields no complete pair, not a panic.
        assert!(parse_info_chunk(&full[..full.len() - 3]).is_empty());
    }

    #[test]
    fn parse_info_chunk_on_an_empty_payload_is_empty() {
        assert!(parse_info_chunk(&[]).is_empty());
    }

    /// Builds a minimal, real `desc`+`info`+`data` CAF file by hand — the
    /// same "construct the exact measured chunk shape" style
    /// `vaco_format_riff::info`'s own tests use for `LIST/INFO` — and opens
    /// it through the real demuxer, not just `parse_info_chunk` in
    /// isolation, to prove `Demuxer::metadata` actually surfaces what the
    /// chunk loop found.
    fn minimal_caf_with_info(info_payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(b"caff");
        f.extend_from_slice(&1u16.to_be_bytes()); // mFileVersion
        f.extend_from_slice(&0u16.to_be_bytes()); // mFileFlags

        // 'desc': mono, 8-bit little-endian PCM, so `desc` alone (no
        // `chan`) is enough for `CafDemuxer::open` to accept the file.
        f.extend_from_slice(b"desc");
        f.extend_from_slice(&32u64.to_be_bytes());
        f.extend_from_slice(&8000.0f64.to_be_bytes()); // mSampleRate
        f.extend_from_slice(b"lpcm"); // mFormatID
        f.extend_from_slice(&0u32.to_be_bytes()); // mFormatFlags (big-endian int)
        f.extend_from_slice(&1u32.to_be_bytes()); // mBytesPerPacket
        f.extend_from_slice(&1u32.to_be_bytes()); // mFramesPerPacket
        f.extend_from_slice(&1u32.to_be_bytes()); // mChannelsPerFrame
        f.extend_from_slice(&8u32.to_be_bytes()); // mBitsPerChannel

        f.extend_from_slice(b"info");
        f.extend_from_slice(&(info_payload.len() as u64).to_be_bytes());
        f.extend_from_slice(info_payload);

        f.extend_from_slice(b"data");
        f.extend_from_slice(&4u64.to_be_bytes()); // mEditCount(4) + no payload
        f.extend_from_slice(&0u32.to_be_bytes()); // mEditCount

        f
    }

    #[test]
    fn caf_demuxer_metadata_reports_the_encoder_tag_from_a_real_file_shape() {
        let bytes = minimal_caf_with_info(&measured_info_payload());
        let demuxer = CafDemuxer::open(
            Box::new(MemorySource::new(bytes)),
            &FormatOptions::default(),
        )
        .unwrap();
        assert_eq!(
            demuxer.metadata(),
            &[("encoder".to_owned(), "Lavf62.12.100".to_owned())]
        );
    }

    #[test]
    fn caf_demuxer_metadata_is_empty_without_an_info_chunk() {
        let mut f = Vec::new();
        f.extend_from_slice(b"caff");
        f.extend_from_slice(&1u16.to_be_bytes());
        f.extend_from_slice(&0u16.to_be_bytes());
        f.extend_from_slice(b"desc");
        f.extend_from_slice(&32u64.to_be_bytes());
        f.extend_from_slice(&8000.0f64.to_be_bytes());
        f.extend_from_slice(b"lpcm");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(&8u32.to_be_bytes());
        f.extend_from_slice(b"data");
        f.extend_from_slice(&4u64.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());

        let demuxer =
            CafDemuxer::open(Box::new(MemorySource::new(f)), &FormatOptions::default()).unwrap();
        assert!(demuxer.metadata().is_empty());
    }
}
