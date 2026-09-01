//! The ASF demuxer: the Header Object walk, the fixed-size-packet Data
//! Object walk, fragment reassembly, and seeking.
//!
//! # The clock
//!
//! Every payload that carries Replicated Data states a Presentation Time in
//! milliseconds ([\[ASF\] §5.2.3.1](vaco_format_asf)); this crate rescales
//! that into each stream's own [`vaco_format_core::Stream::time_base`] (a
//! stream's sample rate for audio, [`TIME_BASE_100NS`] for everything else,
//! matching the container's own 100-nanosecond tick). A payload with no
//! Replicated Data at all (legal, per spec, when Replicated Data Length is
//! `0`) carries no timestamp; this crate falls back to a running per-stream
//! tick counter for that rare case, the same "count objects, do not guess a
//! clock" fallback `vaco-demux-avi` uses for `dwSampleSize == 0`.
//!
//! # Fragment reassembly
//!
//! A media object bigger than one Data Packet is split across several
//! ordinary payloads (never compressed sub-payloads, which are always whole
//! objects by definition). [`PendingObject`] is the per-stream buffer that
//! reassembles them: a payload at `offset == 0` starts a new one (flushing
//! whatever the previous one had collected, if its length was never known
//! and so was never auto-completed); any other payload extends the pending
//! object only if its offset matches how much has been collected so far,
//! and is otherwise dropped as desynchronised input rather than corrupting a
//! neighbouring stream's data.

use std::collections::VecDeque;

use vaco_core::{Duration, Error, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_format_asf::object::HEADER_LEN as OBJECT_HEADER_LEN;
use vaco_format_asf::well_known;
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, IncrementalVec, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::header::{self, Encryption};
use crate::index;
use crate::packet::{ParsedPayload, parse_packet};

/// ASF has no native tick unit smaller than 100 nanoseconds
/// (`Creation Date`, `Play Duration`, a Simple Index's time interval are all
/// stated in it), so it is the fallback time base for any stream whose own
/// media type does not imply a better one (audio uses its sample rate; video
/// and everything else uses this).
pub(crate) const TIME_BASE_100NS: Rational = Rational::new(1, 10_000_000);

/// Milliseconds, the unit every payload's Presentation Time and Send Time
/// are stated in.
const TIME_BASE_MS: Rational = Rational::new(1, 1000);

/// This container's declared capabilities.
///
/// [`FormatFlags::NOBINSEARCH`]: a byte position does not imply a
/// presentation time without decoding at least one packet header to read its
/// Send Time, and this crate has not built or verified a bisection strategy
/// against real multi-gigabyte content — see `docs/format/vaco-demux-asf.md`.
/// [`FormatFlags::GENERIC_INDEX`]: a non-seekable open (or one with neither a
/// Simple Index Object nor an Index Object) falls back to indexing packets as
/// they are read, like every other index-based demuxer in this workspace.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX.union(FormatFlags::NOBINSEARCH);

/// Content probe: the Header Object's GUID at byte 0.
///
/// Strict on purpose (D6/the probe-vs-demux lesson): this checks the exact
/// 16-byte GUID, not "does something downstream of this parse", so a file
/// that merely starts with plausible-looking bytes cannot claim the format.
/// Measured against `ffprobe 8.1`'s own `asf` probe, which is exactly this
/// fixed-position magic check.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(&well_known::HEADER_OBJECT.as_bytes()) {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// `asf_o`'s probe: always [`ProbeScore::NONE`].
///
/// Measured (`ffmpeg -h demuxer=asf_o`): the reference registers this
/// demuxer with no options and it is never the format `ffprobe` reports for
/// an ordinary `.asf` file, which is what "never auto-detected, select with
/// `-f asf_o`" looks like from outside. Since this crate's `asf_o` reads the
/// identical byte layout as `asf` (see [`DEMUXER_O`]'s doc comment), giving
/// it a real probe would only make it race `asf` for the same files.
#[must_use]
pub fn probe_opaque(_data: &ProbeData<'_>) -> ProbeScore {
    ProbeScore::NONE
}

/// The `asf` registry descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "asf",
    long_name: "ASF (Advanced / Active Streaming Format)",
    extensions: &["asf", "wmv", "wma"],
    mime_types: &["video/x-ms-asf"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

/// The `asf_o` registry descriptor: the same reader, never auto-probed (see
/// [`probe_opaque`]), with binary search and byte seeking additionally
/// disabled — it is the variant a caller reaches for over a source it does
/// not expect to be able to seek at all (`ffmpeg`'s own `asf_o` is documented
/// nowhere beyond its name; this crate's version is "the `asf` demuxer with
/// no seek assumptions", which is the honest amount of behaviour to claim
/// without opening `~/repos/FFmpeg`).
pub const DEMUXER_O: DemuxerDesc = DemuxerDesc {
    name: "asf_o",
    long_name: "ASF (Advanced / Active Streaming Format)",
    extensions: &[],
    mime_types: &[],
    flags: FLAGS.union(FormatFlags::NO_BYTE_SEEK),
    probe: probe_opaque,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(AsfDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

/// A media object being reassembled from one or more fragments on one
/// stream. See the module docs.
#[derive(Debug)]
struct PendingObject {
    media_object_number: u32,
    key_frame: bool,
    pts_ms: Option<u32>,
    /// `None` when no payload has yet supplied Replicated Data — the object
    /// cannot be auto-completed and is instead flushed when the next
    /// `offset == 0` payload for this stream arrives, or at end of stream.
    total_len: Option<u32>,
    /// Grows towards `total_len` (or, when that is unknown, without a
    /// declared cap beyond the budget itself) rather than trusting it up
    /// front — the same "grow to what actually arrives" discipline every
    /// other attacker-controlled-length buffer in this workspace uses.
    buf: IncrementalVec<u8>,
}

/// The ASF demuxer.
#[derive(Debug)]
pub struct AsfDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    /// `streams[i]`'s ASF `Stream Number` (1..=127), parallel to `streams`.
    stream_numbers: Vec<u8>,
    packet_size: u64,
    data_start: u64,
    /// End of the packet-addressable region, clamped to the file size when
    /// known. `None` when the source cannot report a size and the File
    /// Properties' Broadcast Flag leaves the total packet count invalid —
    /// the walk then simply reads until the source runs out.
    data_end: Option<u64>,
    metadata: Vec<(String, String)>,
    encryption: Option<Encryption>,
    duration: Option<Duration>,
    /// File Properties' Preroll (ms): every Data Packet's Send Time is
    /// measured from the start of buffering, not from the first presented
    /// sample, so a packet's real presentation time is `send_time -
    /// preroll` — measured against `ffmpeg 9.0.1`, whose `start_time` on a
    /// real `libx264`-in-ASF fixture is exactly this file's preroll less
    /// than the raw Send Time on the first packet. Stored here because it
    /// was previously consulted only for the aggregate duration
    /// calculation and dropped, leaving every packet timestamp too large by
    /// the same fixed amount.
    preroll_ms: u64,
    index: PacketIndex,
    pending: std::collections::BTreeMap<u8, PendingObject>,
    /// Fallback per-stream tick counter for a payload that carries no
    /// Replicated Data at all (Replicated Data Length == 0) and therefore no
    /// Presentation Time — see module docs.
    fallback_ticks: std::collections::BTreeMap<u8, i64>,
    queue: VecDeque<Packet>,
    budget: Budget,
    eof: bool,
}

impl AsfDemuxer {
    /// Open an ASF file.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when the Header Object, File Properties Object,
    /// or every Stream Properties Object is missing or malformed;
    /// [`Error::LimitExceeded`] past the configured allocation ceiling;
    /// whatever the transport reports.
    pub fn open(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling — the constructor a fuzz
    /// target or an embedder that cares about memory reaches for, mirroring
    /// `vaco-demux-avi` and `vaco-demux-matroska`.
    ///
    /// # Errors
    /// As [`AsfDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut budget = Budget::new(limits);

        let mut prefix = [0u8; 30];
        io.read_exact(&mut prefix)?;
        let guid = vaco_format_asf::guid::Guid::parse(&prefix)
            .ok_or(Error::InvalidData("asf: truncated header object"))?;
        if guid != well_known::HEADER_OBJECT {
            return Err(Error::InvalidData(
                "asf: not an ASF file (Header Object GUID mismatch)",
            ));
        }
        let header_size = prefix
            .get(16..24)
            .and_then(<[u8]>::first_chunk::<8>)
            .map_or(0, |b| u64::from_le_bytes(*b));
        if header_size < 30 {
            return Err(Error::InvalidData(
                "asf: Header Object smaller than its own prefix",
            ));
        }
        let payload_len = usize::try_from(header_size - 30).unwrap_or(usize::MAX);
        let mut payload = budget.alloc::<u8>(payload_len)?;
        io.read_exact(&mut payload)?;

        let info = header::parse_header_object(&payload, &mut budget)?;
        let file_properties = info
            .file_properties
            .ok_or(Error::InvalidData("asf: no File Properties Object"))?;
        if info.streams.is_empty() {
            return Err(Error::InvalidData("asf: no Stream Properties Object"));
        }
        let cap = usize::try_from(opts.max_streams).unwrap_or(usize::MAX);
        if info.streams.len() > cap {
            return Err(Error::LimitExceeded {
                limit: "max_streams",
                requested: info.streams.len() as u64,
                cap: cap as u64,
            });
        }

        let packet_size = u64::from(
            file_properties
                .max_packet_size
                .max(file_properties.min_packet_size),
        );
        if packet_size == 0 {
            return Err(Error::InvalidData("asf: packet size is zero"));
        }

        // Immediately after the Header Object sits the Data Object:
        // ObjectID(16) + ObjectSize(8) + FileID(16) + TotalDataPackets(8) +
        // Reserved(2) = 50 bytes, then the first Data Packet.
        let data_object_pos = io.pos();
        let mut data_prefix = [0u8; 50];
        io.read_exact(&mut data_prefix)?;
        let data_guid = vaco_format_asf::guid::Guid::parse(&data_prefix)
            .ok_or(Error::InvalidData("asf: truncated data object"))?;
        if data_guid != well_known::DATA_OBJECT {
            return Err(Error::InvalidData(
                "asf: Header Object not followed by a Data Object",
            ));
        }
        let data_object_size = data_prefix
            .get(16..24)
            .and_then(<[u8]>::first_chunk::<8>)
            .map_or(0, |b| u64::from_le_bytes(*b));
        let data_start = io.pos();

        let file_size = io.size();
        let data_end = if file_properties.broadcast {
            file_size
        } else if data_object_size >= OBJECT_HEADER_LEN {
            let declared_end = data_object_pos.saturating_add(data_object_size);
            Some(file_size.map_or(declared_end, |sz| declared_end.min(sz)))
        } else {
            file_size
        };

        let mut streams = Vec::new();
        let mut stream_numbers = Vec::new();
        for (i, s) in info.streams.iter().enumerate() {
            let idx = u32::try_from(i).unwrap_or(u32::MAX);
            let mut stream = Stream::new(idx, s.media_type, s.time_base);
            stream.id = Some(i64::from(s.stream_number));
            stream.params = s.params.clone();
            if s.encrypted {
                stream.metadata_set("encrypted", "1");
            }
            streams.push(stream);
            stream_numbers.push(s.stream_number);
        }

        let mut index = PacketIndex::with_options(opts);
        if vaco_format_core::seek::use_container_index(opts)
            && io.seekability() != Seekability::None
            && data_object_size > 0
        {
            let scan_start = data_object_pos.saturating_add(data_object_size);
            if let Ok(found) =
                scan_trailing_index_objects(&mut io, scan_start, file_size, &mut budget)
            {
                let video_stream_numbers: Vec<u8> = info
                    .streams
                    .iter()
                    .filter(|s| s.media_type == MediaType::Video)
                    .map(|s| s.stream_number)
                    .collect();
                for (i, raw) in found.simple_index_objects.iter().enumerate() {
                    // Simple Index Objects have no stream number field of
                    // their own; they are positional, in ascending
                    // video-stream order (§6.1).
                    if video_stream_numbers.get(i).is_none() {
                        continue;
                    }
                    if let Ok(simple) = index::parse_simple_index(raw, &mut budget) {
                        let converted = index::simple_index_to_packet_index(
                            &simple,
                            data_start,
                            packet_size,
                            opts,
                        );
                        for e in converted.entries() {
                            index.add(*e);
                        }
                    }
                }
                if let Some(raw) = &found.index_object
                    && let Ok(parsed) = index::parse_index_object(raw, &mut budget)
                {
                    let converted = index::index_object_to_packet_index(&parsed, data_start, opts);
                    for e in converted.entries() {
                        index.add(*e);
                    }
                }
            }
            io.seek(data_start)?;
        }

        let effective_100ns = file_properties
            .play_duration_100ns
            .saturating_sub(file_properties.preroll_ms.saturating_mul(10_000));
        #[allow(
            clippy::integer_division,
            reason = "100-nanosecond units convert to microseconds by an exact factor of 10"
        )]
        let duration = (effective_100ns > 0).then(|| {
            Duration::from_micros(i64::try_from(effective_100ns / 10).unwrap_or(i64::MAX))
        });

        Ok(Self {
            io,
            streams,
            stream_numbers,
            packet_size,
            data_start,
            data_end,
            metadata: info.metadata,
            encryption: info.encryption,
            duration,
            preroll_ms: file_properties.preroll_ms,
            index,
            pending: std::collections::BTreeMap::new(),
            fallback_ticks: std::collections::BTreeMap::new(),
            queue: VecDeque::new(),
            budget,
            eof: false,
        })
    }

    fn stream_index_for(&self, stream_number: u8) -> Option<usize> {
        self.stream_numbers.iter().position(|&n| n == stream_number)
    }

    /// Build a [`Packet`] from a whole (already-reassembled or never
    /// fragmented) media object.
    fn make_packet(
        &mut self,
        stream_number: u8,
        key: bool,
        pts_ms: Option<u32>,
        data: &[u8],
        pos: u64,
    ) -> Result<Option<Packet>> {
        let Some(sidx) = self.stream_index_for(stream_number) else {
            // A payload naming a stream number this file never declared: not
            // our business to invent one, per the same discipline
            // `vaco-demux-avi` uses for an out-of-range chunk tag.
            return Ok(None);
        };
        let mut pkt = Packet::from_slice(&mut self.budget, data)?;
        pkt.stream_index = u32::try_from(sidx).unwrap_or(0);
        pkt.pos = Some(pos);
        let Some(stream) = self.streams.get(sidx) else {
            return Ok(None);
        };
        let time_base = stream.time_base;
        let media_type = stream.media_type();
        let ts = if let Some(ms) = pts_ms {
            // Send Time is measured from the start of buffering; subtract
            // Preroll to land on presentation time, matching `ffmpeg`'s own
            // ASF demuxer (measured: its `start_time` on a real fixture is
            // exactly the raw first Send Time less this file's own
            // Preroll). Saturating rather than wrapping negative -- a
            // packet whose Send Time falls inside the preroll window has no
            // real presentation time before zero.
            let presentation_ms = u64::from(ms).saturating_sub(self.preroll_ms);
            Timestamp::new(i64::try_from(presentation_ms).unwrap_or(i64::MAX)).rescale(
                TIME_BASE_MS,
                time_base,
                Rounding::default(),
            )
        } else {
            let tick = self.fallback_ticks.entry(stream_number).or_insert(0);
            let ts = Timestamp::new(*tick);
            *tick = tick.saturating_add(1);
            ts
        };
        // ASF's Data Packet header carries one timestamp per payload, and it
        // is decode order for video, not display order: measured against
        // `ffmpeg 9.0.1`, every ASF video packet reports `pts=N/A` (`dts`
        // only) while every audio packet reports `pts` equal to `dts`. A
        // video stream can legally reorder for display (B-frames) with
        // nothing in this one timestamp to say by how much, so — same
        // reasoning as `vaco-demux-avi::demux::read_one` — pts stays unset
        // for video rather than fabricating a value the reference never
        // claims to have.
        pkt.pts = if media_type == Some(MediaType::Video) { Timestamp::NONE } else { ts };
        pkt.dts = ts;
        pkt.duration = Duration::ZERO;
        pkt.flags = if key {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        Ok(Some(pkt))
    }

    /// Feed one parsed payload from the current physical packet into
    /// reassembly, pushing any packet(s) it completes into `self.queue`.
    fn feed_payload(&mut self, p: &ParsedPayload<'_>, pos: u64) -> Result<()> {
        let Some(offset) = p.offset else {
            // A compressed sub-payload: always a whole object already.
            if let Some(pkt) =
                self.make_packet(p.stream_number, p.key_frame, p.pts_ms, p.data, pos)?
            {
                self.queue.push_back(pkt);
            }
            return Ok(());
        };

        if offset == 0 {
            if let Some(old) = self.pending.remove(&p.stream_number)
                && old.total_len.is_none()
                && let Some(pkt) = self.make_packet(
                    p.stream_number,
                    old.key_frame,
                    old.pts_ms,
                    old.buf.as_slice(),
                    pos,
                )?
            {
                self.queue.push_back(pkt);
            }
            let declared = p.total_len.map_or(usize::MAX, |t| t as usize);
            let mut buf = IncrementalVec::new(declared);
            buf.push_slice(&mut self.budget, p.data)?;
            self.pending.insert(
                p.stream_number,
                PendingObject {
                    media_object_number: p.media_object_number,
                    key_frame: p.key_frame,
                    pts_ms: p.pts_ms,
                    total_len: p.total_len,
                    buf,
                },
            );
        } else if let Some(pending) = self.pending.get_mut(&p.stream_number)
            && pending.media_object_number == p.media_object_number
            && pending.buf.len() as u32 == offset
        {
            // A lying Replicated Data (real bytes exceeding the object size
            // it declared) surfaces as `Error::LimitExceeded`, per this
            // workspace's convention that reaching a declared-size limit is
            // the system working as intended, not a crash to guard against
            // separately.
            pending.buf.push_slice(&mut self.budget, p.data)?;
        } else {
            // Desynchronised: an offset that does not continue anything we
            // are tracking. Dropped rather than guessed at.
            return Ok(());
        }

        let done = self.pending.get(&p.stream_number).is_some_and(|pending| {
            pending
                .total_len
                .is_some_and(|total| pending.buf.len() as u32 >= total)
        });
        if done
            && let Some(pending) = self.pending.remove(&p.stream_number)
            && let Some(pkt) = self.make_packet(
                p.stream_number,
                pending.key_frame,
                pending.pts_ms,
                pending.buf.as_slice(),
                pos,
            )?
        {
            self.queue.push_back(pkt);
        }
        Ok(())
    }

    /// Read and parse the next physical Data Packet, feeding its payloads
    /// into reassembly. Returns `false` at the end of the packet-addressable
    /// region.
    fn read_one_physical_packet(&mut self) -> Result<bool> {
        let pos = self.io.pos();
        if let Some(end) = self.data_end
            && pos >= end
        {
            return Ok(false);
        }
        let remaining = self.data_end.map(|end| end.saturating_sub(pos));
        let want = remaining.map_or(self.packet_size, |r| r.min(self.packet_size));
        if want == 0 {
            return Ok(false);
        }
        let n = usize::try_from(want).unwrap_or(usize::MAX);
        let mut buf = self.budget.alloc::<u8>(n)?;
        // `read_partial` is explicitly allowed to return a short read that is
        // *not* end of stream — its own doc comment says so, and a source
        // whose internal buffer happens to run low mid-file will do exactly
        // that. `read_exact` loops until `buf` is genuinely full or the
        // source is genuinely exhausted, which is the distinction this
        // fixed-size-packet format needs: a short read here must mean no
        // more packets, never "try again for the rest of this one".
        match self.io.read_exact(&mut buf) {
            Ok(()) => {}
            Err(Error::UnexpectedEof) => return Ok(false),
            Err(e) => return Err(e),
        }
        // A short final packet (declared length not a whole multiple of
        // `packet_size`, or a source that ended early) would have already
        // been clamped into `want` above; `parse_packet` additionally bounds
        // every field to what it is given, so nothing here needs a second
        // truncation check.
        let payloads = parse_packet(&buf)?;
        for p in &payloads {
            self.feed_payload(p, pos)?;
        }
        Ok(true)
    }

    /// Flush every still-incomplete pending object as a best-effort packet —
    /// called once at end of stream so a truncated file's last fragment is
    /// not silently dropped.
    fn flush_pending(&mut self) -> Result<()> {
        let pending = core::mem::take(&mut self.pending);
        for (stream_number, obj) in pending {
            if let Some(pkt) = self.make_packet(
                stream_number,
                obj.key_frame,
                obj.pts_ms,
                obj.buf.as_slice(),
                self.data_start,
            )? {
                self.queue.push_back(pkt);
            }
        }
        Ok(())
    }

    /// The index built from the container's own Simple Index/Index Objects,
    /// or from packets seen so far under [`FormatFlags::GENERIC_INDEX`].
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    /// Detected DRM, if the Header Object carried a Content Encryption
    /// Object (or its two variants). See the module docs for what this
    /// crate does about it (nothing beyond reporting it).
    #[must_use]
    pub const fn encryption(&self) -> Option<Encryption> {
        self.encryption
    }
}

/// Scan forward from `scan_start` (the byte immediately after the Data
/// Object) for Simple Index Objects and a top-level Index Object, restoring
/// the source's position afterward. Bounded to a small number of top-level
/// objects, the same discipline `vaco-demux-avi`'s `peek_idx1` uses — in
/// practice these are the very next objects in the file.
fn scan_trailing_index_objects(
    io: &mut IoContext,
    scan_start: u64,
    file_size: Option<u64>,
    budget: &mut Budget,
) -> Result<header::HeaderInfo> {
    let resume = io.pos();
    let mut info = header::HeaderInfo::default();
    let mut pos = scan_start;
    for _ in 0..64 {
        if let Some(sz) = file_size
            && pos.saturating_add(OBJECT_HEADER_LEN) > sz
        {
            break;
        }
        io.seek(pos)?;
        let mut hdr = [0u8; 24];
        if io.read_exact(&mut hdr).is_err() {
            break;
        }
        let guid = vaco_format_asf::guid::Guid::parse(&hdr)
            .ok_or(Error::InvalidData("asf: truncated trailing object"))?;
        let size = hdr
            .get(16..24)
            .and_then(<[u8]>::first_chunk::<8>)
            .map_or(0, |b| u64::from_le_bytes(*b));
        if size < OBJECT_HEADER_LEN {
            break;
        }
        let payload_len = size - OBJECT_HEADER_LEN;
        let is_wanted = guid == well_known::SIMPLE_INDEX_OBJECT || guid == well_known::INDEX_OBJECT;
        if is_wanted {
            let n = usize::try_from(payload_len).unwrap_or(usize::MAX);
            let mut buf = budget.alloc::<u8>(n)?;
            if io.read_exact(&mut buf).is_err() {
                break;
            }
            if guid == well_known::SIMPLE_INDEX_OBJECT {
                info.simple_index_objects.push(buf);
            } else {
                info.index_object = Some(buf);
            }
        }
        pos = pos.saturating_add(size);
    }
    let _ = io.seek(resume);
    Ok(info)
}

impl Demuxer for AsfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if let Some(enc) = self.encryption {
            return Err(Error::Unsupported(match enc {
                Encryption::DrmV1 => {
                    "asf: content is protected by a Content Encryption Object (DRM v1); decryption is out of scope"
                }
                Encryption::ExtendedDrm => {
                    "asf: content is protected by an Extended Content Encryption Object (DRM v7); decryption is out of scope"
                }
                Encryption::AlternateDrm => {
                    "asf: content is protected by an Alternate Extended Content Encryption Object; decryption is out of scope"
                }
            }));
        }
        loop {
            if let Some(pkt) = self.queue.pop_front() {
                return Ok(pkt);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            if !self.read_one_physical_packet()? {
                self.flush_pending()?;
                self.eof = true;
                if let Some(pkt) = self.queue.pop_front() {
                    return Ok(pkt);
                }
                return Err(Error::Eof);
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = match target.stream_index() {
            Some(i) => {
                let st = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.streams.get(i))
                    .ok_or(Error::InvalidData("asf: seek names an unknown stream"))?;
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
                    .ok_or(Error::InvalidData("asf: seek names an unknown stream"))?;
                let want = ts.rescale(
                    st.time_base,
                    vaco_format_core::time::TIME_BASE_Q,
                    Rounding::default(),
                );
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
                    SeekStrategy::Byte => self.resync(self.data_start),
                    SeekStrategy::BinarySearch | SeekStrategy::Unsupported => {
                        Err(Error::NotSeekable)
                    }
                }
            }
            SeekTarget::Frame { .. } => Err(Error::Unsupported("asf: unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

impl AsfDemuxer {
    /// Land on the packet boundary at or before `pos` and discard whatever
    /// fragment-reassembly state was in flight — a seek always lands cleanly
    /// on a packet, never mid-fragment, so nothing salvageable survives it.
    fn resync(&mut self, pos: u64) -> Result<()> {
        self.eof = false;
        self.queue.clear();
        self.pending.clear();
        let clamped = pos.max(self.data_start);
        let offset = clamped.saturating_sub(self.data_start);
        #[allow(
            clippy::integer_division,
            reason = "rounding a byte offset down to the nearest whole packet boundary"
        )]
        let packet_index = offset / self.packet_size;
        let landed = self
            .data_start
            .saturating_add(packet_index.saturating_mul(self.packet_size));
        self.io.seek(landed)?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn object(guid: vaco_format_asf::Guid, payload: &[u8]) -> Vec<u8> {
        let mut out = guid.as_bytes().to_vec();
        out.extend_from_slice(&(OBJECT_HEADER_LEN + payload.len() as u64).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn file_properties_with_preroll(min_max_packet: u32, preroll_ms: u64) -> Vec<u8> {
        let mut p = vec![0u8; 16]; // file id
        p.extend_from_slice(&0u64.to_le_bytes()); // file size
        p.extend_from_slice(&0u64.to_le_bytes()); // creation date
        p.extend_from_slice(&2u64.to_le_bytes()); // data packets count
        p.extend_from_slice(&0u64.to_le_bytes()); // play duration
        p.extend_from_slice(&0u64.to_le_bytes()); // send duration
        p.extend_from_slice(&preroll_ms.to_le_bytes()); // preroll
        p.extend_from_slice(&0x02u32.to_le_bytes()); // flags: seekable
        p.extend_from_slice(&min_max_packet.to_le_bytes());
        p.extend_from_slice(&min_max_packet.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes()); // max bitrate
        p
    }

    fn audio_stream_properties(stream_number: u8) -> Vec<u8> {
        let mut wfx = Vec::new();
        wfx.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
        wfx.extend_from_slice(&1u16.to_le_bytes()); // mono
        wfx.extend_from_slice(&8000u32.to_le_bytes()); // sample rate
        wfx.extend_from_slice(&16000u32.to_le_bytes()); // avg bytes/sec
        wfx.extend_from_slice(&2u16.to_le_bytes()); // block align
        wfx.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        let mut p = well_known::AUDIO_MEDIA.as_bytes().to_vec();
        p.extend_from_slice(&well_known::NO_ERROR_CORRECTION.as_bytes());
        p.extend_from_slice(&0u64.to_le_bytes()); // time offset
        p.extend_from_slice(&(wfx.len() as u32).to_le_bytes()); // type-specific data length
        p.extend_from_slice(&0u32.to_le_bytes()); // error correction data length
        let flags: u16 = u16::from(stream_number); // stream number, not encrypted
        p.extend_from_slice(&flags.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes()); // reserved
        p.extend_from_slice(&wfx);
        p
    }

    fn header_extension() -> Vec<u8> {
        let mut p = well_known::RESERVED_1.as_bytes().to_vec();
        p.extend_from_slice(&6u16.to_le_bytes());
        p.extend_from_slice(&0u32.to_le_bytes()); // header extension data size: 0
        p
    }

    /// One non-multiple-payload, non-fragmented, non-compressed Data Packet
    /// of exactly `packet_size` bytes carrying `data` for `stream_number`.
    fn simple_packet(
        packet_size: usize,
        stream_number: u8,
        media_object_number: u8,
        pts_ms: u32,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0x08u8, 0x5D]; // length type flags (padding=BYTE), property flags
        // Fixed prefix so far: 2 bytes. Padding Length (BYTE) placeholder,
        // Send Time (DWORD), Duration (WORD).
        out.push(0); // padding length placeholder, patched below
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.push(stream_number | 0x80); // key frame
        out.push(media_object_number);
        out.extend_from_slice(&0u32.to_le_bytes()); // offset into media object
        out.push(8); // replicated data length
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&pts_ms.to_le_bytes());
        out.extend_from_slice(data);
        let padding = packet_size.saturating_sub(out.len());
        out[2] = u8::try_from(padding).unwrap();
        out.resize(packet_size, 0);
        out
    }

    fn build_minimal_asf(packet_size: usize, packets: &[Vec<u8>]) -> Vec<u8> {
        build_minimal_asf_with_preroll(packet_size, 0, packets)
    }

    fn build_minimal_asf_with_preroll(
        packet_size: usize,
        preroll_ms: u64,
        packets: &[Vec<u8>],
    ) -> Vec<u8> {
        let fp = object(
            well_known::FILE_PROPERTIES_OBJECT,
            &file_properties_with_preroll(packet_size as u32, preroll_ms),
        );
        let sp = object(
            well_known::STREAM_PROPERTIES_OBJECT,
            &audio_stream_properties(1),
        );
        let he = object(well_known::HEADER_EXTENSION_OBJECT, &header_extension());
        let mut header_payload = Vec::new();
        header_payload.extend_from_slice(&fp);
        header_payload.extend_from_slice(&sp);
        header_payload.extend_from_slice(&he);

        let mut header_obj = well_known::HEADER_OBJECT.as_bytes().to_vec();
        header_obj.extend_from_slice(&(30 + header_payload.len() as u64).to_le_bytes());
        header_obj.extend_from_slice(&3u32.to_le_bytes()); // num header objects
        header_obj.push(1); // reserved1
        header_obj.push(2); // reserved2
        header_obj.extend_from_slice(&header_payload);

        let mut data_payload = vec![0u8; 16]; // file id
        data_payload.extend_from_slice(&(packets.len() as u64).to_le_bytes());
        data_payload.extend_from_slice(&[1u8, 1]); // reserved 0x0101
        for p in packets {
            data_payload.extend_from_slice(p);
        }
        let data_obj = object(well_known::DATA_OBJECT, &data_payload);

        let mut out = header_obj;
        out.extend_from_slice(&data_obj);
        out
    }

    #[test]
    fn opens_and_reads_a_minimal_hand_built_file() {
        let packet_size = 64usize;
        let packets = vec![
            simple_packet(packet_size, 1, 0, 0, b"AAAA"),
            simple_packet(packet_size, 1, 1, 500, b"BBBB"),
        ];
        let bytes = build_minimal_asf(packet_size, &packets);
        let src = Box::new(MemorySource::new(bytes));
        let mut demux = AsfDemuxer::open(
            src,
            &vaco_format_core::discovery::NoParsers,
            &FormatOptions::default(),
        )
        .unwrap();
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Audio));
        assert_eq!(
            demux.streams()[0]
                .params
                .audio
                .as_ref()
                .unwrap()
                .sample_rate,
            8000
        );

        let p0 = demux.read_packet().unwrap();
        assert_eq!(p0.stream_index, 0);
        assert_eq!(p0.payload(), b"AAAA");
        assert!(p0.is_key());
        assert_eq!(p0.pts.ticks(), Some(0));

        let p1 = demux.read_packet().unwrap();
        assert_eq!(p1.payload(), b"BBBB");
        // 500ms at an 8000Hz time base is 4000 ticks.
        assert_eq!(p1.pts.ticks(), Some(4000));

        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
        // Sticky.
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    /// A non-zero Preroll is subtracted from every packet's Send Time to
    /// land on presentation time -- measured against `ffmpeg 9.0.1`: its
    /// `start_time` on a real fixture is exactly the raw first Send Time
    /// less that fixture's own File Properties Preroll. Before this fix,
    /// `Preroll` was consulted only for the aggregate duration calculation
    /// and every packet timestamp came out too large by the same fixed
    /// amount -- a real, measured bug, not a hypothetical one: it made a
    /// fresh one-second `-c copy -f asf` fixture report `start_time=3.157`
    /// where the reference reports `0.057` (3100ms is this test's own
    /// preroll value once discovered).
    #[test]
    fn a_nonzero_preroll_is_subtracted_from_every_packet_timestamp() {
        let packet_size = 64usize;
        let preroll_ms = 3100u64;
        let packets = vec![
            simple_packet(packet_size, 1, 0, 3100, b"AAAA"),
            simple_packet(packet_size, 1, 1, 3600, b"BBBB"),
        ];
        let bytes = build_minimal_asf_with_preroll(packet_size, preroll_ms, &packets);
        let src = Box::new(MemorySource::new(bytes));
        let mut demux = AsfDemuxer::open(
            src,
            &vaco_format_core::discovery::NoParsers,
            &FormatOptions::default(),
        )
        .unwrap();

        // Raw Send Time 3100ms minus 3100ms preroll lands exactly on 0.
        let p0 = demux.read_packet().unwrap();
        assert_eq!(p0.pts.ticks(), Some(0));
        // Raw Send Time 3600ms minus preroll is 500ms, i.e. 4000 ticks at
        // this stream's 8000Hz time base -- same as the no-preroll test
        // above, confirming the subtraction and not just a lucky zero.
        let p1 = demux.read_packet().unwrap();
        assert_eq!(p1.pts.ticks(), Some(4000));
    }

    #[test]
    fn probe_matches_the_header_object_guid() {
        let mut bytes = well_known::HEADER_OBJECT.as_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        let data = ProbeData::new(&bytes);
        assert_eq!(probe(&data), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn probe_rejects_plain_text() {
        let data = ProbeData::new(b"this is not an ASF file, just some prose.");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn asf_o_never_self_selects() {
        let mut bytes = well_known::HEADER_OBJECT.as_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        let data = ProbeData::new(&bytes);
        assert_eq!(probe_opaque(&data), ProbeScore::NONE);
    }
}
