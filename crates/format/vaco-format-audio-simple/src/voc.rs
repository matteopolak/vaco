//! Creative Voice File (`.voc`).
//!
//! De facto Sound Blaster format from Creative Labs, long stable and widely
//! documented (e.g. the historical "VOC file format" notes distributed with
//! Sound Blaster SDKs). A 26-byte header followed by a chain of
//! type-tagged blocks — unlike every other format in this crate, the audio
//! itself is not one contiguous span: a writer that does not know its total
//! length up front (every streaming encoder) splits the data across
//! multiple "sound data" blocks, each individually length-prefixed.
//!
//! # Layout
//!
//! ```text
//! header: "Creative Voice File" 0x1A   header_size:le16   version:le16   checksum:le16
//!         (checksum = !version + 0x1234, not verified — a writer that gets
//!         it wrong still produces audio no different from one that does not)
//!
//! block, repeated until type 0 (terminator) or EOF:
//!   type:u8   length:le24 (3-byte little-endian)   payload[length]
//!
//! type 1  "Sound data" (legacy): time_constant:u8  codec:u8  pcm[length-2]
//! type 2  "Sound data continuation": pcm[length] (same format as the last
//!         type 1/8/9 block)
//! type 8  "Extended": time_constant:le16  pack_method:u8  mode:u8 (sets up
//!         the format for the type-1 block that immediately follows it —
//!         **not decoded by this module**, see below)
//! type 9  "Sound data (new format)": sample_rate:le32  bits:u8  channels:u8
//!         codec:le16  reserved:le32  pcm[length-12]
//! type 0  terminator, no length field
//! types 3-7  silence/marker/text/loop metadata — no PCM, skipped whole
//! ```
//!
//! **Measured**: `ffmpeg -f voc` (8.1) always writes one type-9 block
//! (12-byte sub-header, `codec=4` = 16-bit signed PCM) followed by as many
//! 16384-byte type-2 continuation blocks as the data needs — never type 1,
//! never type 8. So the type-8/type-1 pairing this module does not decode is
//! the *legacy* form, present in the format for backward compatibility with
//! files this crate did not write, not something the reference itself
//! produces.
//!
//! # What is not read
//!
//! Type 8 (legacy stereo/extended setup) is skipped as an opaque block
//! rather than combined with the type-1 block that follows it, so a legacy
//! VOC that relies on it for stereo will be read back as the mono type-1
//! defaults instead. Types 3 (silence), 4 (marker), 5 (text), 6/7 (loop) are
//! skipped entirely — no silence is synthesised, no loop point recorded.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, Rational, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, FormatOptions, Muxer, MuxerDesc, ParserProvider, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

use crate::pcm::{self, PcmLayout};

const SIGNATURE: &[u8; 20] = b"Creative Voice File\x1A";

const BLOCK_TERMINATOR: u8 = 0;
const BLOCK_SOUND_DATA: u8 = 1;
const BLOCK_CONTINUATION: u8 = 2;
const BLOCK_SILENCE: u8 = 3;
const BLOCK_MARKER: u8 = 4;
const BLOCK_TEXT: u8 = 5;
const BLOCK_REPEAT_START: u8 = 6;
const BLOCK_REPEAT_END: u8 = 7;
const BLOCK_EXTENDED: u8 = 8;
const BLOCK_NEW_FORMAT: u8 = 9;

/// New-format codec `4` — the only one this module resolves to a
/// [`SampleFmt`]; every other value (ADPCM variants, A-law, µ-law — all
/// present in the format's registry) is structurally skipped, not decoded.
const NEW_FORMAT_CODEC_PCM16: u16 = 4;
/// Legacy (type-1) codec `0`: 8-bit unsigned linear PCM.
const LEGACY_CODEC_PCM8: u8 = 0;

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// voc` file with no extension is `100`.
pub const VOC_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(SIGNATURE) {
        VOC_SCORE
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "voc",
    long_name: "Creative Voice",
    extensions: &["voc"],
    mime_types: &["audio/x-voc"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "voc",
    long_name: "Creative Voice",
    extensions: &["voc"],
    default_video: None,
    default_audio: Some(CodecId::Pcm),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(VocDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(VocMuxer::new(sink)?))
}

#[derive(Debug)]
pub struct VocDemuxer {
    io: IoContext,
    stream: Stream,
    budget: Budget,
    bytes_per_frame: u32,
    /// Bytes left in the block currently being read; `0` means the next
    /// call must read a fresh block header.
    block_remaining: u64,
    frames_emitted: u64,
    eof: bool,
}

impl VocDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the signature does not match, or no
    /// audio-bearing block is found before the terminator/EOF.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut sig = [0u8; 20];
        io.read_exact(&mut sig)?;
        if &sig != SIGNATURE {
            return Err(Error::InvalidData("voc: missing signature"));
        }
        let header_size = io.rl16()?;
        let _version = io.rl16()?;
        let _checksum = io.rl16()?;
        io.seek(u64::from(header_size))?;

        // Scan forward to the first audio-bearing block, establishing the
        // stream's format from it.
        let mut sample_rate = 8000u32;
        let mut channels = 1u16;
        let mut bytes_per_frame = 1u32;
        let mut format = Some(SampleFmt::U8);
        let mut codec_id = Some(CodecId::Pcm);
        let mut found = false;
        let mut remaining = 0u64;

        while let Ok(btype) = io.r8() {
            if btype == BLOCK_TERMINATOR {
                break;
            }
            let len = io.rl24()?;
            if btype == BLOCK_SOUND_DATA {
                let time_constant = io.r8()?;
                let codec = io.r8()?;
                sample_rate = sample_rate_from_time_constant(time_constant);
                channels = 1;
                (format, codec_id, bytes_per_frame) = legacy_format(codec);
                remaining = u64::from(len).saturating_sub(2);
                found = true;
                break;
            } else if btype == BLOCK_NEW_FORMAT {
                sample_rate = io.rl32()?.max(1);
                let bits = io.r8()?;
                channels = u16::from(io.r8()?.max(1));
                let codec = io.rl16()?;
                let _reserved = io.rl32()?;
                (format, codec_id) = new_format(codec, bits);
                bytes_per_frame =
                    u32::from(channels).saturating_mul(u32::from(bits.div_ceil(8).max(1)));
                remaining = u64::from(len).saturating_sub(12);
                found = true;
                break;
            }
            io.skip(u64::from(len))?;
        }

        if !found {
            return Err(Error::InvalidData("voc: no audio data block found"));
        }

        let mut stream = pcm::new_stream(Rational::new(1, i32::try_from(sample_rate).unwrap_or(1)));
        stream.params = pcm::params(
            PcmLayout::new(sample_rate, channels, bytes_per_frame),
            codec_id,
            format,
            format.map(|f| f.bits_per_sample() as u8),
            None,
        );

        Ok(Self {
            io,
            stream,
            budget: Budget::new(vaco_limits::Limits::permissive()),
            bytes_per_frame: bytes_per_frame.max(1),
            block_remaining: remaining,
            frames_emitted: 0,
            eof: false,
        })
    }

    /// Advance past non-audio blocks until one with bytes to read is
    /// current, or the terminator/EOF is reached.
    fn ensure_block(&mut self) -> Result<bool> {
        while self.block_remaining == 0 {
            let Ok(btype) = self.io.r8() else {
                return Ok(false);
            };
            if btype == BLOCK_TERMINATOR {
                return Ok(false);
            }
            let len = u64::from(self.io.rl24()?);
            match btype {
                BLOCK_CONTINUATION => self.block_remaining = len,
                BLOCK_SOUND_DATA => {
                    let _time_constant = self.io.r8()?;
                    let _codec = self.io.r8()?;
                    self.block_remaining = len.saturating_sub(2);
                }
                BLOCK_NEW_FORMAT => {
                    self.io.skip(len.min(12))?;
                    self.block_remaining = len.saturating_sub(12);
                }
                BLOCK_SILENCE | BLOCK_MARKER | BLOCK_TEXT | BLOCK_REPEAT_START
                | BLOCK_REPEAT_END | BLOCK_EXTENDED => {
                    self.io.skip(len)?;
                }
                _ => {
                    // An unrecognised block type: skip its declared payload
                    // rather than guess, per the "never trust past what
                    // exists" rule this workspace's other chunk-based
                    // parsers use — one more unknown type does not change
                    // the walk's termination.
                    self.io.skip(len)?;
                }
            }
        }
        Ok(true)
    }
}

fn sample_rate_from_time_constant(tc: u8) -> u32 {
    // The classic Sound Blaster mono formula: sample_rate = 1_000_000 /
    // (256 - time_constant). Stereo halves it in the legacy convention, but
    // that requires the type-8 extended block this module does not decode
    // (module docs), so mono is assumed here. `256 - time_constant` is
    // always in `1..=256`, but computed with `checked_div` rather than
    // trusted, so a boundary case fails safe to a plausible default instead
    // of dividing by zero.
    let denom = 256u32.saturating_sub(u32::from(tc));
    1_000_000u32.checked_div(denom).unwrap_or(8000)
}

fn legacy_format(codec: u8) -> (Option<SampleFmt>, Option<CodecId>, u32) {
    if codec == LEGACY_CODEC_PCM8 {
        (Some(SampleFmt::U8), Some(CodecId::Pcm), 1)
    } else {
        (None, None, 1)
    }
}

/// New-format codec `0`: 8-bit unsigned linear PCM (mirrors the legacy
/// type-1 codec `0`).
const NEW_FORMAT_CODEC_PCM8: u16 = 0;

/// `bits` (the header's own bit-depth field) is not consulted: the `codec`
/// value alone determines the format for the two codecs this module maps
/// (module docs), and a file whose `bits` disagrees with its `codec` is
/// malformed in a way no safe reinterpretation fixes.
fn new_format(codec: u16, bits: u8) -> (Option<SampleFmt>, Option<CodecId>) {
    let _ = bits;
    match codec {
        NEW_FORMAT_CODEC_PCM8 => (Some(SampleFmt::U8), Some(CodecId::Pcm)),
        NEW_FORMAT_CODEC_PCM16 => (Some(SampleFmt::S16), Some(CodecId::Pcm)),
        _ => (None, None),
    }
}

impl Demuxer for VocDemuxer {
    fn streams(&self) -> &[Stream] {
        std::slice::from_ref(&self.stream)
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        if !self.ensure_block()? {
            self.eof = true;
            return Err(Error::Eof);
        }
        let want = usize::try_from(self.block_remaining.min(4096))
            .unwrap_or(4096)
            .max(1);
        let mut pkt = Packet::alloc(&mut self.budget, want)?;
        let n = self.io.read_partial(pkt.payload_mut())?;
        if n == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        pkt.len = n;
        pkt.stream_index = 0;
        pkt.pts = vaco_core::Timestamp::new(i64::try_from(self.frames_emitted).unwrap_or(i64::MAX));
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        self.block_remaining = self.block_remaining.saturating_sub(n as u64);
        self.frames_emitted = self
            .frames_emitted
            .saturating_add(pcm::frames_in(n as u64, self.bytes_per_frame));
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // The block chain means a byte offset does not correspond to a fixed
        // audio offset without re-walking it; not implemented, matching the
        // "structurally present, not exhaustive" bar for this format.
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

/// Writes exactly one type-9 header followed by exactly one continuation-free
/// block (no 16 KiB chunking): the whole payload as one block if it fits the
/// 3-byte (16 MiB) length field, refusing otherwise rather than silently
/// truncating. `vaco-format-audio-simple`'s own [`VocDemuxer`] reads this
/// back correctly since it already walks an arbitrary block chain; producing
/// only the single-block form is the deliberate "do not over-engineer"
/// simplification for this format (brief §2).
#[derive(Debug)]
pub struct VocMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    buffered: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u16,
}

/// Largest payload a single VOC block's 3-byte length field can hold.
const MAX_BLOCK_LEN: usize = 0x00FF_FFFF - 12;

impl VocMuxer {
    /// # Errors
    /// Propagates transport failure from `sink`.
    pub fn new(sink: Box<dyn MediaSink>) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream: None,
            header_written: false,
            buffered: Vec::new(),
        })
    }
}

impl Muxer for VocMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("voc: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("voc: not an audio stream"))?;
        if audio.format != Some(SampleFmt::S16) {
            return Err(Error::Unsupported(
                "voc: only 16-bit signed PCM is supported for writing",
            ));
        }
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1) as u16;
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.stream.is_none() {
            return Err(Error::InvalidData("voc: no stream added"));
        }
        self.out.write(SIGNATURE)?;
        self.out.wl16(26)?; // header_size
        self.out.wl16(0x0114)?; // version 1.20, matching the value ffmpeg itself writes
        self.out.wl16(0x111F)?; // checksum: !version + 0x1234, i.e. 0xFFFF - 0x0114 + 0x1234
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("voc: packet written before the header"));
        }
        if self.buffered.len().saturating_add(packet.payload().len()) > MAX_BLOCK_LEN {
            return Err(Error::Unsupported(
                "voc: total audio exceeds one block's 16 MiB limit",
            ));
        }
        self.buffered.extend_from_slice(packet.payload());
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
            .ok_or(Error::InvalidData("voc: no stream added"))?;
        if !self.header_written {
            return Err(Error::InvalidData("voc: trailer written before the header"));
        }
        let block_len = self.buffered.len() + 12;
        self.out.w8(BLOCK_NEW_FORMAT)?;
        self.out
            .wl24(u32::try_from(block_len).unwrap_or(u32::MAX))?;
        self.out.wl32(s.sample_rate)?;
        self.out.w8(16)?; // bits
        self.out.w8(u8::try_from(s.channels).unwrap_or(1))?;
        self.out.wl16(NEW_FORMAT_CODEC_PCM16)?;
        self.out.wl32(0)?; // reserved
        self.out.write(&self.buffered)?;
        self.out.w8(BLOCK_TERMINATOR)?;
        self.out.flush()
    }
}
