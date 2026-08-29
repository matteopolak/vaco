//! The Ogg demuxer: page framing, packet reassembly, chained/multiplexed
//! logical streams, and the per-codec granule-position timeline.
//!
//! # What "packet" means here
//!
//! A packet can span pages (the segment table's trailing `255` lacing value
//! means "continued"), and one page commonly holds many packets — measured
//! files in this crate's test corpus show 44 Vorbis packets sharing one
//! page. [`crate::page`] turns a page's segment table into byte ranges;
//! this module is what stitches a range that does not finish on this page
//! into the next page that resumes it (which must carry the `CONTINUED`
//! flag — a page that resumes nothing, or a page that fails to resume
//! something pending, is a corruption this module reports through
//! [`OggDemuxStats`] rather than propagating as a hard error, matching
//! `vaco-demux-mpegts`'s stance on continuity gaps).
//!
//! # Chained and multiplexed streams
//!
//! Both are handled by the same rule: **a new serial number is a new
//! entry in [`OggDemuxer::streams`], appended, never replacing an earlier
//! one** — whether it appears interleaved with existing streams from the
//! start (multiplexed) or only after an earlier stream's `EOS` page
//! (chained). No special-casing is needed because every packet is already
//! processed per serial number, independently of any other stream's state.
//! What is **not** handled: re-deriving a single continuous timeline across
//! a chain boundary (the reference reports a stream/format change there; so
//! do we, by simply starting a fresh [`crate::granule::GranuleTimeline`] for
//! the new serial number — its timestamps are relative to *its own* start,
//! not offset to continue the previous logical stream's).
//!
//! # What reaches a caller through `ParserProvider`
//!
//! Only Opus, and only for streams discovered during [`OggDemuxer::open`].
//! The frozen [`Demuxer::read_packet`] takes no provider, so a logical
//! stream that first appears later (a chained Ogg past the first one) gets
//! no parser and falls back to the page-anchored equal-division estimate —
//! see [`crate::granule`]. This is a real, disclosed gap, not silent
//! degradation: [`LogicalStream::parser`] is `None` in exactly that case and
//! nothing pretends otherwise.

use vaco_chlayout::ChannelLayout;
use vaco_sampfmt::SampleFmt;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser, VideoParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::codec::{self, OggCodec};
use crate::granule::{self, GranuleMapping, GranuleTimeline};
use crate::page::{self, OggHeaderFlags, OggPageHeader};

/// What Ogg declares it can do.
///
/// No `SHOW_IDS`: the bitstream serial number is a real per-logical-stream
/// identifier, but `ffprobe -bitexact -show_streams` prints `id=N/A` for
/// every stream of a real Ogg/Vorbis file regardless — the same call it
/// makes for Matroska's `TrackNumber`, an equally real identifier it also
/// declines to print. `GENERIC_INDEX` because nothing here carries a seek
/// index of its own — an index is only ever what packets already read
/// build up.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX;

/// Bytes scanned looking for the next page's capture pattern before this
/// crate calls the stream unrecoverable. Generous relative to
/// [`page::MAX_PAGE_LEN`], since a single dropped/corrupt page must not
/// abort a whole file.
pub const MAX_RESYNC_BYTES: u64 = 1 << 20;

/// Resync attempts allowed inside one call before giving up. Bounds the
/// pathological "the file is nothing but false `OggS` matches" case to a
/// fixed amount of work rather than a fuzzer-visible hang.
const MAX_RESYNC_ATTEMPTS: u32 = 4096;

/// Largest a single logical packet may grow across page continuations before
/// this crate treats the stream as hostile and abandons it. Separate from
/// (and much larger than) [`page::MAX_BODY_LEN`], which bounds one page;
/// this bounds how many pages may keep extending the *same* packet.
pub const MAX_PACKET_BYTES: usize = 32 << 20;

/// Bytes of the initial page-scan `open()` performs looking for every
/// currently-known stream's headers, before giving up on a file whose
/// headers never complete.
pub const MAX_HEADER_SCAN_BYTES: u64 = 8 << 20;

/// Bytes read from near the end of a seekable source when scanning for each
/// logical stream's final granule position (`duration_ts`). Comfortably
/// larger than [`page::MAX_PAGE_LEN`], so a normally-paced file's last page
/// is found even if the window's own start lands mid-page; a pathological
/// tail run of oversized pages simply reports no duration, same as a source
/// this scan cannot seek at all.
pub const TAIL_SCAN_WINDOW: u64 = 256 << 10;

/// Counters a caller can read for triage. None of them changes behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OggDemuxStats {
    /// Pages whose stored checksum did not match.
    pub crc_failures: u64,
    /// Bytes skipped resynchronising to the next page's capture pattern.
    pub resync_bytes: u64,
    /// A page carried `CONTINUED` with nothing pending, or omitted it with
    /// something pending — either way the partial packet was discarded.
    pub dangling_continuations: u64,
    /// A packet's page-spanning reassembly passed [`MAX_PACKET_BYTES`].
    pub oversized_packets: u64,
    /// Page sequence numbers that did not increase by exactly one.
    pub sequence_gaps: u64,
}

struct LogicalStream {
    serial: u32,
    stream_index: u32,
    codec: OggCodec,
    mapping: GranuleMapping,
    timeline: GranuleTimeline,
    time_base: Rational,
    header_total: u32,
    header_seen: u32,
    pending: Vec<u8>,
    pending_open: bool,
    last_sequence: Option<u32>,
    eos: bool,
    /// Every header packet seen so far, in order — accumulated so the last
    /// one arriving can trigger [`codec::pack_xiph_headers`] into
    /// `Stream::params.extradata`. Empty once the stream has emitted its
    /// packed extradata (or immediately, for a codec whose one-packet
    /// `extradata` convention `describe` already sets correctly), so this
    /// never holds more than one stream's worth of header bytes at a time.
    header_bytes: Vec<Vec<u8>>,
    /// Only ever `Some` for Opus, and only when [`OggDemuxer::open`] itself
    /// discovered the stream — see the module docs.
    parser: Option<Box<dyn Parser>>,
}

impl core::fmt::Debug for LogicalStream {
    #[allow(
        clippy::missing_fields_in_debug,
        reason = "`parser` is a trait object with no Debug impl; `has_parser` \
                  stands in for it deliberately, not by omission"
    )]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LogicalStream")
            .field("serial", &self.serial)
            .field("stream_index", &self.stream_index)
            .field("codec", &self.codec)
            .field("mapping", &self.mapping)
            .field("timeline", &self.timeline)
            .field("time_base", &self.time_base)
            .field("header_total", &self.header_total)
            .field("header_seen", &self.header_seen)
            .field("pending_len", &self.pending.len())
            .field("pending_open", &self.pending_open)
            .field("last_sequence", &self.last_sequence)
            .field("eos", &self.eos)
            .field("header_bytes_len", &self.header_bytes.len())
            .field("has_parser", &self.parser.is_some())
            .finish()
    }
}

/// The Ogg demuxer.
#[derive(Debug)]
pub struct OggDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    logical: Vec<LogicalStream>,
    budget: Budget,
    queue: std::collections::VecDeque<Packet>,
    eof: bool,
    stats: OggDemuxStats,
}

impl OggDemuxer {
    /// Open an Ogg physical stream.
    ///
    /// Scans forward, consuming header packets for every logical stream
    /// discovered near the start of the file, until every stream seen so far
    /// has finished its headers or [`MAX_HEADER_SCAN_BYTES`] is exhausted.
    /// Chained streams that begin later are picked up by
    /// [`Demuxer::read_packet`] as ordinary reading reaches them.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when no valid page can be found at all.
    pub fn open(
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        _opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, parsers, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling — the constructor a fuzz
    /// target or an embedder that cares reaches for, matching the shape
    /// `vaco-demux-mpegts::MpegTsDemuxer::open_with_limits` already
    /// established.
    ///
    /// # Errors
    /// As [`OggDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        limits: Limits,
    ) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut me = Self {
            io,
            streams: Vec::new(),
            logical: Vec::new(),
            budget: Budget::new(limits),
            queue: std::collections::VecDeque::new(),
            eof: false,
            stats: OggDemuxStats::default(),
        };
        let start = me.io.pos();
        loop {
            if me.io.pos().saturating_sub(start) >= MAX_HEADER_SCAN_BYTES {
                break;
            }
            if me.headers_complete() {
                break;
            }
            match me.pump_one_page(Some(parsers)) {
                Ok(()) => {}
                Err(Error::Eof) => break,
                Err(e) if e.is_recoverable() => {}
                Err(e) => return Err(e),
            }
        }
        if me.streams.is_empty() {
            return Err(Error::InvalidData("no Ogg logical bitstream found"));
        }
        // Whatever data packets the header scan already queued are real
        // packets (a page can legitimately mix trailing header packets with
        // the start of data, as the Vorbis measurement in `granule.rs`
        // shows) — they stay queued rather than being discarded, since
        // `read_packet` must not skip real content just because it arrived
        // during discovery.
        me.scan_tail_for_durations();
        Ok(me)
    }

    /// Best-effort: state each logical stream's `duration_ts` from the last
    /// granule position it ever pages out.
    ///
    /// Ogg carries no length field anywhere — measured, the reference's own
    /// `duration_ts` for a Vorbis stream is exactly the final page's raw
    /// granule position, un-adjusted by [`GranuleMapping::timestamp`]'s
    /// pre-roll subtraction (that adjustment is for a *packet's* pts, not a
    /// summary duration): a 1 000-sample-per-page-boundary file whose last
    /// page's granule reads `44160` reports `duration_ts=44160`, matching
    /// `44160 / 44100 Hz = 1.001361 s` exactly.
    ///
    /// Reads a single bounded window from near the end of the source and
    /// scans it for page headers — no reliance on the forward scan's own
    /// state, so a malformed page earlier in the window does not stop the
    /// scan. Does nothing, silently, when the source cannot report a size or
    /// cannot seek (a pipe): every stream simply keeps `duration_ts = None`,
    /// exactly as before this pass.
    fn scan_tail_for_durations(&mut self) {
        if self.io.seekability() == Seekability::None {
            return;
        }
        let Some(size) = self.io.size() else { return };
        let saved = self.io.pos();
        let window = TAIL_SCAN_WINDOW.min(size);
        let Ok(()) = self.io.seek(size.saturating_sub(window)).map(|_| ()) else {
            return;
        };
        let read = self.budget.alloc::<u8>(usize::try_from(window).unwrap_or(0));
        let buf = match read {
            Ok(mut buf) => {
                let n = self.io.read_partial(&mut buf).unwrap_or(0);
                buf.truncate(n);
                buf
            }
            Err(_) => Vec::new(),
        };
        // Restore the forward-scan position before anything else can observe
        // it, even if the window above came back empty.
        let _ = self.io.seek(saved);

        let mut last_granule: Vec<(u32, i64)> = Vec::new();
        let mut at = 0usize;
        while let Some(rel) = buf
            .get(at..)
            .and_then(|s| s.windows(4).position(|w| w == page::CAPTURE_PATTERN))
        {
            let start = at + rel;
            if let Ok((header, _)) = page::parse_header(buf.get(start..).unwrap_or(&[]))
                && let Some(g) = header.granule()
            {
                match last_granule.iter_mut().find(|(s, _)| *s == header.serial) {
                    Some((_, slot)) => *slot = g,
                    None => last_granule.push((header.serial, g)),
                }
            }
            at = start + 4;
        }

        for (serial, granule) in last_granule {
            if let Some(index) = self.logical_index(serial)
                && let Some(stream_index) = self
                    .logical
                    .get(index)
                    .map(|l| usize::try_from(l.stream_index).unwrap_or(usize::MAX))
                && let Some(stream) = self.streams.get_mut(stream_index)
                && stream.duration_ts.is_none()
            {
                stream.set_duration_ts(granule);
            }
        }
    }

    /// Counters for triage.
    #[must_use]
    pub const fn stats(&self) -> OggDemuxStats {
        self.stats
    }

    fn headers_complete(&self) -> bool {
        !self.logical.is_empty() && self.logical.iter().all(|l| l.header_seen >= l.header_total)
    }

    fn logical_index(&self, serial: u32) -> Option<usize> {
        self.logical.iter().position(|l| l.serial == serial)
    }

    /// Read exactly one page, feed it through reassembly, and push whatever
    /// complete data packets it yields onto [`Self::queue`].
    ///
    /// `parsers` is only ever `Some` while [`Self::open`] is still running —
    /// see the module docs on why a later-discovered stream cannot reach it.
    fn pump_one_page(&mut self, parsers: Option<&dyn ParserProvider>) -> Result<()> {
        let (header, body) = self.read_one_page()?;
        let granule = header.granule_position;

        let is_new = self.logical_index(header.serial).is_none();
        let idx = if is_new {
            self.begin_logical_stream(&header, &body, parsers)
        } else {
            self.logical_index(header.serial).unwrap_or(0)
        };

        self.check_sequence_gap(idx, header.sequence);

        let spans = page::packet_spans(&header.segments);
        let mut completed: Vec<Vec<u8>> = Vec::new();
        let mut span_iter = spans.iter().enumerate().peekable();

        // A leading span continuing a packet already in flight.
        if let Some((_, first)) = span_iter.peek().copied() {
            let continuing = header.flags.contains(OggHeaderFlags::CONTINUED);
            let has_pending = self.logical.get(idx).is_some_and(|l| l.pending_open);
            if continuing != has_pending {
                // Dangling either way: a flagged continuation with nothing to
                // continue, or an unflagged page while something was open.
                self.stats.dangling_continuations =
                    self.stats.dangling_continuations.saturating_add(1);
                self.discard_pending(idx);
            }
            if continuing && has_pending {
                let chunk = body.get(first.start..first.end).unwrap_or(&[]);
                self.append_pending(idx, chunk)?;
                let _ = span_iter.next();
                if first.complete
                    && let Some(pkt) = self.take_pending(idx)
                {
                    completed.push(pkt);
                }
            }
        }

        for (_, span) in span_iter {
            let chunk = body.get(span.start..span.end).unwrap_or(&[]).to_vec();
            if span.complete {
                completed.push(chunk);
            } else {
                self.append_pending(idx, &chunk)?;
                let Some(l) = self.logical.get_mut(idx) else {
                    continue;
                };
                l.pending_open = true;
            }
        }

        self.classify_and_emit(idx, granule, completed);

        if header.flags.contains(OggHeaderFlags::EOS)
            && let Some(l) = self.logical.get_mut(idx)
        {
            l.eos = true;
        }
        Ok(())
    }

    fn check_sequence_gap(&mut self, idx: usize, sequence: u32) {
        let Some(l) = self.logical.get_mut(idx) else {
            return;
        };
        if let Some(prev) = l.last_sequence
            && sequence != prev.wrapping_add(1)
        {
            self.stats.sequence_gaps = self.stats.sequence_gaps.saturating_add(1);
        }
        l.last_sequence = Some(sequence);
    }

    fn discard_pending(&mut self, idx: usize) {
        if let Some(l) = self.logical.get_mut(idx) {
            let n = l.pending.len() as u64;
            l.pending.clear();
            l.pending_open = false;
            self.budget.release(n);
        }
    }

    fn append_pending(&mut self, idx: usize, chunk: &[u8]) -> Result<()> {
        self.budget.charge(chunk.len() as u64)?;
        let Some(l) = self.logical.get_mut(idx) else {
            return Ok(());
        };
        if l.pending.len().saturating_add(chunk.len()) > MAX_PACKET_BYTES {
            let n = l.pending.len() as u64;
            l.pending.clear();
            l.pending_open = false;
            self.budget.release(n);
            self.budget.release(chunk.len() as u64);
            self.stats.oversized_packets = self.stats.oversized_packets.saturating_add(1);
            return Ok(());
        }
        l.pending.extend_from_slice(chunk);
        Ok(())
    }

    fn take_pending(&mut self, idx: usize) -> Option<Vec<u8>> {
        let l = self.logical.get_mut(idx)?;
        l.pending_open = false;
        let n = l.pending.len() as u64;
        self.budget.release(n);
        Some(core::mem::take(&mut l.pending))
    }

    /// Classify each packet completed on this page as header or data, and
    /// timestamp the data ones through the granule timeline.
    fn classify_and_emit(&mut self, idx: usize, page_granule: i64, completed: Vec<Vec<u8>>) {
        let mut data_bytes: Vec<Vec<u8>> = Vec::new();
        for bytes in completed {
            let Some((header_index, header_total, codec, stream_index)) =
                self.logical.get(idx).map(|l| {
                    (l.header_seen, l.header_total, l.codec, l.stream_index)
                })
            else {
                continue;
            };
            if header_index < header_total {
                if let Some(l) = self.logical.get_mut(idx) {
                    l.header_seen = l.header_seen.saturating_add(1);
                }
                // Packet index 1 is the comment header for both Vorbis
                // (spec §4.1) and Opus (RFC 7845 §5.2's `OpusTags`) — the
                // one header packet this crate reads past its fixed-offset
                // fields, since `ffprobe -bitexact -show_streams` prints
                // its `TITLE`/`ENCODER`/etc. fields as `TAG:` entries and a
                // demuxer that never opens the packet cannot say the same.
                if header_index == 1 {
                    let magic = match codec {
                        OggCodec::Vorbis => Some(codec::VORBIS_COMMENT_MAGIC),
                        OggCodec::Opus => Some(codec::OPUS_COMMENT_MAGIC),
                        _ => None,
                    };
                    if let Some(magic) = magic {
                        let tags = codec::parse_comment_header(&bytes, magic);
                        let si = usize::try_from(stream_index).unwrap_or(usize::MAX);
                        if let Some(stream) = self.streams.get_mut(si) {
                            for (key, value) in tags {
                                stream.metadata_set(&key, value);
                            }
                        }
                    }
                }
                // Vorbis's and Theora's `extradata` convention both need
                // every header packet, not just the identification one
                // `describe` already stored (see `codec::pack_xiph_headers`'s
                // doc comment for the measured layout) — accumulated here and
                // packed once the last one arrives, since that is the first
                // point all of them exist. `header_index` was read before
                // this packet's own increment above, so
                // `header_index + 1 == header_total` means this member of
                // `completed` was that last header.
                //
                // Before this, Theora's `extradata` was only ever the raw
                // identification (BOS) packet `describe` sets — the comment
                // and setup headers were counted (via
                // `total_header_packets`, so they were correctly withheld
                // from the data-packet stream) but never packed in, which
                // silently discarded the setup header every real Theora
                // decoder needs (quantization tables, Huffman tables, loop
                // filter limits) for every single Ogg/Theora stream. Found by
                // running `vaco-codec-theora`'s decoder against a real
                // `ffmpeg`-produced `.ogv` fixture (`bear.ogv`, `ffmpeg`
                // FATE suite) rather than a synthetic extradata blob.
                let is_last_header = header_index.saturating_add(1) == header_total;
                if matches!(codec, OggCodec::Vorbis | OggCodec::Theora) {
                    if let Some(l) = self.logical.get_mut(idx) {
                        l.header_bytes.push(bytes);
                    }
                    if is_last_header {
                        let packed = self
                            .logical
                            .get_mut(idx)
                            .map(|l| codec::pack_xiph_headers(&core::mem::take(&mut l.header_bytes)));
                        if let Some(packed) = packed {
                            let si = usize::try_from(stream_index).unwrap_or(usize::MAX);
                            if let Some(stream) = self.streams.get_mut(si) {
                                stream.params.extradata = Some(packed);
                            }
                        }
                    }
                }
                continue;
            }
            data_bytes.push(bytes);
        }
        if data_bytes.is_empty() {
            return;
        }
        let Some(l) = self.logical.get(idx) else {
            return;
        };
        let time_base = l.time_base;
        let mapping = l.mapping.clone();
        // The fallback distributes the *delta* this page's granule
        // represents over the cursor's current position — never the raw
        // granule value, which is an absolute position and would treat
        // every page after the first as spanning from tick zero — weighted
        // by each packet's own byte length rather than split evenly.
        //
        // Measured on the FLAC file `crate::granule`'s doc comments
        // describe: an even split puts the final page's short trailing
        // frame's shortfall on *every* packet in the page (all ten read
        // 4212 instead of nine at 4608 and one at 648, since 42120 divides
        // evenly by ten). A real encoder's frame size correlates with its
        // sample count — smaller frames decode to fewer samples far more
        // often than they decode to the same count at a different bit rate
        // — so weighting by length is a strictly better guess with no extra
        // parsing. `assign` still snaps the page's *last* packet to the
        // granule exactly regardless, so this only changes the intra-page
        // distribution, never the per-page total.
        let total_bytes: i64 = data_bytes.iter().map(|b| len_i64(b.len())).sum();
        let fallback_delta = mapping
            .timestamp(page_granule)
            .map(|target| target.saturating_sub(l.timeline.planned_cursor(&mapping)));
        let nominal: Vec<i64> = data_bytes
            .iter()
            .map(|b| {
                granule::nominal_duration(&mapping, l.parser.as_deref(), time_base, b)
                    .unwrap_or_else(|| {
                        let Some(delta) = fallback_delta else {
                            return 0;
                        };
                        if total_bytes <= 0 {
                            return delta
                                .checked_div(i64::try_from(data_bytes.len().max(1)).unwrap_or(1))
                                .unwrap_or(0)
                                .max(0);
                        }
                        delta
                            .saturating_mul(len_i64(b.len()))
                            .checked_div(total_bytes)
                            .unwrap_or(0)
                            .max(0)
                    })
            })
            .collect();
        let Some(l) = self.logical.get_mut(idx) else {
            return;
        };
        let assigned = l.timeline.assign(&mapping, page_granule, &nominal);
        let stream_index = l.stream_index;
        let is_video = matches!(mapping, GranuleMapping::Theora { .. });
        let theora_mask = match mapping {
            GranuleMapping::Theora { granule_shift } => {
                (1i64 << granule_shift.min(62)).saturating_sub(1)
            }
            _ => 0,
        };
        let last = assigned.len().saturating_sub(1);
        for (i, (bytes, (pts, dur))) in data_bytes.into_iter().zip(assigned).enumerate() {
            let Ok(mut pkt) = Packet::from_slice(&mut self.budget, &bytes) else {
                continue;
            };
            pkt.stream_index = stream_index;
            pkt.pts = Timestamp::new(pts);
            pkt.dts = pkt.pts;
            pkt.duration = Timestamp::new(dur)
                .to_duration(time_base)
                .unwrap_or(Duration::ZERO);
            let key = if is_video {
                i == last
                    && page_granule != page::GRANULE_UNSET
                    && (page_granule & theora_mask) == 0
            } else {
                true
            };
            pkt.flags = if key {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            };
            self.queue.push_back(pkt);
        }
    }

    /// A page whose serial number has not been seen before: identify the
    /// codec from its first packet and register a new [`Stream`].
    fn begin_logical_stream(
        &mut self,
        header: &OggPageHeader,
        body: &[u8],
        parsers: Option<&dyn ParserProvider>,
    ) -> usize {
        // The BOS packet is always the first span; if the table is empty or
        // the span is incomplete (an implausibly tiny first page), fall back
        // to whatever bytes are available rather than failing the file.
        let spans = page::packet_spans(&header.segments);
        let bos_bytes: Vec<u8> = spans
            .first()
            .map(|s| body.get(s.start..s.end).unwrap_or(&[]).to_vec())
            .unwrap_or_default();
        let codec = codec::identify(&bos_bytes);
        let mapping = GranuleMapping::from_bos(codec, &bos_bytes);
        let header_total = codec::total_header_packets(codec, &bos_bytes);

        let (media, time_base, params) = describe(codec, &bos_bytes);
        let stream_index = u32::try_from(self.streams.len()).unwrap_or(u32::MAX);
        let mut stream = Stream::new(stream_index, media, time_base);
        stream.params = params;
        self.streams.push(stream);

        let parser = if codec == OggCodec::Opus {
            parsers
                .and_then(|p| p.parser_for(CodecId::Opus))
                .map(|mut parser: Box<dyn Parser>| {
                    let _ = parser.set_extradata(&bos_bytes);
                    parser
                })
        } else {
            None
        };

        self.logical.push(LogicalStream {
            serial: header.serial,
            stream_index,
            codec,
            mapping,
            timeline: GranuleTimeline::new(),
            time_base,
            header_total,
            header_seen: 0,
            pending: Vec::new(),
            pending_open: false,
            last_sequence: None,
            eos: false,
            header_bytes: Vec::new(),
            parser,
        });
        self.logical.len().saturating_sub(1)
    }
}

/// Media type, time base and initial [`CodecParameters`] for a newly
/// discovered stream, from its identification packet.
fn describe(codec: OggCodec, bos: &[u8]) -> (MediaType, Rational, CodecParameters) {
    match codec {
        OggCodec::Opus => {
            let ident = codec::parse_opus_head(bos);
            let mut params = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Opus);
            params.extradata = Some(bos.to_vec());
            params.audio = Some(AudioParameters {
                sample_rate: granule::opus_time_base().den.unsigned_abs(),
                layout: ident.and_then(|h| ChannelLayout::default_for(u32::from(h.channel_count))),
                initial_padding: u32::from(ident.map_or(0, |h| h.pre_skip)),
                // Measured, not assumed:
                //   ffprobe -of csv=p=0 -show_entries stream=sample_fmt t.opus  # fltp
                format: Some(SampleFmt::F32P),
                ..AudioParameters::default()
            });
            (MediaType::Audio, granule::opus_time_base(), params)
        }
        OggCodec::Vorbis => {
            let ident = codec::parse_vorbis_ident(bos);
            let rate = ident.map_or(0, |v| v.sample_rate);
            let mut params = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Vorbis);
            // Placeholder until `classify_and_emit` sees the comment and
            // setup headers too and replaces this with
            // `codec::pack_xiph_headers`'s packed blob — see its doc
            // comment. Left as the identification packet alone in the
            // meantime rather than `None`, so a caller that only ever reads
            // this far still gets something.
            params.extradata = Some(bos.to_vec());
            params.audio = Some(AudioParameters {
                sample_rate: rate,
                layout: ident.and_then(|v| ChannelLayout::default_for(u32::from(v.channels))),
                // Measured, not assumed:
                //   ffprobe -of csv=p=0 -show_entries stream=sample_fmt t.ogg  # fltp
                // Same answer as Opus above and for the same reason: both
                // decode to ffmpeg's internal float-planar representation.
                format: Some(SampleFmt::F32P),
                ..AudioParameters::default()
            });
            let tb = safe_rational(1, rate);
            (MediaType::Audio, tb, params)
        }
        OggCodec::Flac => {
            let info = codec::parse_flac_streaminfo(bos);
            let rate = info.map_or(0, |i| i.sample_rate);
            let mut params = CodecParameters::new(MediaType::Audio).with_codec(CodecId::Flac);
            params.extradata = Some(bos.to_vec());
            params.audio = Some(AudioParameters {
                sample_rate: rate,
                layout: info.and_then(|i| ChannelLayout::default_for(u32::from(i.channels))),
                bits_per_raw_sample: info.map(|i| i.bits_per_sample),
                // 16-bit FLAC reports `s16`, 24-bit reports `s32` — measured
                // on both, which is every depth this reference build's FLAC
                // encoder will actually produce (it clamps 8, 20 and 32 to
                // 24). The threshold is therefore pinned at the two points we
                // have and stated rather than extrapolated: above 16 bits the
                // samples do not fit an i16.
                format: info.map(|i| {
                    if i.bits_per_sample > 16 {
                        SampleFmt::S32
                    } else {
                        SampleFmt::S16
                    }
                }),
                ..AudioParameters::default()
            });
            let tb = safe_rational(1, rate);
            (MediaType::Audio, tb, params)
        }
        OggCodec::Speex => {
            let ident = codec::parse_speex_ident(bos);
            let rate = ident.map_or(0, |s| s.rate);
            // No `CodecId::Speex` exists (confirmed by reading
            // `vaco-codec-core`'s enum) — recorded as metadata only, the
            // same gap `vaco-demux-mpegts` documents for codecs its own
            // table cannot name.
            let mut params = CodecParameters::new(MediaType::Audio);
            params.extradata = Some(bos.to_vec());
            params.audio = Some(AudioParameters {
                sample_rate: rate,
                layout: ident.and_then(|s| ChannelLayout::default_for(s.channels)),
                ..AudioParameters::default()
            });
            let tb = safe_rational(1, rate);
            (MediaType::Audio, tb, params)
        }
        OggCodec::Theora => {
            let ident = codec::parse_theora_ident(bos);
            let mut params = CodecParameters::new(MediaType::Video).with_codec(CodecId::Theora);
            params.extradata = Some(bos.to_vec());
            let (num, den) = ident.map_or((0, 1), |t| (t.fps_numerator, t.fps_denominator));
            let frame_rate = safe_rational(num, den);
            params.video = Some(VideoParameters {
                width: ident.map_or(0, |t| t.width),
                height: ident.map_or(0, |t| t.height),
                coded_width: ident.map_or(0, |t| t.width),
                coded_height: ident.map_or(0, |t| t.height),
                frame_rate,
                ..VideoParameters::default()
            });
            let tb = if frame_rate.is_defined() && !frame_rate.is_zero() {
                frame_rate.inverse()
            } else {
                Rational::new(1, 1)
            };
            (MediaType::Video, tb, params)
        }
        OggCodec::Unknown => (
            MediaType::Data,
            Rational::new(1, 1),
            CodecParameters::new(MediaType::Data),
        ),
    }
}

impl OggDemuxer {
    /// Read one whole page (header, segment table and body) starting at the
    /// I/O context's current position, resynchronising on a bad capture
    /// pattern or a failed checksum.
    fn read_one_page(&mut self) -> Result<(OggPageHeader, Vec<u8>)> {
        for _ in 0..MAX_RESYNC_ATTEMPTS {
            let pos = self.io.pos();
            let mut fixed = [0u8; page::FIXED_HEADER_LEN];
            match self.io.read_exact(&mut fixed) {
                Ok(()) => {}
                Err(Error::UnexpectedEof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            }
            if fixed.get(0..4) != Some(&page::CAPTURE_PATTERN[..]) {
                self.resync_from(pos)?;
                continue;
            }
            let page_segments = usize::from(*fixed.get(26).unwrap_or(&0));
            let mut segments = vec![0u8; page_segments];
            if self.io.read_exact(&mut segments).is_err() {
                self.eof = true;
                return Err(Error::Eof);
            }
            let mut full = Vec::new();
            full.extend_from_slice(&fixed);
            full.extend_from_slice(&segments);
            let Ok((header, _)) = page::parse_header(&full) else {
                self.resync_from(pos)?;
                continue;
            };
            let body_len = header.body_len();
            self.budget.charge(body_len as u64)?;
            let mut body = vec![0u8; body_len];
            if self.io.read_exact(&mut body).is_err() {
                self.budget.release(body_len as u64);
                self.eof = true;
                return Err(Error::Eof);
            }
            full.extend_from_slice(&body);
            let ok = page::verify_checksum(&full).unwrap_or(false);
            self.budget.release(body_len as u64);
            if !ok {
                self.stats.crc_failures = self.stats.crc_failures.saturating_add(1);
                self.resync_from(pos)?;
                continue;
            }
            return Ok((header, body));
        }
        Err(Error::InvalidData("too many consecutive invalid Ogg pages"))
    }

    /// Seek back to `after + 1` and scan forward for the next capture
    /// pattern, landing on it. Bounded by [`MAX_RESYNC_BYTES`].
    fn resync_from(&mut self, after: u64) -> Result<()> {
        let from = after.saturating_add(1);
        let limit = from.saturating_add(MAX_RESYNC_BYTES);
        self.io.seek(from)?;
        let mut window = [0u8; 4];
        for slot in &mut window {
            let Ok(b) = self.io.r8() else {
                self.eof = true;
                return Err(Error::Eof);
            };
            *slot = b;
        }
        let mut at = from;
        loop {
            if window == page::CAPTURE_PATTERN {
                self.io.seek(at)?;
                self.stats.resync_bytes = self.stats.resync_bytes.saturating_add(at - from);
                return Ok(());
            }
            at = at.saturating_add(1);
            if at >= limit {
                return Err(Error::InvalidData(
                    "no Ogg page found within the resync window",
                ));
            }
            let Ok(next) = self.io.r8() else {
                self.eof = true;
                return Err(Error::Eof);
            };
            window = [window[1], window[2], window[3], next];
        }
    }
}

/// A byte length as `i64`, saturating rather than wrapping — every real
/// length here is bounded well under `i64::MAX` by [`MAX_PACKET_BYTES`], so
/// this only ever matters for making the cast lint provably safe.
fn len_i64(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// `Rational::new(num, den)`, falling back to an undefined-but-safe `1/1`
/// when `den` is zero or the values do not fit — a malformed header field
/// must not produce a time base nothing can rescale against.
fn safe_rational(num: u32, den: u32) -> Rational {
    if den == 0 {
        return Rational::new(1, 1);
    }
    match (i32::try_from(num), i32::try_from(den)) {
        (Ok(n), Ok(d)) => Rational::new(n, d),
        _ => Rational::new(1, 1),
    }
}

impl Demuxer for OggDemuxer {
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
            match self.pump_one_page(None) {
                Ok(()) => {}
                Err(Error::Eof) => {
                    self.eof = true;
                    if self.queue.is_empty() {
                        return Err(Error::Eof);
                    }
                }
                Err(e) if e.is_recoverable() => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        // No index, no bisection oracle beyond what `crate::page` already
        // gives a caller directly — only the byte path is implemented today.
        // See the docs file: full timestamp seeking needs the same
        // page-granule bisection `vaco-demux-mpegts` does for MPEG-TS, which
        // is future work rather than something this pass attempted.
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                self.io.seek(pos)?;
                self.eof = false;
                self.queue.clear();
                for l in &mut self.logical {
                    l.pending.clear();
                    l.pending_open = false;
                }
                Ok(())
            }
            _ => Err(Error::Unsupported("Ogg timestamp seeking")),
        }
    }

    // `duration()` is not overridden: this pass does not implement the tail
    // scan a real estimate needs (see the docs file's "gaps" section), and
    // the trait's default `None` is a more honest answer than a guess.
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::{OggCodec, describe};
    use vaco_sampfmt::SampleFmt;

    /// A minimal Ogg-FLAC identification packet: the `\x7FFLAC` mapping header
    /// followed by a STREAMINFO metadata block. Only the packed 64-bit region
    /// at offset 10 of the block matters here — 20 bits sample rate, 3 bits
    /// channels-1, 5 bits bits_per_sample-1, 36 bits total samples.
    fn flac_bos(sample_rate: u32, channels: u8, bits_per_sample: u8) -> Vec<u8> {
        let mut v = vec![0x7F];
        v.extend_from_slice(b"FLAC");
        v.extend_from_slice(&[1, 0]); // mapping major/minor
        v.extend_from_slice(&[0, 1]); // header count
        v.extend_from_slice(b"fLaC");
        v.push(0x00); // metadata block header: STREAMINFO, not last
        v.extend_from_slice(&[0, 0, 34]); // block length
        v.extend_from_slice(&[0_u8; 10]); // block/frame size fields
        let packed = (u64::from(sample_rate) << 44)
            | (u64::from(channels.saturating_sub(1)) << 41)
            | (u64::from(bits_per_sample.saturating_sub(1)) << 36);
        v.extend_from_slice(&packed.to_be_bytes());
        v.extend_from_slice(&[0_u8; 16]); // MD5
        v
    }

    /// FLAC's `sample_fmt` follows the bit depth, and the threshold is 16.
    ///
    /// Measured against the reference on the only two depths its FLAC encoder
    /// will actually emit — it clamps 8, 20 and 32 to 24:
    ///
    /// ```sh
    /// ffprobe -v quiet -of csv=p=0 -show_entries stream=sample_fmt of16.ogg  # s16
    /// ffprobe -v quiet -of csv=p=0 -show_entries stream=sample_fmt of24.ogg  # s32
    /// ```
    #[test]
    fn flac_sample_fmt_follows_the_bit_depth() {
        for (bits, want) in [(16_u8, SampleFmt::S16), (24, SampleFmt::S32)] {
            let (_, _, params) = describe(OggCodec::Flac, &flac_bos(44_100, 2, bits));
            let audio = params.audio.as_ref().unwrap();
            assert_eq!(audio.format, Some(want), "{bits}-bit");
            assert_eq!(audio.bits_per_raw_sample, Some(bits));
        }
    }

    /// Opus is always `fltp`, whatever the container says about the source.
    ///
    /// ```sh
    /// ffprobe -v quiet -of csv=p=0 -show_entries stream=sample_fmt t.opus  # fltp
    /// ```
    #[test]
    fn opus_sample_fmt_is_fltp() {
        let mut bos = b"OpusHead".to_vec();
        bos.push(1); // version
        bos.push(2); // channel count
        bos.extend_from_slice(&312_u16.to_le_bytes()); // pre-skip
        bos.extend_from_slice(&48_000_u32.to_le_bytes());
        bos.extend_from_slice(&0_i16.to_le_bytes()); // output gain
        bos.push(0); // channel mapping family
        let (_, _, params) = describe(OggCodec::Opus, &bos);
        assert_eq!(
            params.audio.as_ref().unwrap().format,
            Some(SampleFmt::F32P)
        );
    }
}
