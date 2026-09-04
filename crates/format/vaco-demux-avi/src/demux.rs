//! The AVI demuxer: the `hdrl`/`strl`/`movi` walk, the index, and seeking.
//!
//! # Why this is not a two-pass reader for every source
//!
//! `idx1` (and any `OpenDML` super-index) sits *after* the `movi` data it
//! describes. Reading it up front — the way [`open_with_limits`] does — means
//! seeking past the entire media payload and back, which only a seekable
//! source can do. On a non-seekable source (a pipe) this demuxer opens with no
//! index at all and relies on [`vaco_format_core::flags::FormatFlags::GENERIC_INDEX`]
//! to build a coarse one from whatever keyframes it happens to read — the same
//! trade every other index-based demuxer in this workspace makes.
//!
//! # The clock
//!
//! AVI states no per-packet timestamp. What it states is `dwScale`/`dwRate`
//! (a stream's time base) and, per `strh.dwSampleSize`, how to turn a chunk's
//! position in that stream's own sequence into a tick count:
//!
//! * `dwSampleSize == 0` — one chunk is one tick (video; VBR audio). The
//!   `n`-th chunk of a stream has timestamp `n`.
//! * `dwSampleSize != 0` — constant-bitrate audio, one chunk can be many
//!   samples. The timestamp is the running **byte** count for that stream
//!   divided by `dwSampleSize`.
//!
//! Both [`crate::index::build_from_idx1`] (replaying `idx1` at open time, to
//! build the seek index) and [`AviDemuxer::read_one`] (walking `movi`
//! sequentially, to emit packets) run the identical arithmetic — one over
//! index metadata, the other over bytes actually read — which is why a
//! well-formed file's packet timestamps agree with what its own index implies.

use vaco_core::{Duration, Error, MediaType, Result, Rounding, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::options::FormatOptions;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use vaco_format_riff::chunk::{RiffHeader, ids as riff_ids};

use crate::hdrl::{self, ids as avi_ids};
use crate::index::{self, Idx1Entry};

/// The flags this container declares.
///
/// [`FormatFlags::NOBINSEARCH`] because an AVI chunk's timestamp is a running
/// count since the start of its own stream — landing on an arbitrary byte
/// offset tells a reader nothing about where in that count it is, so
/// bisection cannot recover a timestamp the way it can for MPEG-TS.
/// [`FormatFlags::GENERIC_INDEX`] because a non-seekable open has no `idx1` to
/// work from and must build one from what it reads.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX.union(FormatFlags::NOBINSEARCH);

/// Content probe: the `RIFF....AVI ` signature.
///
/// Measured against `ffprobe 8.1`'s own `avi` demuxer probe, which is also a
/// fixed-position magic check (`RIFF` at 0, `AVI ` at 8) with no further
/// content inspection — so [`ProbeScore::MAGIC_CHECKED`] here (both anchors
/// verified) is the right tier rather than [`ProbeScore::MAGIC`].
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.tag(0) == Some(*b"RIFF") && data.tag(8) == Some(*b"AVI ") {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::NONE
    }
}

/// The registry descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "avi",
    long_name: "AVI (Audio Video Interleaved)",
    extensions: &["avi"],
    mime_types: &["video/x-msvideo"],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(AviDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

/// Per-stream state the sequential `movi` walk needs beyond the public
/// [`Stream`] — AVI's clock inputs, not carried by the generic model.
#[derive(Debug, Clone, Copy)]
struct StreamState {
    sample_size: u32,
    start: i64,
    chunks: u64,
    bytes: u64,
    /// One `sample_size == 0` chunk's duration, in this stream's own
    /// `time_base` ticks — see [`crate::hdrl::StreamBuild::native_ticks_per_chunk`]'s
    /// doc for why this is not always `1`.
    native_ticks_per_chunk: i64,
}

/// The AVI demuxer.
#[derive(Debug)]
pub struct AviDemuxer {
    io: IoContext,
    streams: Vec<Stream>,
    state: Vec<StreamState>,
    /// Byte offset of `movi`'s first child chunk — where sequential reading
    /// resumes after `open` has (optionally) peeked past it for the index.
    movi_children_start: u64,
    /// End of the `movi` region, clamped to the file size when known. Never
    /// trusted past that clamp, so a lying `LIST` size cannot make the
    /// sequential walk wander into `idx1` and misparse it as chunk data.
    movi_end: u64,
    /// Seek points, in [`TIME_BASE_Q`] — see the module docs for why this
    /// crate normalises across streams rather than keeping one index per
    /// stream (which the generic [`PacketIndex`] type does not itself model).
    index: PacketIndex,
    /// Chunk header byte offset -> keyframe flag, from `idx1`/`OpenDML`. Empty
    /// when neither was available (no index, or a non-seekable source).
    keyframe_by_pos: std::collections::BTreeMap<u64, bool>,
    metadata: Vec<(String, String)>,
    budget: Budget,
    duration: Option<Duration>,
    /// End of stream is sticky — see `vaco-format-core::vacoraw`'s
    /// `VacoRawDemuxer` for why every demuxer in this workspace needs this.
    eof: bool,
}

impl AviDemuxer {
    /// Open an AVI file.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the RIFF signature, form type, `hdrl` or
    /// `movi` are missing or malformed; [`Error::LimitExceeded`] past the
    /// configured allocation ceiling; whatever the transport reports.
    pub fn open(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, opts, Limits::permissive())
    }

    /// Open with an explicit allocation ceiling — the constructor a fuzz
    /// target or an embedder that cares about memory reaches for, mirroring
    /// `vaco-demux-mpegts` and `vaco-demux-matroska`.
    ///
    /// # Errors
    /// As [`AviDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default().with_limits(limits.clone()))?;
        let mut budget = Budget::new(limits);

        let mut header = [0u8; RiffHeader::LEN];
        io.read_exact(&mut header)?;
        let riff = RiffHeader::parse(&header)?;
        if riff.form_type != riff_ids::AVI_ {
            return Err(Error::InvalidData("avi: RIFF form is not AVI"));
        }

        let mut main: Option<hdrl::MainHeader> = None;
        let mut streams: Vec<Stream> = Vec::new();
        let mut state: Vec<StreamState> = Vec::new();
        let mut super_indexes: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut metadata: Vec<(String, String)> = Vec::new();
        // Assigned on the one path that `break`s out of the loop below
        // (finding `LIST movi`); every other path either loops again or
        // returns early, so these are always initialised by the time they
        // are read.
        let movi_fourcc_pos: u64;
        let movi_children_start: u64;
        let movi_declared_len: u64;

        // Pass 1: walk top-level chunks until `movi` is found. Everything
        // before it (`hdrl`, an optional `LIST INFO`, `JUNK`) is small and
        // read whole; `movi` itself is never read here — only located.
        loop {
            let pos = io.pos();
            let id = match io.tag() {
                Ok(t) => t,
                Err(Error::UnexpectedEof) => {
                    return Err(Error::InvalidData("avi: no movi chunk found"));
                }
                Err(e) => return Err(e),
            };
            let size = u64::from(io.rl32()?);
            if id == riff_ids::LIST.as_bytes() {
                let list_type = io.tag()?;
                if list_type == avi_ids::HDRL.as_bytes() {
                    let n = usize::try_from(size.saturating_sub(4)).unwrap_or(usize::MAX);
                    let mut buf = budget.alloc::<u8>(n)?;
                    io.read_exact(&mut buf)?;
                    let parsed = hdrl::parse_hdrl(&buf, &mut budget)?;
                    main = Some(parsed.main);
                    for (i, build) in parsed.streams.into_iter().enumerate() {
                        if let Some(idx_bytes) = build.super_index {
                            super_indexes.push((i, idx_bytes));
                        }
                        state.push(StreamState {
                            sample_size: build.sample_size,
                            start: i64::from(build.start),
                            chunks: 0,
                            bytes: 0,
                            native_ticks_per_chunk: build.native_ticks_per_chunk,
                        });
                        streams.push(build.stream);
                    }
                    skip_odd_pad(&mut io, size)?;
                } else if list_type == avi_ids::MOVI.as_bytes() {
                    movi_fourcc_pos = pos.saturating_add(8);
                    movi_children_start = pos.saturating_add(12);
                    movi_declared_len = size;
                    // Do not consume the payload; sequential reading resumes
                    // exactly here after the index (below) is resolved.
                    break;
                } else if list_type == riff_ids::INFO.as_bytes() {
                    let n = usize::try_from(size.saturating_sub(4)).unwrap_or(usize::MAX);
                    let mut buf = budget.alloc::<u8>(n)?;
                    io.read_exact(&mut buf)?;
                    parse_info_list(&buf, &mut metadata);
                    skip_odd_pad(&mut io, size)?;
                } else {
                    io.skip(size.saturating_sub(4))?;
                    skip_odd_pad(&mut io, size)?;
                }
            } else {
                // A plain top-level chunk before `movi` (`JUNK`, and anything
                // this crate does not otherwise recognise) is skipped whole.
                io.skip(size)?;
                skip_odd_pad(&mut io, size)?;
            }
        }

        let main = main.ok_or(Error::InvalidData("avi: no hdrl chunk found"))?;
        if streams.is_empty() {
            return Err(Error::InvalidData("avi: no streams declared"));
        }
        let cap = usize::try_from(opts.max_streams).unwrap_or(usize::MAX);
        if streams.len() > cap {
            return Err(Error::LimitExceeded {
                limit: "max_streams",
                requested: streams.len() as u64,
                cap: cap as u64,
            });
        }

        let file_size = io.size();
        let movi_end = movi_fourcc_pos.saturating_add(movi_declared_len);
        let movi_end = file_size.map_or(movi_end, |sz| movi_end.min(sz));

        let mut index = PacketIndex::with_options(opts);
        let mut keyframe_by_pos = std::collections::BTreeMap::new();

        let any_index_declared = main.has_index() || !super_indexes.is_empty();
        if any_index_declared
            && vaco_format_core::seek::use_container_index(opts)
            && io.seekability() != Seekability::None
        {
            let resume = movi_children_start;
            if let Some(idx1) = peek_idx1(&mut io, movi_end, resume, &mut budget)
                && let Ok(entries) = index::parse_idx1(&idx1, &mut budget)
            {
                let base = index::detect_offset_base(&mut io, &entries, movi_fourcc_pos);
                let resolved =
                    build_resolved(&entries, base, movi_fourcc_pos, &streams, &state, opts);
                index = resolved.index;
                keyframe_by_pos = resolved.keyframe_by_pos;
            }
            // `OpenDML`: only the keyframe map is populated (see `index.rs`'s
            // module docs) — the timestamp index for these positions is left
            // to the generic `GENERIC_INDEX` fallback, since replaying
            // `OpenDML` entries into an accurate common timeline needs them
            // interleaved with `idx1`'s in true file order, which nothing in
            // this crate has been able to verify against a real >4 GiB file.
            for (_stream_idx, raw) in &super_indexes {
                if let Ok(super_index) = index::parse_super_index(raw, &mut budget) {
                    for e in index::resolve_opendml(&mut io, &super_index, &mut budget) {
                        keyframe_by_pos.insert(e.pos, e.is_key);
                    }
                }
            }
            io.seek(resume)?;
        }

        // Some MPEG-4 Part 2/MS-MPEG4 encoders (measured: real ffmpeg's own
        // `-f avi -c:v mpeg4` writer) put no configuration record in `strf`
        // at all -- unlike `avc1`/`hvc1` and unlike other real-world AVI
        // muxers for this same codec family (Xvid/DivX's own tools
        // routinely do write one there, which `hdrl::carries_strf_extradata`
        // now covers) -- and rely entirely on every keyframe repeating its
        // VOL header in-band instead. Real ffmpeg's own probe extracts that
        // repeated header as `extradata` by scanning the bitstream; nothing
        // in `strf` gives a reader here the same fact, so a stream left
        // without this peek reports no extradata at all even though the
        // file plainly carries one (measured: 46 bytes, matching real
        // ffmpeg's own `extradata_size` on the identical file exactly).
        // Seekable-only, the same gate the `idx1`/`OpenDML` peeks above use:
        // non-seekable input already loses that free lunch, exactly as
        // those peeks would.
        if io.seekability() != Seekability::None {
            for (i, st) in streams.iter_mut().enumerate() {
                let needs_it = st.params.codec_id == Some(vaco_codec_core::CodecId::Mpeg4)
                    && st.params.extradata.as_ref().is_none_or(Vec::is_empty);
                if !needs_it {
                    continue;
                }
                if let Some(extra) =
                    peek_mpeg4_vol_header(&mut io, movi_children_start, movi_end, i, &mut budget)
                {
                    st.params.extradata = Some(extra);
                }
                // `io`'s position is restored by `peek_mpeg4_vol_header`
                // itself on every return path, so sequential reading below
                // still resumes at `movi_children_start` regardless of
                // whether this stream needed (or found) anything.
            }
        }

        let duration = duration_from_main(&main, &state);

        Ok(Self {
            io,
            streams,
            state,
            movi_children_start,
            movi_end,
            index,
            keyframe_by_pos,
            metadata,
            budget,
            duration,
            eof: false,
        })
    }

    /// The index built (from `idx1`, `OpenDML`, or generically as packets were
    /// read) so far.
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    fn is_key_at(&self, pos: u64, media_type: Option<MediaType>) -> bool {
        if let Some(&k) = self.keyframe_by_pos.get(&pos) {
            return k;
        }
        // No index entry for this exact position. Audio (and anything that is
        // not video) has no dependency structure worth tracking here, so
        // every chunk is a valid resume point. Video with no index at all
        // gets no free pass — claiming otherwise would be a guess, and
        // `SeekFlags::ANY` exists precisely for a caller willing to accept an
        // inexact landing.
        !matches!(media_type, Some(MediaType::Video))
    }

    fn read_one(&mut self) -> Result<Packet> {
        loop {
            if self.eof {
                return Err(Error::Eof);
            }
            if self.io.pos() >= self.movi_end {
                self.eof = true;
                return Err(Error::Eof);
            }
            let pos = self.io.pos();
            let id = match self.io.tag() {
                Ok(t) => t,
                Err(Error::UnexpectedEof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            };
            let size = self.io.rl32()?;
            if id == riff_ids::LIST.as_bytes() {
                let list_type = self.io.tag()?;
                if list_type == avi_ids::REC_.as_bytes() {
                    // A grouping wrapper with no payload of its own beyond its
                    // children — skip just its 12-byte header and let the
                    // loop read its first child as an ordinary chunk.
                    continue;
                }
                self.io.skip(u64::from(size).saturating_sub(4))?;
                skip_odd_pad(&mut self.io, u64::from(size))?;
                continue;
            }
            let Some((stream_idx, _kind)) = index::parse_chunk_tag(id) else {
                // `idx1`, `JUNK`, an `OpenDML` `ix##`, or anything else that is
                // not stream data.
                self.io.skip(u64::from(size))?;
                skip_odd_pad(&mut self.io, u64::from(size))?;
                continue;
            };
            let Some(sidx) = usize::try_from(stream_idx).ok() else {
                self.io.skip(u64::from(size))?;
                skip_odd_pad(&mut self.io, u64::from(size))?;
                continue;
            };
            if sidx >= self.streams.len() {
                self.io.skip(u64::from(size))?;
                skip_odd_pad(&mut self.io, u64::from(size))?;
                continue;
            }

            // Bound the declared length against what the file actually has
            // left before allocating anything — the classic amplification
            // lever for a length-prefixed chunk format.
            if let Some(fsize) = self.io.size()
                && u64::from(size) > fsize.saturating_sub(self.io.pos())
            {
                return Err(Error::InvalidData(
                    "avi: chunk claims more bytes than remain",
                ));
            }
            let n = usize::try_from(size).unwrap_or(usize::MAX);
            let mut pkt = Packet::alloc(&mut self.budget, n)?;
            self.io.read_exact(pkt.payload_mut())?;
            skip_odd_pad(&mut self.io, u64::from(size))?;

            let media_type = self.streams.get(sidx).and_then(Stream::media_type);
            let time_base = self.streams.get(sidx).map_or(TIME_BASE_Q, |s| s.time_base);
            let Some(st) = self.state.get_mut(sidx) else {
                continue;
            };
            let (ticks, dur_ticks) = if st.sample_size == 0 {
                // One AVI "chunk" is one tick of `strh`'s own `dwScale`/
                // `dwRate` clock, not necessarily one tick of `time_base`:
                // video's `time_base` never diverges from that clock (one
                // chunk is one frame, `native_ticks_per_chunk == 1`), but
                // audio's `time_base` is overridden to the format's true
                // sample rate, finer than the chunk clock in every
                // compressed-audio AVI measured for this crate — see
                // `hdrl::StreamBuild::native_ticks_per_chunk`'s own doc.
                let t = i64::try_from(st.chunks)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(st.native_ticks_per_chunk);
                st.chunks = st.chunks.saturating_add(1);
                (t, st.native_ticks_per_chunk)
            } else {
                let denom = u64::from(st.sample_size);
                // `dwSampleSize` is a byte count, not a ratio a float would
                // approximate sensibly — the whole point of this branch is an
                // exact integer sample count.
                #[allow(
                    clippy::integer_division,
                    reason = "dwSampleSize divides a byte count into an exact sample count, not a ratio"
                )]
                let t = i64::try_from(st.bytes / denom.max(1)).unwrap_or(i64::MAX);
                #[allow(
                    clippy::integer_division,
                    reason = "dwSampleSize divides a byte count into an exact sample count, not a ratio"
                )]
                let dur = i64::try_from(u64::from(size) / denom.max(1))
                    .unwrap_or(0)
                    .max(1);
                st.bytes = st.bytes.saturating_add(u64::from(size));
                (t, dur)
            };
            let ts = Timestamp::new(ticks.saturating_add(st.start));

            let is_key = self.is_key_at(pos, media_type);
            pkt.stream_index = stream_idx;
            // AVI carries no explicit presentation timestamp for video: `ts`
            // here is decode order (frame count), and a video stream can
            // legally reorder for display (B-frames) with nothing in the
            // container to say by how much. The reference leaves video's pts
            // unset for exactly this reason (measured: `ffprobe` reports
            // `pts=N/A` on every AVI video packet, `dts` only) and only
            // back-fills pts=dts for streams that cannot reorder. Setting
            // both unconditionally, as this used to do, fabricates a value
            // the reference never claims to have -- and a muxer downstream
            // that refuses to write a packet with no pts (mpegts, Matroska)
            // then silently accepts what should be a hard error.
            pkt.pts = if media_type == Some(MediaType::Video) {
                Timestamp::NONE
            } else {
                ts
            };
            pkt.dts = ts;
            pkt.pos = Some(pos);
            pkt.duration = Timestamp::new(dur_ticks)
                .to_duration(time_base)
                .unwrap_or(Duration::ZERO);
            pkt.set_duration_ts(dur_ticks);
            pkt.flags = if is_key {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            };

            if pkt.is_key() {
                let us = ts.rescale(time_base, TIME_BASE_Q, Rounding::default());
                self.index
                    .add(vaco_format_core::seek::IndexEntry::keyframe(pos, us));
            }
            return Ok(pkt);
        }
    }

    /// Position the source at the next recognisable stream-data chunk at or
    /// after `pos`, scanning byte by byte within `movi`.
    fn resync(&mut self, pos: u64) -> Result<()> {
        self.eof = false;
        let start = pos.max(self.movi_children_start);
        let limit = self.movi_end;
        if start >= limit {
            self.io.seek(limit)?;
            self.eof = true;
            return Err(Error::Eof);
        }
        self.io.seek(start)?;
        let mut at = start;
        let mut window = [0u8; 4];
        if self.io.read_exact(&mut window).is_err() {
            self.eof = true;
            return Err(Error::Eof);
        }
        loop {
            if index::parse_chunk_tag(window)
                .is_some_and(|(s, _)| (s as usize) < self.streams.len())
            {
                self.io.seek(at)?;
                return Ok(());
            }
            at = at.saturating_add(1);
            if at.saturating_add(4) > limit {
                self.eof = true;
                return Err(Error::Eof);
            }
            let Ok(next) = self.io.r8() else {
                self.eof = true;
                return Err(Error::Eof);
            };
            window = [window[1], window[2], window[3], next];
        }
    }
}

fn duration_from_main(main: &hdrl::MainHeader, state: &[StreamState]) -> Option<Duration> {
    if main.micro_sec_per_frame == 0 || main.total_frames == 0 {
        return None;
    }
    let _ = state;
    let micros = u64::from(main.micro_sec_per_frame).saturating_mul(u64::from(main.total_frames));
    Some(Duration::from_micros(
        i64::try_from(micros).unwrap_or(i64::MAX),
    ))
}

/// Read `idx1`'s payload, if it is present and reachable, restoring the
/// source's position to `resume` before returning. `None` on any failure —
/// a missing or unreachable index is not an error, just less to seek with.
fn peek_idx1(
    io: &mut IoContext,
    movi_end: u64,
    resume: u64,
    budget: &mut Budget,
) -> Option<Vec<u8>> {
    let mut pos = movi_end;
    // Scan a bounded number of top-level chunks past `movi` looking for
    // `idx1`. In practice it is the very next chunk; the bound exists so a
    // file with many trailing `JUNK`/`LIST` siblings cannot turn this into an
    // unbounded scan.
    for _ in 0..64 {
        io.seek(pos).ok()?;
        let id = io.tag().ok()?;
        let size = u64::from(io.rl32().ok()?);
        if id == avi_ids::IDX1.as_bytes() {
            let n = usize::try_from(size).unwrap_or(usize::MAX);
            let mut buf = budget.alloc::<u8>(n).ok()?;
            io.read_exact(&mut buf).ok()?;
            let _ = io.seek(resume);
            return Some(buf);
        }
        // Every top-level chunk, `LIST` included, advances by the same
        // formula: an 8-byte header, its declared payload, and the pad byte
        // an odd-sized payload carries.
        pos = pos
            .saturating_add(8)
            .saturating_add(size)
            .saturating_add(size % 2);
    }
    let _ = io.seek(resume);
    None
}

/// Scan forward from `resume` for the first `movi` chunk belonging to
/// `target_stream_idx`, and return the leading bytes of its payload up to
/// (not including) the first MPEG-4 Part 2 group-of-pictures (`00 00 01 B3`)
/// or picture (`00 00 01 B6`) start code, whichever comes first — the VOL
/// header real ffmpeg's own probe extracts as `extradata` for a stream whose
/// `strf` carries none. `None` if no such marker turns up within the scanned
/// window, or on any parse/IO failure; `io`'s position is always restored to
/// `resume` before returning, on every path, so a caller need not repeat
/// that itself.
///
/// Bounded the same two ways [`peek_idx1`] is: at most 256 top-level `movi`
/// children examined (comfortably past any plausible amount of other
/// streams' interleaved chunks before this stream's first one), and the
/// payload read for a matching chunk is capped at 4 KiB — orders of
/// magnitude more than a real VOL header has ever measured at, and small
/// enough that a malformed file with no start code at all cannot turn this
/// into an unbounded read.
fn peek_mpeg4_vol_header(
    io: &mut IoContext,
    resume: u64,
    movi_end: u64,
    target_stream_idx: usize,
    budget: &mut Budget,
) -> Option<Vec<u8>> {
    const HEADER_CAP: usize = 4096;
    let mut pos = resume;
    for _ in 0..256 {
        if pos >= movi_end {
            break;
        }
        io.seek(pos).ok()?;
        let id = io.tag().ok()?;
        let size = u64::from(io.rl32().ok()?);
        if id == riff_ids::LIST.as_bytes() {
            let list_type = io.tag().ok()?;
            if list_type == avi_ids::REC_.as_bytes() {
                // A grouping wrapper with no payload of its own — descend
                // into its first child as an ordinary chunk, the same way
                // `read_one`'s own sequential walk does.
                pos = pos.saturating_add(12);
                continue;
            }
            pos = pos
                .saturating_add(8)
                .saturating_add(size)
                .saturating_add(size % 2);
            continue;
        }
        let Some((stream_idx, _kind)) = index::parse_chunk_tag(id) else {
            pos = pos
                .saturating_add(8)
                .saturating_add(size)
                .saturating_add(size % 2);
            continue;
        };
        if usize::try_from(stream_idx).ok() != Some(target_stream_idx) {
            pos = pos
                .saturating_add(8)
                .saturating_add(size)
                .saturating_add(size % 2);
            continue;
        }
        let want = usize::try_from(size).unwrap_or(usize::MAX).min(HEADER_CAP);
        let mut buf = budget.alloc::<u8>(want).ok()?;
        io.read_exact(&mut buf).ok()?;
        let _ = io.seek(resume);
        let gop = buf.windows(4).position(|w| w == [0x00, 0x00, 0x01, 0xB3]);
        let vop = buf.windows(4).position(|w| w == [0x00, 0x00, 0x01, 0xB6]);
        let cut = match (gop, vop) {
            (Some(g), Some(v)) => g.min(v),
            (Some(g), None) => g,
            (None, Some(v)) => v,
            (None, None) => return None,
        };
        return (cut > 0).then(|| buf.get(..cut).unwrap_or(&[]).to_vec());
    }
    let _ = io.seek(resume);
    None
}

fn build_resolved(
    entries: &[Idx1Entry],
    base: index::OffsetBase,
    movi_fourcc_pos: u64,
    streams: &[Stream],
    state: &[StreamState],
    opts: &FormatOptions,
) -> index::Resolved {
    // `index::build_from_idx1` wants `&[index::ClockView]`; by the time this
    // runs, `open_with_limits` has already split `hdrl::StreamBuild` into
    // `streams` (public) and `state` (private clock inputs), so this rejoins
    // the two facts it needs rather than threading `StreamBuild` itself
    // through the whole open path.
    let views: Vec<index::ClockView> = streams
        .iter()
        .zip(state)
        .map(|(s, st)| index::ClockView {
            time_base: s.time_base,
            sample_size: st.sample_size,
            start: u32::try_from(st.start.max(0)).unwrap_or(0),
            native_ticks_per_chunk: st.native_ticks_per_chunk,
        })
        .collect();
    index::build_from_idx1(entries, base, movi_fourcc_pos, &views, opts)
}

fn parse_info_list(payload: &[u8], metadata: &mut Vec<(String, String)>) {
    // Measured against `ffprobe 8.1`: `ffmpeg -metadata title=... artist=...
    // comment=... copyright=... genre=... date=... -f avi out.avi`, then
    // `-show_entries format_tags`. See `docs/format/vaco-demux-avi.md`.
    fn key_for(id: [u8; 4]) -> Option<&'static str> {
        match &id {
            b"INAM" => Some("title"),
            b"IART" => Some("artist"),
            b"ICMT" => Some("comment"),
            b"ICOP" => Some("copyright"),
            b"IGNR" => Some("genre"),
            b"ICRD" => Some("date"),
            b"ISFT" => Some("software"),
            _ => None,
        }
    }
    for chunk in vaco_format_riff::chunk::ChunkIter::new(payload, 0).flatten() {
        let Some(key) = key_for(chunk.id.as_bytes()) else {
            continue;
        };
        let end = chunk
            .payload
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(chunk.payload.len());
        let text = String::from_utf8_lossy(chunk.payload.get(..end).unwrap_or(&[])).into_owned();
        metadata.push((key.to_owned(), text));
    }
}

/// Skip the one pad byte a chunk of odd `declared_size` carries, having
/// already consumed exactly `declared_size` bytes of its payload (or skipped
/// past it).
fn skip_odd_pad(io: &mut IoContext, declared_size: u64) -> Result<()> {
    if declared_size % 2 == 1 {
        io.skip(1)?;
    }
    Ok(())
}

impl Demuxer for AviDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
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
                    .ok_or(Error::InvalidData("avi: seek names an unknown stream"))?;
                let rate = st
                    .params
                    .video
                    .as_ref()
                    .map_or(vaco_core::Rational::ZERO, |v| v.frame_rate);
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
                    .ok_or(Error::InvalidData("avi: seek names an unknown stream"))?;
                let want = ts.rescale(st.time_base, TIME_BASE_Q, Rounding::default());
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
                        self.eof = false;
                        self.resync(entry.pos)
                    }
                    SeekStrategy::Byte => self.resync(self.movi_children_start),
                    SeekStrategy::BinarySearch | SeekStrategy::Unsupported => {
                        Err(Error::NotSeekable)
                    }
                }
            }
            SeekTarget::Frame { .. } => Err(Error::Unsupported("avi: unresolved frame seek")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}
