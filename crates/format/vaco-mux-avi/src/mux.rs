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
use vaco_format_core::mux::BitstreamAction;
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc, StreamSpec};
use vaco_format_nalu::{LengthSize, convert::length_prefixed_to_annexb};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The fixed grid every video stream is placed on. AVI carries no
/// per-packet timestamp — a frame's presentation time is its ordinal — so
/// time is expressed by *position*: every stream advances 600 slots per
/// second, a real frame occupies the slot its timestamp rounds to, and
/// every other slot gets a zero-length placeholder chunk. This is constant
/// across source frame rates, not derived from the stream's own rate.
const GRID_RATE: Rational = Rational::new(1, 600);

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
    // Measured: `ffmpeg -h muxer=avi` -> mpeg4 / mp3, not h264.
    default_video: Some(CodecId::Mpeg4),
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
    /// Running frame (or, for CBR audio, sample) count — for a video stream
    /// on the 600 Hz grid ([`AviMuxer::write_packet`]), this instead tracks
    /// the next unfilled slot, which **is** the correct `dwLength`/
    /// `dwTotalFrames` value once every packet has been written, so no
    /// separate field is needed to carry both meanings.
    count: u64,
    /// Video's `biCompression` `FourCC`.
    video_fourcc: [u8; 4],
    width: u32,
    height: u32,
    /// Dimensions before display cropping — `vprp`'s per-field
    /// `CompressedBM{Width,Height}`. Equal to `width`/`height` when the
    /// codec does not distinguish the two.
    coded_width: u32,
    coded_height: u32,
    /// `vprp`'s `dwFrameAspectRatio`. `Rational::new(1, 1)` (square pixels)
    /// when the source declared none.
    sample_aspect_ratio: Rational,
    /// The time base packets for this stream arrive in at
    /// [`Muxer::add_stream_with`], when the caller supplied a better answer
    /// than [`CodecParameters`] alone implies — typically the input's own
    /// track time base for a stream-copy output. Used only for
    /// `avih.dwMicroSecPerFrame`, which tracks the *source* time base, not
    /// [`GRID_RATE`], and so is internally inconsistent with `strh`.
    source_time_base: Option<Rational>,
    /// The first video packet's own timestamp, in [`GRID_RATE`] ticks, once
    /// one has been seen. AVI has no absolute-time field anywhere, so a
    /// source whose clock does not start at zero (routine for MPEG-TS) must
    /// be rebased against its own first frame, or every slot number carries
    /// however far the source's clock had already run before this stream
    /// started.
    grid_origin: Option<i64>,
    /// Audio's `wFormatTag`.
    audio_format_tag: u16,
    channels: u16,
    bits_per_sample: u16,
    /// `Some` when this video stream needs converting from length-prefixed
    /// (MP4/`avcC`/`hvcC`-style) framing to Annex B before it goes into a
    /// `movi` chunk — see [`AviMuxer::maybe_convert`]. `None` for anything
    /// that either is not H.264/HEVC or already declared Annex-B framing
    /// (`nal_length_size` unset or `0`, `vaco_codec_core::VideoParameters`'s
    /// own convention for "already start-code framed").
    length_size: Option<LengthSize>,
    /// Set the first time [`AviMuxer::check_bitstream`] answers `Insert` for
    /// this stream, so the second ask in the same chain-building loop
    /// answers `Keep` instead of the same name again — a muxer that keeps
    /// answering `Insert` is a loop, per
    /// [`vaco_format_core::mux::MuxWriter`]'s own "the duplicate-name check
    /// ... stops that from looping" doc, and this is the state that check
    /// needs a muxer to carry.
    bsf_decided: bool,
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
    /// Bookkeeping for [`maybe_convert`](Self::maybe_convert)'s
    /// length-prefixed-to-Annex-B rewrite — permissive because a conversion
    /// this crate itself drives is not attacker-controlled the way a
    /// demuxer's input is; it only bounds runaway output on a malformed
    /// length prefix.
    convert_budget: Budget,
    /// Bounds the 600 Hz grid's empty-slot backfill in
    /// [`AviMuxer::write_packet`]. Unlike `convert_budget`, the loop this
    /// guards is attacker-controlled the ordinary way: the gap between two
    /// video packets' timestamps comes straight from the input container,
    /// and with no cap a single crafted timestamp jump would try to write
    /// and index billions of empty chunks. `Limits::permissive`'s fuel and
    /// byte cap reject that while allowing any real recording.
    grid_budget: Budget,
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
            convert_budget: Budget::new(Limits::permissive()),
            grid_budget: Budget::new(Limits::permissive()),
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
    }

    /// Rewrite `payload` to Annex B if the stream at `index` declared
    /// length-prefixed framing at [`Muxer::add_stream`] time.
    ///
    /// AVI has no out-of-band configuration record the way MP4's `avcC`/
    /// `hvcC` do, so an H.264/HEVC stream sourced from a length-prefixed
    /// container (typically MP4, via `-c copy`) must be reframed with start
    /// codes before it can go into a `movi` chunk — otherwise the "length"
    /// this crate would write is a byte count for a NAL unit stream no AVI
    /// reader's convention matches (the two aren't different lengths of the
    /// same bytes; the bytes are structured differently). Mirrors
    /// `vaco-mux-mpegts::MpegTsMuxer::maybe_convert`, which solves the exact
    /// same problem for the exact same codecs.
    ///
    /// # Why this is not the whole story any more
    ///
    /// This is pure framing — it does not splice SPS/PPS in front of a
    /// keyframe the way `vaco-bsf-h2645::h264_mp4toannexb` does, because it
    /// cannot: AVI's own `strf` carries no configuration record for this
    /// crate to read parameter sets out of, and this method never sees more
    /// than one packet at a time. A caller driving this muxer through
    /// [`vaco_format_core::mux::MuxWriter`] with a real
    /// [`vaco_format_core::mux::BsfProvider`] gets the correct, spliced
    /// conversion from [`AviMuxer::check_bitstream`]'s M6 request instead,
    /// and arrives here already in Annex B — the guard below is what stops
    /// that already-converted payload from being reframed a second time as
    /// if it were still length-prefixed. A caller driving [`Muxer`] directly
    /// (every existing test, and any caller with no filter chain at all)
    /// still gets exactly this method's old, unspliced behaviour, which is
    /// wrong in the same way it always was but not a regression from it.
    fn maybe_convert(&mut self, index: usize, payload: &[u8]) -> Result<Vec<u8>> {
        let Some(stream) = self.streams.get(index) else {
            return Ok(payload.to_vec());
        };
        let Some(length_size) = stream.length_size else {
            return Ok(payload.to_vec());
        };
        if starts_with_annexb_start_code(payload) {
            return Ok(payload.to_vec());
        }
        let mut out = Vec::new();
        length_prefixed_to_annexb(payload, length_size, &mut out, &mut self.convert_budget)?;
        Ok(out)
    }

    /// Writes a zero-length `00dc`-style chunk for every slot on the video
    /// grid strictly between `self.streams[index]`'s last-used slot and this
    /// packet's own. Leaves `self.streams[index].count` at this packet's
    /// target slot; the caller advances past it once the real chunk is
    /// written.
    ///
    /// # Errors
    /// [`vaco_limits::LimitError`] (surfaced as [`Error`]) if the gap between
    /// slots is implausibly large — see `grid_budget`'s doc comment.
    fn backfill_grid_slots(&mut self, index: usize, tag: [u8; 4], packet: &Packet) -> Result<()> {
        let Some(stream) = self.streams.get(index) else {
            return Ok(());
        };
        let next_slot = stream.count;
        // `dts` is preferred over `pts` for the slot, since AVI has no field
        // at all for reordering: writing chunks in anything but
        // non-decreasing `dts` order would make some frame's slot precede
        // one already on disk. Falling back to `pts`, then to "the next
        // slot in line", keeps this total even on a packet with neither.
        let raw_ticks = packet.dts.ticks().or_else(|| packet.pts.ticks());
        // Rebase against the first frame's own timestamp (see
        // `grid_origin`'s doc comment) so a source clock that does not
        // start at zero does not push every slot number out by however far
        // it had already run.
        let origin = match (stream.grid_origin, raw_ticks) {
            (Some(o), _) => o,
            (None, Some(t)) => t,
            (None, None) => 0,
        };
        if stream.grid_origin.is_none()
            && let Some(t) = raw_ticks
            && let Some(s) = self.streams.get_mut(index)
        {
            s.grid_origin = Some(t);
        }
        let slot = raw_ticks
            .map(|t| t.saturating_sub(origin))
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(next_slot)
            .max(next_slot);
        let gap = slot - next_slot;
        if gap > 0 {
            let entry_bytes = u64::try_from(core::mem::size_of::<IdxEntry>()).unwrap_or(u64::MAX);
            self.grid_budget.consume_fuel(gap)?;
            let idx_bytes = gap
                .checked_mul(entry_bytes)
                .ok_or(Error::Unsupported("avi: video timestamp gap too large"))?;
            self.grid_budget.charge(idx_bytes)?;
        }
        for _ in 0..gap {
            let pos = self.out.pos();
            self.out.write_tag(&tag)?;
            self.out.wl32(0)?;
            self.idx.push(IdxEntry {
                tag,
                flags: 0,
                abs_pos: pos,
                size: 0,
            });
        }
        if let Some(stream) = self.streams.get_mut(index) {
            stream.count = slot;
        }
        Ok(())
    }
}

/// Whether `payload` already opens with an Annex B start code (`00 00 01` or
/// `00 00 00 01`).
///
/// A length-prefixed sample's first four bytes are a big-endian byte count,
/// which coincides with this only for a NAL exactly one byte long (`00 00 00
/// 01`) — a unit too short to carry a NAL header, so not a real ambiguity in
/// practice. Used to make [`AviMuxer::maybe_convert`] a no-op on a payload
/// [`AviMuxer::check_bitstream`]'s filter chain has already reframed.
fn starts_with_annexb_start_code(payload: &[u8]) -> bool {
    payload.starts_with(&[0, 0, 1]) || payload.starts_with(&[0, 0, 0, 1])
}

/// Whether `id` is a codec whose length-prefixed framing needs converting to
/// Annex B for AVI — the same two codecs `vaco-mux-mpegts` converts, and for
/// the same reason: neither has an AVI (or MPEG-TS) out-of-band
/// configuration record, so in-band start-code-framed NAL units with inline
/// parameter sets are the only layout either container's readers expect.
fn is_h264_or_hevc(id: CodecId) -> bool {
    matches!(id, CodecId::H264 | CodecId::Hevc)
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
            coded_width: 0,
            coded_height: 0,
            sample_aspect_ratio: Rational::new(1, 1),
            source_time_base: None,
            grid_origin: None,
            audio_format_tag: 0,
            channels: 1,
            bits_per_sample: 16,
            length_size: None,
            bsf_decided: false,
        };

        if is_video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "avi: video stream has no VideoParameters",
            ))?;
            // Every video stream sits on the fixed 600 Hz grid regardless of
            // its own frame rate — not derived from `v.frame_rate`.
            out.time_base = GRID_RATE;
            out.video_fourcc = video_fourcc(codec_id)
                .ok_or(Error::Unsupported("avi: codec has no AVI video FourCC"))?;
            out.width = v.width;
            out.height = v.height;
            out.coded_width = if v.coded_width > 0 {
                v.coded_width
            } else {
                v.width
            };
            out.coded_height = if v.coded_height > 0 {
                v.coded_height
            } else {
                v.height
            };
            if v.sample_aspect_ratio.is_defined()
                && !v.sample_aspect_ratio.is_zero()
                && !v.sample_aspect_ratio.is_infinite()
            {
                out.sample_aspect_ratio = v.sample_aspect_ratio;
            }
            if is_h264_or_hevc(codec_id) {
                out.length_size = v
                    .nal_length_size
                    .filter(|&n| n > 0)
                    .and_then(LengthSize::new);
            }
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "avi: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported("avi: audio stream has no sample rate"));
            }
            // AVI's `WAVE_FORMAT_AAC` entry expects raw, `AudioSpecificConfig`-
            // framed AAC (the config carried once, out of band), not ADTS's
            // self-contained per-frame header. A stream with no extradata at
            // all is the observable signal this crate has for "this is
            // ADTS-framed, not raw" (MP4/`esds`-sourced AAC always has one),
            // so it is refused here rather than written as a
            // technically-malformed `WAVE_FORMAT_AAC` chunk stream.
            if codec_id == CodecId::Aac && params.extradata.as_deref().is_none_or(<[u8]>::is_empty)
            {
                return Err(Error::Unsupported(
                    "avi: ADTS-framed AAC has no AVI representation; needs raw AudioSpecificConfig extradata",
                ));
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

    /// [`Muxer::add_stream`], plus capturing `spec.time_base`.
    /// `avih.dwMicroSecPerFrame` needs the *source* time base, which
    /// [`GRID_RATE`] deliberately is not, so this is the only place that
    /// value is still available — `write_packet` only ever sees packets
    /// already rescaled into [`GRID_RATE`].
    fn add_stream_with(&mut self, params: &CodecParameters, spec: &StreamSpec) -> Result<u32> {
        let index = self.add_stream(params)?;
        if let Some(tb) = spec.time_base
            && let Ok(i) = usize::try_from(index)
            && let Some(s) = self.streams.get_mut(i)
        {
            s.source_time_base = Some(tb);
        }
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

        // A video stream sits on the fixed 600 Hz grid, so this packet may
        // need empty placeholder chunks in front of it for every slot since
        // the last real one. Leaves `self.streams[idx].count` at this
        // packet's own target slot; audio's `count` keeps its own,
        // unrelated meaning.
        if is_video {
            self.backfill_grid_slots(idx, tag, packet)?;
        }

        // A length-prefixed H.264/HEVC sample must be reframed to Annex B
        // before it is a legal AVI chunk payload — see `maybe_convert`'s doc
        // comment. `payload`'s length, not `packet.len`, drives every size
        // field below from here on, since the two can differ once
        // conversion runs.
        let payload = self.maybe_convert(idx, packet.payload())?;

        let pos = self.out.pos();
        self.out.write_tag(&tag)?;
        let len = u32::try_from(payload.len())
            .map_err(|_| Error::Unsupported("avi: packet too large"))?;
        self.out.wl32(len)?;
        self.out.write(&payload)?;
        if payload.len() % 2 == 1 {
            self.out.w8(0)?;
        }

        let stream = self
            .streams
            .get_mut(idx)
            .ok_or(Error::InvalidData("avi: packet names an unknown stream"))?;
        if is_video {
            // `count` already holds this packet's own slot (set by
            // `backfill_grid_slots`); advance past it so the *next* call
            // starts backfilling from here.
            stream.count = stream.count.saturating_add(1);
        } else {
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
        }

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

    /// Ask M6 for `h264_mp4toannexb`/`hevc_mp4toannexb` when the stream
    /// declared length-prefixed framing at [`Muxer::add_stream`] — the same
    /// condition [`AviMuxer::maybe_convert`] uses, so a caller driven through
    /// [`vaco_format_core::mux::MuxWriter`] with a real `BsfProvider` gets the
    /// splice-correct conversion instead of this crate's own framing-only
    /// fallback (see that method's docs).
    fn check_bitstream(
        &mut self,
        params: &CodecParameters,
        pkt: &Packet,
    ) -> Result<BitstreamAction> {
        let idx = usize::try_from(pkt.stream_index).ok();
        if idx
            .and_then(|i| self.streams.get(i))
            .is_some_and(|s| s.bsf_decided)
        {
            return Ok(BitstreamAction::Keep);
        }
        if let Some(s) = idx.and_then(|i| self.streams.get_mut(i)) {
            s.bsf_decided = true;
        }
        let needs_annexb = params.codec_id.is_some_and(is_h264_or_hevc)
            && params
                .video
                .as_ref()
                .and_then(|v| v.nal_length_size)
                .is_some_and(|n| n > 0);
        if !needs_annexb {
            return Ok(BitstreamAction::Keep);
        }
        Ok(BitstreamAction::Insert {
            name: match params.codec_id {
                Some(CodecId::Hevc) => "hevc_mp4toannexb",
                _ => "h264_mp4toannexb",
            },
        })
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
    let primary_video = streams.iter().find(|s| s.is_video);
    let (width, height) = primary_video.map_or((0, 0), |s| (s.width, s.height));
    // `dwMicroSecPerFrame` tracks the *source* time base
    // (`1e6 * num / den`, truncating), not `GRID_RATE` — internally
    // inconsistent with `strh`'s own `dwRate`, but that is what the
    // reference writes. `0` is the fallback when no source time base was
    // ever supplied (`add_stream` called directly, bypassing
    // `add_stream_with`).
    let us_per_frame = primary_video
        .and_then(|s| s.source_time_base)
        .filter(|tb| tb.den != 0)
        .and_then(|tb| {
            let num = u64::from(tb.num.unsigned_abs());
            let den = u64::from(tb.den.unsigned_abs());
            #[allow(
                clippy::integer_division,
                reason = "microseconds per source tick is an exact unit conversion, not a ratio"
            )]
            let us_per_frame = 1_000_000u64.checked_mul(num).map(|us| us / den.max(1));
            us_per_frame
        })
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    out.extend_from_slice(b"avih");
    out.extend_from_slice(&56u32.to_le_bytes());
    out.extend_from_slice(&us_per_frame.to_le_bytes()); // dwMicroSecPerFrame
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
        let vprp = build_vprp(s);
        strl.extend_from_slice(b"vprp");
        strl.extend_from_slice(&(vprp.len() as u32).to_le_bytes());
        strl.extend_from_slice(&vprp);
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

/// `vprp` (`AVIEXTHEADER`/`VPRP`, the `OpenDML` video-properties chunk) for a
/// video stream. Field layout matches the public `OpenDML` AVI File Format
/// Extensions document.
///
/// Only one `VIDEO_FIELD_DESC` is written (`nbFieldPerFrame = 1`): an
/// interlaced source would need a second descriptor and a
/// `dwVerticalRefreshRate`/`nbFieldPerFrame` convention this crate does not
/// yet produce.
fn build_vprp(s: StreamOut) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u32.to_le_bytes()); // VideoFormatToken: unknown/unspecified
    out.extend_from_slice(&0u32.to_le_bytes()); // VideoStandard: unknown/unspecified
    // `dwVerticalRefreshRate` is the grid rate, not the source frame rate —
    // written from `GRID_RATE` directly so the two cannot drift apart.
    let refresh_rate = u32::try_from(GRID_RATE.den).unwrap_or(0);
    out.extend_from_slice(&refresh_rate.to_le_bytes()); // dwVerticalRefreshRate
    out.extend_from_slice(&s.width.to_le_bytes()); // dwHTotalInT
    out.extend_from_slice(&s.height.to_le_bytes()); // dwVTotalInLines
    let sar_num = u16::try_from(s.sample_aspect_ratio.num.max(1)).unwrap_or(1);
    let sar_den = u16::try_from(s.sample_aspect_ratio.den.max(1)).unwrap_or(1);
    let aspect = (u32::from(sar_num) << 16) | u32::from(sar_den);
    out.extend_from_slice(&aspect.to_le_bytes()); // dwFrameAspectRatio
    out.extend_from_slice(&s.width.to_le_bytes()); // dwFrameWidthInPixels
    out.extend_from_slice(&s.height.to_le_bytes()); // dwFrameHeightInLines
    out.extend_from_slice(&1u32.to_le_bytes()); // nbFieldPerFrame
    // VIDEO_FIELD_DESC[0]
    out.extend_from_slice(&s.coded_height.to_le_bytes()); // CompressedBMHeight
    out.extend_from_slice(&s.coded_width.to_le_bytes()); // CompressedBMWidth
    out.extend_from_slice(&s.height.to_le_bytes()); // ValidBMHeight
    out.extend_from_slice(&s.width.to_le_bytes()); // ValidBMWidth
    out.extend_from_slice(&0u32.to_le_bytes()); // ValidBMXOffset
    out.extend_from_slice(&0u32.to_le_bytes()); // ValidBMYOffset
    out.extend_from_slice(&0u32.to_le_bytes()); // VideoXOffsetInT
    out.extend_from_slice(&0u32.to_le_bytes()); // VideoYValidStartLine
    out
}
