//! `sup` — raw HDMV Presentation Graphic Stream (PGS / Blu-ray) subtitles.
//!
//! # Provenance (D6/D7)
//!
//! There is no freely published PGS specification. The segment framing below
//! — `"PG"` magic, a 4-byte PTS and a 4-byte DTS at a 90 kHz clock, a 1-byte
//! segment type, a 2-byte big-endian size, then that many payload bytes,
//! repeated to end of file — is the structure independently documented by
//! numerous public, non-`FFmpeg` write-ups of the format (`MKVToolNix`,
//! `PGSToSrt`, and others arrived at the same layout independently, which is
//! the "measure it" this project asks for: `ffmpeg`'s own `sup` demuxer and
//! muxer round-trip a `-c:s copy`d `.sup` unmodified, consistent with this
//! being exactly what is on disk). `~/repos/FFmpeg` was not opened for this.
//!
//! `ffmpeg` has no PGS **encoder** (`ffmpeg -h encoder=hdmv_pgs_subtitle` does
//! not exist; only `pgssub` *decodes*), so a real `.sup` could not be
//! generated from scratch for this crate's tests the way `dvbsub`'s
//! `dvdsub`/`dvbsub` samples were — this module's fixtures are hand-built
//! from the documented byte layout, not extracted from a reference-encoded
//! file. Flagged here rather than left implicit, per this project's honesty
//! bar on unverified work.
//!
//! # The demuxer/decoder line
//!
//! One packet per **segment** (not per display set): a display set is a
//! `PCS`, optional `WDS`/`PDS`/`ODS` segments and a terminating `END`, and
//! waiting for the `END` before emitting anything would make a demuxer that
//! stalls forever on a stream truncated mid-composition. Emitting per segment
//! is both simpler and more lenient — a decoder assembles the display set
//! from the segment types it sees, same as it would from a live BD player's
//! feed. A packet's payload is the segment's bytes **verbatim, header
//! included** (`"PG"` through the end of its data): the header's own PTS/DTS
//! are still copied onto [`vaco_packet::Packet::pts`]/`dts` for the
//! container-level timeline, but nothing here decodes a `PDS`'s palette
//! entries or an `ODS`'s run-length pixel string — that stays decoder work.

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::bytes::{rb16, rb32};

/// `"PG"`.
pub const MAGIC: [u8; 2] = *b"PG";

/// `"PG"` + pts(4) + dts(4) + type(1) + size(2).
pub const HEADER_LEN: usize = 13;

/// PGS's clock: 90 kHz, the same width MPEG systems layers use.
pub const TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};

/// A segment's type byte. `Other` keeps the framing total over a type this
/// crate does not name — new segment types cost decoders nothing to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Palette Definition Segment.
    Pds,
    /// Object Definition Segment: the run-length pixel data.
    Ods,
    /// Presentation Composition Segment: which objects are on screen.
    Pcs,
    /// Window Definition Segment.
    Wds,
    /// End of display set.
    End,
    Other(u8),
}

impl SegmentType {
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            0x14 => Self::Pds,
            0x15 => Self::Ods,
            0x16 => Self::Pcs,
            0x17 => Self::Wds,
            0x80 => Self::End,
            other => Self::Other(other),
        }
    }

    /// Whether `self` is one of the five types real PGS streams use. A
    /// stream of nothing but [`SegmentType::Other`] is not implausible
    /// framing so much as not PGS at all — see [`probe`].
    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

/// One segment's fixed-size header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub pts: u32,
    pub dts: u32,
    pub kind: SegmentType,
    /// Payload length in bytes, following the header.
    pub size: u16,
}

/// Parse the fixed 13-byte header at the start of `buf`. `None` if `buf` is
/// short or does not start with [`MAGIC`].
#[must_use]
pub fn parse_header(buf: &[u8]) -> Option<SegmentHeader> {
    if buf.first() != Some(&MAGIC[0]) || buf.get(1) != Some(&MAGIC[1]) {
        return None;
    }
    let pts = rb32(buf, 2)?;
    let dts = rb32(buf, 6)?;
    let kind = *buf.get(10)?;
    let size = rb16(buf, 11)?;
    Some(SegmentHeader {
        pts,
        dts,
        kind: SegmentType::from_u8(kind),
        size,
    })
}

/// Walk `data` as a sequence of segments, lenient: stops (without error)
/// at the first byte position that is not a complete segment, so a
/// truncated or damaged tail yields every whole segment before it rather
/// than nothing at all.
#[must_use]
pub fn iter_segments(data: &[u8]) -> Segments<'_> {
    Segments { data, pos: 0 }
}

#[derive(Debug)]
pub struct Segments<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Segments<'a> {
    /// The header, and the *whole record* (header bytes included) — the same
    /// shape [`PgsDemuxer::read_packet`] hands out as a packet payload.
    type Item = (SegmentHeader, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let head = self.data.get(self.pos..)?;
        let header = parse_header(head)?;
        let total = HEADER_LEN.checked_add(usize::from(header.size))?;
        let record = head.get(..total)?;
        self.pos = self.pos.checked_add(total)?;
        Some((header, record))
    }
}

/// How many consecutive, well-typed segments open `data`, capped at
/// [`PROBE_CHAIN_CAP`] — enough to be confident without scanning a whole
/// multi-megabyte `.sup` file during probing.
const PROBE_CHAIN_CAP: u32 = 8;

/// Content probe: a chain of segments whose type byte is one of the five
/// PGS names, each one's declared size actually reaching the next `"PG"`.
/// Random bytes essentially never keep producing plausible chains — the same
/// "rhythm" argument `vaco-demux-mpegps`'s probe makes for MPEG-PS start
/// codes.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let mut buf = Vec::new();
    let mut i = 0usize;
    while let Some(b) = data.get(i) {
        buf.push(b);
        i = i.saturating_add(1);
        if i > 1 << 20 {
            break;
        }
    }
    let mut chain = 0u32;
    for (header, _) in iter_segments(&buf) {
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
        ProbeScore::from_extension(data, &["sup"])
    }
}

/// The one subtitle stream every `.sup` carries.
#[derive(Debug)]
pub struct PgsDemuxer {
    io: IoContext,
    streams: [Stream; 1],
    budget: Budget,
    eof: bool,
}

/// How many resync bytes to scan for a `"PG"` before giving up on a corrupt
/// stream — bounded so a hostile file cannot make this loop unbounded.
const MAX_RESYNC_BYTES: u64 = 1 << 20;

impl PgsDemuxer {
    /// # Errors
    /// Propagates I/O failure from opening `src`.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let mut stream = Stream::new(0, MediaType::Subtitle, TIME_BASE);
        stream.params =
            CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::HdmvPgsSubtitle);
        Ok(Self {
            io,
            streams: [stream],
            budget: Budget::new(Limits::permissive()),
            eof: false,
        })
    }

    /// Scan forward for the next `"PG"` magic, consuming everything before
    /// it. `Ok(false)` at end of input.
    fn resync(&mut self) -> Result<bool> {
        let mut prev = 0u8;
        let mut have_prev = false;
        let mut scanned = 0u64;
        loop {
            let mut b = [0u8; 1];
            if self.io.read_partial(&mut b)? == 0 {
                return Ok(false);
            }
            scanned = scanned.saturating_add(1);
            if scanned > MAX_RESYNC_BYTES {
                return Ok(false);
            }
            if have_prev && prev == MAGIC[0] && b[0] == MAGIC[1] {
                return Ok(true);
            }
            let Some(byte) = b.first().copied() else {
                return Ok(false);
            };
            prev = byte;
            have_prev = true;
        }
    }
}

impl Demuxer for PgsDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let mut head = [0u8; HEADER_LEN];
        // The first two bytes were already consumed by a prior resync; put
        // them back logically by reading only the remaining 11 up front and
        // re-checking on the first attempt via a direct peek instead.
        let peeked = self.io.peek(HEADER_LEN)?;
        let header = if peeked.len() == HEADER_LEN {
            parse_header(peeked)
        } else {
            None
        };
        let header = if let Some(h) = header {
            // Consume the header we just peeked.
            self.io.read_exact(&mut head)?;
            h
        } else {
            if !self.resync()? {
                self.eof = true;
                return Err(Error::Eof);
            }
            head[0] = MAGIC[0];
            head[1] = MAGIC[1];
            let Some(rest) = head.get_mut(2..) else {
                self.eof = true;
                return Err(Error::Eof);
            };
            if self.io.read_exact(rest).is_err() {
                self.eof = true;
                return Err(Error::Eof);
            }
            let Some(h) = parse_header(&head) else {
                self.eof = true;
                return Err(Error::Eof);
            };
            h
        };

        let payload_len = usize::from(header.size);
        let mut payload =
            self.budget
                .alloc::<u8>(payload_len)
                .map_err(|_| Error::LimitExceeded {
                    limit: "sup_segment_bytes",
                    requested: u64::from(header.size),
                    cap: self.budget.limits().max_alloc_single,
                })?;
        if self.io.read_exact(&mut payload).is_err() {
            self.eof = true;
            return Err(Error::Eof);
        }

        let mut record = Vec::new();
        record.extend_from_slice(&head);
        record.extend_from_slice(&payload);
        let mut pkt = Packet::from_slice(&mut self.budget, &record)?;
        pkt.stream_index = 0;
        pkt.pts = Timestamp::new(i64::from(header.pts));
        pkt.dts = Timestamp::new(i64::from(header.dts));
        pkt.duration = Duration::ZERO;
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
    Ok(Box::new(PgsDemuxer::open(src)?))
}

/// Flags: no generic index/binary-search seek (this demuxer has none of its
/// own either — a `.sup` is read forward-only here), and `TS_NONSTRICT`
/// because consecutive segments of one display set legitimately share a PTS.
pub const DEMUX_FLAGS: FormatFlags = FormatFlags::NOBINSEARCH
    .union(FormatFlags::NOGENSEARCH)
    .union(FormatFlags::TS_NONSTRICT);

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "sup",
    long_name: "raw HDMV Presentation Graphic Stream subtitles",
    extensions: &["sup"],
    mime_types: &["application/x-pgs"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

/// The muxer: every packet's payload is already a complete, verbatim segment
/// record (see the module docs), so muxing is concatenation.
#[derive(Debug, Default)]
struct SupMuxer {
    out: Option<IoWriter>,
    stream_added: bool,
}

impl Muxer for SupMuxer {
    fn flags(&self) -> FormatFlags {
        FormatFlags::TS_NONSTRICT
    }

    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.stream_added {
            return Err(Error::Unsupported(
                "sup carries exactly one subtitle stream",
            ));
        }
        if params.codec_id != Some(CodecId::HdmvPgsSubtitle) {
            return Err(Error::Unsupported("sup only carries hdmv_pgs_subtitle"));
        }
        self.stream_added = true;
        Ok(0)
    }

    fn write_header(&mut self) -> Result<()> {
        if !self.stream_added {
            return Err(Error::InvalidData("header written before add_stream"));
        }
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        let Some(out) = self.out.as_mut() else {
            return Err(Error::InvalidData("sup muxer has no sink"));
        };
        out.write(packet.payload())
    }

    fn write_trailer(&mut self) -> Result<()> {
        let Some(out) = self.out.as_mut() else {
            return Err(Error::InvalidData("sup muxer has no sink"));
        };
        out.flush()
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    let out = IoWriter::new(sink, &IoOptions::default())?;
    Ok(Box::new(SupMuxer {
        out: Some(out),
        stream_added: false,
    }))
}

pub const MUXER: MuxerDesc = MuxerDesc {
    name: "sup",
    long_name: "raw HDMV Presentation Graphic Stream subtitles",
    extensions: &["sup"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn segment(kind: u8, pts: u32, dts: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MAGIC);
        v.extend_from_slice(&pts.to_be_bytes());
        v.extend_from_slice(&dts.to_be_bytes());
        v.push(kind);
        v.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn sample_file() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(segment(0x16, 90_000, 90_000, &[1, 2, 3])); // PCS
        v.extend(segment(0x17, 90_000, 90_000, &[4, 5])); // WDS
        v.extend(segment(0x14, 90_000, 90_000, &[6])); // PDS
        v.extend(segment(0x15, 90_000, 90_000, &[7, 8, 9, 10])); // ODS
        v.extend(segment(0x80, 90_000, 90_000, &[])); // END
        v
    }

    #[test]
    fn iter_segments_walks_every_record_in_order() {
        let data = sample_file();
        let kinds: Vec<SegmentType> = iter_segments(&data).map(|(h, _)| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SegmentType::Pcs,
                SegmentType::Wds,
                SegmentType::Pds,
                SegmentType::Ods,
                SegmentType::End,
            ]
        );
    }

    #[test]
    fn iter_segments_stops_cleanly_on_a_truncated_tail() {
        let mut data = sample_file();
        data.truncate(data.len() - 2); // chop the END segment's payload... (it has none) chop header
        let count = iter_segments(&data).count();
        assert_eq!(count, 4); // everything but the truncated END
    }

    #[test]
    fn probe_accepts_a_real_sup_chain() {
        let bytes = sample_file();
        let data = ProbeData::new(&bytes);
        assert!(probe(&data).value() > ProbeScore::RETRY.value());
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let data = ProbeData::new(b"The quick brown fox jumps over the lazy dog, repeatedly.\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn demux_yields_one_packet_per_segment_with_header_timing() {
        use vaco_io::MemorySource;
        let data = sample_file();
        let src = MemorySource::new(data);
        let mut d = PgsDemuxer::open(Box::new(src)).unwrap();
        let mut count = 0;
        loop {
            match d.read_packet() {
                Ok(pkt) => {
                    assert_eq!(pkt.pts, Timestamp::new(90_000));
                    count += 1;
                }
                Err(Error::Eof) => break,
                Err(e) => unreachable!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn mux_writes_packet_payloads_verbatim() {
        use vaco_format_core::vacoraw::MemorySink;
        let data = sample_file();
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut mux = open_muxer(Box::new(sink)).unwrap();
        mux.add_stream(
            &CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::HdmvPgsSubtitle),
        )
        .unwrap();
        mux.write_header().unwrap();
        for (_, record) in iter_segments(&data) {
            let mut budget = Budget::new(Limits::permissive());
            let pkt = Packet::from_slice(&mut budget, record).unwrap();
            mux.write_packet(&pkt).unwrap();
        }
        mux.write_trailer().unwrap();
        assert_eq!(shared.snapshot(), data);
    }
}
