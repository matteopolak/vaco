//! The AVI muxer: `hdrl`/`strl` header, `movi` chunks, `idx1`.
//!
//! # What AVI chunks do not carry
//!
//! Unlike most containers, an AVI chunk has **no timestamp field at all** —
//! see `vaco-demux-avi`'s module docs for how a reader recovers one from
//! `strh.dwSampleSize` and a running per-stream count. That means this muxer
//! does not need `packet.pts`/`packet.dts` to decide what bytes to write; it
//! only needs packets to arrive in the order the caller wants them replayed,
//! which is exactly what [`Muxer::stream_time_base`] plus the generic
//! interleave machinery already guarantee upstream of this crate.
//!
//! # What gets patched, and what does not
//!
//! `dwTotalFrames`/`dwLength` and `movi`'s own declared `LIST` size are not
//! known until every packet has been written. If the sink can seek,
//! [`AviMuxer::write_trailer`] goes back and patches them; if it cannot, they
//! are left at the placeholder values [`vaco_format_riff`]'s own chunk reader
//! already documents as legitimate — `0` for the frame counts (a real
//! decoder discovers the truth from `idx1`/EOF) and `0xFFFF_FFFF` ("length
//! unknown, read to EOF") for `movi`'s size. `idx1` itself needs no seeking
//! and is always written, since nothing about appending it depends on the
//! sink's ability to go backwards.
//!
//! `idx1`'s `dwOffset` is written **movi-relative** — the convention
//! `vaco-demux-avi` measured `ffmpeg 8.1`'s own writer using, and the one its
//! `detect_offset_base` prefers when neither candidate can be confirmed. See
//! that crate's `index` module docs for the measurement.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Rational, Result};
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

/// `AVIF_HASINDEX | AVIF_ISINTERLEAVED`, per the field this crate's sibling
/// demuxer already interprets.
const AVIH_FLAGS: u32 = 0x0000_0010 | 0x0000_0100;

/// `AVIIF_KEYFRAME`.
const AVIIF_KEYFRAME: u32 = 0x0000_0010;

/// The `movi` region's declared size when it cannot be patched afterward —
/// the same "length unknown, read to EOF" convention
/// [`vaco_format_riff::chunk`] documents readers already having to accept.
const LENGTH_UNKNOWN: u32 = 0xFFFF_FFFF;

/// The registry descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "avi",
    long_name: "AVI (Audio Video Interleaved)",
    extensions: &["avi"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Mp3),
    open: open_muxer,
};

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(AviMuxer::new(sink, &FormatOptions::default())?))
}

#[derive(Debug, Clone, Copy)]
struct StreamOut {
    is_video: bool,
    time_base: Rational,
    /// `dwSampleSize`: `0` for video and VBR audio, else the CBR block size.
    sample_size: u32,
    /// Byte offset, within the in-memory `hdrl` buffer, of this stream's
    /// `strh.dwLength` field — patched at `write_trailer` once the true
    /// count is known.
    length_field_at: usize,
    /// Running frame (or, for CBR audio, sample) count.
    count: u64,
    /// Video's `biCompression` `FourCC`.
    video_fourcc: [u8; 4],
    width: u32,
    height: u32,
    /// Audio's `wFormatTag`.
    audio_format_tag: u16,
    channels: u16,
    bits_per_sample: u16,
}

/// One `idx1` entry, with an absolute file position not yet converted to the
/// movi-relative offset the chunk itself states.
#[derive(Debug, Clone, Copy)]
struct IdxEntry {
    tag: [u8; 4],
    flags: u32,
    abs_pos: u64,
    size: u32,
}

/// The AVI muxer.
#[derive(Debug)]
pub struct AviMuxer {
    out: IoWriter,
    streams: Vec<StreamOut>,
    header_written: bool,
    trailer_written: bool,
    /// Absolute position of `avih`'s `dwTotalFrames` field, patched at
    /// `write_trailer`.
    avih_total_frames_at: u64,
    /// Absolute position of the outer `RIFF` chunk's declared-size field.
    riff_size_at: u64,
    /// Absolute position of `movi`'s own declared-size field.
    movi_size_at: u64,
    /// Absolute position of the byte at which `movi`'s four-character
    /// list-type text begins — the base `idx1` offsets are written relative
    /// to.
    movi_fourcc_pos: u64,
    idx: Vec<IdxEntry>,
}

impl AviMuxer {
    /// A muxer over `sink`.
    ///
    /// # Errors
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, _opts: &FormatOptions) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            streams: Vec::new(),
            header_written: false,
            trailer_written: false,
            avih_total_frames_at: 0,
            riff_size_at: 0,
            movi_size_at: 0,
            movi_fourcc_pos: 0,
            idx: Vec::new(),
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
    }
}

/// A stream's `movi` chunk tag, e.g. `"00dc"` for video stream 0.
fn chunk_tag(stream_index: u32, is_video: bool) -> Result<[u8; 4]> {
    if stream_index > 99 {
        return Err(Error::Unsupported("avi: more than 100 streams"));
    }
    #[allow(
        clippy::integer_division,
        reason = "decimal digit extraction for a two-digit ASCII tag, not a ratio"
    )]
    let tens = u8::try_from(stream_index / 10).unwrap_or(0);
    let ones = u8::try_from(stream_index % 10).unwrap_or(0);
    let kind: [u8; 2] = if is_video { *b"dc" } else { *b"wb" };
    Ok([b'0' + tens, b'0' + ones, kind[0], kind[1]])
}

/// A video codec's `biCompression` `FourCC`, for the codecs this crate can
/// mux. `None` for anything [`vaco_format_riff::video_tags`] would not read
/// back to the same [`CodecId`] — writing a tag this crate cannot itself
/// demux again is worse than refusing to.
fn video_fourcc(id: CodecId) -> Option<[u8; 4]> {
    match id {
        CodecId::H264 => Some(*b"H264"),
        CodecId::Hevc => Some(*b"HEVC"),
        CodecId::Vp8 => Some(*b"VP80"),
        CodecId::Vp9 => Some(*b"VP90"),
        CodecId::Jpeg => Some(*b"MJPG"),
        CodecId::Png => Some(*b"MPNG"),
        _ => None,
    }
}

/// An audio codec's `wFormatTag`, mirroring `vaco_format_riff::wave_tags` in
/// reverse.
///
/// `CodecId` used to have a single `Pcm` bucket for every uncompressed
/// width; it now carries the specific flavours `vaco-format-riff`'s own
/// `wave_tags::codec_id` produces on the read side (see that function's
/// doc comment). Both the generic bucket and the specific little-endian
/// flavours map to `WAVE_FORMAT_PCM` here, since a `WAVEFORMATEX` in a RIFF
/// file is little-endian by definition — the big-endian flavours have no
/// AVI representation and are refused, same as any other codec with no
/// mapping.
fn audio_format_tag(id: CodecId) -> Option<u16> {
    match id {
        CodecId::Pcm
        | CodecId::PcmU8
        | CodecId::PcmS16le
        | CodecId::PcmS24le
        | CodecId::PcmS32le => {
            Some(0x0001) // WAVE_FORMAT_PCM
        }
        CodecId::PcmF32le | CodecId::PcmF64le => Some(0x0003), // WAVE_FORMAT_IEEE_FLOAT
        CodecId::PcmAlaw => Some(0x0006),                      // WAVE_FORMAT_ALAW
        CodecId::PcmMulaw => Some(0x0007),                     // WAVE_FORMAT_MULAW
        CodecId::Mp3 => Some(0x0055),                          // WAVE_FORMAT_MPEGLAYER3
        CodecId::Aac => Some(0x00FF),                          // WAVE_FORMAT_AAC
        _ => None,
    }
}

/// Whether `id` is an uncompressed PCM flavour this crate can write — the
/// set `audio_format_tag` maps to something other than a compressed codec's
/// tag. Uncompressed is what makes `strh.dwSampleSize` a fixed
/// bytes-per-sample constant (CBR); a compressed codec is VBR from AVI's
/// point of view even at a constant bitrate, because one chunk does not
/// carry a fixed sample count.
fn is_uncompressed_pcm(id: CodecId) -> bool {
    matches!(
        id,
        CodecId::Pcm
            | CodecId::PcmU8
            | CodecId::PcmS16le
            | CodecId::PcmS24le
            | CodecId::PcmS32le
            | CodecId::PcmF32le
            | CodecId::PcmF64le
            | CodecId::PcmAlaw
            | CodecId::PcmMulaw
    )
}

/// The `wBitsPerSample` a specific PCM flavour implies, overriding whatever
/// the caller's `bits_per_coded_sample` says — `vaco-format-riff`'s
/// `wave_tags::codec_id` derives the flavour from `wBitsPerSample` (and a
/// fixed 8 bits for A-law/mu-law regardless of the field), so writing
/// anything else here would not read back as the same `CodecId`. `None` for
/// the generic `CodecId::Pcm` bucket, which carries no width of its own —
/// the caller's field is trusted there.
fn pcm_bits_per_sample(id: CodecId) -> Option<u16> {
    match id {
        CodecId::PcmU8 | CodecId::PcmAlaw | CodecId::PcmMulaw => Some(8),
        CodecId::PcmS16le => Some(16),
        CodecId::PcmS24le => Some(24),
        CodecId::PcmS32le | CodecId::PcmF32le => Some(32),
        CodecId::PcmF64le => Some(64),
        _ => None,
    }
}

impl Muxer for AviMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "avi: streams must be added before the header is written",
            ));
        }
        let media = params
            .effective_media_type()
            .ok_or(Error::Unsupported("avi: stream has no media type"))?;
        let is_video = match media {
            MediaType::Video => true,
            MediaType::Audio => false,
            _ => return Err(Error::Unsupported("avi: only video and audio streams")),
        };
        let codec_id = params
            .codec_id
            .ok_or(Error::Unsupported("avi: stream has no codec id"))?;

        let mut out = StreamOut {
            is_video,
            time_base: Rational::new(1, 25),
            sample_size: 0,
            length_field_at: 0,
            count: 0,
            video_fourcc: *b"    ",
            width: 0,
            height: 0,
            audio_format_tag: 0,
            channels: 1,
            bits_per_sample: 16,
        };

        if is_video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "avi: video stream has no VideoParameters",
            ))?;
            let fps = v.frame_rate;
            if fps.is_defined() && !fps.is_zero() && !fps.is_infinite() {
                out.time_base = fps.inverse();
            }
            out.video_fourcc = video_fourcc(codec_id)
                .ok_or(Error::Unsupported("avi: codec has no AVI video FourCC"))?;
            out.width = v.width;
            out.height = v.height;
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "avi: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported("avi: audio stream has no sample rate"));
            }
            out.time_base = Rational::new(1, i32::try_from(a.sample_rate).unwrap_or(i32::MAX));
            out.audio_format_tag = audio_format_tag(codec_id)
                .ok_or(Error::Unsupported("avi: codec has no AVI wFormatTag"))?;
            out.channels = u16::try_from(a.layout.as_ref().map_or(1, |l| l.channels)).unwrap_or(1);
            out.bits_per_sample = pcm_bits_per_sample(codec_id)
                .unwrap_or_else(|| u16::from(a.bits_per_coded_sample.unwrap_or(16)).max(8));
            // CBR only when the codec is uncompressed PCM; a compressed
            // codec (MP3, AAC) is VBR from AVI's point of view even at a
            // constant bitrate, because one chunk does not carry a fixed
            // sample count.
            out.sample_size = if is_uncompressed_pcm(codec_id) {
                #[allow(
                    clippy::integer_division,
                    reason = "bytes-per-sample from bits-per-sample is an exact conversion, not a ratio"
                )]
                let bytes_per_sample = (u32::from(out.bits_per_sample) / 8).max(1);
                bytes_per_sample * u32::from(out.channels)
            } else {
                0
            };
        }

        let index = u32::try_from(self.streams.len())
            .map_err(|_| Error::Unsupported("avi: too many streams"))?;
        self.streams.push(out);
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("avi: header written twice"));
        }
        if self.streams.is_empty() {
            return Err(Error::Unsupported("avi: no streams to mux"));
        }

        self.out.write_tag(b"RIFF")?;
        self.riff_size_at = self.out.pos();
        self.out.wl32(0)?; // patched in write_trailer if seekable
        self.out.write_tag(b"AVI ")?;

        // `hdrl` is fully determined by `add_stream` alone, so it is built
        // in memory first and written as one chunk — the only way to get its
        // `LIST` size right without requiring the sink to seek.
        let mut hdrl = Vec::new();
        hdrl.extend_from_slice(b"hdrl");
        let avih_total_frames_rel = write_avih(&mut hdrl, &self.streams);
        let mut length_fields = Vec::new();
        for s in &self.streams {
            let rel = write_strl(&mut hdrl, *s);
            length_fields.push(rel);
        }

        self.out.write_tag(b"LIST")?;
        self.out
            .wl32(u32::try_from(hdrl.len()).unwrap_or(u32::MAX))?;
        let hdrl_body_start = self.out.pos();
        self.out.write(&hdrl)?;
        self.avih_total_frames_at = hdrl_body_start + avih_total_frames_rel as u64;
        for (s, rel) in self.streams.iter_mut().zip(length_fields) {
            s.length_field_at = (hdrl_body_start + rel as u64) as usize;
        }

        self.out.write_tag(b"LIST")?;
        self.movi_size_at = self.out.pos();
        self.out.wl32(LENGTH_UNKNOWN)?;
        self.out.write_tag(b"movi")?;
        self.movi_fourcc_pos = self.movi_size_at + 4;

        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("avi: packet written before the header"));
        }
        let idx = usize::try_from(packet.stream_index).unwrap_or(usize::MAX);
        let is_video = self
            .streams
            .get(idx)
            .ok_or(Error::InvalidData("avi: packet names an unknown stream"))?
            .is_video;
        let tag = chunk_tag(packet.stream_index, is_video)?;

        let pos = self.out.pos();
        self.out.write_tag(&tag)?;
        let len =
            u32::try_from(packet.len).map_err(|_| Error::Unsupported("avi: packet too large"))?;
        self.out.wl32(len)?;
        self.out.write(packet.payload())?;
        if packet.len % 2 == 1 {
            self.out.w8(0)?;
        }

        let stream = self
            .streams
            .get_mut(idx)
            .ok_or(Error::InvalidData("avi: packet names an unknown stream"))?;
        stream.count = if stream.sample_size == 0 {
            stream.count.saturating_add(1)
        } else {
            #[allow(
                clippy::integer_division,
                reason = "dwSampleSize divides a byte count into an exact sample count, not a ratio"
            )]
            let samples = u64::from(len) / u64::from(stream.sample_size.max(1));
            stream.count.saturating_add(samples.max(1))
        };

        self.idx.push(IdxEntry {
            tag,
            flags: if packet.is_key() { AVIIF_KEYFRAME } else { 0 },
            abs_pos: pos,
            size: len,
        });
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<Rational> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|s| s.time_base)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("avi: trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("avi: trailer written twice"));
        }
        self.trailer_written = true;

        let movi_end = self.out.pos();

        if !self.idx.is_empty() {
            self.out.write_tag(b"idx1")?;
            self.out
                .wl32(u32::try_from(self.idx.len() * 16).unwrap_or(u32::MAX))?;
            for e in &self.idx {
                self.out.write_tag(&e.tag)?;
                self.out.wl32(e.flags)?;
                let rel = e.abs_pos.saturating_sub(self.movi_fourcc_pos);
                self.out.wl32(u32::try_from(rel).unwrap_or(u32::MAX))?;
                self.out.wl32(e.size)?;
            }
        }

        if self.out.is_seekable() {
            let end = self.out.pos();

            self.out.seek(self.movi_size_at)?;
            let movi_size = movi_end.saturating_sub(self.movi_fourcc_pos);
            self.out
                .wl32(u32::try_from(movi_size).unwrap_or(u32::MAX))?;

            self.out.seek(self.avih_total_frames_at)?;
            let total = self
                .streams
                .iter()
                .find(|s| s.is_video)
                .or_else(|| self.streams.first())
                .map_or(0, |s| s.count);
            self.out.wl32(u32::try_from(total).unwrap_or(u32::MAX))?;

            for s in &self.streams {
                self.out.seek(s.length_field_at as u64)?;
                self.out.wl32(u32::try_from(s.count).unwrap_or(u32::MAX))?;
            }

            self.out.seek(self.riff_size_at)?;
            self.out
                .wl32(u32::try_from(end.saturating_sub(8)).unwrap_or(u32::MAX))?;
            self.out.seek(end)?;
        }

        self.out.flush()
    }
}

/// Write `LIST hdrl`'s `avih` chunk (56-byte `AVIMAINHEADER`), with every
/// field the caller cannot know yet left at `0`.
///
/// Returns `dwTotalFrames`'s offset within `out`, patched once the true
/// count is known.
fn write_avih(out: &mut Vec<u8>, streams: &[StreamOut]) -> usize {
    let (width, height) = streams
        .iter()
        .find(|s| s.is_video)
        .map_or((0, 0), |s| (s.width, s.height));
    out.extend_from_slice(b"avih");
    out.extend_from_slice(&56u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // dwMicroSecPerFrame
    out.extend_from_slice(&0u32.to_le_bytes()); // dwMaxBytesPerSec
    out.extend_from_slice(&0u32.to_le_bytes()); // dwPaddingGranularity
    out.extend_from_slice(&AVIH_FLAGS.to_le_bytes());
    let total_frames_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes()); // dwTotalFrames (patched)
    out.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    out.extend_from_slice(&(streams.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // dwSuggestedBufferSize
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // dwReserved[4]
    total_frames_at
}

/// Write one `LIST strl` (`strh` + `strf`), returning the byte offset, within
/// `out`, of the `dwLength` field written for this stream — patched once the
/// true count is known.
fn write_strl(out: &mut Vec<u8>, s: StreamOut) -> usize {
    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");

    let mut strh = Vec::new();
    strh.extend_from_slice(if s.is_video { b"vids" } else { b"auds" });
    // `fccHandler`: mirrors `biCompression` for video, matching what
    // `ffmpeg 8.1`'s own writer does (measured: both carry `FMP4` for an
    // mpeg4 stream); left zero for audio, which this crate's own demuxer
    // does not read from here regardless.
    strh.extend_from_slice(if s.is_video {
        &s.video_fourcc
    } else {
        &[0u8; 4]
    });
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwFlags
    strh.extend_from_slice(&0u16.to_le_bytes()); // wPriority
    strh.extend_from_slice(&0u16.to_le_bytes()); // wLanguage
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    strh.extend_from_slice(&s.time_base.num.to_le_bytes()); // dwScale
    strh.extend_from_slice(&s.time_base.den.to_le_bytes()); // dwRate
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwStart
    let length_rel_in_strh = strh.len();
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwLength (patched)
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwSuggestedBufferSize
    strh.extend_from_slice(&(-1i32).to_le_bytes()); // dwQuality: -1 = unspecified
    strh.extend_from_slice(&s.sample_size.to_le_bytes());
    strh.extend_from_slice(&[0u8; 8]); // rcFrame

    strl.extend_from_slice(b"strh");
    strl.extend_from_slice(&(strh.len() as u32).to_le_bytes());
    let strh_body_start = strl.len();
    strl.extend_from_slice(&strh);

    // A `BITMAPINFOHEADER`/`WAVEFORMATEX`, exactly what
    // `vaco-demux-avi`/`vaco-format-riff` read back on the other side.
    if s.is_video {
        let mut strf = Vec::new();
        strf.extend_from_slice(&40u32.to_le_bytes()); // biSize
        strf.extend_from_slice(&s.width.cast_signed().to_le_bytes()); // biWidth
        strf.extend_from_slice(&s.height.cast_signed().to_le_bytes()); // biHeight
        strf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        strf.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        strf.extend_from_slice(&s.video_fourcc);
        strf.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        strf.extend_from_slice(&0i32.to_le_bytes());
        strf.extend_from_slice(&0i32.to_le_bytes());
        strf.extend_from_slice(&0u32.to_le_bytes());
        strf.extend_from_slice(&0u32.to_le_bytes());
        strl.extend_from_slice(b"strf");
        strl.extend_from_slice(&(strf.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strf);
    } else {
        let mut strf = Vec::new();
        strf.extend_from_slice(&s.audio_format_tag.to_le_bytes());
        strf.extend_from_slice(&s.channels.to_le_bytes());
        let rate = s.time_base.den.max(1).unsigned_abs();
        strf.extend_from_slice(&rate.to_le_bytes());
        let block_align = if s.sample_size > 0 {
            s.sample_size
        } else {
            #[allow(
                clippy::integer_division,
                reason = "bytes-per-sample from bits-per-sample is an exact conversion, not a ratio"
            )]
            let bytes_per_sample = (u32::from(s.bits_per_sample) / 8).max(1);
            bytes_per_sample * u32::from(s.channels)
        };
        strf.extend_from_slice(&(rate * block_align).to_le_bytes()); // nAvgBytesPerSec
        strf.extend_from_slice(&u16::try_from(block_align).unwrap_or(u16::MAX).to_le_bytes());
        strf.extend_from_slice(&s.bits_per_sample.to_le_bytes());
        strl.extend_from_slice(b"strf");
        strl.extend_from_slice(&(strf.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strf);
    }

    // `out.len()` is where this `strl`'s own `LIST` tag will land; 8 bytes
    // for that `LIST`'s tag+size, then `strh_body_start` (already the offset
    // of `strh`'s body *within* `strl`, i.e. past `strl`'s own `"strl"`
    // marker and `strh`'s tag+size) plus the field's offset within `strh`.
    let length_field_at = out.len() + 8 + strh_body_start + length_rel_in_strh;
    out.extend_from_slice(b"LIST");
    out.extend_from_slice(&(strl.len() as u32).to_le_bytes());
    out.extend_from_slice(&strl);
    length_field_at
}
