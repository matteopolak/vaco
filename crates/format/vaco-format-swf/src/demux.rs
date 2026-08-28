//! The `swf` demuxer: walk the tag stream, extract video and audio.
//!
//! # Which tags this reads
//!
//! `DefineVideoStream` (60) declares the video stream (dimensions, frame
//! count, codec); `VideoFrame` (61) is one video packet.
//! `SoundStreamHead`/`SoundStreamHead2` (18/45) declare the audio stream
//! (compression, sample rate, channels); `SoundStreamBlock` (19) is one
//! audio packet. `ShowFrame` (1) and `End` (0) are structural. **Every other
//! tag code is skipped by its own declared length** — this crate does not
//! interpret `PlaceObject2`, `SetBackgroundColor`, shapes, sprites, fonts,
//! `ActionScript`, or anything else SWF can carry. See the crate's module
//! docs for why that is enough to read what `ffmpeg -f swf` writes.

use crate::header::SwfHeader;
use crate::tags::{
    TAG_DEFINE_VIDEO_STREAM, TAG_END, TAG_SHOW_FRAME, TAG_SOUND_STREAM_BLOCK,
    TAG_SOUND_STREAM_HEAD, TAG_SOUND_STREAM_HEAD2, TAG_VIDEO_FRAME, TagHeader,
};
use std::collections::VecDeque;
use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// SWF tags have no keyframe index of their own; the core may still build a
/// generic one from the packets it reads.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

/// `SWF` video codec-ID byte -> `CodecId`. Measured: `ffmpeg -f swf` with
/// `-c:v flv1` writes `2`. The others are SWF's own well-known values
/// (screen video = 3, VP6 = 4/5, screen video 2 = 6) but this crate has not
/// measured a real sample of any of them, so they map to `None` (recognised,
/// refused) rather than a guess.
pub(crate) fn video_codec_from_swf(id: u8) -> Option<CodecId> {
    match id {
        2 => Some(CodecId::Flv1),
        _ => None,
    }
}

pub(crate) fn video_codec_to_swf(id: CodecId) -> Option<u8> {
    match id {
        CodecId::Flv1 => Some(2),
        _ => None,
    }
}

/// `SoundStreamHead(2)`'s `StreamSoundCompression` nibble -> `CodecId`.
/// Measured: `-c:a mp3` writes `2`. `0` (uncompressed PCM) is the standard's
/// own value and trivial to decode correctly (raw samples, no frame
/// header), so it is included; everything else (ADPCM, Nellymoser, Speex)
/// is recognised but not decoded — same reasoning as the video table.
pub(crate) fn audio_codec_from_swf(compression: u8) -> Option<CodecId> {
    match compression {
        0 => Some(CodecId::PcmS16le),
        2 => Some(CodecId::Mp3),
        _ => None,
    }
}

pub(crate) fn audio_codec_to_swf(id: CodecId) -> Option<u8> {
    match id {
        CodecId::PcmS16le => Some(0),
        CodecId::Mp3 => Some(2),
        _ => None,
    }
}

/// `StreamSoundRate`'s 2-bit field -> Hz. Fixed by the SWF specification,
/// not a per-file measurement — same status as a container's own magic
/// numbers.
pub const SOUND_RATES: [u32; 4] = [5_512, 11_025, 22_050, 44_100];

/// The video stream this demuxer declares, and the running state needed to
/// keep reading `VideoFrame` tags for it.
struct VideoState {
    character_id: u16,
    codec: Option<CodecId>,
}

struct AudioState {
    codec: Option<CodecId>,
    /// Running sample position, for `pts`/`duration` — `SoundStreamBlock`
    /// carries no frame number of its own, only a sample count per block.
    samples_so_far: u64,
}

/// The `swf` demuxer.
pub struct SwfDemuxer {
    io: IoContext,
    header: SwfHeader,
    streams: Vec<Stream>,
    video: Option<VideoState>,
    audio: Option<AudioState>,
    budget: Budget,
    queue: VecDeque<Packet>,
    eof: bool,
}

impl std::fmt::Debug for SwfDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwfDemuxer")
            .field("header", &self.header)
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

/// Longest tag payload this demuxer will read in one shot before treating
/// the declared length as suspect. `u32`'s own range already caps this at 4
/// GiB; this is the second, much tighter bound `vaco_limits::Budget` (via
/// `Packet::alloc`) enforces against the real allocation ceiling — the tag
/// length is attacker-controlled input, read straight from the stream.
const MAX_REASONABLE_TAG: u32 = 64 * 1024 * 1024;

impl SwfDemuxer {
    /// Open a `swf` file.
    ///
    /// # Errors
    /// [`Error::InvalidData`]/[`Error::Unsupported`] as [`SwfHeader::parse`];
    /// [`Error::Eof`] on an empty input.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        Self::open_with_limits(src, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling.
    ///
    /// # Errors
    /// As [`SwfDemuxer::open`].
    pub fn open_with_limits(src: Box<dyn MediaSource>, limits: Limits) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        // The header's `RECT` is variable-length, so this peeks generously
        // (SWF headers are always small) rather than growing a loop the way
        // a tag-length line scan would.
        let peeked = io.peek(4096)?;
        if peeked.is_empty() {
            return Err(Error::Eof);
        }
        let (header, header_len) = SwfHeader::parse(peeked)?;
        io.skip(header_len as u64)?;

        let mut demux = Self {
            io,
            header,
            streams: Vec::new(),
            video: None,
            audio: None,
            budget: Budget::new(limits),
            queue: VecDeque::new(),
            eof: false,
        };
        // Read until the first media packet (or `End`), so `streams()`
        // never answers with an empty list before a packet has been read —
        // see `planning/AGENT-CONSTRAINTS.md` on an empty collection at
        // construction not being an answer.
        while demux.queue.is_empty() && !demux.eof {
            demux.read_one_tag()?;
        }
        Ok(demux)
    }

    fn frame_time_base(&self) -> Rational {
        Rational {
            num: 256,
            den: i32::from(self.header.frame_rate_raw),
        }
    }

    fn video_stream_index(&self) -> Option<u32> {
        self.streams
            .iter()
            .find(|s| s.media_type() == Some(MediaType::Video))
            .map(|s| s.index)
    }

    fn audio_stream_index(&self) -> Option<u32> {
        self.streams
            .iter()
            .find(|s| s.media_type() == Some(MediaType::Audio))
            .map(|s| s.index)
    }

    /// Read and dispatch exactly one tag, pushing zero or more packets onto
    /// `self.queue`. Returns `Ok(())` even for a tag this crate skips —
    /// `Err(Error::Eof)` means the tag stream genuinely ended (an `End` tag
    /// or running out of input).
    fn read_one_tag(&mut self) -> Result<()> {
        let peeked = self.io.peek(6)?;
        if peeked.is_empty() {
            self.eof = true;
            return Err(Error::Eof);
        }
        let th = TagHeader::parse(peeked)?;
        if th.len > MAX_REASONABLE_TAG {
            return Err(Error::InvalidData(
                "swf: tag declares an implausibly large payload",
            ));
        }
        self.io.skip(u64::from(th.header_len))?;
        if th.code == TAG_END {
            self.eof = true;
            return Err(Error::Eof);
        }
        if th.code == TAG_SHOW_FRAME {
            return Ok(());
        }

        let needs_payload = matches!(
            th.code,
            TAG_DEFINE_VIDEO_STREAM
                | TAG_VIDEO_FRAME
                | TAG_SOUND_STREAM_HEAD
                | TAG_SOUND_STREAM_HEAD2
                | TAG_SOUND_STREAM_BLOCK
        );
        if !needs_payload {
            self.io.skip(u64::from(th.len))?;
            return Ok(());
        }

        let mut payload = self.budget.alloc::<u8>(th.len as usize)?;
        self.io.read_exact(&mut payload)?;

        match th.code {
            TAG_DEFINE_VIDEO_STREAM => self.on_define_video_stream(&payload)?,
            TAG_VIDEO_FRAME => self.on_video_frame(&payload)?,
            TAG_SOUND_STREAM_HEAD | TAG_SOUND_STREAM_HEAD2 => self.on_sound_stream_head(&payload)?,
            TAG_SOUND_STREAM_BLOCK => self.on_sound_stream_block(&payload)?,
            _ => {}
        }
        Ok(())
    }

    fn on_define_video_stream(&mut self, payload: &[u8]) -> Result<()> {
        // Only the first `DefineVideoStream` declares a stream. A second
        // one is not "another video track" (SWF's own semantics would need
        // a matching `PlaceObject2`/`VideoFrame` referencing its own
        // `CharacterID` to mean that, which this crate does not model —
        // see the crate docs); silently accepting it would let a crafted
        // file push an unbounded number of `Stream`s, one per repeated tag,
        // an allocation this demuxer never validated against any limit.
        if self.video.is_some() {
            return Ok(());
        }
        let character_id = u16::from_le_bytes(
            payload.get(0..2).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        let num_frames = u16::from_le_bytes(
            payload.get(2..4).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        let width = u16::from_le_bytes(
            payload.get(4..6).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        let height = u16::from_le_bytes(
            payload.get(6..8).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        let codec_byte = *payload.get(9).ok_or(Error::UnexpectedEof)?;
        let codec = video_codec_from_swf(codec_byte);

        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut params = CodecParameters::new(MediaType::Video);
        if let Some(c) = codec {
            params = params.with_codec(c);
        }
        params.video = Some(VideoParameters {
            width: u32::from(width),
            height: u32::from(height),
            coded_width: u32::from(width),
            coded_height: u32::from(height),
            frame_rate: self.header.frame_rate(),
            ..VideoParameters::default()
        });
        let mut stream = Stream::new(index, MediaType::Video, self.frame_time_base());
        stream.params = params;
        stream.frame_count = Some(u64::from(num_frames));
        self.streams.push(stream);
        self.video = Some(VideoState {
            character_id,
            codec,
        });
        Ok(())
    }

    fn on_video_frame(&mut self, payload: &[u8]) -> Result<()> {
        let Some(video) = &self.video else {
            // A `VideoFrame` with no preceding `DefineVideoStream` is
            // malformed; skip it rather than fabricate a stream from
            // nothing.
            return Ok(());
        };
        let stream_id = u16::from_le_bytes(
            payload.get(0..2).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        if stream_id != video.character_id {
            return Ok(());
        }
        let frame_num = u16::from_le_bytes(
            payload.get(2..4).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        if video.codec.is_none() {
            return Err(Error::Unsupported("swf: unrecognised video codec"));
        }
        let Some(index) = self.video_stream_index() else {
            return Ok(());
        };
        let data = payload.get(4..).unwrap_or(&[]);
        let mut pkt = Packet::from_slice(&mut self.budget, data)?;
        pkt.stream_index = index;
        pkt.pts = Timestamp::new(i64::from(frame_num));
        pkt.dts = pkt.pts;
        pkt.duration = Duration::ZERO; // filled by the caller from time_base if needed
        pkt.flags |= PacketFlags::KEY; // FLV1/Sorenson frames are read independently here
        self.queue.push_back(pkt);
        Ok(())
    }

    fn on_sound_stream_head(&mut self, payload: &[u8]) -> Result<()> {
        // Only the first sound-stream-head tag declares a stream, for the
        // same unbounded-growth reason as `on_define_video_stream`.
        if self.audio.is_some() {
            return Ok(());
        }
        let byte1 = *payload.get(1).ok_or(Error::UnexpectedEof)?;
        let compression = byte1 >> 4;
        let rate_idx = usize::from((byte1 >> 2) & 0b11);
        let is_stereo = byte1 & 1 != 0;
        let sample_rate = *SOUND_RATES.get(rate_idx).unwrap_or(&44_100);
        let channels: u16 = if is_stereo { 2 } else { 1 };
        let codec = audio_codec_from_swf(compression);

        let index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut params = CodecParameters::new(MediaType::Audio);
        if let Some(c) = codec {
            params = params.with_codec(c);
        }
        params.audio = Some(AudioParameters {
            sample_rate,
            layout: ChannelLayout::default_for(u32::from(channels)),
            ..AudioParameters::default()
        });
        let time_base = Rational {
            num: 1,
            den: sample_rate.cast_signed(),
        };
        let mut stream = Stream::new(index, MediaType::Audio, time_base);
        stream.params = params;
        self.streams.push(stream);
        self.audio = Some(AudioState {
            codec,
            samples_so_far: 0,
        });
        Ok(())
    }

    fn on_sound_stream_block(&mut self, payload: &[u8]) -> Result<()> {
        let Some(codec) = self.audio.as_ref().and_then(|a| a.codec) else {
            return if self.audio.is_some() {
                Err(Error::Unsupported("swf: unrecognised audio codec"))
            } else {
                Ok(())
            };
        };
        let Some(index) = self.audio_stream_index() else {
            return Ok(());
        };
        let sample_count = u16::from_le_bytes(
            payload.get(0..2).and_then(|s| s.try_into().ok()).ok_or(Error::UnexpectedEof)?,
        );
        // Only MP3 carries the 2-byte `SeekSamples` field after the sample
        // count (measured: this is what `ffmpeg -c:a mp3` writes); PCM's
        // block is samples straight after the count.
        let data_start = if codec == CodecId::Mp3 { 4 } else { 2 };
        let data = payload.get(data_start..).unwrap_or(&[]);
        let samples_so_far = self.audio.as_ref().map_or(0, |a| a.samples_so_far);

        let mut pkt = Packet::from_slice(&mut self.budget, data)?;
        pkt.stream_index = index;
        pkt.pts = Timestamp::new(samples_so_far.cast_signed());
        pkt.dts = pkt.pts;
        pkt.duration = Duration::ZERO;
        pkt.flags |= PacketFlags::KEY;
        if let Some(audio) = &mut self.audio {
            audio.samples_so_far = audio.samples_so_far.saturating_add(u64::from(sample_count));
        }
        self.queue.push_back(pkt);
        Ok(())
    }
}

impl Demuxer for SwfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            match self.read_one_tag() {
                Ok(()) => {}
                Err(Error::Eof) => {
                    self.eof = true;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}
