//! The MPEG-PS demuxer: pack headers, the system header, PES assembly and
//! `private_stream_1` substream demultiplexing.
//!
//! # Discovery
//!
//! A program stream's system header (§2.5.3.5) lists every `stream_id` up
//! front, but it says nothing about `private_stream_1` sub-streams — those
//! only reveal themselves once their first PES packet's payload is read (its
//! first byte is the sub-stream id, see [`crate::substream`]). So streams
//! are registered lazily, the same way `vaco-demux-mpegts` registers PIDs:
//! [`MpegPsDemuxer::open`] eagerly pumps a bounded prefix so the common case
//! (every stream declared in the first pack or two) is fully known by the
//! time it returns, and [`MpegPsDemuxer::read_packet`] keeps registering new
//! ones for as long as the file keeps introducing them.
//!
//! # Reframing, and the gap that limits it today
//!
//! [`vaco_format_core::ParserProvider`] is used, unlike
//! `vaco-demux-mpegts` (issue #632: that demuxer receives a
//! `ParserProvider` and never calls it, so one PES payload becomes one
//! packet regardless of how many codec frames it holds). This demuxer looks
//! up a [`vaco_codec_core::Parser`] for each newly discovered stream while
//! it still holds the borrow (during [`MpegPsDemuxer::open`]'s scan), and
//! feeds every payload through it when one exists, splitting a PES payload
//! into codec frames exactly the way `Parser::parse` is meant to be driven.
//!
//! **What this does not fix today**: `vaco_codec_core::CodecId` has no
//! MPEG-1/2 video, MPEG audio (layer I/II), AC-3, DTS or DVD-flavoured LPCM
//! variant (surveyed 2026-08-23 — see the docs file), which is most of what
//! a program stream actually carries. With no codec id there is no parser to
//! look up, so in practice every stream on every file this crate can
//! currently classify falls back to whole-PES-payload packets — the same
//! observable shape as #632, but for a different reason (no parser exists to
//! call, not a parser that exists and is ignored), and it will start
//! reframing automatically the day those codec ids and their parsers land,
//! with no change needed here.

use std::collections::VecDeque;

use vaco_codec_core::{CodecId, CodecParameters, Parser};
use vaco_core::{Duration, Error, ExactDuration, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::seek::{SeekFlags, SeekTarget, binary_search};
use vaco_format_core::time::WrapState;
use vaco_format_core::{Demuxer, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::keyframe;
use crate::pack::{
    PACK_START_CODE, PROGRAM_END_CODE, PackHeader, SYSTEM_HEADER_START_CODE, SystemHeader,
};
use crate::pes::{PES_PREFIX_LEN, PsPesHeader, SID_PRIVATE_1, START_CODE};
use crate::substream::{self, SubstreamKind};

/// What MPEG-PS declares it can do.
///
/// `GENERIC_INDEX` because the container carries no index of its own —
/// exactly the same shape as MPEG-TS. `TS_DISCONT` is not set: a program
/// stream has no adaptation-field-style "this jump is legitimate" signal, so
/// an SCR discontinuity is corruption rather than a documented splice point.
pub const FLAGS: FormatFlags = FormatFlags::SHOW_IDS.union(FormatFlags::GENERIC_INDEX);

/// The 33-bit, 90 kHz SCR clock wraps on the same period as MPEG-TS's PCR.
pub const SCR_WRAP_BITS: u32 = 33;

/// Largest PES payload assembled before the stream is treated as hostile.
///
/// A video PES packet with `PES_packet_length == 0` is terminated only by
/// the next start code, so a stream that never produces one would otherwise
/// accumulate without limit. Mirrors `vaco-demux-mpegts::demux::MAX_PES_BYTES`.
pub const MAX_PAYLOAD_BYTES: usize = 6 << 20;

/// How far a resync scan looks for the next start code before giving up.
pub const MAX_RESYNC_BYTES: u64 = 1 << 20;

/// How many packs `open` scans eagerly, trying to see every stream before
/// returning, so [`ParserProvider`] lookups happen while the borrow is live.
const OPEN_SCAN_PACKS: u32 = 64;

/// One elementary stream, keyed by `stream_id` and (for `private_stream_1`)
/// a sub-stream id.
struct EsEntry {
    stream_id: u8,
    sub_id: Option<u8>,
    stream_index: u32,
    parser: Option<Box<dyn Parser>>,
}

/// The MPEG-PS demuxer.
pub struct MpegPsDemuxer {
    io: IoContext,
    opts: FormatOptions,
    streams: Vec<Stream>,
    metadata: Vec<(String, String)>,
    es: Vec<EsEntry>,
    queue: VecDeque<Packet>,
    budget: Budget,
    scr_wrap: WrapState,
    first_scr: Option<i64>,
    last_scr: Option<i64>,
    duration_exact: Option<ExactDuration>,
    eof: bool,
    program_ended: bool,
    first_pack_pos: u64,
}

impl std::fmt::Debug for MpegPsDemuxer {
    /// Hand-written because `EsEntry::parser` is `Option<Box<dyn Parser>>`,
    /// and `Parser` carries no `Debug` bound.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpegPsDemuxer")
            .field("streams", &self.streams.len())
            .field("es", &self.es.len())
            .field("queued", &self.queue.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl MpegPsDemuxer {
    /// Open a program stream.
    ///
    /// # Errors
    /// [`Error::InvalidData`] when no pack start code can be found at all.
    pub fn open(
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive(), parsers)
    }

    /// Open with an explicit allocation ceiling — the constructor a fuzz
    /// target or an embedder reaches for.
    ///
    /// # Errors
    /// As [`MpegPsDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        opts: &FormatOptions,
        limits: Limits,
        parsers: &dyn ParserProvider,
    ) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut me = Self {
            io,
            opts: opts.clone(),
            streams: Vec::new(),
            metadata: Vec::new(),
            es: Vec::new(),
            queue: VecDeque::new(),
            budget: Budget::new(limits),
            scr_wrap: WrapState::new(SCR_WRAP_BITS).with_options(opts),
            first_scr: None,
            last_scr: None,
            duration_exact: None,
            eof: false,
            program_ended: false,
            first_pack_pos: 0,
        };
        // Confirm there is a pack at all before doing anything else.
        let head = me.io.peek(4)?;
        if head != PACK_START_CODE {
            return Err(Error::InvalidData("mpegps: no pack_start_code at start"));
        }
        for _ in 0..OPEN_SCAN_PACKS {
            if me.eof || me.program_ended {
                break;
            }
            if me.queue.len() > 4096 {
                // Enough is queued; stop scanning eagerly and let ordinary
                // `read_packet` calls drain it and continue discovery.
                break;
            }
            match me.pump(Some(parsers)) {
                Ok(()) => {}
                Err(Error::Eof | Error::UnexpectedEof) => {
                    me.eof = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        me.estimate_duration();
        Ok(me)
    }

    fn estimate_duration(&mut self) {
        if let (Some(first), Some(last)) = (self.first_scr, self.last_scr)
            && last > first
        {
            self.duration_exact = ExactDuration::from_ticks(
                last - first,
                Rational {
                    num: 1,
                    den: 90_000,
                },
            );
        }
    }

    /// Find or register the stream for a plain elementary stream id
    /// (`0xC0..=0xEF`), returning its index.
    fn stream_for_id(&mut self, stream_id: u8, parsers: Option<&dyn ParserProvider>) -> usize {
        if let Some(i) = self
            .es
            .iter()
            .position(|e| e.stream_id == stream_id && e.sub_id.is_none())
        {
            return i;
        }
        // `stream_id` alone cannot say whether video is MPEG-1 or MPEG-2, or
        // which MPEG audio layer this is — the same ambiguity
        // `vaco-demux-raw`'s `MPEGVIDEO` bitstream spec already documents
        // and accepts a static answer for. `Mpeg2video` matches that
        // spec's own choice (plain MPEG-1 streams are rare, and
        // `PARSER_MPEG1`/`PARSER_MPEG2` both build the same `Mpeg12Parser`,
        // so either answer reaches a parser that reads the bitstream
        // correctly regardless). `Mp2` matches `vaco-mux-mpegps`'s own
        // `default_audio` for every DVD/VCD/SVCD profile it mux. Neither is
        // sniffed from the payload, so a genuine MPEG-1 video or MP1/MP3
        // audio stream states the wrong `codec_name` — the same accepted,
        // narrower limitation as the raw-bitstream spec, not solved here.
        let (media, codec_id) = if (0xC0..=0xDF).contains(&stream_id) {
            (MediaType::Audio, CodecId::Mp2)
        } else {
            (MediaType::Video, CodecId::Mpeg2video)
        };
        self.register(stream_id, None, media, Some(codec_id), None, parsers)
    }

    /// Find or register a `private_stream_1` substream, returning its index,
    /// or `None` when `sub_id` classifies as nothing this crate recognises.
    fn stream_for_substream(
        &mut self,
        sub_id: u8,
        parsers: Option<&dyn ParserProvider>,
    ) -> Option<usize> {
        let kind = substream::classify(sub_id)?;
        if let Some(i) = self
            .es
            .iter()
            .position(|e| e.stream_id == SID_PRIVATE_1 && e.sub_id == Some(sub_id))
        {
            return Some(i);
        }
        Some(self.register(
            SID_PRIVATE_1,
            Some(sub_id),
            kind.media_type(),
            None,
            Some(kind),
            parsers,
        ))
    }

    fn register(
        &mut self,
        stream_id: u8,
        sub_id: Option<u8>,
        media: MediaType,
        codec_id: Option<CodecId>,
        kind: Option<SubstreamKind>,
        parsers: Option<&dyn ParserProvider>,
    ) -> usize {
        let index = self.streams.len() as u32;
        let time_base = Rational {
            num: 1,
            den: 90_000,
        };
        let mut stream = Stream::new(index, media, time_base);
        // Synthesised container-native id, matching how `vaco-demux-mpegts`
        // uses the PID: the raw stream_id, or `0xBD00 | sub_id` for a
        // private-stream-1 substream, so a `0xBD` sub-id of 0 is still
        // distinguishable from plain `stream_id` 0xBD.
        stream.id = Some(i64::from(stream_id) << 8 | i64::from(sub_id.unwrap_or(0)));
        if let Some(k) = kind {
            stream.metadata_set("mpegps_substream", k.name());
        }
        // `AC-3`/DTS/DVD-LPCM substreams (`kind: Some(_)`) still get no
        // `codec_id`: `CodecId::Ac3`/`Eac3` exist now, but classifying those
        // substreams correctly is untouched here — this fix is scoped to the
        // plain `stream_id`-keyed streams `stream_for_id` registers, which
        // is what reported `codec_name=unknown` on an ordinary, unmutated
        // file. `params.media_type` and the stream's position in the list
        // are unaffected either way.
        let mut params = match media {
            MediaType::Video => CodecParameters::video(),
            MediaType::Audio => CodecParameters::audio(),
            _ => CodecParameters::new(media),
        };
        if let Some(id) = codec_id {
            params = params.with_codec(id);
        }
        stream.params = params;
        let es_index = self.es.len();
        let parser = codec_id.and_then(|id| parsers.and_then(|p| p.parser_for(id)));
        self.es.push(EsEntry {
            stream_id,
            sub_id,
            stream_index: index,
            parser,
        });
        self.streams.push(stream);
        es_index
    }

    /// Correct a raw 33-bit PTS/DTS-space timestamp for wraparound and turn
    /// it into a [`Timestamp`], sharing the SCR's wrap state — both clocks
    /// live in the same 90 kHz, 33-bit space in a single-program multiplex.
    fn correct_ts(&mut self, raw: Option<i64>) -> Timestamp {
        let Some(v) = raw else {
            return Timestamp::NONE;
        };
        self.scr_wrap.correct(Timestamp::new(v))
    }

    /// Read one unit of container structure, queuing zero or more packets.
    /// `parsers` is `Some` only while [`MpegPsDemuxer::open`] still holds the
    /// borrow; newly seen streams outside that window get no parser.
    fn pump(&mut self, parsers: Option<&dyn ParserProvider>) -> Result<()> {
        let head = self.io.peek(4)?;
        if head == PACK_START_CODE {
            return self.read_pack();
        }
        if head == SYSTEM_HEADER_START_CODE {
            return self.read_system_header();
        }
        if head == PROGRAM_END_CODE {
            self.io.skip(4)?;
            self.program_ended = true;
            return Err(Error::Eof);
        }
        if head.get(..3) == Some(&START_CODE[..]) {
            return self.read_pes(parsers);
        }
        self.resync()
    }

    fn read_pack(&mut self) -> Result<()> {
        // 14 fixed bytes plus up to 7 stuffing bytes is the largest an
        // MPEG-2 pack header can be.
        let pos = self.io.pos();
        let buf = self.io.peek(21)?;
        let Some(header) = PackHeader::parse(buf)? else {
            return Err(Error::UnexpectedEof);
        };
        if self.first_scr.is_none() {
            self.first_pack_pos = pos;
        }
        self.last_scr = Some(header.scr_base);
        if self.first_scr.is_none() {
            self.first_scr = Some(header.scr_base);
        }
        self.scr_wrap.observe(header.scr_base);
        self.io.skip(header.len as u64)
    }

    fn read_system_header(&mut self) -> Result<()> {
        // Peek enough for the 6-byte prefix, then re-peek for the full
        // element once its length is known.
        let prefix = self.io.peek(6)?;
        let (Some(&b4), Some(&b5)) = (prefix.get(4), prefix.get(5)) else {
            return Err(Error::UnexpectedEof);
        };
        let declared = usize::from(u16::from_be_bytes([b4, b5]));
        let total = 6usize
            .checked_add(declared)
            .ok_or(Error::InvalidData("mpegps: system header length overflow"))?;
        let buf = self.io.peek(total)?;
        let Some(header) = SystemHeader::parse(buf)? else {
            return Err(Error::UnexpectedEof);
        };
        for bound in &header.streams {
            if bound.stream_id != SID_PRIVATE_1 {
                let _ = self.stream_for_id(bound.stream_id, None);
            }
        }
        self.io.skip(header.len as u64)
    }

    /// Read a framed PES payload: either exactly `PES_packet_length` bytes,
    /// or — when that field is zero — everything up to (not including) the
    /// next start code, bounded by [`MAX_PAYLOAD_BYTES`] and the
    /// [`Budget`].
    fn read_pes(&mut self, parsers: Option<&dyn ParserProvider>) -> Result<()> {
        let head = self.io.peek(64)?;
        let Some(header) = PsPesHeader::parse(head) else {
            // Either genuinely truncated, or an optional header longer than
            // the peek window (the MPEG-2 form allows up to 255 bytes of
            // optional fields). Re-peek at the maximum possible size before
            // giving up.
            let head = self.io.peek(PES_PREFIX_LEN + 3 + 255)?;
            let Some(header) = PsPesHeader::parse(head) else {
                return Err(Error::UnexpectedEof);
            };
            return self.finish_pes(header, parsers);
        };
        self.finish_pes(header, parsers)
    }

    fn finish_pes(
        &mut self,
        header: PsPesHeader,
        parsers: Option<&dyn ParserProvider>,
    ) -> Result<()> {
        let stream_id = header.stream_id;
        let payload_offset = header.payload_offset;
        if header.is_padding() || stream_id == crate::pes::SID_PROGRAM_STREAM_MAP {
            let _ = self.read_payload(payload_offset, header.total_len())?;
            return Ok(());
        }
        let mut payload = self.read_payload(payload_offset, header.total_len())?;

        let (es_pos, payload) = if stream_id == SID_PRIVATE_1 {
            let Some(&sub_id) = payload.first() else {
                return Ok(());
            };
            let Some(es_pos) = self.stream_for_substream(sub_id, parsers) else {
                return Ok(());
            };
            let rest = payload.split_off(1);
            (es_pos, rest)
        } else if (0xC0..=0xEF).contains(&stream_id) {
            let es_pos = self.stream_for_id(stream_id, parsers);
            (es_pos, payload)
        } else {
            // ECM/EMM/DSM-CC/directory/H.222.1-E and anything else with no
            // useful payload for a media pipeline: consumed, not emitted.
            return Ok(());
        };
        let Some(stream_index) = self.es.get(es_pos).map(|e| e.stream_index) else {
            return Err(Error::InvalidData(
                "mpegps: internal stream index out of range",
            ));
        };

        let pts = self.correct_ts(header.pts.ticks());
        let dts = self.correct_ts(header.dts.ticks());
        let is_video = self
            .streams
            .get(stream_index as usize)
            .is_some_and(|s| s.params.media_type == Some(MediaType::Video));
        let key = is_video && keyframe::is_keyframe(&payload).unwrap_or(true);

        if let Some(parser) = self.es.get_mut(es_pos).and_then(|e| e.parser.as_mut()) {
            let mut input = payload.as_slice();
            while !input.is_empty() {
                let (pkt, used) = parser.parse(input)?;
                if used == 0 {
                    break;
                }
                if let Some(mut pkt) = pkt {
                    pkt.stream_index = stream_index;
                    pkt.pts = pts;
                    pkt.dts = dts;
                    if key {
                        pkt.flags |= PacketFlags::KEY;
                    }
                    self.queue.push_back(pkt);
                }
                input = input.get(used..).unwrap_or(&[]);
            }
            return Ok(());
        }

        let mut pkt = Packet::from_slice(&mut self.budget, &payload)?;
        pkt.stream_index = stream_index;
        pkt.pts = pts;
        pkt.dts = dts;
        if key {
            pkt.flags |= PacketFlags::KEY;
        }
        self.queue.push_back(pkt);
        Ok(())
    }

    /// Read the payload bytes of a PES packet already positioned at its
    /// start, given the header's declared `payload_offset` and total length.
    fn read_payload(&mut self, payload_offset: usize, total_len: Option<usize>) -> Result<Vec<u8>> {
        if let Some(total) = total_len {
            self.budget.check(total as u64)?;
            let mut buf = vec![0u8; total];
            self.io.read_exact(&mut buf)?;
            return Ok(buf.get(payload_offset..).unwrap_or(&[]).to_vec());
        }
        // Unbounded (`PES_packet_length == 0`): consume the header, then
        // read forward in bounded chunks until the next start code or the
        // hard ceiling.
        let mut header_buf = vec![0u8; payload_offset];
        self.io.read_exact(&mut header_buf)?;
        let mut out = Vec::new();
        loop {
            if out.len() >= MAX_PAYLOAD_BYTES {
                return Err(Error::LimitExceeded {
                    limit: "mpegps.pes_payload",
                    requested: out.len() as u64,
                    cap: MAX_PAYLOAD_BYTES as u64,
                });
            }
            let window_len = self.io.peek(4)?.len();
            if window_len < 4 {
                // EOF: whatever is left belongs to this payload.
                let rest_len = self.io.peek(1 << 20)?.len();
                if rest_len == 0 {
                    break;
                }
                self.budget.check(rest_len as u64)?;
                let mut rest = vec![0u8; rest_len];
                self.io.read_exact(&mut rest)?;
                out.extend_from_slice(&rest);
                break;
            }
            let window = self.io.peek(4)?;
            if window.get(..3) == Some(&START_CODE[..]) {
                break;
            }
            let one = self.io.peek(1)?;
            let Some(&b) = one.first() else { break };
            self.budget.check(1)?;
            out.push(b);
            self.io.skip(1)?;
        }
        Ok(out)
    }

    /// Scan forward for the next recognisable start code after a framing
    /// error, bounded by [`MAX_RESYNC_BYTES`].
    fn resync(&mut self) -> Result<()> {
        let mut scanned = 0u64;
        loop {
            if scanned >= MAX_RESYNC_BYTES {
                return Err(Error::InvalidData("mpegps: lost sync and could not resync"));
            }
            let window = self.io.peek(4)?;
            if window.len() < 4 {
                return Err(Error::Eof);
            }
            if window.get(..3) == Some(&START_CODE[..]) {
                return Ok(());
            }
            self.io.skip(1)?;
            scanned += 1;
        }
    }
}

impl Demuxer for MpegPsDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(p);
            }
            if self.eof {
                return Err(Error::Eof);
            }
            match self.pump(None) {
                Ok(()) => {}
                Err(Error::Eof | Error::UnexpectedEof) => {
                    self.eof = true;
                    if self.queue.is_empty() {
                        return Err(Error::Eof);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                self.io.seek(pos)?;
                self.queue.clear();
                self.eof = false;
                self.program_ended = false;
                Ok(())
            }
            SeekTarget::Timestamp { ts, .. } => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                let lo = self.first_pack_pos;
                let hi = self.io.size().unwrap_or(u64::MAX);
                let mut index = vaco_format_core::seek::PacketIndex::with_options(&self.opts);
                let landing = binary_search(ts, lo, hi, &mut index, |probe_pos, limit| {
                    self.probe_scr_at(probe_pos, limit)
                })?;
                let at = landing.map_or(lo, |l| l.pos);
                self.io.seek(at)?;
                self.queue.clear();
                self.eof = false;
                self.program_ended = false;
                let _ = flags;
                Ok(())
            }
            SeekTarget::Frame { .. } => Err(Error::Unsupported("mpegps: unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration_exact?
            .to_duration(Rounding::NearestAwayFromZero)
    }

    fn duration_exact(&self) -> Option<ExactDuration> {
        self.duration_exact
    }
}

impl MpegPsDemuxer {
    /// Probe callback for [`binary_search`]: seek to `pos`, scan forward for
    /// the next pack header at or before `limit`, and report its SCR.
    fn probe_scr_at(&mut self, pos: u64, limit: u64) -> Result<Option<(u64, Timestamp)>> {
        self.io.seek(pos)?;
        let mut scanned = pos;
        loop {
            if scanned > limit {
                return Ok(None);
            }
            let window = self.io.peek(4)?;
            if window.len() < 4 {
                return Ok(None);
            }
            if window == PACK_START_CODE {
                let buf = self.io.peek(21)?;
                if let Ok(Some(header)) = PackHeader::parse(buf) {
                    return Ok(Some((scanned, Timestamp::new(header.scr_base))));
                }
            }
            self.io.skip(1)?;
            scanned += 1;
        }
    }
}
