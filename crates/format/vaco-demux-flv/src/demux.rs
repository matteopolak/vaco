//! The FLV demuxer: the tag walk, `onMetaData`, and the codec mapping.
//!
//! # Why stream discovery is progressive
//!
//! Unlike AVI or MP4, FLV states no stream list up front — `DataOffset`
//! (the header's ninth byte onward) names only the byte the first tag starts
//! at. What a real player does, and what this demuxer does, is watch the tag
//! stream: the first video tag creates the video stream, the first audio tag
//! creates the audio stream, and each one's own codec-id nibble (or, for
//! Enhanced RTMP, `FourCC`) names its codec directly — no bitstream parsing
//! required. `onMetaData` (tag type 18), when present, arrives first in every
//! file this crate has seen and supplies `width`/`height`/`duration` ahead of
//! the tags that would otherwise need to state them, so its fields are cached
//! and applied when the corresponding stream is created.
//!
//! This is the same shape `vaco-demux-mpegts` already established for a
//! format with no header stream list: [`Demuxer::streams`] can grow between
//! calls to [`Demuxer::read_packet`], and that growth is not a defect.
//!
//! # Timestamps
//!
//! Trivial by container standards: every tag states its own presentation
//! timestamp directly, in milliseconds, as a 24-bit value plus an 8-bit
//! extension byte in a deliberately unusual order (the extension is the
//! *high* byte, appended after the low 24 bits rather than before them — a
//! layout every FLV reader has to know rather than derive). `dts` differs
//! from `pts` only for AVC/HEVC frames carrying a non-zero
//! `CompositionTime`; see [`tag`]'s module docs for the one case this crate
//! cannot always get exactly right (Enhanced RTMP's ambiguous `CodedFrames`).

use std::collections::VecDeque;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{IndexEntry, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::amf::AmfValue;
use crate::tag::{
    BACK_POINTER_LEN, ExPacketType, FRAME_TYPE_KEY, TAG_AUDIO, TAG_HEADER_LEN, TAG_SCRIPT,
    TAG_VIDEO, fourcc_codec_id, legacy_audio_codec_id, legacy_video_codec_id, read_i24,
};

/// The flags this container declares.
///
/// No [`FormatFlags::NOBINSEARCH`]: unlike AVI, every FLV tag states its own
/// absolute timestamp, so landing on an arbitrary byte offset and resyncing
/// to the next tag header genuinely does recover a real timestamp — bisection
/// is structurally sound here. It is not implemented in this version (see
/// `docs/format/vaco-demux-flv.md`), so the flag is set defensively until it
/// is: claiming a capability this demuxer cannot yet perform would be a worse
/// interface lie than declining a case a future version can pick up.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX.union(FormatFlags::NOBINSEARCH);

/// FLV's one and only time base: milliseconds. Every tag's timestamp is
/// stated directly in it, so streams do not carry a per-stream time base the
/// way AVI's `dwScale/dwRate` or MP4's media timescale do.
const MS_BASE: Rational = Rational::new(1, 1_000);

/// Content probe: the `FLV` signature plus the version byte.
///
/// Measured against `ffprobe 8.1`'s own `flv` probe, which checks `"FLV"`,
/// a version byte, and that the reserved bits of the type-flags byte are
/// zero — [`ProbeScore::MAGIC_CHECKED`] reflects checking all three rather
/// than the bare three-byte tag.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.get(0) == Some(b'F')
        && data.get(1) == Some(b'L')
        && data.get(2) == Some(b'V')
        && data.get(3).is_some()
        && data.get(4).is_some_and(|f| f & 0xFA == 0)
    {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// The registry descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "flv",
    long_name: "FLV (Flash Video)",
    extensions: &["flv"],
    mime_types: &["video/x-flv"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(FlvDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

/// `onMetaData` fields cached until the stream they describe is created.
#[derive(Debug, Clone, Copy, Default)]
struct PendingMeta {
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: Option<f64>,
    duration_seconds: Option<f64>,
}

/// The FLV demuxer.
#[derive(Debug)]
pub struct FlvDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    video_index: Option<u32>,
    audio_index: Option<u32>,
    pending_meta: PendingMeta,
    metadata: Vec<(String, String)>,
    queue: VecDeque<Packet>,
    index: PacketIndex,
    budget: Budget,
    duration: Option<Duration>,
    eof: bool,
}

impl FlvDemuxer {
    /// Open an FLV stream.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the `FLV` signature is missing; otherwise
    /// propagates transport failure. A file whose first tags are unreadable
    /// is not itself an error here — [`Demuxer::read_packet`] reports it.
    pub fn open(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling — the constructor a fuzz
    /// target or a memory-conscious embedder reaches for.
    ///
    /// # Errors
    /// As [`FlvDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut sig = [0u8; 5];
        io.read_exact(&mut sig)?;
        if &sig[..3] != b"FLV" {
            return Err(Error::InvalidData("flv: missing FLV signature"));
        }
        let data_offset = io.rb32()?;
        // `DataOffset` names where the first tag's back-pointer begins.
        // Almost always 9 (the header this crate just read); a larger value
        // is a documented extension point, so it is honoured rather than
        // assumed away.
        io.seek(u64::from(data_offset))?;

        Ok(Self {
            io,
            streams: Vec::new(),
            video_index: None,
            audio_index: None,
            pending_meta: PendingMeta::default(),
            metadata: Vec::new(),
            queue: VecDeque::new(),
            index: PacketIndex::with_options(opts),
            budget: Budget::new(limits),
            duration: None,
            eof: false,
        })
    }

    fn ensure_video_stream(&mut self, codec_id: Option<CodecId>) -> usize {
        if let Some(i) = self.video_index {
            return i as usize;
        }
        let mut params = CodecParameters::video();
        params.codec_id = codec_id;
        if let Some(v) = &mut params.video {
            if let Some(w) = self.pending_meta.width {
                v.width = w;
                v.coded_width = w;
            }
            if let Some(h) = self.pending_meta.height {
                v.height = h;
                v.coded_height = h;
            }
            if let Some(fps) = self.pending_meta.frame_rate
                && fps > 0.0
                && fps.is_finite()
            {
                // `onMetaData`'s `framerate` is a plain double; there is no
                // authored numerator/denominator to preserve; a nine-decimal
                // scale is enough to keep any realistic frame rate exact.
                v.frame_rate = Rational::new((fps * 1_000_000_000.0).round() as i32, 1_000_000_000);
            }
        }
        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut stream = Stream::new(index, MediaType::Video, MS_BASE);
        stream.params = params;
        self.streams.push(stream);
        self.video_index = Some(index);
        index as usize
    }

    fn ensure_audio_stream(&mut self, codec_id: Option<CodecId>) -> usize {
        if let Some(i) = self.audio_index {
            return i as usize;
        }
        let mut params = CodecParameters::audio();
        params.codec_id = codec_id;
        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut stream = Stream::new(index, MediaType::Audio, MS_BASE);
        stream.params = params;
        self.streams.push(stream);
        self.audio_index = Some(index);
        index as usize
    }

    fn set_extradata(&mut self, index: usize, data: &[u8]) {
        if let Some(s) = self.streams.get_mut(index) {
            s.params.extradata = Some(data.to_vec());
        }
    }

    fn emit_packet(
        &mut self,
        stream_index: usize,
        payload: &[u8],
        timestamp_ms: i64,
        composition_time_ms: i32,
        is_key: bool,
        pos: u64,
    ) -> Result<()> {
        let mut pkt = Packet::from_slice(&mut self.budget, payload)?;
        let index = u32::try_from(stream_index).unwrap_or(u32::MAX);
        pkt.stream_index = index;
        let pts_ms = timestamp_ms.saturating_add(i64::from(composition_time_ms));
        pkt.pts = Timestamp::new(pts_ms);
        pkt.dts = Timestamp::new(timestamp_ms);
        pkt.pos = Some(pos);
        pkt.flags = if is_key {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        if is_key {
            let ts =
                Timestamp::new(timestamp_ms).rescale(MS_BASE, TIME_BASE_Q, Rounding::default());
            self.index.add(IndexEntry::keyframe(pos, ts));
        }
        self.queue.push_back(pkt);
        Ok(())
    }

    fn handle_video_tag(&mut self, body: &[u8], timestamp_ms: i64, pos: u64) -> Result<()> {
        let &first = body
            .first()
            .ok_or(Error::InvalidData("flv: empty video tag"))?;
        let is_extended = first & 0x80 != 0;
        if is_extended {
            let frame_type = (first >> 4) & 0x07;
            let packet_type = ExPacketType::from_nibble(first & 0x0F);
            let fourcc: [u8; 4] = body
                .get(1..5)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData("flv: extended video tag missing FourCC"))?;
            let codec_id = fourcc_codec_id(fourcc);
            let payload = body.get(5..).unwrap_or(&[]);
            match packet_type {
                ExPacketType::SequenceStart => {
                    let idx = self.ensure_video_stream(codec_id);
                    self.set_extradata(idx, payload);
                }
                ExPacketType::CodedFrames | ExPacketType::CodedFramesX => {
                    let idx = self.ensure_video_stream(codec_id);
                    self.emit_packet(
                        idx,
                        payload,
                        timestamp_ms,
                        0,
                        frame_type == FRAME_TYPE_KEY,
                        pos,
                    )?;
                }
                ExPacketType::SequenceEnd | ExPacketType::Other => {}
            }
        } else {
            let frame_type = (first >> 4) & 0x0F;
            let codec = first & 0x0F;
            let codec_id = legacy_video_codec_id(codec);
            let idx = self.ensure_video_stream(codec_id);
            if codec == 7 {
                let &packet_type = body
                    .get(1)
                    .ok_or(Error::InvalidData("flv: truncated AVC video tag"))?;
                let comp: [u8; 3] = body
                    .get(2..5)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(Error::InvalidData("flv: truncated AVC video tag"))?;
                let payload = body.get(5..).unwrap_or(&[]);
                match packet_type {
                    0 => self.set_extradata(idx, payload),
                    1 => self.emit_packet(
                        idx,
                        payload,
                        timestamp_ms,
                        read_i24(comp),
                        frame_type == FRAME_TYPE_KEY,
                        pos,
                    )?,
                    _ => {}
                }
            } else {
                let payload = body.get(1..).unwrap_or(&[]);
                self.emit_packet(
                    idx,
                    payload,
                    timestamp_ms,
                    0,
                    frame_type == FRAME_TYPE_KEY,
                    pos,
                )?;
            }
        }
        Ok(())
    }

    fn handle_audio_tag(&mut self, body: &[u8], timestamp_ms: i64, pos: u64) -> Result<()> {
        let &first = body
            .first()
            .ok_or(Error::InvalidData("flv: empty audio tag"))?;
        let high_nibble = (first >> 4) & 0x0F;
        // The Enhanced RTMP audio extension reuses the legacy `SoundFormat`
        // nibble's reserved value `9` as its own "extended header" marker —
        // measured against `ffmpeg -c:a libopus -f flv`: the first byte of
        // every Opus tag is `0x9_`, never a plain `SoundFormat`. This is a
        // different convention from video's single high *bit*, not a
        // mistake — see `docs/format/vaco-demux-flv.md`.
        if high_nibble == 0x9 {
            let packet_type = first & 0x0F;
            let fourcc: [u8; 4] = body
                .get(1..5)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::InvalidData("flv: extended audio tag missing FourCC"))?;
            let codec_id = fourcc_codec_id(fourcc);
            let payload = body.get(5..).unwrap_or(&[]);
            match packet_type {
                0 => {
                    let idx = self.ensure_audio_stream(codec_id);
                    self.set_extradata(idx, payload);
                }
                1 => {
                    let idx = self.ensure_audio_stream(codec_id);
                    // Audio has no dependency structure; every frame is a
                    // valid resume point.
                    self.emit_packet(idx, payload, timestamp_ms, 0, true, pos)?;
                }
                // Multichannel configuration and other side metadata: not
                // decoded (see `docs/format/vaco-demux-flv.md`).
                _ => {}
            }
        } else {
            let format = high_nibble;
            let codec_id = legacy_audio_codec_id(format);
            let idx = self.ensure_audio_stream(codec_id);
            if format == 10 {
                let &packet_type = body
                    .get(1)
                    .ok_or(Error::InvalidData("flv: truncated AAC audio tag"))?;
                let payload = body.get(2..).unwrap_or(&[]);
                match packet_type {
                    0 => self.set_extradata(idx, payload),
                    1 => self.emit_packet(idx, payload, timestamp_ms, 0, true, pos)?,
                    _ => {}
                }
            } else {
                let payload = body.get(1..).unwrap_or(&[]);
                self.emit_packet(idx, payload, timestamp_ms, 0, true, pos)?;
            }
        }
        Ok(())
    }

    fn handle_script_tag(&mut self, body: &[u8]) {
        let Ok((name, consumed)) = crate::amf::decode(body, &mut self.budget) else {
            return;
        };
        if name.as_str() != Some("onMetaData") {
            return;
        }
        let Some(rest) = body.get(consumed..) else {
            return;
        };
        let Ok((meta, _)) = crate::amf::decode(rest, &mut self.budget) else {
            return;
        };
        if let Some(w) = meta.get("width").and_then(AmfValue::as_f64) {
            self.pending_meta.width = Some(w.max(0.0) as u32);
        }
        if let Some(h) = meta.get("height").and_then(AmfValue::as_f64) {
            self.pending_meta.height = Some(h.max(0.0) as u32);
        }
        if let Some(fps) = meta.get("framerate").and_then(AmfValue::as_f64) {
            self.pending_meta.frame_rate = Some(fps);
        }
        if let Some(d) = meta.get("duration").and_then(AmfValue::as_f64) {
            self.pending_meta.duration_seconds = Some(d);
            if d.is_finite() && d >= 0.0 {
                self.duration = Some(Duration::from_micros((d * 1_000_000.0).round() as i64));
            }
        }
        for (key, out_key) in [
            ("title", "title"),
            ("artist", "artist"),
            ("creationdate", "creation_time"),
        ] {
            if let Some(v) = meta.get(key).and_then(AmfValue::as_str) {
                self.metadata.push((out_key.to_owned(), v.to_owned()));
            }
        }
    }

    /// Read and dispatch exactly one tag, queuing zero or more packets.
    fn read_tag(&mut self) -> Result<()> {
        // Recorded as the *back-pointer's* start, not the header's: that is
        // the position `resync` (and every packet's `pos` field) has to
        // agree on, since `resync`'s `probe_tag_header` starts by reading a
        // `PreviousTagSize` field too.
        let pos = self.io.pos();
        let _prev_size = match self.io.rb32() {
            Ok(v) => v,
            Err(Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        };
        let tag_type = match self.io.r8() {
            Ok(t) => t,
            Err(Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        };
        let data_size = self.io.rb24()?;
        let ts_low = self.io.rb24()?;
        let ts_ext = self.io.r8()?;
        // The extension byte is the *high* eight bits, appended after the
        // low 24 — not the natural byte order a 32-bit big-endian value would
        // use. Adobe *Video File Format Specification v10.1* §Annex E.
        let timestamp_ms = (i64::from(ts_ext) << 24) | i64::from(ts_low);
        let _stream_id = self.io.rb24()?;

        if let Some(fsize) = self.io.size()
            && u64::from(data_size) > fsize.saturating_sub(self.io.pos())
        {
            return Err(Error::InvalidData("flv: tag claims more bytes than remain"));
        }
        let n = usize::try_from(data_size).unwrap_or(usize::MAX);
        let mut body = self.budget.alloc::<u8>(n)?;
        self.io.read_exact(&mut body)?;

        match tag_type {
            TAG_AUDIO => self.handle_audio_tag(&body, timestamp_ms, pos)?,
            TAG_VIDEO => self.handle_video_tag(&body, timestamp_ms, pos)?,
            TAG_SCRIPT => self.handle_script_tag(&body),
            _ => {}
        }
        Ok(())
    }

    fn read_one(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            self.read_tag()?;
        }
    }

    /// Scan forward from `pos` for the next byte offset that looks like a
    /// well-formed tag header (a recognised tag type and a data size that
    /// fits inside the file).
    fn resync(&mut self, pos: u64) -> Result<()> {
        self.eof = false;
        self.queue.clear();
        let limit = self.io.size().unwrap_or(u64::MAX);
        let mut at = pos;
        loop {
            if at.saturating_add(BACK_POINTER_LEN + TAG_HEADER_LEN) > limit {
                self.eof = true;
                return Err(Error::Eof);
            }
            if self.io.seek(at).is_ok() && self.probe_tag_header(limit).is_ok() {
                // Land exactly on the back-pointer `read_tag` expects to
                // read first, not past it — `probe_tag_header` restores the
                // position to `at` on both success and failure, so this seek
                // is only needed because the loop above may have wandered.
                self.io.seek(at).ok();
                return Ok(());
            }
            at = at.saturating_add(1);
        }
    }

    /// Read a candidate tag header at the current position (assumed to be a
    /// `PreviousTagSize` field) and validate it minimally, without consuming
    /// the tag's body. Restores the position on failure.
    fn probe_tag_header(&mut self, limit: u64) -> Result<()> {
        let start = self.io.pos();
        let ok = (|| -> Result<()> {
            let _prev = self.io.rb32()?;
            let tag_type = self.io.r8()?;
            if !matches!(tag_type, TAG_AUDIO | TAG_VIDEO | TAG_SCRIPT) {
                return Err(Error::InvalidData("flv: not a tag header"));
            }
            let data_size = self.io.rb24()?;
            let _ts = self.io.rb24()?;
            let _ts_ext = self.io.r8()?;
            let _stream_id = self.io.rb24()?;
            if start
                .saturating_add(BACK_POINTER_LEN)
                .saturating_add(TAG_HEADER_LEN)
                .saturating_add(u64::from(data_size))
                > limit
            {
                return Err(Error::InvalidData("flv: tag would run past the file"));
            }
            Ok(())
        })();
        let _ = self.io.seek(start);
        ok
    }
}

impl Demuxer for FlvDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        self.read_one()
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = match target.stream_index() {
            Some(i) => {
                let st = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.streams.get(i))
                    .ok_or(Error::InvalidData("flv: seek names an unknown stream"))?;
                let rate = st
                    .params
                    .video
                    .as_ref()
                    .map_or(Rational::ZERO, |v| v.frame_rate);
                target.resolve_frames(rate, st.time_base)?
            }
            None => target,
        };
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                self.resync(pos)
            }
            SeekTarget::Timestamp { stream_index, ts } => {
                let st = usize::try_from(stream_index)
                    .ok()
                    .and_then(|i| self.streams.get(i))
                    .ok_or(Error::InvalidData("flv: seek names an unknown stream"))?;
                let want = ts.rescale(st.time_base, TIME_BASE_Q, Rounding::default());
                let seekable = self.io.seekability() != Seekability::None;
                let strategy = SeekStrategy::choose(
                    SeekTarget::Timestamp {
                        stream_index,
                        ts: want,
                    },
                    flags,
                    FLAGS,
                    !self.index.is_empty(),
                    seekable,
                );
                match strategy {
                    SeekStrategy::Index => {
                        let entry = self.index.search(want, flags).ok_or(Error::NotSeekable)?;
                        self.resync(entry.pos)
                    }
                    SeekStrategy::Byte => self.resync(0),
                    SeekStrategy::BinarySearch | SeekStrategy::Unsupported => {
                        Err(Error::NotSeekable)
                    }
                }
            }
            SeekTarget::Frame { .. } => Err(Error::Unsupported("flv: unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}
