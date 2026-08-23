//! `vobsub` — DVD subpicture subtitles, as a `.idx`/`.sub` pair.
//!
//! # Two files, two different amounts of work
//!
//! The `.idx` file is plain text (see [`idx`]) — parsing it is exactly as
//! much container work as parsing an `.ini` file, and this module does it in
//! full: canvas size, palette, and every track's timestamp/byte-offset list.
//! The `.sub` file is MPEG-PS with the subpicture RLE riding as
//! `private_stream_1` (`vaco-demux-mpegps`'s [`substream::SubstreamKind::Subpicture`](vaco_demux_mpegps::substream::SubstreamKind::Subpicture),
//! sub-id `0x20..=0x3F`) — recovering *that* framing is `vaco-demux-mpegps`'s
//! job already, not rewritten here, and the RLE payload inside each recovered
//! packet stays untouched (decoder work).
//!
//! # A real, structural registry-seam gap
//!
//! [`vaco_format_core::DemuxerDesc::open`] is frozen as `fn(Box<dyn
//! MediaSource>, &dyn ParserProvider) -> Result<Box<dyn Demuxer>>` — one
//! source, no filename, no options (the same gap `vaco-demux-raw`'s docs
//! describe for its own option-driven formats). `vobsub` needs **two**
//! sources: the `.idx` `open()` is handed, and a sibling `.sub` this crate has
//! no path, protocol handle or options struct to reach from inside `open()`.
//!
//! So there are two entry points, on purpose:
//!
//! * [`DEMUXER`] (`open_demuxer`) is what the registry can call: it parses
//!   the `.idx` in full and returns every track's correct presentation
//!   timing, canvas [`vaco_format_subtitle_bitmap::Rect`] and
//!   [`vaco_format_subtitle_bitmap::Palette`] — but every packet's payload is
//!   **empty**, because the compressed subpicture bytes live in a file this
//!   entry point cannot open. Reported, not worked around, per
//!   `planning/AGENT-CONSTRAINTS.md`'s "Scope".
//! * [`VobSubDemuxer::open_pair`] is the real thing, for an embedder or a
//!   future CLI layer that has both paths: it opens the `.sub` through
//!   [`vaco_demux_mpegps::MpegPsDemuxer`], matches each subpicture packet to
//!   its track by sub-id, and hands out packets carrying the genuine
//!   (still-compressed) payload, correctly timed from the `.idx`.
//!
//! No muxer: the reference has none for `vobsub` as an input format (its
//! *muxer* side is `dvd`/`vob`, an MPEG-PS muxer, which is a different
//! format entirely and out of this crate's four-format scope).

pub mod idx;

use std::collections::VecDeque;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Result, Timestamp};
use vaco_demux_mpegps::MpegPsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, DemuxerDesc, FormatOptions, ParserProvider, Stream};
use vaco_format_subtitle_bitmap::{Palette, Rect};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Read a whole `.idx` file into memory (a few kilobytes in every real
/// example; bounded so a hostile pipe cannot grow this unbounded).
const MAX_IDX_BYTES: usize = 16 * 1024 * 1024;

/// Bounds [`VobSubDemuxer::open_pair`]'s drain of the inner
/// [`MpegPsDemuxer`], so a hostile or endless `.sub` cannot make it loop
/// forever.
const MAX_SUB_PACKETS: u32 = 200_000;

fn read_all_text(src: Box<dyn MediaSource>) -> Result<String> {
    let mut io = IoContext::new(src, &IoOptions::default())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = io.read_partial(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len().saturating_add(n) > MAX_IDX_BYTES {
            return Err(Error::LimitExceeded {
                limit: "vobsub_idx_bytes",
                requested: buf.len().saturating_add(n) as u64,
                cap: MAX_IDX_BYTES as u64,
            });
        }
        buf.extend_from_slice(chunk.get(..n).unwrap_or(&[]));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Content probe: the literal magic comment line every real `.idx` opens
/// with, or (leniently, for a hand-edited file missing it) at least one
/// `timestamp:`/`filepos:` pair — tokens that essentially never occur
/// together in anything but this format.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    if text.contains("VobSub index file") {
        return ProbeScore::MAGIC;
    }
    let hits = text
        .lines()
        .filter(|l| {
            let l = l.trim();
            l.starts_with("timestamp:") && l.contains("filepos:")
        })
        .take(256)
        .count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["idx"])
    }
}

/// One track's queued entries plus a cursor, for the simple in-memory
/// demuxer both entry points build on.
#[derive(Debug)]
struct QueuedTrack {
    entries: VecDeque<(Duration, Vec<u8>)>,
}

/// The `vobsub` demuxer: every track's cues, already time-ordered per track,
/// served in a single merged, globally time-ordered sequence (matching every
/// other demuxer in the workspace, which hands out packets in one stream of
/// storage order regardless of how many logical streams there are).
#[derive(Debug)]
pub struct VobSubDemuxer {
    streams: Vec<Stream>,
    tracks: Vec<QueuedTrack>,
    /// `(stream_index, time)` pairs, sorted by time, driving which track's
    /// next entry is served.
    order: Vec<(u32, Duration)>,
    pos: usize,
    canvas: Option<Rect>,
    /// The `.idx`'s global palette. File-level, not per-cue, metadata — so
    /// this is a plain accessor rather than something repeated onto every
    /// packet as side data (`vaco_packet::PacketSideData::Palette` would need
    /// a `vaco_pool::Buffer`, a dependency edge nothing in this crate needed
    /// otherwise; a caller that wants the wire-shape can get there itself via
    /// [`vaco_format_subtitle_bitmap::Palette::pack_argb32`]).
    palette: Option<Palette>,
    budget: Budget,
}

impl VobSubDemuxer {
    /// Build from an already-parsed `.idx`, with no payload bytes — the
    /// [`DEMUXER`] registry path. See the module docs for why.
    #[must_use]
    pub fn from_idx_only(file: &idx::IdxFile) -> Self {
        Self::build(file, |_track_index, _entry_index| Vec::new())
    }

    fn build(file: &idx::IdxFile, mut payload_for: impl FnMut(usize, usize) -> Vec<u8>) -> Self {
        let limits = Limits::permissive();
        let canvas = file
            .size
            .and_then(|(w, h)| Rect::new(0, 0, w, h, &limits).ok());
        let mut streams = Vec::new();
        let mut tracks = Vec::new();
        let mut order = Vec::new();
        for (ti, track) in file.tracks.iter().enumerate() {
            let idx = u32::try_from(ti).unwrap_or(u32::MAX);
            let mut stream = Stream::new(idx, MediaType::Subtitle, TIME_BASE_Q);
            stream.params =
                CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::DvdSubtitle);
            if let Some(lang) = &track.lang {
                stream.metadata.push(("language".to_string(), lang.clone()));
            }
            streams.push(stream);
            let mut entries = VecDeque::new();
            for (ei, entry) in track.entries.iter().enumerate() {
                entries.push_back((entry.time, payload_for(ti, ei)));
                order.push((idx, entry.time));
            }
            tracks.push(QueuedTrack { entries });
        }
        order.sort_by_key(|&(_, t)| t);
        Self {
            streams,
            tracks,
            order,
            pos: 0,
            canvas,
            palette: file.palette.clone(),
            budget: Budget::new(Limits::permissive()),
        }
    }

    /// The parsed canvas size, if the `.idx` stated one.
    #[must_use]
    pub fn canvas(&self) -> Option<Rect> {
        self.canvas
    }

    /// The `.idx`'s global palette, if it stated one.
    #[must_use]
    pub fn palette(&self) -> Option<&Palette> {
        self.palette.as_ref()
    }
}

impl Demuxer for VobSubDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let &(stream_index, time) = self.order.get(self.pos).ok_or(Error::Eof)?;
        self.pos = self.pos.saturating_add(1);
        let track = self
            .tracks
            .get_mut(stream_index as usize)
            .ok_or(Error::InvalidData("vobsub: stream index out of range"))?;
        let (_, payload) = track
            .entries
            .pop_front()
            .ok_or(Error::InvalidData("vobsub: track ran out of entries"))?;
        let mut pkt = Packet::from_slice(&mut self.budget, &payload)?;
        pkt.stream_index = stream_index;
        pkt.pts = Timestamp::new(time.as_micros());
        pkt.dts = pkt.pts;
        pkt.flags = PacketFlags::KEY;
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = target.resolve_frames(vaco_core::Rational::ZERO, TIME_BASE_Q)?;
        let SeekTarget::Timestamp { ts, .. } = target else {
            return Err(Error::NotSeekable);
        };
        let Some(target_us) = ts.ticks() else {
            return Err(Error::InvalidData("seek target has no timestamp"));
        };
        self.pos = if flags.contains(SeekFlags::BACKWARD) {
            self.order
                .iter()
                .rposition(|&(_, t)| t.as_micros() <= target_us)
                .unwrap_or(0)
        } else {
            self.order
                .iter()
                .position(|&(_, t)| t.as_micros() >= target_us)
                .unwrap_or(self.order.len())
        };
        Ok(())
    }

    fn duration(&self) -> Option<Duration> {
        self.order.last().map(|&(_, t)| t)
    }
}

impl VobSubDemuxer {
    /// Open both files: the real entry point. Correlates each track's
    /// `.idx`-stated timestamps with the actual subpicture packets recovered
    /// from `sub` by [`MpegPsDemuxer`], matched ordinally per sub-stream id
    /// (`0x20 + track index`, the DVD-Video/SVCD convention
    /// `vaco_demux_mpegps::substream` documents).
    ///
    /// # Errors
    /// Whatever [`MpegPsDemuxer::open`] reports opening `sub`.
    pub fn open_pair(idx_src: Box<dyn MediaSource>, sub_src: Box<dyn MediaSource>) -> Result<Self> {
        let idx_text = read_all_text(idx_src)?;
        let file = idx::parse(&idx_text);

        let mut ps = MpegPsDemuxer::open(sub_src, &NoParsers, &FormatOptions::default())?;
        // sub_id -> queued payloads, in the order MpegPsDemuxer produced them.
        let mut by_sub_id: std::collections::HashMap<u8, VecDeque<Vec<u8>>> =
            std::collections::HashMap::new();
        let sub_id_of: std::collections::HashMap<u32, u8> = ps
            .streams()
            .iter()
            .filter_map(|s| {
                let id = s.id?;
                let stream_id = (id >> 8) & 0xFF;
                if stream_id != 0xBD {
                    return None;
                }
                let sub_id = u8::try_from(id & 0xFF).ok()?;
                Some((s.index, sub_id))
            })
            .collect();

        let mut n = 0u32;
        loop {
            n = n.saturating_add(1);
            if n > MAX_SUB_PACKETS {
                break;
            }
            match ps.read_packet() {
                Ok(pkt) => {
                    if let Some(&sub_id) = sub_id_of.get(&pkt.stream_index) {
                        by_sub_id
                            .entry(sub_id)
                            .or_default()
                            .push_back(pkt.payload().to_vec());
                    }
                }
                Err(_) => break,
            }
        }

        let demuxer = Self::build(&file, |track_index, _entry_index| {
            let sub_id = 0x20u16.saturating_add(track_index as u16);
            let Ok(sub_id) = u8::try_from(sub_id) else {
                return Vec::new();
            };
            by_sub_id
                .get_mut(&sub_id)
                .and_then(VecDeque::pop_front)
                .unwrap_or_default()
        });
        Ok(demuxer)
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    let text = read_all_text(src)?;
    let file = idx::parse(&text);
    Ok(Box::new(VobSubDemuxer::from_idx_only(&file)))
}

/// No byte-seek/binary-search/generic-index: this demuxer resolves every
/// seek against its own in-memory timestamp list, same as
/// `vaco-subtitle-text::engine::CueDemuxer`. `NOSTREAMS` because a `.idx`
/// with no `id:` blocks is legal (if useless).
pub const DEMUX_FLAGS: FormatFlags = FormatFlags::NOBINSEARCH
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK)
    .union(FormatFlags::NOSTREAMS);

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "vobsub",
    long_name: "VobSub subtitle format",
    extensions: &["idx"],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    const SAMPLE_IDX: &str = "\
# VobSub index file, v7\n\
size: 720x480\n\
palette: 000000, ffffff\n\
\n\
id: en, index: 0\n\
timestamp: 00:00:01:000, filepos: 000000000\n\
timestamp: 00:00:02:000, filepos: 000000040\n\
";

    #[test]
    fn probe_accepts_the_magic_comment_line() {
        let data = ProbeData::new(SAMPLE_IDX.as_bytes());
        assert_eq!(probe(&data), ProbeScore::MAGIC);
    }

    #[test]
    fn probe_accepts_timestamp_filepos_pairs_without_the_magic_line() {
        let text = "timestamp: 00:00:01:000, filepos: 000000000\n";
        let data = ProbeData::new(text.as_bytes());
        assert!(probe(&data).value() > ProbeScore::NONE.value());
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let data = ProbeData::new(b"The quick brown fox jumps over the lazy dog, repeatedly.\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_rejects_a_sup_sample() {
        let mut pgs_like = Vec::new();
        pgs_like.extend_from_slice(b"PG");
        pgs_like.extend_from_slice(&90_000u32.to_be_bytes());
        pgs_like.extend_from_slice(&90_000u32.to_be_bytes());
        pgs_like.push(0x16);
        pgs_like.extend_from_slice(&3u16.to_be_bytes());
        pgs_like.extend_from_slice(&[1, 2, 3]);
        let data = ProbeData::new(&pgs_like);
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn open_demuxer_yields_correctly_timed_empty_packets() {
        let mut d = VobSubDemuxer::from_idx_only(&idx::parse(SAMPLE_IDX));
        assert_eq!(d.streams().len(), 1);
        assert_eq!(d.canvas().map(|r| (r.width, r.height)), Some((720, 480)));
        assert_eq!(
            d.palette().map(vaco_format_subtitle_bitmap::Palette::len),
            Some(2)
        );
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts, Timestamp::new(1_000_000));
        assert_eq!(p1.payload().len(), 0);
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.pts, Timestamp::new(2_000_000));
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }

    /// A minimal, hand-built MPEG-2 program stream: one pack header, then one
    /// `private_stream_1` PES packet whose payload is `[sub_id, ..data]` —
    /// exactly the DVD-Video subpicture convention `vaco_demux_mpegps::substream`
    /// documents.
    fn build_minimal_ps(sub_id: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![
            0x00, 0x00, 0x01, 0xba, 0x21, 0x00, 0x01, 0x00, 0x01, 0xa1, 0xa1, 0xad,
        ];
        let mut payload = vec![sub_id];
        payload.extend_from_slice(data);
        let optional_header = [0x80u8, 0x00, 0x00]; // MPEG-2 PES, no PTS/DTS, no extra fields
        let pes_len = optional_header.len().saturating_add(payload.len());
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0xbd]);
        v.extend_from_slice(&(pes_len as u16).to_be_bytes());
        v.extend_from_slice(&optional_header);
        v.extend_from_slice(&payload);
        v
    }

    #[test]
    fn open_pair_correlates_idx_timing_with_real_sub_payloads() {
        let sub_bytes = build_minimal_ps(0x20, b"first-cue-rle-bytes");
        let idx_src = Box::new(MemorySource::new(SAMPLE_IDX.as_bytes().to_vec()));
        let sub_src = Box::new(MemorySource::new(sub_bytes));
        let mut d = VobSubDemuxer::open_pair(idx_src, sub_src).unwrap();
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.pts, Timestamp::new(1_000_000));
        assert_eq!(p1.payload(), b"first-cue-rle-bytes");
        // The second `.idx` entry has no corresponding packet in this
        // minimal `.sub`: leniently empty, not an error.
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.pts, Timestamp::new(2_000_000));
        assert!(p2.payload().is_empty());
    }
}
