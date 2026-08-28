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
use vaco_format_core::{FormatOptions, Muxer, MuxerDesc, StreamSpec};
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

/// `AVIF_HASINDEX | AVIF_ISINTERLEAVED | AVIF_TRUSTCKTYPE`. The first two are
/// the only ones this crate's sibling demuxer interprets; `AVIF_TRUSTCKTYPE`
/// (0x800, "use `ckid`/`dwFlags` to decide key frames, do not guess") is
/// measured on the reference's own output across four fixtures and was
/// simply missing before — nothing here reads it back, but the flag word
/// itself is part of `avih`'s fixed byte layout.
const AVIH_FLAGS: u32 = 0x0000_0010 | 0x0000_0100 | 0x0000_0800;

/// `AVIIF_KEYFRAME`.
const AVIIF_KEYFRAME: u32 = 0x0000_0010;

/// The `movi` region's declared size when it cannot be patched afterward —
/// the same "length unknown, read to EOF" convention
/// [`vaco_format_riff::chunk`] documents readers already having to accept.
const LENGTH_UNKNOWN: u32 = 0xFFFF_FFFF;

/// `avih.dwSuggestedBufferSize`: 1 MiB, measured constant across every
/// fixture tried regardless of stream count, codec or content.
const SUGGESTED_BUFFER_SIZE: u32 = 1_048_576;

/// `JUNK` reservations the reference writes at three fixed points in
/// `hdrl`, all the same size regardless of any stream's own content:
/// measured identical across four fixtures whose `strf` sizes ranged from
/// 16 to 86 bytes and whose stream count and codecs varied (video-only
/// H.264 in both `avc1` and Annex-B framing, H.264 with AAC, and PCM-only)
/// — always exactly these three sizes, at these three points, never scaled
/// by anything this crate has access to. No semantic content depends on
/// the exact size; a reader ([`vaco_format_riff::chunk`]) skips any `JUNK`
/// chunk of any length identically.
///
/// The bytes inside are not simply zero, though. Two of the three turned
/// out to be an unfinished structure the reference reserves room for but
/// never activates (tags it `JUNK` rather than the real chunk id): the
/// per-`strl` one is an `AVISUPERINDEX` header with `wLongsPerEntry = 4`,
/// `nEntriesInUse = 0` and this stream's own `dwChunkId` (`build_strl_junk`,
/// which needs the stream's own chunk tag and so cannot be one shared
/// constant); the `hdrl`-level one is a `LIST 'odml'` containing one
/// `dmlh` (`AVIEXTHEADER`) chunk, `dwGrandFrames` and all, left `0` on
/// every fixture regardless of the file's real frame count. The RIFF-level
/// one measured genuinely all zero, no header of any kind.
const STRL_JUNK_LEN: usize = 4120;

/// One `strl`'s `JUNK` reservation: an inert `AVISUPERINDEX` header for
/// `tag` (this stream's own chunk id) followed by zeroed, unused entry
/// space. See [`STRL_JUNK_LEN`]'s doc comment for the measurement.
fn build_strl_junk(tag: [u8; 4]) -> [u8; STRL_JUNK_LEN] {
    let mut out = [0u8; STRL_JUNK_LEN];
    out[0] = 4; // wLongsPerEntry, LE u16 = 4
    out[8..12].copy_from_slice(&tag); // dwChunkId
    out
}

/// Written once inside `hdrl`, after every `strl`: `LIST 'odml'` (`dmlh`
/// filled with zeros), tagged `JUNK` instead of `LIST`.
const HDRL_JUNK: [u8; 260] = {
    let mut out = [0u8; 260];
    out[0] = b'o';
    out[1] = b'd';
    out[2] = b'm';
    out[3] = b'l';
    out[4] = b'd';
    out[5] = b'm';
    out[6] = b'l';
    out[7] = b'h';
    out[8] = 248; // dmlh's own declared size, LE u32 = 248
    out
};
/// Written once at the top RIFF level, between `hdrl`'s `LIST` and `movi`'s.
const RIFF_JUNK: [u8; 1016] = [0; 1016];

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

#[derive(Debug, Clone)]
struct StreamOut {
    is_video: bool,
    /// Also this stream's answer to [`Muxer::stream_time_base`] — the unit
    /// the generic interleave/rescale machinery upstream delivers packets
    /// in, not just an internal bookkeeping value. See
    /// [`StreamOut::strh_time_base`]'s doc comment for why the two are not
    /// always the same number.
    time_base: Rational,
    /// The value `strh.dwScale`/`dwRate` actually declares. Equal to
    /// `time_base` for video and CBR audio; for a compressed (VBR) audio
    /// stream it is one *frame's* duration instead of one sample's — see
    /// `add_stream`'s doc comment on the assignment for the measurement and
    /// why the two fields must stay separate.
    strh_time_base: Rational,
    /// `dwSampleSize`: `0` for video and VBR audio, else the CBR block size.
    sample_size: u32,
    /// Byte offset, within the in-memory `hdrl` buffer, of this stream's
    /// `strh.dwLength` field — patched at `write_trailer` once the true
    /// count is known.
    length_field_at: usize,
    /// Byte offset of this stream's `strh.dwSuggestedBufferSize` field,
    /// patched the same way from [`StreamOut::max_chunk_size`].
    suggested_buffer_at: usize,
    /// The largest single chunk (`movi` payload, not counting the chunk
    /// header) written for this stream so far. Measured: the reference's own
    /// `strh.dwSuggestedBufferSize` is exactly this — the largest sample,
    /// not a fixed size or a running average — confirmed on both video and
    /// audio streams across four fixtures.
    max_chunk_size: u32,
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
    /// The most recent video packet's own `duration`, in [`GRID_RATE`]
    /// ticks. Video-only; used by [`AviMuxer::backfill_trailing_video_slots`]
    /// to extend the grid past the *last* real frame's own slot by that
    /// frame's own duration, matching the reference — the last frame still
    /// spans time even though nothing on the grid marks its far edge except
    /// the file simply ending there.
    last_video_duration_ticks: i64,
    /// Audio's `wFormatTag`.
    audio_format_tag: u16,
    /// The stream's real sample rate — `strf`'s `nSamplesPerSec`, and the
    /// basis for `nAvgBytesPerSec`. Kept separate from `time_base`: a
    /// compressed stream's `time_base` is one *frame*'s duration
    /// (`compressed_audio_frame_size`), not one sample's, so `time_base.den`
    /// alone no longer says what the true sample rate is once that applies.
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    /// `WAVEFORMATEX`'s trailing `cbSize`-prefixed extension — AAC's raw
    /// `AudioSpecificConfig`, or MS-ADPCM-style coefficients. `None` writes
    /// the classic 16-byte `WAVEFORMATEX` with no `cbSize` field at all,
    /// which `vaco_format_riff::wave::WaveFormatEx::parse` (and the
    /// reference) both accept as "no extension".
    audio_extradata: Option<Vec<u8>>,
    /// The source's own H.264/HEVC extradata, written into `strf` after
    /// `BITMAPINFOHEADER` verbatim, whichever framing it is already in —
    /// `AVCDecoderConfigurationRecord`/`HEVCDecoderConfigurationRecord` for a
    /// length-prefixed source, or start-code-prefixed SPS/PPS for an Annex-B
    /// one. Measured on both: an H.264 sample sourced from a length-prefixed
    /// MP4 stays length-prefixed and writes its `avcC` after `strf`, tagged
    /// `avc1`; one sourced from Annex-B MPEG-TS stays Annex-B and writes its
    /// in-band SPS/PPS (with start codes) after `strf` just the same, tagged
    /// plain `H264` — this crate converts neither the payload framing nor
    /// the extradata shape, only ever copying what the source already has.
    video_extradata: Option<Vec<u8>>,
    /// `VideoParameters::has_b_frames`. Video-only; drives
    /// [`AviMuxer::maybe_backfill_leading_audio_gap`]'s leading-gap size for
    /// a *different* stream, which is why it needs to be readable off this
    /// one rather than recomputed there.
    has_b_frames: u8,
    /// `CodecParameters::bit_rate`, in bits per second. Feeds
    /// `avih.dwMaxBytesPerSec`, which [`write_avih`] sums across every
    /// stream and divides by 8 — measured on four fixtures (video-only,
    /// audio-only, and one of each): the reference's own value matches that
    /// sum exactly, truncated, including `0` when nothing declared a rate.
    bit_rate: Option<u64>,
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
            grid_budget: Budget::new(Limits::permissive()),
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
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

    /// Extends every video stream's grid past its own last real frame by
    /// that frame's own duration, backfilling the gap with the same
    /// zero-length placeholder chunks [`AviMuxer::backfill_grid_slots`]
    /// writes between frames.
    ///
    /// Measured: on two fixtures (25 fps, 150 and 25 real frames, `600/25 =
    /// 24` grid ticks per frame) the reference's `strh.dwLength` is exactly
    /// `frame_count * 24`, which is `24` ticks — one whole frame duration —
    /// past where placing each real frame on its own slot and stopping
    /// leaves the count. Without this, the last frame is on the grid but the
    /// span of time it occupies is not, so the track's own declared length
    /// comes up one frame short.
    ///
    /// Called once, from [`AviMuxer::write_trailer`], since (unlike the
    /// inter-frame gaps) there is no next packet to trigger this the way
    /// [`AviMuxer::backfill_grid_slots`] is triggered.
    ///
    /// # Errors
    /// [`vaco_limits::LimitError`] (surfaced as [`Error`]) if the implied gap
    /// is implausibly large — same bound as `backfill_grid_slots`, since the
    /// last packet's `duration` is as attacker-controlled as any other field
    /// in the source.
    fn backfill_trailing_video_slots(&mut self) -> Result<()> {
        for index in 0..self.streams.len() {
            let Some(stream) = self.streams.get(index) else {
                continue;
            };
            if !stream.is_video || stream.last_video_duration_ticks <= 0 {
                continue;
            }
            let tag = chunk_tag(u32::try_from(index).unwrap_or(u32::MAX), true)?;
            let duration_ticks = u64::try_from(stream.last_video_duration_ticks).unwrap_or(0);
            // `count` already sits one past the last real frame's own slot
            // (see `write_packet`'s comment), so the target is that slot
            // plus the frame's own duration, i.e. `count - 1 + duration`.
            let target = stream
                .count
                .saturating_sub(1)
                .saturating_add(duration_ticks);
            let gap = target.saturating_sub(stream.count);
            if gap > 0 {
                let entry_bytes = u64::try_from(core::mem::size_of::<IdxEntry>()).unwrap_or(u64::MAX);
                self.grid_budget.consume_fuel(gap)?;
                let idx_bytes = gap
                    .checked_mul(entry_bytes)
                    .ok_or(Error::Unsupported("avi: trailing video gap too large"))?;
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
                stream.count = target;
            }
        }
        Ok(())
    }

    /// Before a compressed (VBR) audio stream's *second* chunk, backfill
    /// `2^has_b_frames - 1` zero-length placeholder chunks — `has_b_frames`
    /// read off the *video* stream(s) in this file, the maximum across all
    /// of them if there is more than one.
    ///
    /// Fires before the second chunk, not after the first: measured
    /// position matters here, and the reference interleaves this gap
    /// immediately in front of the second real chunk's own natural position
    /// (which, for a leading encoder-priming frame, lands well after the
    /// first chunk once the surrounding video has advanced), not immediately
    /// after the first chunk's.
    ///
    /// Measured across seven synthetic fixtures (`libx264` at `-bf 0` through
    /// `-bf 7`, each paired with an AAC track whose own encoder priming is
    /// identical in every case): `has_b_frames` of 0, 1 and 2 (`ffprobe`'s
    /// own field, which caps there for every B-frame count this build of
    /// `libx264` produced) measured a leading gap of exactly 0, 1 and 3
    /// zero-length `01wb` chunks respectively, matching `2^n - 1` at every
    /// point tried — the count a binary reference-reordering pyramid of
    /// depth `n` needs primed before its first output. Absent a fixture with
    /// `has_b_frames >= 3`, the formula beyond `n = 2` is inferred from that
    /// shape, not independently confirmed.
    ///
    /// Without this, decoding the reference's own AVI output for a file
    /// shaped this way fails outright (`ffmpeg -i ref.avi -f md5 -` reports
    /// "Input buffer exhausted before END element found" on the *reference's
    /// own* file when the gap is missing) — this is not a cosmetic byte-count
    /// difference. Candidate mechanisms involving audio's own sample rate or
    /// duration were ruled out first: two audio-only fixtures (44.1 kHz
    /// stereo and 48 kHz mono, both AAC with the same one-frame encoder
    /// priming as every video-bearing fixture here) wrote zero placeholder
    /// chunks, and switching only the video's `-bf` while holding the audio
    /// fixed changed the gap on its own.
    ///
    /// # Errors
    /// [`vaco_limits::LimitError`] (surfaced as [`Error`]) if `has_b_frames`
    /// implies an implausible gap — bounded through the same `grid_budget`
    /// [`AviMuxer::backfill_grid_slots`] uses, since `has_b_frames` comes
    /// from the source's own SPS and is exactly as attacker-controlled as a
    /// video timestamp gap.
    fn maybe_backfill_leading_audio_gap(&mut self, audio_index: usize, tag: [u8; 4]) -> Result<()> {
        let has_b_frames = self
            .streams
            .iter()
            .filter(|s| s.is_video)
            .map(|s| s.has_b_frames)
            .max()
            .unwrap_or(0);
        let gap = 1u64
            .checked_shl(u32::from(has_b_frames))
            .map_or(u64::MAX, |v| v.saturating_sub(1));
        if gap > 0 {
            let entry_bytes = u64::try_from(core::mem::size_of::<IdxEntry>()).unwrap_or(u64::MAX);
            self.grid_budget.consume_fuel(gap)?;
            let idx_bytes = gap
                .checked_mul(entry_bytes)
                .ok_or(Error::Unsupported("avi: leading audio gap too large"))?;
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
        if let Some(stream) = self.streams.get_mut(audio_index) {
            stream.count = stream.count.saturating_add(gap);
        }
        Ok(())
    }
}

/// Whether `id` is a codec whose framing depends on whether its source
/// declared length-prefixed or Annex-B samples — the two AVI mirrors as
/// `avc1`/`avcC` or `H264`/`HEVC` respectively, unconverted either way (see
/// [`StreamOut::video_extradata`]'s doc comment for the measurement).
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
///
/// `length_prefixed` picks between H.264/HEVC's two `FourCC` families —
/// `avc1`/`hvc1` (ISO-BMFF style, config record follows) when the source
/// declared length-prefixed framing, `H264`/`HEVC` (Annex B, no record)
/// otherwise. Both spellings resolve back to the same [`CodecId`] on the
/// read side (`vaco_format_riff::video_tags::codec_id`), so either is a
/// legal round trip; which one is written now mirrors the source instead of
/// always picking the Annex-B spelling.
fn video_fourcc(id: CodecId, length_prefixed: bool) -> Option<[u8; 4]> {
    match id {
        CodecId::H264 if length_prefixed => Some(*b"avc1"),
        CodecId::H264 => Some(*b"H264"),
        CodecId::Hevc if length_prefixed => Some(*b"hvc1"),
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

/// Nominal samples per frame for a compressed audio codec — the unit
/// `dwScale/dwRate` uses for a VBR ("one chunk is one frame") stream, as
/// opposed to CBR PCM's per-sample time base. `None` for anything this
/// crate cannot write as compressed audio in the first place.
///
/// AAC-LC's 1024 is measured (see `add_stream`'s doc comment on the call
/// site); MP3's 1152 is the MPEG-1 Audio Layer III specification's own
/// fixed frame size, not independently confirmed against a fixture here.
fn compressed_audio_frame_size(id: CodecId) -> Option<u32> {
    match id {
        CodecId::Aac => Some(1024),
        CodecId::Mp3 => Some(1152),
        _ => None,
    }
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
            strh_time_base: Rational::new(1, 25),
            sample_size: 0,
            length_field_at: 0,
            suggested_buffer_at: 0,
            max_chunk_size: 0,
            count: 0,
            video_fourcc: *b"    ",
            width: 0,
            height: 0,
            coded_width: 0,
            coded_height: 0,
            sample_aspect_ratio: Rational::new(1, 1),
            source_time_base: None,
            grid_origin: None,
            last_video_duration_ticks: 0,
            audio_format_tag: 0,
            sample_rate: 0,
            channels: 1,
            bits_per_sample: 16,
            audio_extradata: None,
            video_extradata: None,
            has_b_frames: 0,
            bit_rate: params.bit_rate,
        };

        if is_video {
            let v = params.video.as_ref().ok_or(Error::Unsupported(
                "avi: video stream has no VideoParameters",
            ))?;
            // Every video stream sits on the fixed 600 Hz grid regardless of
            // its own frame rate — not derived from `v.frame_rate`.
            out.time_base = GRID_RATE;
            out.strh_time_base = GRID_RATE;
            out.has_b_frames = v.has_b_frames;
            let length_prefixed = is_h264_or_hevc(codec_id) && v.nal_length_size.is_some_and(|n| n > 0);
            if is_h264_or_hevc(codec_id) {
                let extra = params.extradata.clone().filter(|e| !e.is_empty());
                if length_prefixed && extra.is_none() {
                    // `strf`'s `avc1`/`hvc1` FourCC promises a configuration
                    // record right after `BITMAPINFOHEADER` — measured on
                    // the reference's own AVI output — so a length-prefixed
                    // stream with nothing to put there is refused rather
                    // than writing a tag with no record behind it. `H264`/
                    // `HEVC` (Annex-B) makes no such promise, so the same
                    // stream with no extradata at all is not an error there
                    // — it just writes the classic 40-byte `strf` with
                    // nothing after it, same as before extradata existed.
                    return Err(Error::Unsupported(
                        "avi: length-prefixed H.264/HEVC needs its avcC/hvcC extradata",
                    ));
                }
                out.video_extradata = extra;
            }
            out.video_fourcc = video_fourcc(codec_id, length_prefixed)
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
        } else {
            let a = params.audio.as_ref().ok_or(Error::Unsupported(
                "avi: audio stream has no AudioParameters",
            ))?;
            if a.sample_rate == 0 {
                return Err(Error::Unsupported("avi: audio stream has no sample rate"));
            }
            out.sample_rate = a.sample_rate;
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
            let sample_rate = i32::try_from(a.sample_rate).unwrap_or(i32::MAX);
            // A compressed (VBR) stream's `dwScale/dwRate` is one *frame*'s
            // duration, not one sample's — measured on AAC (`strh` reduces
            // to `1024/sample_rate`, i.e. `256/11025` at 44100 Hz, matching
            // AAC-LC's fixed 1024-sample frame). Uncompressed PCM keeps the
            // per-sample base, matching `dwSampleSize`'s own CBR unit.
            //
            // This is `strh_time_base`, not `time_base`: `time_base` is also
            // `Muxer::stream_time_base`'s answer, which the generic
            // interleave/rescale machinery upstream uses to decide *when* a
            // packet is due, not just what `strh` should say. Widening it to
            // one-frame ticks changed real interleaving order between audio
            // and video (found by comparing muxed output against the
            // reference, which caught a large block of reordered chunks
            // this crate's own tests never would have) — this crate's own
            // `write_packet` never reads audio timestamps at all, so nothing
            // here needed the coarser unit; only the `strh` field did.
            out.time_base = Rational::new(1, sample_rate);
            out.strh_time_base = compressed_audio_frame_size(codec_id).map_or(
                out.time_base,
                |frame_size| Rational::new(frame_size.cast_signed(), sample_rate).reduced(),
            );
            out.audio_format_tag = audio_format_tag(codec_id)
                .ok_or(Error::Unsupported("avi: codec has no AVI wFormatTag"))?;
            out.audio_extradata = params.extradata.clone().filter(|e| !e.is_empty());
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
        let mut strl_offsets = Vec::new();
        for (index, s) in self.streams.iter().enumerate() {
            strl_offsets.push(write_strl(&mut hdrl, u32::try_from(index).unwrap_or(u32::MAX), s));
        }
        hdrl.extend_from_slice(b"JUNK");
        hdrl.extend_from_slice(&(HDRL_JUNK.len() as u32).to_le_bytes());
        hdrl.extend_from_slice(&HDRL_JUNK);

        self.out.write_tag(b"LIST")?;
        self.out
            .wl32(u32::try_from(hdrl.len()).unwrap_or(u32::MAX))?;
        let hdrl_body_start = self.out.pos();
        self.out.write(&hdrl)?;
        self.avih_total_frames_at = hdrl_body_start + avih_total_frames_rel as u64;
        for (s, offsets) in self.streams.iter_mut().zip(strl_offsets) {
            s.length_field_at = (hdrl_body_start + offsets.length_field_at as u64) as usize;
            s.suggested_buffer_at =
                (hdrl_body_start + offsets.suggested_buffer_at as u64) as usize;
        }

        self.out.write_tag(b"JUNK")?;
        self.out.wl32(u32::try_from(RIFF_JUNK.len()).unwrap_or(u32::MAX))?;
        self.out.write(&RIFF_JUNK)?;

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
        } else if self
            .streams
            .get(idx)
            .is_some_and(|s| s.sample_size == 0 && s.count == 1)
        {
            // This stream's *second* chunk — the trigger for
            // `maybe_backfill_leading_audio_gap`'s doc comment explains why
            // it fires here and not on the first. Runs before this packet's
            // own bytes so the placeholders land immediately in front of it,
            // matching the reference's own interleaving position for them.
            self.maybe_backfill_leading_audio_gap(idx, tag)?;
        }

        // Written exactly as it arrived: an H.264/HEVC sample keeps whatever
        // framing its source declared (see `StreamOut::video_extradata`'s doc
        // comment) — this crate never reframes it.
        let payload = packet.payload();

        let pos = self.out.pos();
        self.out.write_tag(&tag)?;
        let len = u32::try_from(payload.len())
            .map_err(|_| Error::Unsupported("avi: packet too large"))?;
        self.out.wl32(len)?;
        self.out.write(payload)?;
        if payload.len() % 2 == 1 {
            self.out.w8(0)?;
        }

        let stream = self
            .streams
            .get_mut(idx)
            .ok_or(Error::InvalidData("avi: packet names an unknown stream"))?;
        stream.max_chunk_size = stream.max_chunk_size.max(len);
        if is_video {
            // `count` already holds this packet's own slot (set by
            // `backfill_grid_slots`); advance past it so the *next* call
            // starts backfilling from here.
            stream.count = stream.count.saturating_add(1);
            // Remembered so `write_trailer` can extend the grid past the
            // very last frame by its own duration — there is no next packet
            // to trigger that backfill the ordinary way.
            if let Some(ticks) = packet.duration.to_ticks(GRID_RATE)
                && ticks > 0
            {
                stream.last_video_duration_ticks = ticks;
            }
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

    // No `check_bitstream` override: a video sample keeps whatever framing
    // its source declared (see `StreamOut::video_extradata`'s doc comment), so
    // this muxer never asks M6 for a bitstream filter — the trait's default
    // `Keep` is already the right answer.

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("avi: trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("avi: trailer written twice"));
        }
        self.trailer_written = true;
        self.backfill_trailing_video_slots()?;

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
            // The *video* stream's own count, specifically — measured on an
            // audio-only fixture: `avih.dwTotalFrames` stays `0` rather than
            // falling back to an audio stream's sample count when there is
            // no video stream to report one for.
            let total = self
                .streams
                .iter()
                .find(|s| s.is_video)
                .map_or(0, |s| s.count);
            self.out.wl32(u32::try_from(total).unwrap_or(u32::MAX))?;

            for s in &self.streams {
                self.out.seek(s.length_field_at as u64)?;
                self.out.wl32(u32::try_from(s.count).unwrap_or(u32::MAX))?;
                self.out.seek(s.suggested_buffer_at as u64)?;
                self.out.wl32(s.max_chunk_size)?;
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
    // The sum of every stream's own declared `bit_rate`, in bytes/sec —
    // measured on four fixtures (video-only, audio-only, both together, and
    // one with no declared rate on either stream) to match the reference's
    // own `dwMaxBytesPerSec` exactly, division truncated.
    #[allow(
        clippy::integer_division,
        reason = "bits to bytes is an exact unit conversion, not a ratio"
    )]
    let max_bytes_per_sec = (streams.iter().filter_map(|s| s.bit_rate).sum::<u64>() / 8)
        .try_into()
        .unwrap_or(u32::MAX);
    out.extend_from_slice(b"avih");
    out.extend_from_slice(&56u32.to_le_bytes());
    out.extend_from_slice(&us_per_frame.to_le_bytes()); // dwMicroSecPerFrame
    out.extend_from_slice(&max_bytes_per_sec.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // dwPaddingGranularity
    out.extend_from_slice(&AVIH_FLAGS.to_le_bytes());
    let total_frames_at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes()); // dwTotalFrames (patched)
    out.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    out.extend_from_slice(&(streams.len() as u32).to_le_bytes());
    // Measured: constant across every fixture tried, regardless of stream
    // count, codec or content — 1 MiB, not derived from anything here.
    out.extend_from_slice(&SUGGESTED_BUFFER_SIZE.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // dwReserved[4]
    total_frames_at
}

/// Byte offsets, within the buffer [`write_strl`] appends to, of the two
/// `strh` fields [`AviMuxer::write_trailer`] patches once their true values
/// are known: `dwLength` (the final chunk/sample count) and
/// `dwSuggestedBufferSize` (the largest single chunk this stream wrote).
struct StrlOffsets {
    length_field_at: usize,
    suggested_buffer_at: usize,
}

/// Write one `LIST strl` (`strh` + `strf`), returning the offsets
/// [`StrlOffsets`] documents.
fn write_strl(out: &mut Vec<u8>, index: u32, s: &StreamOut) -> StrlOffsets {
    // `chunk_tag` only fails past 100 streams. `write_packet` already
    // returns that as a real error for such a file; here, inside an inert
    // `JUNK` template only a debugger would ever look at, a placeholder tag
    // is a reasonable fallback rather than making header-building fallible
    // for a case nothing downstream can act on differently anyway.
    let tag = chunk_tag(index, s.is_video).unwrap_or(*b"00xx");
    let mut strl = Vec::new();
    strl.extend_from_slice(b"strl");

    let mut strh = Vec::new();
    strh.extend_from_slice(if s.is_video { b"vids" } else { b"auds" });
    // `fccHandler`: mirrors `biCompression` for video, matching what
    // `ffmpeg 8.1`'s own writer does (measured: both carry `FMP4` for an
    // mpeg4 stream). For audio, measured as the raw `u32` value `1`
    // (`WAVE_FORMAT_PCM`'s own tag number) regardless of the stream's
    // actual `wFormatTag` — an AAC stream measured the same `1` an actual
    // PCM stream did, so it is a fixed placeholder here, not a mirror of
    // `audio_format_tag`.
    let audio_fcc_handler = 1u32.to_le_bytes();
    strh.extend_from_slice(if s.is_video {
        &s.video_fourcc
    } else {
        &audio_fcc_handler
    });
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwFlags
    strh.extend_from_slice(&0u16.to_le_bytes()); // wPriority
    strh.extend_from_slice(&0u16.to_le_bytes()); // wLanguage
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    strh.extend_from_slice(&s.strh_time_base.num.to_le_bytes()); // dwScale
    strh.extend_from_slice(&s.strh_time_base.den.to_le_bytes()); // dwRate
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwStart
    let length_rel_in_strh = strh.len();
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwLength (patched)
    let suggested_buffer_rel_in_strh = strh.len();
    strh.extend_from_slice(&0u32.to_le_bytes()); // dwSuggestedBufferSize (patched)
    strh.extend_from_slice(&(-1i32).to_le_bytes()); // dwQuality: -1 = unspecified
    strh.extend_from_slice(&s.sample_size.to_le_bytes());
    // `rcFrame`: `{0, 0, width, height}` for video (measured — not all
    // zero), `{0, 0, 0, 0}` for audio, which has no frame rectangle.
    if s.is_video {
        strh.extend_from_slice(&0i16.to_le_bytes());
        strh.extend_from_slice(&0i16.to_le_bytes());
        strh.extend_from_slice(&i16::try_from(s.width).unwrap_or(i16::MAX).to_le_bytes());
        strh.extend_from_slice(&i16::try_from(s.height).unwrap_or(i16::MAX).to_le_bytes());
    } else {
        strh.extend_from_slice(&[0u8; 8]);
    }

    strl.extend_from_slice(b"strh");
    strl.extend_from_slice(&(strh.len() as u32).to_le_bytes());
    let strh_body_start = strl.len();
    strl.extend_from_slice(&strh);

    // A `BITMAPINFOHEADER`/`WAVEFORMATEX`, exactly what
    // `vaco-demux-avi`/`vaco-format-riff` read back on the other side.
    if s.is_video {
        let mut strf = Vec::new();
        // `biSize`: the classic 40-byte prefix, plus the configuration
        // record's own length when there is one — measured on the
        // reference's own `avc1` output (`biSize=85` for a 45-byte `avcC`,
        // i.e. `40 + 45`, not the classic `40`). Unlike ordinary RIFF chunk
        // padding (external, uncounted — see `vaco_format_riff::chunk`'s
        // module docs), an odd total here is padded with one zero byte
        // that *is* folded into `strf`'s own declared `ckSize`, matching
        // that same measurement (`ckSize=86`, one more than `biSize`).
        let record_len = s.video_extradata.as_deref().map_or(0, <[u8]>::len);
        let bi_size = 40u32.saturating_add(u32::try_from(record_len).unwrap_or(u32::MAX));
        strf.extend_from_slice(&bi_size.to_le_bytes()); // biSize
        strf.extend_from_slice(&s.width.cast_signed().to_le_bytes()); // biWidth
        strf.extend_from_slice(&s.height.cast_signed().to_le_bytes()); // biHeight
        strf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        strf.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
        strf.extend_from_slice(&s.video_fourcc);
        // `biSizeImage`: `width * height * 3` (the raw-RGB byte count
        // `biBitCount = 24` above implies) regardless of the codec actually
        // being compressed — measured identical on `avc1` and Annex-B
        // `H264` alike, so it tracks the header's own declared bit count,
        // not the real (compressed) sample size.
        let size_image = s.width.saturating_mul(s.height).saturating_mul(3);
        strf.extend_from_slice(&size_image.to_le_bytes());
        strf.extend_from_slice(&0i32.to_le_bytes());
        strf.extend_from_slice(&0i32.to_le_bytes());
        strf.extend_from_slice(&0u32.to_le_bytes());
        strf.extend_from_slice(&0u32.to_le_bytes());
        if let Some(record) = &s.video_extradata {
            strf.extend_from_slice(record);
            if strf.len() % 2 == 1 {
                strf.push(0);
            }
        }
        strl.extend_from_slice(b"strf");
        strl.extend_from_slice(&(strf.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strf);
        // Measured order is `strf`, `JUNK`, `vprp` — not `strf`, `vprp`,
        // `JUNK` — so the shared `JUNK` write below is skipped for video and
        // done here instead, ahead of `vprp`.
        let strl_junk = build_strl_junk(tag);
        strl.extend_from_slice(b"JUNK");
        strl.extend_from_slice(&(strl_junk.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strl_junk);
        let vprp = build_vprp(s);
        strl.extend_from_slice(b"vprp");
        strl.extend_from_slice(&(vprp.len() as u32).to_le_bytes());
        strl.extend_from_slice(&vprp);
    } else {
        let mut strf = Vec::new();
        strf.extend_from_slice(&s.audio_format_tag.to_le_bytes());
        strf.extend_from_slice(&s.channels.to_le_bytes());
        let rate = s.sample_rate.max(1);
        strf.extend_from_slice(&rate.to_le_bytes());
        let block_align = if s.sample_size > 0 {
            s.sample_size
        } else {
            #[allow(
                clippy::integer_division,
                reason = "bytes-per-sample from bits-per-sample is an exact conversion, not a ratio"
            )]
            let bytes_per_sample = (u32::from(s.bits_per_sample) / 8).max(1);
            // Measured wrong for compressed audio: the reference's own
            // `nBlockAlign` for an AAC stream did not match this formula
            // (nor the source's declared bit rate, sample rate or channel
            // count in any combination tried), and no second fixture was
            // available to isolate the real rule. Kept as the best
            // available answer for CBR-shaped callers; not verified for
            // VBR ones.
            bytes_per_sample * u32::from(s.channels)
        };
        #[allow(
            clippy::integer_division,
            reason = "bits to bytes is an exact unit conversion, not a ratio"
        )]
        let avg_bytes_per_sec = s
            .bit_rate
            .filter(|_| s.sample_size == 0)
            .and_then(|br| u32::try_from(br / 8).ok())
            .unwrap_or_else(|| rate.saturating_mul(block_align));
        strf.extend_from_slice(&avg_bytes_per_sec.to_le_bytes()); // nAvgBytesPerSec
        strf.extend_from_slice(&u16::try_from(block_align).unwrap_or(u16::MAX).to_le_bytes());
        strf.extend_from_slice(&s.bits_per_sample.to_le_bytes());
        // `WAVEFORMATEX`'s trailing `cbSize`-prefixed extension: AAC's raw
        // `AudioSpecificConfig`, carried once here rather than per frame —
        // without it, a decoder has no object type or channel configuration
        // and desyncs on the very first frame it tries to decode. Absent
        // for a codec with nothing to carry (PCM, MP3), which keeps the
        // classic 16-byte `WAVEFORMATEX` `vaco-demux-avi`'s own read side
        // already treats as "no extension".
        if let Some(extra) = &s.audio_extradata {
            let cb_size = u16::try_from(extra.len()).unwrap_or(u16::MAX);
            strf.extend_from_slice(&cb_size.to_le_bytes());
            strf.extend_from_slice(extra);
            if strf.len() % 2 == 1 {
                strf.push(0);
            }
        }
        strl.extend_from_slice(b"strf");
        strl.extend_from_slice(&(strf.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strf);
        // No `vprp` on the audio side, so `strf` is immediately followed by
        // `JUNK` here — the same relative position the video branch above
        // wrote its own `JUNK` in, just with nothing after it.
        let strl_junk = build_strl_junk(tag);
        strl.extend_from_slice(b"JUNK");
        strl.extend_from_slice(&(strl_junk.len() as u32).to_le_bytes());
        strl.extend_from_slice(&strl_junk);
    }

    // `out.len()` is where this `strl`'s own `LIST` tag will land; 8 bytes
    // for that `LIST`'s tag+size, then `strh_body_start` (already the offset
    // of `strh`'s body *within* `strl`, i.e. past `strl`'s own `"strl"`
    // marker and `strh`'s tag+size) plus each field's offset within `strh`.
    let base = out.len() + 8 + strh_body_start;
    let offsets = StrlOffsets {
        length_field_at: base + length_rel_in_strh,
        suggested_buffer_at: base + suggested_buffer_rel_in_strh,
    };
    out.extend_from_slice(b"LIST");
    out.extend_from_slice(&(strl.len() as u32).to_le_bytes());
    out.extend_from_slice(&strl);
    offsets
}

/// `vprp` (`AVIEXTHEADER`/`VPRP`, the `OpenDML` video-properties chunk) for a
/// video stream. Field layout matches the public `OpenDML` AVI File Format
/// Extensions document.
///
/// Only one `VIDEO_FIELD_DESC` is written (`nbFieldPerFrame = 1`): an
/// interlaced source would need a second descriptor and a
/// `dwVerticalRefreshRate`/`nbFieldPerFrame` convention this crate does not
/// yet produce.
fn build_vprp(s: &StreamOut) -> Vec<u8> {
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
