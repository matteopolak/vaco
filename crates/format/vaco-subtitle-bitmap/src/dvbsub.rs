//! `dvbsub` — raw DVB subtitles (ETSI EN 300 743).
//!
//! # The demuxer/decoder line, measured
//!
//! `ffmpeg -h demuxer=dvbsub` names it `"raw dvbsub"` and lists exactly the
//! `-raw_packet_size` option every other headerless elementary-stream
//! demuxer in the reference has (`h264`, `aac`, …) — the reference's own
//! generic raw-chunk reader, not a segment-aware demuxer. In a real
//! broadcast, DVB subtitles arrive inside MPEG-TS PES packets, which already
//! carry the framing and timing (`crates/format/vaco-demux-mpegts`, out of
//! this crate's scope); `dvbsub` as a **standalone** format — a bare
//! elementary stream with no PES, no PTS of its own — genuinely has nothing
//! for a demuxer to frame on beyond "here are the bytes", which is exactly
//! what the measured reference does. [`DEMUXER`] matches that: fixed
//! [`RAW_PACKET_SIZE`]-byte packets, [`FormatFlags::NOTIMESTAMPS`].
//!
//! The EN 300 743 segment structure itself — [`segments`] — is real and is
//! used, just not for packetisation: [`probe`] uses it to score far more
//! specifically than "this file has the right extension", and it is exposed
//! for a future decoder. See [`segments`]'s module docs for exactly which
//! segment types it reads and why it stops where it does.
//!
//! No muxer: the reference has none (`ffmpeg -muxers` names no `dvbsub`
//! entry), so no `MUXER` is registered here.

pub mod segments;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Result};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

/// The reference's own default (`ffmpeg -h demuxer=dvbsub`'s
/// `-raw_packet_size`, default `1024`).
pub const RAW_PACKET_SIZE: usize = 1024;

/// Consecutive well-typed segments found gives [`ProbeScore::MAX`]-adjacent
/// confidence; capped so probing never walks a whole large file.
const PROBE_CHAIN_CAP: u32 = 8;

/// Content probe: a chain of [`segments::SYNC_BYTE`]-prefixed, known-typed
/// segments, each one's length actually reaching the next sync byte — the
/// same "rhythm" argument [`crate::sup::probe`] and `vaco-demux-mpegps`'s
/// probe make for their own start codes. Deliberately *not* what
/// [`DEMUXER`]'s packetisation uses (see the module docs).
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let mut buf = Vec::new();
    let mut i = 0usize;
    while let Some(b) = data.get(i) {
        buf.push(b);
        i = i.saturating_add(1);
        if i > 1 << 16 {
            break;
        }
    }
    let mut chain = 0u32;
    for (header, _) in segments::iter_segments(&buf) {
        if !header.kind.is_known() {
            break;
        }
        chain = chain.saturating_add(1);
        if chain >= PROBE_CHAIN_CAP {
            break;
        }
    }
    if chain > 0 {
        ProbeScore::repeating(chain)
    } else {
        ProbeScore::NONE
    }
}

/// Fixed-size raw chunk reader, matching the measured reference demuxer.
#[derive(Debug)]
pub struct DvbSubDemuxer {
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    eof: bool,
}

impl DvbSubDemuxer {
    /// # Errors
    /// Propagates I/O failure from opening `src`.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let mut stream = Stream::new(0, MediaType::Subtitle, TIME_BASE_Q);
        stream.params = CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::DvbSubtitle);
        Ok(Self {
            io,
            streams: [stream],
            budget: Budget::new(vaco_limits::Limits::permissive()),
            eof: false,
        })
    }
}

impl Demuxer for DvbSubDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let mut buf = self.budget.alloc::<u8>(RAW_PACKET_SIZE)?;
        let mut got = 0usize;
        while got < buf.len() {
            let Some(rest) = buf.get_mut(got..) else {
                break;
            };
            let n = self.io.read_partial(rest)?;
            if n == 0 {
                break;
            }
            got = got.saturating_add(n);
        }
        if got == 0 {
            self.eof = true;
            return Err(Error::Eof);
        }
        if got < buf.len() {
            self.eof = true;
        }
        let mut pkt = Packet::from_slice(&mut self.budget, buf.get(..got).unwrap_or(&[]))?;
        pkt.stream_index = 0;
        pkt.flags = PacketFlags::KEY;
        Ok(pkt)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }

    fn duration(&self) -> Option<Duration> {
        None
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(DvbSubDemuxer::open(src)?))
}

/// A raw elementary stream carries no timestamps of its own — matching every
/// other headerless bitstream demuxer in the workspace (`vaco-demux-raw`).
pub const DEMUX_FLAGS: FormatFlags = FormatFlags::NOTIMESTAMPS
    .union(FormatFlags::NOBINSEARCH)
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "dvbsub",
    long_name: "raw dvbsub",
    extensions: &[],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn seg(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![segments::SYNC_BYTE, kind, 0, 1];
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn sample() -> Vec<u8> {
        let mut v = seg(0x10, &[1, 2]);
        v.extend(seg(0x11, &[0, 0, 0, 10, 0, 10]));
        v.extend(seg(0x80, &[]));
        v
    }

    #[test]
    fn probe_accepts_a_real_segment_chain() {
        let bytes = sample();
        let data = ProbeData::new(&bytes);
        assert!(probe(&data).value() > ProbeScore::RETRY.value());
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let data = ProbeData::new(b"The quick brown fox jumps over the lazy dog, repeatedly.\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_rejects_a_pgs_sample() {
        // Confusable at a glance (both are small binary subtitle streams),
        // distinguished by `sync_byte` (0x0F) vs. `"PG"` and by segment type.
        let mut pgs_like = Vec::new();
        pgs_like.extend_from_slice(b"PG");
        pgs_like.extend_from_slice(&90_000u32.to_be_bytes());
        pgs_like.extend_from_slice(&90_000u32.to_be_bytes());
        pgs_like.push(0x16); // PCS
        pgs_like.extend_from_slice(&3u16.to_be_bytes());
        pgs_like.extend_from_slice(&[1, 2, 3]);
        let data = ProbeData::new(&pgs_like);
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn demux_yields_fixed_size_chunks() {
        let data = vec![0xABu8; RAW_PACKET_SIZE * 2 + 10];
        let mut d = DvbSubDemuxer::open(Box::new(MemorySource::new(data.clone()))).unwrap();
        let p1 = d.read_packet().unwrap();
        assert_eq!(p1.payload().len(), RAW_PACKET_SIZE);
        let p2 = d.read_packet().unwrap();
        assert_eq!(p2.payload().len(), RAW_PACKET_SIZE);
        let p3 = d.read_packet().unwrap();
        assert_eq!(p3.payload().len(), 10);
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
}
