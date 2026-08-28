//! AIFF and AIFF-C (`AIFC`).
//!
//! Apple *Audio Interchange File Format* v1.3, and *AIFF-C* (the
//! compression-carrying extension of the same spec). Big-endian throughout,
//! including the audio data itself — the one thing every one of this
//! crate's other formats except CAF and AU gets to skip.
//!
//! # Layout
//!
//! `FORM` + be32 size + form type (`AIFF` or `AIFC`), then IFF chunks (big-
//! endian size, one pad byte when odd — the same grammar RIFF uses, just
//! with the size field big-endian instead of little):
//!
//! ```text
//! COMM (AIFF, 18 bytes):
//!   numChannels:be16  numSampleFrames:be32  sampleSize:be16
//!   sampleRate:extended80 (10 bytes, see crate::extended80)
//!
//! COMM (AIFC, >= 22 bytes): the above, plus
//!   compressionType:FourCC  compressionName:pstring (1-byte length prefix)
//!
//! SSND:
//!   offset:be32  blockSize:be32  soundData[...]
//! ```
//!
//! # Measured: plain `AIFF` means big-endian signed integer PCM, full stop
//!
//! `ffmpeg -c:a pcm_s24be -f aiff` writes form type `AIFF` (not `AIFC`) with
//! an 18-byte `COMM` — no `compressionType` at all — while every codec that
//! is not big-endian signed integer PCM (`pcm_s16le` → `sowt`, `pcm_f32be` →
//! `fl32`, `pcm_f64be` → `fl64`, `pcm_u8` → `raw `, `pcm_alaw`/`pcm_mulaw` →
//! `alaw`/`ulaw`) gets form type `AIFC` with the compression type naming it.
//! So `COMM`'s length alone (18 vs. longer) tells a reader which case it is
//! in without needing to inspect the form type text, and this module uses
//! exactly that.
//!
//! # What is not read
//!
//! `MARK`/`INST` (cue points and instrument loop data) and a leading/trailing
//! `ID3 ` chunk are not parsed — deferred, per the brief's "structurally
//! present but untested" allowance. `ANNO`/`COMT`/`NAME`/`AUTH`/`(c) `
//! text chunks likewise.

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

use crate::extended80;
use crate::pcm::{self, PcmLayout, RawPcmDemuxer};

const FORM: [u8; 4] = *b"FORM";
const AIFF: [u8; 4] = *b"AIFF";
const AIFC: [u8; 4] = *b"AIFC";
const COMM: [u8; 4] = *b"COMM";
const SSND: [u8; 4] = *b"SSND";

/// **Measured**: `ffprobe` 8.1's `format.probe_score` on a plain `ffmpeg -f
/// aiff` file with no extension is `100`.
pub const AIFF_SCORE: ProbeScore = ProbeScore::MAX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) != Some(FORM) {
        return ProbeScore::NONE;
    }
    match data.tag(8) {
        Some(f) if f == AIFF || f == AIFC => AIFF_SCORE,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "aiff",
    long_name: "Audio IFF",
    extensions: &["aif", "aiff", "afc", "aifc"],
    mime_types: &["audio/aiff", "audio/x-aiff"],
    flags: FormatFlags::GENERIC_INDEX,
    probe,
    open: open_demuxer,
};

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "aiff",
    long_name: "Audio IFF",
    extensions: &["aif", "aiff"],
    // Measured: `ffmpeg -h muxer=aiff` -> `Default video codec: png.`
    // AIFF carries cover art, which is why an audio container has one.
    default_video: Some(CodecId::Png),
    // `ffmpeg -h muxer=aiff` says "Default audio codec: pcm_s16be." The
    // generic `Pcm` that was here is not a codec the reference ever names.
    default_audio: Some(CodecId::PcmS16be),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(AiffDemuxer::open(src, &FormatOptions::default())?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(AiffMuxer::new(sink)?))
}

/// The AIFF-C compression types this module has a mapping for.
fn compression_to_format(
    tag: [u8; 4],
    sample_size: u8,
) -> (
    Option<CodecId>,
    Option<vaco_sampfmt::SampleFmt>,
    Option<u8>,
    Option<u8>,
) {
    match &tag {
        // `sowt` is `twos` spelled backwards, and that is exactly what it
        // means: little-endian. Lumping the two together was wrong, and
        // invisible while every branch returned the generic `CodecId::Pcm`.
        //
        //   ffmpeg -c:a pcm_s16le out.aiff
        //   ffprobe -show_entries stream=codec_name,codec_tag_string out.aiff
        //   # pcm_s16le,sowt
        //
        // A plain (uncompressed) AIFF has no compression tag at all — the
        // reference prints `codec_tag_string=[0][0][0][0]` — and is
        // big-endian, which is what `NONE`/`twos` stand for here.
        b"NONE" | b"twos" | b"TWOS" => {
            let (fmt, raw) = pcm::sample_fmt_for(sample_size, false);
            let id = pcm::codec_id_for(sample_size, false, true, true);
            (Some(id), fmt, Some(sample_size), raw)
        }
        b"sowt" | b"SOWT" => {
            let (fmt, raw) = pcm::sample_fmt_for(sample_size, false);
            let id = pcm::codec_id_for(sample_size, false, false, true);
            (Some(id), fmt, Some(sample_size), raw)
        }
        // `fl32`/`fl64` are big-endian floats: `ffmpeg -c:a pcm_f32be` writes
        // `fl32`, and there is no little-endian spelling in AIFF-C.
        b"fl32" | b"FL32" | b"fl64" | b"FL64" => {
            let (fmt, raw) = pcm::sample_fmt_for(sample_size, true);
            let id = pcm::codec_id_for(sample_size, true, true, true);
            (Some(id), fmt, Some(sample_size), raw)
        }
        b"raw " | b"RAW " => (
            Some(CodecId::PcmU8),
            Some(vaco_sampfmt::SampleFmt::U8),
            Some(sample_size),
            None,
        ),
        // Both decode to `s16` while being neither signed nor 16-bit — the
        // sample format is the *decoded* one, measured, not the coded one.
        b"alaw" | b"ALAW" => (
            Some(CodecId::PcmAlaw),
            Some(vaco_sampfmt::SampleFmt::S16),
            Some(sample_size),
            None,
        ),
        b"ulaw" | b"ULAW" => (
            Some(CodecId::PcmMulaw),
            Some(vaco_sampfmt::SampleFmt::S16),
            Some(sample_size),
            None,
        ),
        _ => (None, None, Some(0), None),
    }
}

#[derive(Debug)]
pub struct AiffDemuxer {
    inner: RawPcmDemuxer,
    budget: Budget,
}

impl AiffDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the `FORM`/`AIFF`/`AIFC` signature or the
    /// `COMM`/`SSND` chunks do not parse.
    pub fn open(src: Box<dyn MediaSource>, _opts: &FormatOptions) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.tag()? != FORM {
            return Err(Error::InvalidData("aiff: missing FORM signature"));
        }
        let _size = io.rb32()?;
        let form = io.tag()?;
        if form != AIFF && form != AIFC {
            return Err(Error::InvalidData("aiff: form type is not AIFF/AIFC"));
        }

        let mut budget = Budget::new(Limits::permissive());
        let mut channels = 0u16;
        let mut sample_size = 0u16;
        let mut sample_rate = 0u32;
        let mut compression = *b"NONE";
        let mut have_comm = false;
        let mut data_start = 0u64;
        let mut data_declared: Option<u64> = None;

        while let Ok(id) = io.tag() {
            let size = io.rb32()?;
            if id == COMM {
                let take = usize::try_from(size).unwrap_or(0).min(4096);
                let mut buf = budget.alloc::<u8>(take)?;
                io.read_exact(&mut buf)?;
                if size % 2 == 1 {
                    io.skip(1)?;
                }
                let mut r = vaco_bitstream::ByteReader::new(&buf);
                channels = r.be16();
                let _frames = r.be32();
                sample_size = r.be16();
                let rate_bytes = r.bytes(10);
                sample_rate = extended80::to_f64(rate_bytes).round() as u32;
                if buf.len() >= 22 {
                    let ct = r.bytes(4);
                    compression = <[u8; 4]>::try_from(ct).unwrap_or(*b"NONE");
                }
                have_comm = true;
            } else if id == SSND {
                let offset = io.rb32()?;
                let _block_size = io.rb32()?;
                data_start = io.pos().saturating_add(u64::from(offset));
                io.skip(u64::from(offset))?;
                let payload = u64::from(size).saturating_sub(8);
                data_declared = Some(payload);
                break;
            } else {
                io.skip(u64::from(size).saturating_add(u64::from(size % 2)))?;
            }
        }

        if !have_comm {
            return Err(Error::InvalidData("aiff: no COMM chunk"));
        }
        let Some(declared_len) = data_declared else {
            return Err(Error::InvalidData("aiff: no SSND chunk"));
        };

        let sample_size_u8 = u8::try_from(sample_size.min(255)).unwrap_or(255);
        let (codec_id, format, bits_coded, bits_raw) = if form == AIFF {
            // Plain AIFF (form type `AIFF`, no compression field at all) is
            // signed big-endian at whatever width `COMM` states — the same
            // family `AIFC`'s `NONE`/`twos` names, reached by a different
            // door. This branch is why `pcm_s16be`, `pcm_s24be` and `pcm_s8`
            // still probed as the generic `pcm` after the compression table
            // was fixed: the common case never consults that table.
            let (fmt, raw) = pcm::sample_fmt_for(sample_size_u8, false);
            let id = pcm::codec_id_for(sample_size_u8, false, true, true);
            (Some(id), fmt, Some(sample_size_u8), raw)
        } else {
            compression_to_format(compression, sample_size_u8)
        };
        let bytes_per_sample = u32::from(sample_size_u8.div_ceil(8).max(1));
        let bytes_per_frame = u32::from(channels.max(1)) * bytes_per_sample;

        let mut params: CodecParameters = pcm::params(
            PcmLayout::new(sample_rate.max(1), channels, bytes_per_frame),
            codec_id,
            format,
            bits_coded,
            bits_raw,
        );
        params.codec_tag = Some(compression);

        let mut stream = pcm::new_stream(Rational::new(1, sample_rate.max(1).cast_signed()));
        stream.params = params;

        let inner = RawPcmDemuxer::new(
            io,
            stream,
            data_start,
            Some(declared_len),
            bytes_per_frame.max(1),
        );
        Ok(Self {
            inner,
            budget: Budget::new(Limits::permissive()),
        })
    }
}

impl Demuxer for AiffDemuxer {
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

/// Writes plain big-endian signed integer PCM in a plain `AIFF` form (16-bit
/// and 8-bit unsigned via `AIFC`/`raw `). Float and little-endian PCM
/// (`fl32`/`fl64`/`sowt`) are not yet emitted — see
/// `docs/format/vaco-format-audio-simple.md`.
#[derive(Debug)]
pub struct AiffMuxer {
    out: IoWriter,
    stream: Option<MuxStream>,
    header_written: bool,
    data_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct MuxStream {
    sample_rate: u32,
    channels: u16,
    sample_size: u16,
    /// The AIFF-C `compressionType`, or `None` for a plain `AIFF`.
    ///
    /// Measured across twelve `-c:a` values. Plain `AIFF` with an 18-byte
    /// `COMM` is written **only** for big-endian signed integer PCM —
    /// `pcm_s8`, `pcm_s16be`, `pcm_s24be`, `pcm_s32be`. Everything else gets
    /// `AIFC`, an `FVER` chunk and a 24-byte `COMM`:
    ///
    /// ```text
    /// pcm_u8 -> raw     pcm_s16le -> sowt   pcm_alaw -> alaw
    /// pcm_mulaw -> ulaw pcm_f32be -> fl32   pcm_f64be -> fl64
    /// ```
    ///
    /// `pcm_s24le` and `pcm_s32le` the reference refuses outright, and so do
    /// we: `sowt` is defined for 16-bit only.
    compression: Option<[u8; 4]>,
    bytes_per_frame: u32,
}

/// The `FVER` chunk's `timestamp`: AIFF-C version 1, 1991-05-23.
const AIFC_VERSION_1: u32 = 0xA280_5140;

impl AiffMuxer {
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

/// `COMM`'s payload length: 18 for a plain `AIFF`, 24 for `AIFF-C` — the
/// extra four bytes of `compressionType` and a two-byte empty
/// `compressionName`. `COMM`'s length alone is what tells a reader which case
/// it is looking at, which is why it is derived here rather than written twice.
const fn comm_size(compression: Option<[u8; 4]>) -> u32 {
    if compression.is_some() { 24 } else { 18 }
}

impl Muxer for AiffMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream.is_some() {
            return Err(Error::Unsupported("aiff: only one stream is supported"));
        }
        let audio = params
            .audio
            .as_ref()
            .ok_or(Error::InvalidData("aiff: not an audio stream"))?;
        let format = audio
            .format
            .ok_or(Error::Unsupported("aiff: sample format must be known"))?;
        if format.is_planar() {
            return Err(Error::Unsupported(
                "aiff: planar sample formats are not supported",
            ));
        }
        // The compression type comes from the *codec*, not from the decoded
        // sample format, and that is the bug this replaced. `pcm_s16le` is
        // neither float nor planar, so it passed the old guard, and its
        // little-endian bytes were written verbatim under a plain `AIFF`
        // header — which is big-endian by definition. The reference read our
        // own output back as `pcm_s16be`: every sample byte-swapped, silently
        // (CONFORMANCE-FINDINGS 43).
        let codec = params
            .codec_id
            .ok_or(Error::Unsupported("aiff: the codec must be known"))?;
        let compression = match codec {
            CodecId::PcmS8 | CodecId::PcmS16be | CodecId::PcmS24be | CodecId::PcmS32be => None,
            CodecId::PcmU8 => Some(*b"raw "),
            CodecId::PcmS16le => Some(*b"sowt"),
            CodecId::PcmAlaw => Some(*b"alaw"),
            CodecId::PcmMulaw => Some(*b"ulaw"),
            CodecId::PcmF32be => Some(*b"fl32"),
            CodecId::PcmF64be => Some(*b"fl64"),
            _ => {
                return Err(Error::Unsupported(
                    "aiff: this codec has no AIFF or AIFF-C mapping",
                ));
            }
        };
        let coded_bits = pcm::coded_bits(codec)
            .ok_or(Error::Unsupported("aiff: only PCM-shaped codecs are supported"))?;
        let channels = audio.layout.as_ref().map_or(1, |l| l.channels).max(1) as u16;
        self.stream = Some(MuxStream {
            sample_rate: audio.sample_rate.max(1),
            channels,
            sample_size: u16::from(coded_bits),
            compression,
            bytes_per_frame: u32::from(channels)
                .saturating_mul(u32::from(coded_bits.div_ceil(8))),
        });
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        let s = self
            .stream
            .ok_or(Error::InvalidData("aiff: no stream added"))?;
        self.out.write(&FORM)?;
        self.out.wb32(0)?; // patched in write_trailer
        self.out
            .write(if s.compression.is_some() { &AIFC } else { &AIFF })?;

        if s.compression.is_some() {
            self.out.write(b"FVER")?;
            self.out.wb32(4)?;
            self.out.wb32(AIFC_VERSION_1)?;
        }

        self.out.write(&COMM)?;
        self.out.wb32(comm_size(s.compression))?;
        self.out.wb16(s.channels)?;
        self.out.wb32(0)?; // numSampleFrames, patched in write_trailer
        self.out.wb16(s.sample_size)?;
        self.out
            .write(&extended80::from_f64(f64::from(s.sample_rate)))?;
        if let Some(tag) = s.compression {
            self.out.write(&tag)?;
            // `compressionName`, a pstring: length 0, then one pad byte to an
            // even total. Measured — the reference writes `00 00`, not a
            // spelled-out name.
            self.out.wb16(0)?;
        }

        self.out.write(&SSND)?;
        self.out.wb32(0)?; // patched in write_trailer
        self.out.wb32(0)?; // offset
        self.out.wb32(0)?; // blockSize
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("aiff: packet written before the header"));
        }
        self.out.write(packet.payload())?;
        // Bytes, then one division at the end — not a per-packet frame count.
        // `frames_in` floors, so counting per packet loses up to
        // `bytes_per_frame - 1` bytes *every packet* whenever a packet is not
        // a whole number of frames. A 24-bit stream lost nine bytes across the
        // file and declared an `SSND` three frames short of what it had
        // actually written (CONFORMANCE-FINDINGS 43).
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
            return Err(Error::InvalidData(
                "aiff: trailer written before the header",
            ));
        }
        let Some(s) = self.stream else {
            return Err(Error::InvalidData("aiff: no stream added"));
        };
        if !self.out.is_seekable() {
            return self.out.flush();
        }
        let data_bytes = self.data_bytes;
        let frames = pcm::frames_in(data_bytes, s.bytes_per_frame.max(1));
        let end = self.out.pos();

        // FORM size: everything after the FORM id+size fields. AIFF-C adds an
        // `FVER` chunk (8 + 4) and six bytes of `COMM`.
        let fver = if s.compression.is_some() { 8 + 4 } else { 0 };
        let comm = u64::from(comm_size(s.compression));
        let form_size = 4 + fver + (8 + comm) + (8 + 8 + data_bytes);
        self.out.seek(4)?;
        self.out
            .wb32(u32::try_from(form_size).unwrap_or(u32::MAX))?;

        // COMM.numSampleFrames: FORM header(12) + FVER, when present +
        // COMM tag+size(8) + channels(2) lands right at it.
        self.out.seek(12 + fver + (4 + 4) + 2)?;
        self.out
            .wb32(u32::try_from(frames).unwrap_or(u32::MAX))?;

        // SSND size (8 header fields + data): FORM header(12) + FVER when
        // present + COMM tag+size+payload + SSND tag(4) lands right at SSND's
        // own size field. Both offsets move with the form type, which is the
        // trap: inserting a chunk without updating them patches the *wrong*
        // field with a plausible-looking length.
        let ssnd_size_pos = 12 + fver + (4 + 4 + comm) + 4;
        self.out.seek(ssnd_size_pos)?;
        self.out
            .wb32(u32::try_from(8 + data_bytes).unwrap_or(u32::MAX))?;

        self.out.seek(end)?;
        self.out.flush()
    }
}
