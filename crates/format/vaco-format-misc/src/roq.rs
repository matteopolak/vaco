//! id Software's `RoQ`, the full-motion-video format used by Quake III Arena,
//! Return to Castle Wolfenstein and (originally) The 11th Hour.
//!
//! `Vaco-Spec-Ref idroq-format-doc` (Dr. Tim Ferguson's reverse-engineered
//! description, the format's only public technical writeup — id Software
//! never published one) gives the chunk framing this module needs. It does
//! **not** give the video codec's own bitstream, which this demuxer never
//! reads.
//!
//! # Layout
//!
//! Eight magic bytes, then a flat sequence of chunks:
//!
//! ```text
//!   0  2   chunk id
//!   2  4   payload length (the signature chunk uses 0xFFFFFFFF here and has
//!          no payload; its "argument" carries the frame rate instead)
//!   6  2   chunk argument, meaning depends on the id
//!   8  …   payload
//! ```
//!
//! Chunk ids this module recognises: `0x1084` signature, `0x1001` `RoQ_INFO`
//! (width/height), `0x1002` `RoQ_QUAD_CODEBOOK`, `0x1011` `RoQ_QUAD_VQ`,
//! `0x1020`/`0x1021` `RoQ_SOUND_MONO`/`RoQ_SOUND_STEREO`. Anything else
//! (`RoQ_JPEG`, `RoQ_HANG`, `RoQ_PACKET`, or a chunk id nobody has documented)
//! is treated the same as `RoQ_QUAD_CODEBOOK` — accumulated, never
//! interpreted — which is the same "unknown chunk, keep the framing lenient"
//! stance the rest of this family takes.
//!
//! # Packetisation, measured against the reference (`ffprobe` 8.1)
//!
//! No official encoder exists to produce a real `.roq` file, so every
//! fixture here is **hand-built from the chunk grammar above** and then
//! fed to `ffprobe` to observe how the reference actually splits it into
//! streams and packets — a black-box measurement of chunk *framing*, not of
//! the video/audio bitstream, which this module never decodes.
//!
//! * A video packet is the concatenation of every whole chunk (its header
//!   plus payload, unmodified) collected since the last flush, up to and
//!   including a `RoQ_QUAD_VQ` chunk. `RoQ_QUAD_CODEBOOK` immediately
//!   followed by `RoQ_QUAD_VQ` therefore becomes **one** packet, not two —
//!   confirmed by comparing chunk offsets against reported packet
//!   `size`/`pos`.
//! * A `RoQ_SOUND_MONO`/`RoQ_SOUND_STEREO` chunk becomes its **own** audio
//!   packet (again, whole chunk bytes, unmodified) **only when nothing is
//!   currently accumulating for the video packet.** Ordering a sound chunk
//!   between a codebook and its VQ chunk — legal by the chunk grammar, just
//!   not the order id Software's own encoder writes — makes it vanish into
//!   the *video* packet's bytes instead and no audio stream is created at
//!   all. Measured by building the same three-chunk group both ways: sound
//!   first (audio stream present, two packets per group) and sound between
//!   codebook and VQ (no audio stream, one 30-byte video packet per group
//!   where the sound-first ordering gives a 12-byte audio packet plus an
//!   18-byte video one). This is exactly the shape plan 18's probing traps
//!   describe — the frame's byte order deciding which object a value lands
//!   on — just at the chunk level instead of the option-parsing level.
//! * Audio's sample rate is a fixed **22050 Hz**, stated nowhere in the
//!   chunk and only recoverable by asking the reference.
//! * The video stream's `r_frame_rate` and `avg_frame_rate` both come out
//!   as the signature chunk's argument over 1 — unlike `ivf`, which leaves
//!   `avg_frame_rate` at `0/0`. Two containers in the same family disagree
//!   here, so this was not assumed from the other one.
//! * Every audio packet reports `flags=K`; every video packet in the
//!   synthetic fixtures reports no flags at all. The video side is reported
//!   as measured rather than as a general codec fact, since the fixture's
//!   `RoQ_QUAD_VQ` payload is not a real encoded frame and this module has
//!   no way to ask the reference to encode one.
//!
//! # What is not implemented
//!
//! Neither `roq` (video) nor `roq_dpcm` (audio) has a [`CodecId`] variant in
//! `vaco-codec-core` today — this family has no game-video codec ids at all
//! yet. Both streams therefore carry `codec_id: None`; `-show_streams` will
//! print `codec_name=unknown` where the reference names a codec. Reported
//! separately as an interface gap rather than worked around here.

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};
use vaco_sampfmt::SampleFmt;

const SIGNATURE_ID: u16 = 0x1084;
const INFO_ID: u16 = 0x1001;
const QUAD_VQ_ID: u16 = 0x1011;
const SOUND_MONO_ID: u16 = 0x1020;
const SOUND_STEREO_ID: u16 = 0x1021;

/// Fixed by the format, not stated in any chunk — measured against
/// `ffprobe` 8.1's `sample_rate` on every `RoQ_SOUND_*` fixture tried.
const AUDIO_SAMPLE_RATE: u32 = 22050;

const MAX_CHUNK: u32 = 64 << 20;
/// Bound on how many chunks [`RoqDemuxer::open`] will scan looking for the
/// first video packet before giving up on discovering an audio stream that
/// might start later in the file. Generous for any real `RoQ` file, which
/// interleaves audio every frame.
const MAX_LOOKAHEAD_CHUNKS: u32 = 4096;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.rl16(0) == Some(SIGNATURE_ID) && data.rl32(2) == Some(u32::MAX) {
        ProbeScore::MAX
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "roq",
    long_name: "id RoQ",
    extensions: &["roq"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(RoqDemuxer::open(src)?))
}

#[derive(Debug)]
struct ChunkHeader {
    id: u16,
    size: u32,
    arg: u16,
}

#[derive(Debug)]
pub struct RoqDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    audio_index: Option<u32>,
    video_accum: Vec<u8>,
    video_accum_pos: Option<u64>,
    video_frame: i64,
    audio_sample: i64,
    pending: VecDeque<Packet>,
    budget: Budget,
    eof: bool,
}

impl RoqDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the signature does not match.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        if io.rl16()? != SIGNATURE_ID {
            return Err(Error::InvalidData("roq: missing signature chunk id"));
        }
        if io.rl32()? != u32::MAX {
            return Err(Error::InvalidData("roq: signature chunk has a payload"));
        }
        let arg = io.rl16()?;
        let fps = u32::from(arg).max(1);

        let mut video = Stream::new(0, MediaType::Video, Rational::new(1, fps.cast_signed()));
        video.r_frame_rate = Rational::new(fps.cast_signed(), 1);
        video.avg_frame_rate = video.r_frame_rate;
        video.params.video = Some(vaco_codec_core::VideoParameters {
            field_order: vaco_codec_core::FieldOrder::Unknown,
            ..Default::default()
        });
        let mut me = Self {
            io,
            streams: vec![video],
            audio_index: None,
            video_accum: Vec::new(),
            video_accum_pos: None,
            video_frame: 0,
            audio_sample: 0,
            pending: VecDeque::new(),
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        };

        let mut chunks = 0u32;
        while me.pending.is_empty() && !me.eof && chunks < MAX_LOOKAHEAD_CHUNKS {
            me.advance()?;
            chunks += 1;
        }
        Ok(me)
    }

    fn read_chunk_header(&mut self) -> Result<Option<ChunkHeader>> {
        let id = match self.io.rl16() {
            Ok(v) => v,
            Err(Error::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        let size = self.io.rl32()?;
        let arg = self.io.rl16()?;
        if size > MAX_CHUNK {
            return Err(Error::LimitExceeded {
                limit: "roq_chunk",
                requested: u64::from(size),
                cap: u64::from(MAX_CHUNK),
            });
        }
        Ok(Some(ChunkHeader { id, size, arg }))
    }

    /// Read one more chunk from the source and update accumulation/pending
    /// state accordingly. A no-op on `pending` is possible (an `INFO` chunk
    /// updates dimensions and produces nothing); callers loop until either
    /// `pending` gains an entry or [`Self::eof`] is set.
    fn advance(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        let pos = self.io.pos();
        let Some(header) = self.read_chunk_header()? else {
            self.eof = true;
            if !self.video_accum.is_empty() {
                // A partial group at EOF: not a full RoQ_QUAD_VQ-terminated
                // packet, so it is dropped rather than guessed at.
                self.video_accum.clear();
                self.video_accum_pos = None;
            }
            return Ok(());
        };
        // Measured: a lone-chunk audio packet's `pos` is the payload's start
        // (after the 8-byte chunk header), while a multi-chunk video
        // packet's `pos` is the *group's* first chunk header — two
        // different code paths in the reference, reproduced as two
        // different capture points here rather than forced to agree.
        let payload_pos = self.io.pos();
        let n = usize::try_from(header.size).unwrap_or(usize::MAX);
        let mut payload = self.budget.alloc::<u8>(n)?;
        self.io.read_exact(&mut payload)?;

        if header.id == INFO_ID {
            if let (Some(&w0), Some(&w1), Some(&h0), Some(&h1)) =
                (payload.first(), payload.get(1), payload.get(2), payload.get(3))
            {
                let width = u32::from(u16::from_le_bytes([w0, w1]));
                let height = u32::from(u16::from_le_bytes([h0, h1]));
                if let Some(stream) = self.streams.first_mut()
                    && let Some(video) = stream.params.video.as_mut()
                {
                    video.width = width;
                    video.height = height;
                    video.coded_width = width;
                    video.coded_height = height;
                }
            }
            return Ok(());
        }

        let is_sound = header.id == SOUND_MONO_ID || header.id == SOUND_STEREO_ID;
        if is_sound && self.video_accum.is_empty() {
            let channels = if header.id == SOUND_STEREO_ID { 2u32 } else { 1 };
            let index = self.ensure_audio_stream(channels);
            let byte_count = i64::try_from(payload.len()).unwrap_or(i64::MAX);
            let samples = if channels == 2 { byte_count >> 1 } else { byte_count };
            let mut pkt = Packet::from_slice(&mut self.budget, chunk_bytes(&header, &payload).as_slice())?;
            pkt.stream_index = index;
            pkt.pts = Timestamp::new(self.audio_sample);
            pkt.dts = pkt.pts;
            pkt.pos = Some(payload_pos);
            pkt.duration = Timestamp::new(samples)
                .to_duration(Rational::new(1, AUDIO_SAMPLE_RATE.cast_signed()))
                .unwrap_or(vaco_core::Duration::ZERO);
            pkt.flags = PacketFlags::KEY;
            self.audio_sample = self.audio_sample.saturating_add(samples);
            self.pending.push_back(pkt);
            return Ok(());
        }

        if self.video_accum.is_empty() {
            self.video_accum_pos = Some(pos);
        }
        self.video_accum.extend_from_slice(&header.id.to_le_bytes());
        self.video_accum.extend_from_slice(&header.size.to_le_bytes());
        self.video_accum.extend_from_slice(&header.arg.to_le_bytes());
        self.video_accum.extend_from_slice(&payload);

        if header.id == QUAD_VQ_ID {
            let mut pkt = Packet::from_slice(&mut self.budget, &self.video_accum)?;
            self.video_accum.clear();
            pkt.stream_index = 0;
            pkt.pts = Timestamp::new(self.video_frame);
            pkt.dts = pkt.pts;
            pkt.pos = self.video_accum_pos.take();
            pkt.duration = self
                .streams
                .first()
                .map_or(vaco_core::Duration::ZERO, |s| {
                    Timestamp::new(1).to_duration(s.time_base).unwrap_or(vaco_core::Duration::ZERO)
                });
            self.video_frame = self.video_frame.saturating_add(1);
            self.pending.push_back(pkt);
        }
        Ok(())
    }

    fn ensure_audio_stream(&mut self, channels: u32) -> u32 {
        if let Some(idx) = self.audio_index {
            return idx;
        }
        let idx = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut stream = Stream::new(
            idx,
            MediaType::Audio,
            Rational::new(1, AUDIO_SAMPLE_RATE.cast_signed()),
        );
        let mut params = CodecParameters::audio();
        if let Some(audio) = params.audio.as_mut() {
            audio.sample_rate = AUDIO_SAMPLE_RATE;
            audio.format = Some(SampleFmt::S16);
            audio.layout = ChannelLayout::default_for(channels);
        }
        stream.params = params;
        self.streams.push(stream);
        self.audio_index = Some(idx);
        idx
    }
}

fn chunk_bytes(header: &ChunkHeader, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&header.id.to_le_bytes());
    out.extend_from_slice(&header.size.to_le_bytes());
    out.extend_from_slice(&header.arg.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

impl Demuxer for RoqDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        while self.pending.is_empty() && !self.eof {
            self.advance()?;
        }
        self.pending.pop_front().ok_or(Error::Eof)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported("roq: seeking is not implemented"))
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn chunk(id: u16, arg: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&id.to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&arg.to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn signature(fps: u16) -> Vec<u8> {
        let mut v = vec![0x84, 0x10, 0xff, 0xff, 0xff, 0xff];
        v.extend_from_slice(&fps.to_le_bytes());
        v
    }

    #[test]
    fn probe_needs_the_exact_signature() {
        assert_eq!(probe(&ProbeData::new(&signature(30))), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"not roq!")), ProbeScore::NONE);
    }

    /// Hand-built per `Vaco-Spec-Ref idroq-format-doc`: sound before the
    /// codebook, matching the document's own "typical" chunk order.
    #[test]
    fn sound_before_codebook_becomes_its_own_audio_stream() {
        let mut data = signature(30);
        data.extend_from_slice(&chunk(INFO_ID, 0, &[64, 0, 64, 0, 8, 0, 4, 0]));
        data.extend_from_slice(&chunk(SOUND_MONO_ID, 0, &[1, 2, 3, 4]));
        data.extend_from_slice(&chunk(0x1002, 0x0101, &[0; 10]));
        data.extend_from_slice(&chunk(QUAD_VQ_ID, 0, &[0; 4]));

        let mut d = RoqDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().len(), 2);
        assert_eq!(d.streams().first().unwrap().media_type(), Some(MediaType::Video));
        assert_eq!(d.streams().get(1).unwrap().media_type(), Some(MediaType::Audio));

        let audio = d.read_packet().unwrap();
        assert_eq!(audio.stream_index, 1);
        assert!(audio.is_key());
        assert_eq!(audio.payload().len(), 12);

        let video = d.read_packet().unwrap();
        assert_eq!(video.stream_index, 0);
        // codebook (8 + 10) + vq (8 + 4) concatenated whole.
        assert_eq!(video.payload().len(), 30);

        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    /// Same three chunks, sound moved between the codebook and the VQ chunk
    /// — legal chunk-id sequencing, just not the reference encoder's order.
    /// Measured: the sound bytes are absorbed into the video packet and no
    /// audio stream appears at all.
    #[test]
    fn sound_between_codebook_and_vq_has_no_audio_stream() {
        let mut data = signature(30);
        data.extend_from_slice(&chunk(INFO_ID, 0, &[64, 0, 64, 0, 8, 0, 4, 0]));
        data.extend_from_slice(&chunk(0x1002, 0x0101, &[0; 10]));
        data.extend_from_slice(&chunk(SOUND_MONO_ID, 0, &[1, 2, 3, 4]));
        data.extend_from_slice(&chunk(QUAD_VQ_ID, 0, &[0; 4]));

        let mut d = RoqDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().len(), 1);
        let video = d.read_packet().unwrap();
        assert_eq!(video.stream_index, 0);
        // codebook (18) + sound (12) + vq (12) all merged.
        assert_eq!(video.payload().len(), 42);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn width_and_height_come_from_the_info_chunk() {
        let mut data = signature(30);
        data.extend_from_slice(&chunk(INFO_ID, 0, &[128, 0, 96, 0, 8, 0, 4, 0]));
        data.extend_from_slice(&chunk(0x1002, 0x0101, &[0; 10]));
        data.extend_from_slice(&chunk(QUAD_VQ_ID, 0, &[0; 4]));
        let d = RoqDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let video = d.streams().first().unwrap().params.video.as_ref().unwrap();
        assert_eq!((video.width, video.height), (128, 96));
    }

    #[test]
    fn rejects_a_signature_with_a_real_payload() {
        let mut v = vec![0x84, 0x10, 0x01, 0x00, 0x00, 0x00, 0x1e, 0x00, 0x00];
        assert!(RoqDemuxer::open(Box::new(MemorySource::new(std::mem::take(&mut v)))).is_err());
    }
}
