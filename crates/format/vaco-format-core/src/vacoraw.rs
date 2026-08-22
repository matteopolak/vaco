//! A worked example container, and the crate's own proof that the traits work.
//!
//! No concrete container crate exists yet, so every design decision in this
//! crate would otherwise be a hypothesis. `vacoraw` is a deliberately trivial
//! format that nevertheless exercises **every seam**: a content probe, stream
//! discovery, packets with timestamps and keyframe flags, an optional trailing
//! index, all three seek strategies, and a muxer that round-trips its own
//! demuxer's output.
//!
//! It is not a format anybody should store media in and it is not registered
//! for general use. It exists so that `vaco-demux-mp4` can be written against
//! an interface that has already been driven end to end rather than one that
//! has only been declared.
//!
//! # Layout
//!
//! All fields big-endian. Offsets are from the start of the stream.
//!
//! ```text
//! header
//!   0  8   magic       "VACORAW" + version byte (currently 1)
//!   8  4   flags       bit 0: a trailing index is present
//!  12  8   index_pos   byte offset of the index block, 0 when absent
//!  20  2   n_streams
//!  22  …   one 14-byte stream entry each:
//!            0 1  media       0 video, 1 audio, 2 subtitle, 3 data
//!            1 1  (reserved)
//!            2 4  codec tag   the codec's CLI name, space padded ("h264")
//!            6 4  tb_num
//!           10 4  tb_den
//!
//! packet, repeated
//!   0  4   "PACK"
//!   4  2   stream_index
//!   6  1   flags       bit 0: keyframe
//!   7  1   (reserved)
//!   8  8   pts         i64::MIN means absent
//!  16  8   dts         i64::MIN means absent
//!  24  4   length
//!  28  …   payload
//!
//! index block, optional, after the last packet
//!   0  4   "INDX"
//!   4  4   count
//!   8  …   one 19-byte entry each:
//!            0 2  stream_index
//!            2 8  timestamp
//!           10 8  position
//!           18 1  flags       bit 0: keyframe
//! ```
//!
//! # What each seam it exercises is for
//!
//! | Seam | How `vacoraw` reaches it |
//! |---|---|
//! | [`crate::probe`] | magic plus a self-consistency check on the stream count |
//! | [`crate::seek`] index path | the trailing index, when the writer could seek to patch its offset |
//! | [`crate::seek`] bisection | the same file written to a pipe, where no index could be recorded |
//! | [`crate::seek`] byte path | `SeekFlags::BYTE`, with resynchronisation on the packet magic |
//! | [`crate::interleave`] | the muxer feeds every packet through the queue |
//! | [`crate::time`] | [`crate::discovery::Discovery`] wraps the demuxer in the round-trip test |

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Rounding, TimeBase, Timestamp};
use vaco_io::{IoContext, IoOptions, IoWriter, MediaSink, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::flags::FormatFlags;
use crate::options::FormatOptions;
use crate::probe::{ProbeData, ProbeScore};
use crate::seek::{
    IndexEntry, PacketIndex, SeekFlags, SeekLanding, SeekStrategy, SeekTarget, binary_search,
};
use crate::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider, Stream};

/// Magic plus version byte.
pub const MAGIC: &[u8; 8] = b"VACORAW\x01";
const PACKET_TAG: [u8; 4] = *b"PACK";
const INDEX_TAG: [u8; 4] = *b"INDX";
const HEADER_FIXED: u64 = 22;
const STREAM_ENTRY: u64 = 14;
const INDEX_ENTRY: usize = 19;

/// The flags this container declares.
///
/// `SHOW_IDS` because the stream index is the container's own identifier;
/// `GENERIC_INDEX` because a file written to a pipe carries no index and the
/// core is welcome to build one.
pub const FLAGS: FormatFlags = FormatFlags::SHOW_IDS
    .union(FormatFlags::GENERIC_INDEX)
    .union(FormatFlags::TS_NEGATIVE);

/// Largest payload a single packet may declare, before any budget applies.
///
/// A length field is the classic attacker-controlled allocation, so it is
/// bounded twice: here, structurally, and again by the [`Budget`] every
/// allocation goes through.
const MAX_PAYLOAD: u32 = 64 << 20;

/// Bytes [`VacoRawDemuxer`] will scan looking for the next packet magic.
const MAX_RESYNC: u64 = 1 << 20;

/// Content probe.
///
/// Full marks for magic plus a self-consistency check; [`ProbeScore::MAGIC`]
/// when only the magic is there, which is the convention table's row for
/// "unambiguous signature, nothing further checked".
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if !data.starts_with(MAGIC) {
        return ProbeScore::NONE;
    }
    let Some(n) = data.rb16(20) else {
        return ProbeScore::MAGIC;
    };
    // The header must fit inside a plausible file, and a container with no
    // streams at all is not one we wrote.
    if n == 0 || u64::from(n) > 4096 {
        return ProbeScore::MAGIC;
    }
    // Either the first packet magic follows the stream table, or the buffer
    // ended before it — in which case we cannot confirm and do not claim to.
    let at = HEADER_FIXED.saturating_add(u64::from(n).saturating_mul(STREAM_ENTRY));
    match usize::try_from(at).ok().and_then(|a| data.tag(a)) {
        Some(t) if t == PACKET_TAG || t == INDEX_TAG => ProbeScore::MAGIC_CHECKED,
        _ => ProbeScore::MAGIC,
    }
}

/// The descriptor a registry would hold.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "vacoraw",
    long_name: "Vaco reference container (worked example)",
    extensions: &["vacoraw"],
    mime_types: &["application/x-vacoraw"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "vacoraw",
    long_name: "Vaco reference container (worked example)",
    extensions: &["vacoraw"],
    default_video: Some(CodecId::H264),
    default_audio: Some(CodecId::Opus),
    open: open_muxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(VacoRawDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(VacoRawMuxer::new(
        sink,
        &FormatOptions::default(),
    )?))
}

// ------------------------------------------------------------------ demuxer

/// The worked-example demuxer.
#[derive(Debug)]
pub struct VacoRawDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    index: PacketIndex,
    budget: Budget,
    /// Byte offset of the first packet.
    first_packet: u64,
    /// Byte offset of the index block, when the file has one.
    index_pos: Option<u64>,
    duration: Option<Duration>,
    /// End of stream is sticky.
    ///
    /// Not decoration: `read_packet` consumes bytes before it can tell whether
    /// a packet follows, so without this the *second* call after end of stream
    /// reads the middle of the index block and reports it as corruption. The
    /// frozen [`Demuxer`] trait does not say `Eof` must be stable; it should,
    /// and every demuxer needs a flag like this one. See the docs file.
    eof: bool,
}

impl VacoRawDemuxer {
    /// Read the header and, when the source allows it, the trailing index.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed header, [`Error::LimitExceeded`]
    /// when the stream count is over `max_streams`, and whatever the transport
    /// reports.
    pub fn open(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut magic = [0u8; 8];
        io.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(Error::InvalidData("not a vacoraw stream"));
        }
        let _flags = io.rb32()?;
        let index_pos = io.rb64()?;
        let n = usize::from(io.rb16()?);
        let cap = usize::try_from(opts.max_streams).unwrap_or(usize::MAX);
        if n > cap {
            return Err(Error::LimitExceeded {
                limit: "max_streams",
                requested: n as u64,
                cap: cap as u64,
            });
        }
        let mut streams = Vec::new();
        for index in 0..n {
            streams.push(read_stream_entry(&mut io, index as u32)?);
        }
        let first_packet = io.pos();
        let mut me = Self {
            io,
            streams,
            index: PacketIndex::with_options(opts),
            budget: Budget::new(Limits::permissive()),
            first_packet,
            index_pos: (index_pos != 0).then_some(index_pos),
            duration: None,
            eof: false,
        };
        if crate::seek::use_container_index(opts) {
            me.load_index();
        }
        Ok(me)
    }

    /// Read the trailing index, if there is one and the source can reach it.
    ///
    /// Failure is not an error: a truncated file is still readable
    /// sequentially, and refusing to open it because its index is missing would
    /// be worse behaviour than seeking coarsely.
    fn load_index(&mut self) {
        let Some(pos) = self.index_pos else { return };
        if self.io.seekability() == Seekability::None {
            return;
        }
        let resume = self.io.pos();
        if self.read_index_at(pos).is_err() {
            self.index.clear();
        }
        let _ = self.io.seek(resume);
    }

    fn read_index_at(&mut self, pos: u64) -> Result<()> {
        self.io.seek(pos)?;
        let tag = self.io.tag()?;
        if tag != INDEX_TAG {
            return Err(Error::InvalidData("index block is not where it claims"));
        }
        let count = self.io.rb32()?;
        // Bound the work before doing any of it: the count is a file field.
        #[allow(
            clippy::integer_division,
            reason = "INDEX_ENTRY is a non-zero constant; this converts a byte size into an entry count"
        )]
        let max = self
            .io
            .size()
            .map_or(u64::from(count), |s| s / INDEX_ENTRY as u64);
        let mut last_ts: Option<i64> = None;
        for _ in 0..u64::from(count).min(max) {
            let stream = u32::from(self.io.rb16()?);
            let ts = self.io.rb64()?.cast_signed();
            let at = self.io.rb64()?;
            let flags = self.io.r8()?;
            if usize::try_from(stream).is_ok_and(|s| s < self.streams.len()) {
                let entry = if flags & 1 == 1 {
                    IndexEntry::keyframe(at, Timestamp::new(ts))
                } else {
                    IndexEntry::frame(at, Timestamp::new(ts))
                };
                self.index.add(entry);
            }
            last_ts = Some(ts);
        }
        if let (Some(ts), Some(first)) = (last_ts, self.streams.first()) {
            self.duration = Timestamp::new(ts).to_duration(first.time_base);
        }
        Ok(())
    }

    /// The index built so far, for tests and for a caller that wants to know
    /// whether seeks will be exact.
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    /// Read one packet header and payload at the current position.
    fn read_one(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        let tag = match self.io.tag() {
            Ok(t) => t,
            // A clean end of file, and the one place `UnexpectedEof` is not an
            // error: there simply is no next packet.
            Err(Error::UnexpectedEof) => {
                self.eof = true;
                return Err(Error::Eof);
            }
            Err(e) => return Err(e),
        };
        if tag == INDEX_TAG {
            self.eof = true;
            return Err(Error::Eof);
        }
        if tag != PACKET_TAG {
            return Err(Error::InvalidData("expected a vacoraw packet header"));
        }
        let stream_index = u32::from(self.io.rb16()?);
        let flags = self.io.r8()?;
        let _reserved = self.io.r8()?;
        let pts = crate::time::decode_ts(self.io.rb64()?.cast_signed());
        let dts = crate::time::decode_ts(self.io.rb64()?.cast_signed());
        let len = self.io.rb32()?;
        if len > MAX_PAYLOAD {
            return Err(Error::LimitExceeded {
                limit: "vacoraw_packet",
                requested: u64::from(len),
                cap: u64::from(MAX_PAYLOAD),
            });
        }
        if usize::try_from(stream_index).is_ok_and(|s| s >= self.streams.len()) {
            return Err(Error::InvalidData("packet names an undeclared stream"));
        }
        // A declared length larger than the bytes that remain is the classic
        // amplification: reject it before allocating rather than after. Only
        // possible where the transport knows its own size — on a pipe the
        // budget is the only bound, which is what the budget is for.
        if let Some(size) = self.io.size()
            && u64::from(len) > size.saturating_sub(self.io.pos())
        {
            return Err(Error::InvalidData("packet claims more bytes than remain"));
        }
        let n = usize::try_from(len).unwrap_or(usize::MAX);
        let mut pkt = Packet::alloc(&mut self.budget, n)?;
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = stream_index;
        pkt.pts = pts;
        pkt.dts = dts;
        pkt.pos = Some(pos);
        pkt.flags = if flags & 1 == 1 {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        if let Some(st) = usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            && let Some(d) = crate::time::duration_from_rate(frame_rate_of(st))
        {
            pkt.duration = d;
        }
        // Every packet we read is a seek point we now know about. This is what
        // makes a GENERIC_INDEX format seekable on a second pass without the
        // container carrying an index at all.
        if pkt.is_key() {
            self.index.add(IndexEntry::keyframe(pos, pkt.dts));
        }
        Ok(pkt)
    }

    /// Scan forward from `pos` for the next packet magic, and report its DTS.
    ///
    /// This is the `read_timestamp` hook the frozen [`Demuxer`] trait does not
    /// have, kept as an inherent method so the demuxer can hand it to
    /// [`binary_search`] itself.
    fn probe_at(io: &mut IoContext, pos: u64, limit: u64) -> Result<Option<(u64, Timestamp)>> {
        let mut at = pos;
        io.seek(at)?;
        let end = limit.min(at.saturating_add(MAX_RESYNC));
        let mut window = [0u8; 4];
        if io.read_exact(&mut window).is_err() {
            return Ok(None);
        }
        loop {
            if window == PACKET_TAG {
                // Header follows; read enough of it to answer.
                let _stream = io.rb16()?;
                let _flags = io.r8()?;
                let _reserved = io.r8()?;
                let _pts = io.rb64()?;
                let dts = crate::time::decode_ts(io.rb64()?.cast_signed());
                return Ok(Some((at, dts)));
            }
            if at >= end {
                return Ok(None);
            }
            at = at.saturating_add(1);
            window = [
                window[1],
                window[2],
                window[3],
                match io.r8() {
                    Ok(b) => b,
                    Err(_) => return Ok(None),
                },
            ];
        }
    }

    /// Position the source at the next packet at or after `pos`.
    fn resync(&mut self, pos: u64) -> Result<()> {
        self.eof = false;
        let limit = self.io.size().unwrap_or(u64::MAX);
        match Self::probe_at(&mut self.io, pos.max(self.first_packet), limit)? {
            Some((found, _)) => {
                self.io.seek(found)?;
                Ok(())
            }
            None => Err(Error::Eof),
        }
    }

    fn seek_timestamp(&mut self, stream_index: u32, ts: Timestamp, flags: SeekFlags) -> Result<()> {
        let seekable = self.io.seekability() != Seekability::None;
        let strategy = SeekStrategy::choose(
            SeekTarget::Timestamp { stream_index, ts },
            flags,
            FLAGS,
            !self.index.is_empty(),
            seekable,
        );
        match strategy {
            SeekStrategy::Index => {
                self.eof = false;
                let entry = self.index.search(ts, flags).ok_or(Error::NotSeekable)?;
                self.io.seek(entry.pos)?;
                Ok(())
            }
            SeekStrategy::BinarySearch => {
                self.eof = false;
                let hi = self.io.size().unwrap_or(u64::MAX);
                let lo = self.first_packet;
                // The index has to leave the struct so the probe closure can
                // borrow the I/O context. It goes straight back.
                let mut index = core::mem::take(&mut self.index);
                let io = &mut self.io;
                let landing: Result<Option<SeekLanding>> =
                    binary_search(ts, lo, hi, &mut index, |p, l| Self::probe_at(io, p, l));
                self.index = index;
                match landing? {
                    Some(l) => {
                        self.io.seek(l.pos)?;
                        Ok(())
                    }
                    None => Err(Error::NotSeekable),
                }
            }
            SeekStrategy::Byte => self.resync(self.first_packet),
            SeekStrategy::Unsupported => Err(Error::NotSeekable),
        }
    }
}

fn frame_rate_of(st: &Stream) -> Rational {
    st.params
        .video
        .as_ref()
        .map_or(Rational::ZERO, |v| v.frame_rate)
}

fn read_stream_entry(io: &mut IoContext, index: u32) -> Result<Stream> {
    let media = match io.r8()? {
        0 => MediaType::Video,
        1 => MediaType::Audio,
        2 => MediaType::Subtitle,
        _ => MediaType::Data,
    };
    let _reserved = io.r8()?;
    let tag = io.tag()?;
    let num = io.rb32()?.cast_signed();
    let den = io.rb32()?.cast_signed();
    let time_base = Rational::new(num, den);
    if !time_base.is_defined() {
        return Err(Error::InvalidData("stream declares an unusable time base"));
    }
    let name = core::str::from_utf8(&tag)
        .map_err(|_| Error::InvalidData("codec tag is not utf-8"))?
        .trim_end();
    let mut params = CodecParameters::new(media);
    params.codec_tag = Some(tag);
    if let Some(id) = CodecId::from_name(name) {
        params = params.with_codec(id);
    }
    match media {
        MediaType::Video => params.video = Some(vaco_codec_core::VideoParameters::default()),
        MediaType::Audio => params.audio = Some(vaco_codec_core::AudioParameters::default()),
        _ => {}
    }
    // Built through `Stream::new` rather than as a literal: a literal has to
    // be edited every time the model widens, and the model widened three times
    // in one wave.
    let mut stream = Stream::new(index, media, time_base);
    stream.id = Some(i64::from(index));
    stream.params = params;
    Ok(stream)
}

impl Demuxer for VacoRawDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn read_packet(&mut self) -> Result<Packet> {
        self.read_one()
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let target = match target.stream_index() {
            Some(i) => {
                let st = usize::try_from(i)
                    .ok()
                    .and_then(|i| self.streams.get(i))
                    .ok_or(Error::InvalidData("seek names an unknown stream"))?;
                target.resolve_frames(frame_rate_of(st), st.time_base)?
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
                self.seek_timestamp(stream_index, ts, flags)
            }
            // `resolve_frames` turned every frame target into a timestamp.
            SeekTarget::Frame { .. } => Err(Error::Unsupported("unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

// -------------------------------------------------------------------- muxer

/// The worked-example muxer.
///
/// Writes the index only when the sink can seek, because the index offset lives
/// in the header and a non-seekable sink cannot go back and patch it. That is
/// the same trade every real container makes — MP4's `moov` placement, Matroska's
/// `SeekHead` — and it is here specifically so the round-trip test can cover
/// both branches.
#[derive(Debug)]
pub struct VacoRawMuxer {
    out: IoWriter,
    streams: Vec<StreamOut>,
    index: Vec<(u32, Timestamp, u64, bool)>,
    header_written: bool,
    trailer_written: bool,
}

#[derive(Debug, Clone, Copy)]
struct StreamOut {
    media: u8,
    tag: [u8; 4],
    time_base: TimeBase,
}

impl VacoRawMuxer {
    /// A muxer over `sink`.
    ///
    /// # Errors
    ///
    /// Propagates buffer allocation failure from [`IoWriter`].
    pub fn new(sink: Box<dyn MediaSink>, _opts: &FormatOptions) -> Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            streams: Vec::new(),
            index: Vec::new(),
            header_written: false,
            trailer_written: false,
        })
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.out.pos()
    }
}

impl Muxer for VacoRawMuxer {
    fn add_stream(&mut self, params: &CodecParameters) -> Result<u32> {
        if self.header_written {
            return Err(Error::InvalidData(
                "streams must be added before the header is written",
            ));
        }
        let media = match params.effective_media_type() {
            Some(MediaType::Video) => 0,
            Some(MediaType::Audio) => 1,
            Some(MediaType::Subtitle) => 2,
            _ => 3,
        };
        let mut tag = *b"    ";
        if let Some(id) = params.codec_id {
            let name = id.name().as_bytes();
            for (slot, &b) in tag.iter_mut().zip(name.iter()) {
                *slot = b;
            }
        } else if let Some(t) = params.codec_tag {
            tag = t;
        }
        // The muxer, not the caller, decides what the container can express.
        // vacoraw stores a full rational, so it honours the codec's own rate
        // where there is one and falls back to microseconds.
        let time_base = params
            .video
            .as_ref()
            .map(|v| v.frame_rate)
            .filter(|r| r.is_defined() && !r.is_zero() && !r.is_infinite())
            .map_or(crate::time::TIME_BASE_Q, Rational::inverse);
        let index = u32::try_from(self.streams.len())
            .map_err(|_| Error::InvalidData("too many streams"))?;
        self.streams.push(StreamOut {
            media,
            tag,
            time_base,
        });
        Ok(index)
    }

    fn write_header(&mut self) -> Result<()> {
        if self.header_written {
            return Err(Error::InvalidData("header written twice"));
        }
        self.out.write(MAGIC)?;
        self.out.wb32(u32::from(self.out.is_seekable()))?;
        // Patched by `write_trailer` when the sink can seek.
        self.out.wb64(0)?;
        let n = u16::try_from(self.streams.len())
            .map_err(|_| Error::InvalidData("too many streams"))?;
        self.out.wb16(n)?;
        for s in &self.streams {
            self.out.w8(s.media)?;
            self.out.w8(0)?;
            self.out.write_tag(&s.tag)?;
            self.out.wb32(s.time_base.num as u32)?;
            self.out.wb32(s.time_base.den as u32)?;
        }
        self.header_written = true;
        Ok(())
    }

    fn write_packet(&mut self, packet: &Packet) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("packet written before the header"));
        }
        if usize::try_from(packet.stream_index).is_ok_and(|i| i >= self.streams.len()) {
            return Err(Error::InvalidData("packet names an unknown stream"));
        }
        let pos = self.out.pos();
        self.out.write_tag(&PACKET_TAG)?;
        self.out.wb16(
            u16::try_from(packet.stream_index)
                .map_err(|_| Error::InvalidData("stream index does not fit"))?,
        )?;
        self.out.w8(u8::from(packet.is_key()))?;
        self.out.w8(0)?;
        self.out.wb64(encode_ts(packet.pts))?;
        self.out.wb64(encode_ts(packet.dts))?;
        let len =
            u32::try_from(packet.len).map_err(|_| Error::InvalidData("packet is too large"))?;
        self.out.wb32(len)?;
        self.out.write(packet.payload())?;
        if packet.is_key() {
            self.index
                .push((packet.stream_index, packet.dts, pos, true));
        }
        Ok(())
    }

    fn stream_time_base(&self, stream_index: u32) -> Option<TimeBase> {
        usize::try_from(stream_index)
            .ok()
            .and_then(|i| self.streams.get(i))
            .map(|s| s.time_base)
    }

    fn write_trailer(&mut self) -> Result<()> {
        if !self.header_written {
            return Err(Error::InvalidData("trailer written before the header"));
        }
        if self.trailer_written {
            return Err(Error::InvalidData("trailer written twice"));
        }
        self.trailer_written = true;
        if !self.out.is_seekable() || self.index.is_empty() {
            // No index: the file is still perfectly readable, and a demuxer
            // reading it will bisect instead.
            return self.out.flush();
        }
        let index_pos = self.out.pos();
        self.out.write_tag(&INDEX_TAG)?;
        let count =
            u32::try_from(self.index.len()).map_err(|_| Error::InvalidData("index too large"))?;
        self.out.wb32(count)?;
        for &(stream, ts, pos, key) in &self.index {
            self.out.wb16(
                u16::try_from(stream).map_err(|_| Error::InvalidData("stream index too large"))?,
            )?;
            self.out.wb64(encode_ts(ts))?;
            self.out.wb64(pos)?;
            self.out.w8(u8::from(key))?;
        }
        // Patch the header's index offset now that we know it.
        let end = self.out.pos();
        self.out.seek(12)?;
        self.out.wb64(index_pos)?;
        self.out.seek(end)?;
        self.out.flush()
    }
}

/// Encode a timestamp, using the `i64::MIN` alias for "absent".
const fn encode_ts(ts: Timestamp) -> u64 {
    match ts.ticks() {
        Some(v) => v as u64,
        None => i64::MIN as u64,
    }
}

/// Rescale a timestamp into a stream's base with the standard rounding, for
/// callers building `vacoraw` files by hand.
#[must_use]
pub fn to_stream_base(ts: Timestamp, from: TimeBase, to: TimeBase) -> Timestamp {
    ts.rescale(from, to, Rounding::default())
}

// ------------------------------------------------------------- memory sink

/// A byte buffer shared between a [`MemorySink`] and whoever wants to read what
/// was written to it.
///
/// The sink is moved into the muxer, so the only way to see the result is to
/// hold a second handle on the same storage.
#[derive(Debug, Clone, Default)]
pub struct SharedBytes(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl SharedBytes {
    /// A copy of the bytes written so far.
    ///
    /// Empty if the lock was poisoned by a panic in another thread, which in a
    /// test means the test has already failed for a better reason.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Bytes written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().map(|g| g.len()).unwrap_or_default()
    }

    /// Whether nothing has been written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A seekable in-memory [`MediaSink`].
///
/// `vaco-io` ships [`vaco_io::MemorySource`] for reading but has no writable
/// counterpart, and the muxer's header-patch path cannot be tested without one.
/// It lives here rather than there because reaching into another crate is
/// exactly what the parallel-execution protocol forbids; it is reported as a
/// small gap in `vaco-io` rather than worked around silently.
#[derive(Debug, Default)]
pub struct MemorySink {
    data: SharedBytes,
    pos: usize,
}

impl MemorySink {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A second handle on this sink's storage, valid after the sink is moved
    /// into a muxer.
    #[must_use]
    pub fn shared(&self) -> SharedBytes {
        self.data.clone()
    }
}

impl MediaSink for MemorySink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        let Ok(mut data) = self.data.0.lock() else {
            return Err(Error::Io(std::io::Error::other(
                "memory sink lock poisoned",
            )));
        };
        let end = self.pos.saturating_add(buf.len());
        if data.len() < end {
            data.resize(end, 0);
        }
        if let Some(dst) = data.get_mut(self.pos..end) {
            dst.copy_from_slice(buf);
        }
        self.pos = end;
        Ok(())
    }

    fn seek(&mut self, pos: u64) -> Result<u64> {
        self.pos = usize::try_from(pos).unwrap_or(usize::MAX);
        Ok(pos)
    }

    fn position(&self) -> u64 {
        self.pos as u64
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// A [`MemorySink`] that refuses to seek, for exercising the no-index branch.
#[derive(Debug, Default)]
pub struct ForwardOnlySink(MemorySink);

impl ForwardOnlySink {
    /// An empty forward-only sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A second handle on this sink's storage.
    #[must_use]
    pub fn shared(&self) -> SharedBytes {
        self.0.shared()
    }
}

impl MediaSink for ForwardOnlySink {
    fn write(&mut self, buf: &[u8]) -> Result<()> {
        MediaSink::write(&mut self.0, buf)
    }

    fn seek(&mut self, _pos: u64) -> Result<u64> {
        Err(Error::NotSeekable)
    }

    fn position(&self) -> u64 {
        self.0.position()
    }

    fn is_seekable(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
