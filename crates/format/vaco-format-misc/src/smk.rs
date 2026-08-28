//! RAD Game Tools Smacker (`.smk`) — the older sibling of Bink, with a fixed
//! 104-byte header, three side tables (per-frame size, per-frame content
//! flags, packed Huffman trees) read up front, then a flat run of frame
//! data with no further offsets: each frame's own (masked) size in the
//! table is what tells a reader where the next one starts.
//!
//! `Vaco-Spec-Ref multimedia-wiki-smacker` is the format's public
//! specification and gives every field and chunk layout below in full,
//! including the parts (Huffman tree bit-packing, block-type decoding) this
//! demuxer never needs, since none of it is required to find frame and
//! chunk boundaries.
//!
//! # Layout
//!
//! ```text
//! header (104 bytes)
//!    0   4   signature: "SMK2" or "SMK4"
//!    4   4   width
//!    8   4   height
//!   12   4   frame count (logical; see below)
//!   16   4   frame rate (i32; see the formula below)
//!   20   4   flags (bit 0: file has an extra "ring" frame not counted above)
//!   24  28   AudioSize[7]: largest unpacked buffer per track, informational
//!   52   4   TreesSize: bytes of packed Huffman tree data that follows the
//!            two tables below
//!   56  16   MMap_Size, MClr_Size, Full_Size, Type_Size: decoder table
//!            allocation hints, informational
//!   72  28   AudioRate[7]: one 32-bit descriptor per track (bit 31
//!            compressed, bit 30 present, bit 29 16-bit, bit 28 stereo,
//!            bits 23-0 sample rate)
//!  100   4   unused
//!
//! FrameSizes: one u32 per physical frame (frame count, plus one more if
//!   the ring-frame flag is set). Bit 0 set means a keyframe; bit 1 is
//!   reserved. Both must be masked off to get the real byte length — the
//!   masked value is also the frame's exact physical size on disk.
//!
//! FrameTypes: one byte per physical frame. Bit 0: a palette chunk opens
//!   the frame. Bits 1-7: audio data for tracks 0-6 respectively follows,
//!   in track order, after the palette chunk if any.
//!
//! HuffmanTrees: TreesSize raw bytes, opaque to this demuxer.
//!
//! frame, once per FrameSizes/FrameTypes entry
//!    [ palette chunk, if FrameTypes bit 0 ]
//!       1   Length: total palette chunk bytes, including this byte,
//!           divided by 4
//!    (Length*4)-1   palette change blocks (not decoded here)
//!    [ one chunk per active audio track, low bit to high ]
//!       4   Length: bytes of this chunk including this field
//!       4   UnpackedLength, only if that track's AudioRate bit 31 is set
//!    Length-4(-4)   audio data
//!    remainder-of-frame   video chunk
//! ```
//!
//! # Measured against the reference (`ffmpeg`/`ffprobe` 8.1)
//!
//! No encoder exists. `ffprobe -show_streams`/`-show_packets` need the
//! `smackvid` decoder to open even to report container-level facts, and it
//! refuses to open over a fixture with anything less than a fully valid
//! packed Huffman tree — building one by hand (per the spec's own "typical
//! tree initialization" bit-packing algorithm) got the decoder to open, but
//! its own audio/video packet *payload* turned out to package more than
//! this demuxer can derive from the public file-format spec alone (see
//! below), so the working measurement tool for this format was `ffmpeg -i
//! FIXTURE -c copy -f framemd5 -`, a stream-copy path that only needs the
//! codec to be *found*, not opened.
//!
//! * `probe_score` is **100** (`ffmpeg -v debug`'s "Format smk probed
//!   with... score=100"), magic alone.
//! * `extradata` is **not** just the `HuffmanTrees` bytes: measured at 26
//!   bytes against a 10-byte `TreesSize`, which is exactly `4×u32` (16
//!   bytes: `MMap_Size, MClr_Size, Full_Size, Type_Size`, in that order)
//!   followed by the tree bytes. This demuxer reproduces that concatenation
//!   exactly.
//! * An audio chunk's reported packet is its **data only** — the 4-byte
//!   `Length` field (and the `UnpackedLength` field, when present) are
//!   stripped, unlike Bink's equivalent chunk, which keeps its own
//!   4-byte sample-count field in the packet. The two formats are
//!   documented in the same paragraph on the wiki and still differ here;
//!   checked independently rather than assumed identical.
//! * Audio `time_base` measured as `1/(sample_rate × bytes_per_sample)`,
//!   **not** `1/sample_rate` — a 4-byte payload at 22050 Hz 16-bit mono
//!   produced `duration=4` at `tb=1/44100`, not `duration=2` at
//!   `tb=1/22050` (both describe the same 90.7 µs, so this is a choice of
//!   tick granularity, not a different number). Reproduced for the 16-bit
//!   mono case measured; the multiplier for stereo is applied by analogy
//!   (bytes per channel-interleaved frame) and has not been independently
//!   checked against a stereo fixture.
//! * The reference's **video packet is not the raw video chunk**: an
//!   otherwise-empty frame (a 4-byte palette chunk, nothing else) still
//!   produced a 769-byte video packet, and adding 8 real video-chunk bytes
//!   made it 777 — consistently `769 + n`. `769 = 1 + 256×3` strongly
//!   suggests a one-byte flag plus a synthesised 256-entry RGB palette
//!   table prepended ahead of the real video bytes, but three independent
//!   byte-layout guesses (flag value, prefix vs. suffix, an all-zero vs. a
//!   partially-set palette) were checked against the measured MD5 hash and
//!   all failed to reproduce it. Pinning the exact construction would mean
//!   probing well past "container framing" into a specific, undocumented
//!   internal packet convention, which was judged not worth further
//!   iteration (see `planning/TECH-DEBT.md`). **This demuxer's video
//!   packets are the raw video-chunk bytes only** — a real, measured,
//!   unresolved divergence from the reference's packet `size`/hash for
//!   video specifically, not a guess presented as fact.
//!
//! # `CodecId`
//!
//! The video stream is always `Smacker`. Each audio track's `AudioRate`
//! entry states whether its bytes are Smacker's own compressed audio
//! (`SmackAudio`) or raw, uncompressed PCM in disguise — measured against
//! the reference on both settings: an uncompressed track reports
//! `pcm_s16le`/`pcm_u8` (by the same bit-depth flag this module already
//! reads), never `smackaudio`.
//!
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

const NUM_TRACKS: usize = 7;
const MAX_PHYSICAL_FRAMES: u32 = 1 << 24;
const MAX_TREES: u32 = 64 << 20;
const MAX_CHUNK: u32 = 256 << 20;

const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

fn is_smk_magic(tag: [u8; 4]) -> bool {
    &tag == b"SMK2" || &tag == b"SMK4"
}

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    match data.tag(0) {
        Some(t) if is_smk_magic(t) => ProbeScore::MAX,
        _ => ProbeScore::NONE,
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "smk",
    long_name: "Smacker",
    extensions: &["smk"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(SmkDemuxer::open(src)?))
}

#[derive(Debug, Clone, Copy)]
struct AudioTrack {
    stream_index: u32,
    compressed: bool,
}

#[derive(Debug)]
pub struct SmkDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    /// One slot per of the 7 possible tracks; `None` where `AudioRate`'s
    /// present bit was clear.
    tracks: [Option<AudioTrack>; NUM_TRACKS],
    frame_sizes: Vec<u32>,
    frame_types: Vec<u8>,
    time_base: Rational,
    frame_index: u32,
    audio_ticks: [i64; NUM_TRACKS],
    pending: VecDeque<Packet>,
    budget: Budget,
}

fn frame_rate_time_base(raw: i32) -> Rational {
    match raw.cmp(&0) {
        std::cmp::Ordering::Greater => Rational::new(raw, 1000).reduced(),
        // fps = 100000 / -raw, so one frame is (-raw)/100000 seconds.
        std::cmp::Ordering::Less => {
            Rational::new(raw.unsigned_abs().cast_signed(), 100_000).reduced()
        }
        std::cmp::Ordering::Equal => Rational::new(1, 10),
    }
}

impl SmkDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] if the 104-byte header does not parse.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(vaco_limits::Limits::permissive());

        let sig = io.tag()?;
        if !is_smk_magic(sig) {
            return Err(Error::InvalidData("smk: missing SMK2/SMK4 signature"));
        }
        let width = io.rl32()?;
        let height = io.rl32()?;
        let frames = io.rl32()?;
        let frame_rate = io.rl32()?.cast_signed();
        let flags = io.rl32()?;
        let mut audio_size = [0u32; NUM_TRACKS];
        for slot in &mut audio_size {
            *slot = io.rl32()?;
        }
        let _ = audio_size; // informational decoder-buffer hint; not needed for framing
        let trees_size = io.rl32()?;
        if trees_size > MAX_TREES {
            return Err(Error::LimitExceeded {
                limit: "smk_trees_size",
                requested: u64::from(trees_size),
                cap: u64::from(MAX_TREES),
            });
        }
        let table_sizes = [io.rl32()?, io.rl32()?, io.rl32()?, io.rl32()?];
        let mut audio_rate = [0u32; NUM_TRACKS];
        for slot in &mut audio_rate {
            *slot = io.rl32()?;
        }
        let _dummy = io.rl32()?;

        let ring_frame = flags & 1 != 0;
        let physical_frames = frames.saturating_add(u32::from(ring_frame));
        if physical_frames > MAX_PHYSICAL_FRAMES {
            return Err(Error::LimitExceeded {
                limit: "smk_frame_count",
                requested: u64::from(physical_frames),
                cap: u64::from(MAX_PHYSICAL_FRAMES),
            });
        }
        let n = usize::try_from(physical_frames).unwrap_or(0);

        let time_base = frame_rate_time_base(frame_rate);
        let mut video = Stream::new(0, MediaType::Video, time_base);
        let fps = time_base.inverse();
        video.r_frame_rate = fps;
        let mut vparams = CodecParameters::video().with_codec(vaco_codec_core::CodecId::Smacker);
        if let Some(v) = vparams.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.frame_rate = fps;
            v.field_order = vaco_codec_core::FieldOrder::Unknown;
        }
        video.params = vparams;
        let mut streams = vec![video];

        let mut tracks: [Option<AudioTrack>; NUM_TRACKS] = [None; NUM_TRACKS];
        for (t, &rate_desc) in audio_rate.iter().enumerate() {
            if rate_desc & (1 << 30) == 0 {
                continue;
            }
            let compressed = rate_desc & (1 << 31) != 0;
            let sixteen_bit = rate_desc & (1 << 29) != 0;
            let stereo = rate_desc & (1 << 28) != 0;
            let sample_rate = (rate_desc & 0x00FF_FFFF).max(1);
            let bytes_per_sample = u32::from(sixteen_bit) + 1;
            let channels = if stereo { 2 } else { 1 };

            let stream_index = u32::try_from(streams.len()).unwrap_or(u32::MAX);
            let audio_tb = Rational::new(
                1,
                sample_rate.saturating_mul(bytes_per_sample).cast_signed(),
            );
            let mut stream = Stream::new(stream_index, MediaType::Audio, audio_tb);
            // The compressed bit does not just change framing (whether a
            // chunk carries an extra unpacked-length prefix, read below) --
            // an uncompressed track's bytes are raw PCM, not Smacker audio
            // at all. Measured against the reference on both settings:
            // compressed reports `smackaudio`, uncompressed reports
            // `pcm_s16le`/`pcm_u8` depending on the bit-depth flag.
            let audio_codec = if compressed {
                vaco_codec_core::CodecId::SmackAudio
            } else if sixteen_bit {
                vaco_codec_core::CodecId::PcmS16le
            } else {
                vaco_codec_core::CodecId::PcmU8
            };
            let mut aparams = CodecParameters::audio().with_codec(audio_codec);
            if let Some(a) = aparams.audio.as_mut() {
                a.sample_rate = sample_rate;
                a.format = Some(if sixteen_bit {
                    SampleFmt::S16
                } else {
                    SampleFmt::U8
                });
                a.layout = ChannelLayout::default_for(channels);
            }
            stream.params = aparams;
            streams.push(stream);

            if let Some(slot) = tracks.get_mut(t) {
                *slot = Some(AudioTrack {
                    stream_index,
                    compressed,
                });
            }
        }

        let mut frame_sizes = budget.alloc::<u32>(n)?;
        for slot in &mut frame_sizes {
            *slot = io.rl32()?;
        }
        let mut frame_types = budget.alloc::<u8>(n)?;
        for slot in &mut frame_types {
            *slot = io.r8()?;
        }

        let trees_len = usize::try_from(trees_size).unwrap_or(usize::MAX);
        let mut tree_bytes = budget.alloc::<u8>(trees_len)?;
        io.read_exact(&mut tree_bytes)?;
        let mut extradata = Vec::new();
        for size in table_sizes {
            extradata.extend_from_slice(&size.to_le_bytes());
        }
        extradata.extend_from_slice(&tree_bytes);
        if let Some(video) = streams.first_mut() {
            video.params.extradata = Some(extradata);
        }

        Ok(Self {
            io,
            streams,
            tracks,
            frame_sizes,
            frame_types,
            time_base,
            frame_index: 0,
            audio_ticks: [0i64; NUM_TRACKS],
            pending: VecDeque::new(),
            budget,
        })
    }

    fn fill_frame(&mut self) -> Result<()> {
        let i = usize::try_from(self.frame_index).unwrap_or(usize::MAX);
        let Some(&raw_size) = self.frame_sizes.get(i) else {
            return Err(Error::Eof);
        };
        let frame_type = self.frame_types.get(i).copied().unwrap_or(0);
        let is_key = raw_size & 1 != 0;
        let total_len = raw_size & !0b11;
        let frame_start = self.io.pos();
        let frame_end = frame_start.saturating_add(u64::from(total_len));

        if frame_type & 1 != 0 {
            let length_byte = self.io.r8()?;
            let palette_total = u32::from(length_byte).saturating_mul(4);
            self.io.skip(u64::from(palette_total.saturating_sub(1)))?;
        }

        for t in 0..NUM_TRACKS {
            let bit = 1u8 << (t + 1);
            if frame_type & bit == 0 {
                continue;
            }
            let Some(track) = self.tracks.get(t).copied().flatten() else {
                continue;
            };
            let length = self.io.rl32()?;
            if length > MAX_CHUNK {
                return Err(Error::LimitExceeded {
                    limit: "smk_audio_chunk",
                    requested: u64::from(length),
                    cap: u64::from(MAX_CHUNK),
                });
            }
            let mut remaining = length.saturating_sub(4);
            if track.compressed {
                let _unpacked_len = self.io.rl32()?;
                remaining = remaining.saturating_sub(4);
            }
            let n = usize::try_from(remaining).unwrap_or(usize::MAX);
            let mut pkt = Packet::alloc(&mut self.budget, n)?;
            self.io.read_exact(pkt.payload_mut())?;
            pkt.stream_index = track.stream_index;
            let ticks = self.audio_ticks.get(t).copied().unwrap_or(0);
            pkt.pts = Timestamp::new(ticks);
            pkt.dts = pkt.pts;
            pkt.flags = PacketFlags::KEY;
            if let Some(slot) = self.audio_ticks.get_mut(t) {
                *slot = slot.saturating_add(i64::from(remaining));
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

        self.io.seek(frame_end)?;
        self.frame_index = self.frame_index.saturating_add(1);
        Ok(())
    }
}

impl Demuxer for SmkDemuxer {
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
        Err(Error::Unsupported("smk: seeking is not implemented"))
    }

    fn duration(&self) -> Option<vaco_core::Duration> {
        let frames = i64::try_from(self.frame_sizes.len()).ok()?;
        Timestamp::new(frames).to_duration(self.time_base)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "test fixture builder, mirrors the AudioRate bit layout directly"
    )]
    fn audio_rate(
        present: bool,
        sixteen_bit: bool,
        stereo: bool,
        compressed: bool,
        rate: u32,
    ) -> u32 {
        let mut v = 0u32;
        if compressed {
            v |= 1 << 31;
        }
        if present {
            v |= 1 << 30;
        }
        if sixteen_bit {
            v |= 1 << 29;
        }
        if stereo {
            v |= 1 << 28;
        }
        v | (rate & 0x00FF_FFFF)
    }

    fn palette_chunk(blocks: &[u8]) -> Vec<u8> {
        let total = 1 + blocks.len();
        assert_eq!(total % 4, 0);
        let mut v = vec![(total >> 2) as u8];
        v.extend_from_slice(blocks);
        v
    }

    fn audio_chunk(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(4 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn pad4(mut b: Vec<u8>) -> Vec<u8> {
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        b
    }

    /// Hand-built per `Vaco-Spec-Ref multimedia-wiki-smacker`: two frames,
    /// one audio track (mono, 16-bit, uncompressed), no Huffman tree
    /// content (`TreesSize = 0`) since this demuxer never parses it.
    fn build_fixture() -> Vec<u8> {
        let mut header = vec![0u8; 104];
        header[0..4].copy_from_slice(b"SMK4");
        header[4..8].copy_from_slice(&64u32.to_le_bytes());
        header[8..12].copy_from_slice(&48u32.to_le_bytes());
        header[12..16].copy_from_slice(&2u32.to_le_bytes());
        header[16..20].copy_from_slice(&66i32.to_le_bytes());
        // flags, AudioSize[7], TreesSize, table sizes: all zero.
        header[72..76].copy_from_slice(&audio_rate(true, true, false, false, 22050).to_le_bytes());

        let pal = palette_chunk(&[0x00, 0x00, 0x00]);
        let aud0 = audio_chunk(&[1, 2, 3, 4]);
        let frame0 = pad4([pal, aud0, vec![0xAA; 6]].concat());

        let aud1 = audio_chunk(&[5, 6, 7, 8]);
        let frame1 = pad4([aud1, vec![0xBB; 5]].concat());

        let frame_sizes = [frame0.len() as u32 | 1, frame1.len() as u32];
        let frame_types = [0b0000_0011u8, 0b0000_0010u8];

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        for s in frame_sizes {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out.extend_from_slice(&frame_types);
        out.extend_from_slice(&frame0);
        out.extend_from_slice(&frame1);
        out
    }

    #[test]
    fn probe_needs_smk2_or_smk4() {
        assert_eq!(probe(&ProbeData::new(b"SMK4")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"SMK2")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b"nope")), ProbeScore::NONE);
    }

    #[test]
    fn frame_rate_formula_matches_the_documented_three_cases() {
        assert_eq!(frame_rate_time_base(66), Rational::new(66, 1000).reduced());
        assert_eq!(
            frame_rate_time_base(-500),
            Rational::new(500, 100_000).reduced()
        );
        assert_eq!(frame_rate_time_base(0), Rational::new(1, 10));
    }

    #[test]
    fn streams_and_extradata() {
        let data = build_fixture();
        let d = SmkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams().len(), 2);
        assert_eq!(d.streams()[0].media_type(), Some(MediaType::Video));
        assert_eq!(d.streams()[1].media_type(), Some(MediaType::Audio));
        // 4 table-size u32s, no tree bytes (TreesSize = 0 in the fixture).
        assert_eq!(
            d.streams()[0].params.extradata.as_deref(),
            Some([0u8; 16].as_slice())
        );
        assert_eq!(
            d.streams()[0].params.codec_id,
            Some(vaco_codec_core::CodecId::Smacker)
        );
        // `build_fixture`'s audio track is uncompressed (16-bit), which
        // decodes to raw PCM in the reference, not `smackaudio` --
        // `compressed_audio_track_carries_the_smackaudio_codec_id` covers
        // the compressed case.
        assert_eq!(
            d.streams()[1].params.codec_id,
            Some(vaco_codec_core::CodecId::PcmS16le)
        );
    }

    #[test]
    fn compressed_audio_track_carries_the_smackaudio_codec_id() {
        // Same shape as `build_fixture`, with the audio track's `compressed`
        // bit set instead of clear.
        let mut header = vec![0u8; 104];
        header[0..4].copy_from_slice(b"SMK4");
        header[4..8].copy_from_slice(&64u32.to_le_bytes());
        header[8..12].copy_from_slice(&48u32.to_le_bytes());
        header[12..16].copy_from_slice(&2u32.to_le_bytes());
        header[16..20].copy_from_slice(&66i32.to_le_bytes());
        header[72..76].copy_from_slice(&audio_rate(true, true, false, true, 22050).to_le_bytes());

        let pal = palette_chunk(&[0x00, 0x00, 0x00]);
        let aud0 = audio_chunk(&[1, 2, 3, 4]);
        let frame0 = pad4([pal, aud0, vec![0xAA; 6]].concat());
        let aud1 = audio_chunk(&[5, 6, 7, 8]);
        let frame1 = pad4([aud1, vec![0xBB; 5]].concat());
        let frame_sizes = [frame0.len() as u32 | 1, frame1.len() as u32];
        let frame_types = [0b0000_0011u8, 0b0000_0010u8];

        let mut data = header;
        for s in frame_sizes {
            data.extend_from_slice(&s.to_le_bytes());
        }
        data.extend_from_slice(&frame_types);
        data.extend_from_slice(&frame0);
        data.extend_from_slice(&frame1);

        let d = SmkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(
            d.streams()[1].params.codec_id,
            Some(vaco_codec_core::CodecId::SmackAudio)
        );
    }

    #[test]
    fn audio_packet_is_data_only_and_video_is_the_frame_remainder() {
        let data = build_fixture();
        let mut d = SmkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        let a0 = d.read_packet().unwrap();
        assert_eq!(a0.stream_index, 1);
        assert_eq!(a0.payload(), &[1, 2, 3, 4]);
        assert!(a0.is_key());

        let v0 = d.read_packet().unwrap();
        assert_eq!(v0.stream_index, 0);
        assert!(v0.is_key());
        assert_eq!(v0.payload().len(), 8); // 6 real bytes + 2 bytes of 4-byte frame padding

        let a1 = d.read_packet().unwrap();
        assert_eq!(a1.payload(), &[5, 6, 7, 8]);
        let v1 = d.read_packet().unwrap();
        assert!(!v1.is_key());
        assert_eq!(v1.payload().len(), 8); // 5 real bytes + 3 bytes of 4-byte frame padding

        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn audio_time_base_is_sample_rate_times_bytes_per_sample() {
        let data = build_fixture();
        let d = SmkDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
        assert_eq!(d.streams()[1].time_base, Rational::new(1, 22050 * 2));
    }
}
