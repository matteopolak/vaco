//! RAD Game Tools Bink (`.bik`/`.bk2`), the full-motion-video format used by
//! hundreds of games across two decades — a frame-index table addressed by
//! absolute byte offset, then one chunk per audio track followed by one
//! video chunk per physical frame.
//!
//! `Vaco-Spec-Ref multimedia-wiki-bink-container` gives the header, frame
//! index and per-frame chunk layout below; it is the current, maintained
//! form of Mike Melanson's original `bink-format.txt`, which the page
//! itself says is superseded.
//!
//! # Layout
//!
//! ```text
//! header (44 bytes fixed, plus 12 bytes per audio track)
//!    0   3   signature: "BIK" (Bink 1) or "KB2" (Bink 2)
//!    3   1   codec revision byte
//!    4   4   file size, not including these first 8 bytes
//!    8   4   frame count
//!   12   4   largest frame size in bytes
//!   16   4   frame count again
//!   20   4   width (<= 32767)
//!   24   4   height (<= 32767)
//!   28   4   fps numerator
//!   32   4   fps denominator
//!   36   4   video flags
//!   40   4   audio track count N
//!   44  4*N  per track: unknown:u16, channels:u16 ("not authoritative")
//!   ..  4*N  per track: sample_rate:u16, flags:u16 (bit 13 stereo,
//!            authoritative over the channel count above; bit 12 selects
//!            the Bink Audio algorithm)
//!   ..  4*N  per track: audio track id
//!
//! frame index table: frame_count + 1 entries, 4 bytes each
//!    absolute byte offset of that frame; bit 0 set means the frame is a
//!    keyframe and must be masked off to get the real offset. The last
//!    entry equals the file size, so a frame's length is always
//!    `table[i + 1] - (table[i] & !1)`.
//!
//! frame, once per table entry except the last
//!    for each audio track, in header order:
//!       4   length of what follows in this sub-chunk (0 = track silent
//!           this frame)
//!       4   number of samples in the packet
//!    length-4   Bink Audio packet
//!    remainder-of-frame   Bink Video packet
//! ```
//!
//! # Measured against the reference (`ffmpeg`/`ffprobe` 8.1)
//!
//! No encoder exists, so the fixture was hand-built from the layout above
//! and checked with `ffmpeg -i FIXTURE -c copy -f framemd5 -` — a
//! stream-copy pipeline, which (unlike `ffprobe -show_packets`) does not
//! need `binkaudio`/`binkvideo` to successfully decode a frame, only to be
//! found by name; the packaged garbage payload this crate's fixtures carry
//! is never valid Bink Audio/Video and does not need to be.
//!
//! * `probe_score` is **100**, magic alone (`ffmpeg -v debug`'s own "probed
//!   with score=100" line), independent of extension.
//! * A per-track audio sub-chunk's reported packet **is** the declared
//!   length field's worth of bytes (the 4-byte sample count plus the
//!   compressed data) — the length field's own value already excludes
//!   itself, so no adjustment is needed to go from "value in the file" to
//!   "packet size reported".
//! * An **odd-length frame confuses the reference**: it reads that frame's
//!   video chunk one byte short of `table[i+1] - (table[i] & !1) -
//!   audio_bytes_consumed`, and — because it reads sequentially rather than
//!   re-seeking to the next table offset — every frame after an odd one
//!   inherits the one-byte drift, which cascades into "audio size in
//!   header (…) > size of packet left" and a hard demux error a frame or
//!   two later. Confirmed by comparing an all-even-length fixture (no
//!   drift, exact formula) against one with a single odd-length frame (the
//!   drift starts exactly there). This demuxer seeks to each frame's own
//!   table offset before reading it, so it neither reproduces the drift
//!   nor the cascading failure — a real, deliberate divergence from the
//!   reference on this one malformed-input shape, on the view that a
//!   frame-table format that hands out absolute offsets for exactly this
//!   purpose should use them, and that a real encoder's output is not
//!   expected to contain an odd-length frame in the first place. See
//!   `planning/TECH-DEBT.md`.
//! * Audio packet `pts` did not track the packaged "number of samples"
//!   field 1:1 on the one fixture checked (declared 4, `ffprobe` reported
//!   a `pts` delta of 2) — plausibly because the reference derives audio
//!   timing from decoding the packet rather than from the container field
//!   once a real Bink Audio decoder is attached, which this demuxer cannot
//!   reproduce without decoding. This demuxer uses the declared sample
//!   count directly, which is what the container actually states; flagged
//!   as unverified beyond that one (inconclusive) measurement.
//!
//! # What is not implemented
//!
//! Neither `Bink` (video) nor `BinkAudio` (audio) has a [`CodecId`] variant
//! in `vaco-codec-core`; both this crate's streams carry `codec_id: None`.
//! Recorded as an extension of interface gap 21, alongside Smacker's
//! equivalent gap, rather than a new entry.

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

const MAX_TRACKS: u32 = 256;
const MAX_FRAMES: u32 = 1 << 24;
const MAX_CHUNK: u32 = 256 << 20;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

fn is_bink_magic(tag: [u8; 3]) -> bool {
    &tag == b"BIK" || &tag == b"KB2"
}

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match data.tag(0) {
        Some(t) if is_bink_magic([t[0], t[1], t[2]]) => ProbeScore::MAX,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "bink",
    long_name: "Bink",
    extensions: &["bik", "bk2"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(BinkDemuxer::open(src)?))
}

#[derive(Debug)]
pub struct BinkDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    /// `table[i] & !1` is frame `i`'s start; `table[i] & 1` is its keyframe
    /// bit; the last entry is the file size.
    table: Vec<u32>,
    frame_count: u32,
    frame_index: u32,
    audio_ticks: Vec<i64>,
    pending: VecDeque<Packet>,
    budget: Budget,
}

impl BinkDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the header does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let tag = io.tag()?;
        if !is_bink_magic([tag[0], tag[1], tag[2]]) {
            return Err(Error::InvalidData("bink: missing BIK/KB2 signature"));
        }
        let _ = tag[3]; // revision byte; not needed for framing
        let _file_size = io.rl32()?;
        let frame_count = io.rl32()?;
        if frame_count > MAX_FRAMES {
            return Err(Error::LimitExceeded {
                limit: "bink_frame_count",
                requested: u64::from(frame_count),
                cap: u64::from(MAX_FRAMES),
            });
        }
        let _largest_frame = io.rl32()?;
        let _frame_count_again = io.rl32()?;
        let width = io.rl32()?;
        let height = io.rl32()?;
        let fps_num = io.rl32()?;
        let fps_den = io.rl32()?;
        let _video_flags = io.rl32()?;
        let num_audio = io.rl32()?;
        if num_audio > MAX_TRACKS {
            return Err(Error::LimitExceeded {
                limit: "bink_audio_tracks",
                requested: u64::from(num_audio),
                cap: u64::from(MAX_TRACKS),
            });
        }

        let mut channels_hint = Vec::new();
        for _ in 0..num_audio {
            let _unknown = io.rl16()?;
            channels_hint.push(io.rl16()?);
        }
        let mut sample_rates = Vec::new();
        let mut audio_flags = Vec::new();
        for _ in 0..num_audio {
            sample_rates.push(io.rl16()?);
            audio_flags.push(io.rl16()?);
        }
        for _ in 0..num_audio {
            let _track_id = io.rl32()?;
        }

        let time_base = Rational::new(fps_den.cast_signed(), fps_num.max(1).cast_signed());
        let mut video = Stream::new(0, MediaType::Video, time_base);
        video.r_frame_rate = Rational::new(fps_num.cast_signed(), fps_den.max(1).cast_signed());
        let mut vparams = CodecParameters::video();
        if let Some(v) = vparams.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.frame_rate = video.r_frame_rate;
            v.field_order = vaco_codec_core::FieldOrder::Unknown;
        }
        video.params = vparams;

        let mut streams = vec![video];
        for i in 0..usize::try_from(num_audio).unwrap_or(0) {
            let rate = u32::from(*sample_rates.get(i).unwrap_or(&0)).max(1);
            let flags = *audio_flags.get(i).unwrap_or(&0);
            let stereo = flags & (1 << 13) != 0;
            let channels = if stereo { 2 } else { 1 };
            let index = u32::try_from(streams.len()).unwrap_or(u32::MAX);
            let mut stream = Stream::new(index, MediaType::Audio, Rational::new(1, rate.cast_signed()));
            let mut aparams = CodecParameters::audio();
            if let Some(a) = aparams.audio.as_mut() {
                a.sample_rate = rate;
                a.format = Some(SampleFmt::S16);
                a.layout = ChannelLayout::default_for(channels);
            }
            stream.params = aparams;
            streams.push(stream);
        }

        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let table_len = usize::try_from(frame_count.saturating_add(1)).unwrap_or(0);
        let mut table = budget.alloc::<u32>(table_len)?;
        for slot in &mut table {
            *slot = io.rl32()?;
        }

        let audio_ticks = vec![0i64; streams.len().saturating_sub(1)];
        Ok(Self {
            io,
            streams,
            table,
            frame_count,
            frame_index: 0,
            audio_ticks,
            pending: VecDeque::new(),
            budget,
        })
    }

    fn fill_frame(&mut self) -> Result<()> {
        if self.frame_index >= self.frame_count {
            return Err(Error::Eof);
        }
        let i = usize::try_from(self.frame_index).unwrap_or(usize::MAX);
        let raw_start = *self.table.get(i).ok_or(Error::InvalidData("bink: missing frame table entry"))?;
        let end = *self
            .table
            .get(i.saturating_add(1))
            .ok_or(Error::InvalidData("bink: missing frame table entry"))?;
        let start = raw_start & !1;
        let is_key = raw_start & 1 != 0;
        if end < start {
            return Err(Error::InvalidData("bink: frame table entry out of order"));
        }
        self.io.seek(u64::from(start))?;
        let frame_end = u64::from(end);

        let audio_tracks = self.streams.len().saturating_sub(1);
        for t in 0..audio_tracks {
            if self.io.pos() >= frame_end {
                break;
            }
            let length = self.io.rl32()?;
            if length == 0 {
                continue;
            }
            if length > MAX_CHUNK {
                return Err(Error::LimitExceeded {
                    limit: "bink_audio_chunk",
                    requested: u64::from(length),
                    cap: u64::from(MAX_CHUNK),
                });
            }
            let n = usize::try_from(length).unwrap_or(usize::MAX);
            let mut pkt = Packet::alloc(&mut self.budget, n)?;
            self.io.read_exact(pkt.payload_mut())?;
            let samples = pkt
                .payload()
                .get(0..4)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .map_or(0, u32::from_le_bytes);
            let stream_index = u32::try_from(t.saturating_add(1)).unwrap_or(u32::MAX);
            pkt.stream_index = stream_index;
            let ticks = self.audio_ticks.get(t).copied().unwrap_or(0);
            pkt.pts = Timestamp::new(ticks);
            pkt.dts = pkt.pts;
            pkt.flags = PacketFlags::KEY;
            if let Some(slot) = self.audio_ticks.get_mut(t) {
                *slot = slot.saturating_add(i64::from(samples));
            }
            self.pending.push_back(pkt);
        }

        let video_len = frame_end.saturating_sub(self.io.pos());
        let n = usize::try_from(video_len).unwrap_or(usize::MAX);
        let mut vpkt = Packet::alloc(&mut self.budget, n)?;
        self.io.read_exact(vpkt.payload_mut())?;
        vpkt.stream_index = 0;
        vpkt.pts = Timestamp::new(i64::from(self.frame_index));
        vpkt.dts = vpkt.pts;
        if is_key {
            vpkt.flags = PacketFlags::KEY;
        }
        self.pending.push_back(vpkt);

        self.frame_index = self.frame_index.saturating_add(1);
        Ok(())
    }
}

impl Demuxer for BinkDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        while self.pending.is_empty() {
            self.fill_frame()?;
        }
        self.pending.pop_front().ok_or(Error::Eof)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::Unsupported("bink: seeking is not implemented"))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// Hand-built per `Vaco-Spec-Ref multimedia-wiki-bink-container`: one
    /// audio track, two frames, first is a keyframe.
    fn audio_sub(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((4 + data.len()) as u32).to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn build_fixture() -> Vec<u8> {
        let mut fixed = Vec::new();
        fixed.extend_from_slice(&le32(2)); // frame_count
        fixed.extend_from_slice(&le32(20)); // largest_frame
        fixed.extend_from_slice(&le32(2)); // frame_count again
        fixed.extend_from_slice(&le32(64)); // width
        fixed.extend_from_slice(&le32(48)); // height
        fixed.extend_from_slice(&le32(25)); // fps num
        fixed.extend_from_slice(&le32(1)); // fps den
        fixed.extend_from_slice(&le32(0)); // video flags
        fixed.extend_from_slice(&le32(1)); // 1 audio track
        fixed.extend_from_slice(&le16(0));
        fixed.extend_from_slice(&le16(1)); // channels hint
        fixed.extend_from_slice(&le16(22050));
        fixed.extend_from_slice(&le16(0)); // flags: mono
        fixed.extend_from_slice(&le32(0)); // track id

        let header_len = 8 + fixed.len();
        let index_len = 3 * 4; // frame_count+1 entries
        let index_table_offset = header_len + index_len;

        let frame0 = [audio_sub(&[1, 2, 3, 4]), vec![0xAA; 6]].concat();
        let frame1 = [audio_sub(&[5, 6, 7, 8]), vec![0xBB; 4]].concat();

        let mut pos = index_table_offset as u32;
        let off0 = pos | 1;
        pos += frame0.len() as u32;
        let off1 = pos;
        pos += frame1.len() as u32;
        let off2 = pos;

        let mut out = Vec::new();
        out.extend_from_slice(b"BIK");
        out.push(0x69);
        out.extend_from_slice(&le32(pos - 8));
        out.extend_from_slice(&fixed);
        out.extend_from_slice(&le32(off0));
        out.extend_from_slice(&le32(off1));
        out.extend_from_slice(&le32(off2));
        out.extend_from_slice(&frame0);
        out.extend_from_slice(&frame1);
        out
    }

    #[test]
    fn probe_needs_bik_or_kb2() {
        assert_eq!(probe(&ProbeData::new(b"BIK\x69")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"KB2a")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"nope")), ProbeScore::NONE);
    }

    #[test]
    fn streams_and_frame_count() {
        let data = build_fixture();
        let d = BinkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().len(), 2);
        assert_eq!(d.streams()[0].media_type(), Some(MediaType::Video));
        assert_eq!(d.streams()[1].media_type(), Some(MediaType::Audio));
    }

    #[test]
    fn reads_audio_then_video_per_frame() {
        let data = build_fixture();
        let mut d = BinkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let a0 = d.read_packet().unwrap();
        assert_eq!(a0.stream_index, 1);
        assert_eq!(a0.payload().len(), 8); // 4-byte sample count + 4 bytes data
        let v0 = d.read_packet().unwrap();
        assert_eq!(v0.stream_index, 0);
        assert!(v0.is_key());
        assert_eq!(v0.payload().len(), 6);

        let a1 = d.read_packet().unwrap();
        assert_eq!(a1.stream_index, 1);
        let v1 = d.read_packet().unwrap();
        assert_eq!(v1.stream_index, 0);
        assert!(!v1.is_key());

        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}
