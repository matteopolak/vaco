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
//! # What is not read
//!
//! `chan` (channel layout), `info` (a Vorbis-comment-shaped key/value list),
//! `pakt` (the variable packet table compressed formats need), `kuki`
//! (codec magic cookie) and `free` are skipped, not decoded — deferred, and
//! the reason compressed CAF streams (anything whose `mFormatID` is not
//! `lpcm`/`ulaw`/`alaw`) get `codec_id: None`: without `pakt`, packet
//! boundaries for a variable-bitrate codec cannot be recovered.

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

#[derive(Debug)]
pub struct CafDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
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
        })
    }
}

impl Demuxer for CafDemuxer {
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
        // Everything below comes from the *codec*, not from the decoded sample
        // format, and that distinction is the bug this replaced. `mFormatFlags`
        // was written as `u32::from(format.is_float())`, so the little-endian
        // bit was never set and `-c copy` of a `pcm_s16le` stream produced a
        // file every reader takes as big-endian: byte-swapped audio, no error.
        // The A-law path was worse — `lpcm` with two bytes per frame over
        // one-byte-per-sample data (CONFORMANCE-FINDINGS 43).
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("caf: the codec must be known"))?;
        let coded_bits = pcm::coded_bits(codec)
            .ok_or(Error::Unsupported("caf: only PCM-shaped codecs are supported"))?;
        let (format_id, format_flags) = match codec {
            vaco_codec_core::CodecId::PcmAlaw => (*b"alaw", 0),
            vaco_codec_core::CodecId::PcmMulaw => (*b"ulaw", 0),
            // CAF's `lpcm` flags say float-or-not and endian-or-not, and
            // nothing about signedness — so there is no way to state unsigned
            // 8-bit, and writing it as signed silently offsets every sample by
            // 128. The reference refuses `pcm_u8` for CAF outright; so do we,
            // rather than write a file whose audio is wrong.
            vaco_codec_core::CodecId::PcmU8 => {
                return Err(Error::Unsupported(
                    "caf: unsigned 8-bit PCM has no representation in a CAF                      `lpcm` description",
                ));
            }
            _ => {
                let little_endian = pcm::is_little_endian(codec).unwrap_or(false);
                (
                    *b"lpcm",
                    u32::from(format.is_float()) | (u32::from(little_endian) << 1),
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

        // `chan`: the reference writes one for every stream it muxes, measured
        // across all nine codecs above. `mChannelLayoutTag` 0x640001 is
        // `kCAFChannelLayoutTag_Mono`; 0x650002 is stereo. Anything else is
        // described by a bitmap rather than a tag, which this muxer does not
        // build, so it falls back to `UseChannelDescriptions` with none —
        // the layout the reference itself writes when it has nothing better.
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
        // DATA chunk size field: CAFF header(8) + desc tag+size+payload
        // (4+8+32) + chan tag+size+payload (4+8+12) + DATA tag(4) lands right
        // at it.
        //
        // Recorded because it is the trap this arithmetic sets: inserting the
        // `chan` chunk moved this offset, and the seek then patched *chan's*
        // size field with the data length. The file was still exactly the
        // right length and the header still looked plausible — only the
        // chunk walk gave it away.
        self.out.seek(8 + (4 + 8 + 32) + (4 + 8 + 12) + 4)?;
        self.out.wb64(self.data_bytes + 4)?;
        self.out.seek(end)?;
        self.out.flush()
    }
}
