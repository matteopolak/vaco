//! The demuxer: header, tracks, clusters, cues and seeking.
//!
//! # Specification
//!
//! RFC 9559 throughout — section 5 for the element definitions, section 10 for
//! blocks and lacing, section 11 for the timestamp model, section 12 for
//! languages and section 16 for `Segment Position`. RFC 8794 section 6.2 for
//! unknown-size termination, which lives in [`crate::ebml`].
//!
//! # Shape
//!
//! Two parsers, split by whether the element is bounded:
//!
//! * **Header elements** (`Info`, `Tracks`, `Cues`, `Tags`, `Chapters`,
//!   `Attachments`, `SeekHead`) are read whole into a budgeted buffer and walked
//!   in memory. They are bounded by the file, always have a known size, and
//!   parsing them from a slice is both simpler and faster than streaming.
//! * **Clusters** are streamed, because one may be of unknown size, may be
//!   arbitrarily large, and in a live `WebM` file arrives on a pipe.
//!
//! [`MatroskaDemuxer::read_packet`] is therefore a small state machine over
//! [`ebml::Stack`] plus a queue, since one laced block yields many packets.

use std::collections::VecDeque;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecParameters, FieldOrder, VideoParameters};
use vaco_color::{
    ChromaLocation, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_core::{Duration, Error, MediaType, Rational, Result, Rounding, Timestamp, rescale_rnd};
use vaco_format_core::seek::{IndexEntry, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
use vaco_format_core::{
    Chapter, Demuxer, DemuxerDesc, Disposition, FormatFlags, FormatOptions, ParserProvider, Stream,
};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags, PacketSideData};

use crate::block::{self, Lacing};
use crate::codec;
use crate::ebml::{self, schema as el};

/// What this container declares to the generic machinery.
///
/// `TS_NEGATIVE` because `CodecDelay` routinely pushes the first audio packet
/// before zero; `SEEK_TO_PTS` because `Cues` index presentation time;
/// `GENERIC_INDEX` because a file may carry no `Cues` at all and the core's
/// index is then the only one there is.
pub const FLAGS: FormatFlags = FormatFlags::TS_NEGATIVE
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::SEEK_TO_PTS)
    .union(FormatFlags::VARIABLE_FPS)
    .union(FormatFlags::GENERIC_INDEX);

/// Nanoseconds per second, the unit every Matroska duration field is in.
const NS: i64 = 1_000_000_000;

/// The nanosecond time base every `*_ns` field is rescaled from.
const NS_BASE: Rational = Rational::new(1, 1_000_000_000);

/// Largest header element we will buffer, before the [`Budget`] also applies.
///
/// `Attachments` can legitimately be tens of megabytes (a font, a cover image),
/// so this is generous; it exists to turn a 2^56 declared size into an error
/// before any allocation rather than to be a tight bound.
const MAX_HEADER_ELEMENT: u64 = 256 << 20;

/// Largest single block element we will buffer.
const MAX_BLOCK: u64 = 256 << 20;

/// Elements the level-1 scan will step over before giving up.
///
/// A file whose `Segment` is a million empty `Void`s is not a file anyone wrote;
/// the cap turns it from a long wait into an error.
const MAX_LEVEL1_ELEMENTS: u32 = 1 << 20;

/// The descriptor a registry holds.
///
/// The name is the comma-joined family the reference reports as `format_name`
/// for **both** spellings — measured, not assumed: `ffprobe` prints
/// `matroska,webm` for a `DocType=webm` file as readily as for a `matroska`
/// one. `planning/18-formats.md` section 3.2.4 predicts the `DocType` is
/// reported instead; it is not.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "matroska,webm",
    long_name: "Matroska / WebM",
    extensions: &["mkv", "mk3d", "mka", "mks", "webm"],
    // Empty deliberately. `ffprobe -h demuxer=matroska` prints a
    // "Common extensions" line and no "Mime type" line, so the *demuxer* claims
    // none — the `webm` muxer is where `video/webm` is declared. Claiming one
    // here would let a MIME hint outscore a content probe on ambiguous input.
    mime_types: &[],
    flags: crate::FLAGS,
    probe: crate::probe::probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(MatroskaDemuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
    )?))
}

// ------------------------------------------------------------------- tracks

/// How a track's frames were transformed before storage.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Encoding {
    /// `ContentCompAlgo` 0.
    Zlib,
    /// `ContentCompAlgo` 3: `ContentCompSettings` prefixes every frame.
    HeaderStrip(Vec<u8>),
    /// Anything we do not implement. The track's blocks are skipped.
    Unsupported,
}

/// The per-track state the packet path needs.
#[derive(Debug)]
struct Track {
    number: u64,
    /// `TrackUID`, which is what `Tags` target — not the track number.
    uid: u64,
    stream_index: u32,
    /// `DefaultDuration` in nanoseconds, when the track states one.
    default_duration_ns: Option<u64>,
    /// `CodecDelay` already rescaled into the stream time base, which is what
    /// gets subtracted from every timestamp on this track.
    delay_ticks: i64,
    /// `CodecDelay` in samples, the leading `SkipSamples` on the first packet.
    delay_samples: u32,
    sample_rate: u32,
    /// Set once the first packet of this track has carried its leading skip.
    emitted_delay: bool,
    encodings: Vec<Encoding>,
    /// Whether any encoding is one we cannot undo.
    droppable: bool,
}

// ------------------------------------------------------------------ demuxer

/// The Matroska/WebM demuxer.
#[derive(Debug)]
pub struct MatroskaDemuxer {
    io: IoContext,
    budget: Budget,
    caps: ebml::Caps,
    doc_type: String,
    /// `TimestampScale`: nanoseconds per tick. RFC 9559 section 5.1.2.9.
    timestamp_scale: u64,
    time_base: Rational,
    streams: Vec<Stream>,
    tracks: Vec<Track>,
    chapters: Vec<Chapter>,
    metadata: Vec<(String, String)>,
    index: PacketIndex,
    duration: Option<Duration>,

    /// Byte offset of the `Segment`'s first child, the origin every
    /// `SeekPosition` and `CueClusterPosition` is relative to (RFC 9559 §16).
    segment_data_pos: u64,
    segment_end: Option<u64>,
    first_cluster: Option<u64>,

    /// Open masters below `Segment`, for RFC 8794 section 6.2.
    stack: ebml::Stack,
    /// `Cluster\Timestamp` of the cluster currently open, in ticks.
    cluster_timestamp: i64,
    /// Byte offset of the currently open `Cluster`'s ID octet.
    ///
    /// This — not the block's own position — is what an index entry records: a
    /// seek has to land on a cluster boundary or the parse restarts mid-element.
    cluster_pos: u64,
    in_cluster: bool,
    queue: VecDeque<Packet>,
    eof: bool,
}

impl MatroskaDemuxer {
    /// Open a Matroska or `WebM` stream and read everything before the first
    /// packet.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the EBML header or `Segment` is malformed,
    /// [`Error::Unsupported`] for a `DocType` we do not read, and
    /// [`Error::LimitExceeded`] when a declared size is over budget.
    pub fn open(
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        opts: &FormatOptions,
    ) -> Result<Self> {
        Self::open_with_limits(src, parsers, opts, Limits::permissive())
    }

    /// [`MatroskaDemuxer::open`] with an explicit allocation budget.
    ///
    /// The fuzz targets and any embedder handling untrusted input use
    /// [`Limits::strict`] here; the CLI default is
    /// [`Limits::permissive`], which is what [`MatroskaDemuxer::open`] passes.
    ///
    /// # Errors
    ///
    /// As [`MatroskaDemuxer::open`].
    pub fn open_with_limits(
        src: Box<dyn MediaSource>,
        _parsers: &dyn ParserProvider,
        opts: &FormatOptions,
        limits: Limits,
    ) -> Result<Self> {
        let io = IoContext::new(src, &IoOptions::default())?;
        let mut me = Self {
            io,
            budget: Budget::new(limits),
            caps: ebml::Caps::default(),
            doc_type: String::new(),
            timestamp_scale: 1_000_000,
            time_base: Rational::new(1, 1000),
            streams: Vec::new(),
            tracks: Vec::new(),
            chapters: Vec::new(),
            metadata: Vec::new(),
            index: PacketIndex::with_options(opts),
            duration: None,
            segment_data_pos: 0,
            segment_end: None,
            first_cluster: None,
            stack: ebml::Stack::new(),
            cluster_timestamp: 0,
            cluster_pos: 0,
            in_cluster: false,
            queue: VecDeque::new(),
            eof: false,
        };
        me.read_ebml_header()?;
        me.enter_segment()?;
        me.scan_level1(opts)?;
        me.rewind_to_first_cluster()?;
        Ok(me)
    }

    /// The `DocType` the EBML header declared: `matroska` or `webm`.
    ///
    /// Not the format name — see [`DEMUXER`].
    #[must_use]
    pub fn doc_type(&self) -> &str {
        &self.doc_type
    }

    /// `TimestampScale`, in nanoseconds per tick.
    #[must_use]
    pub const fn timestamp_scale(&self) -> u64 {
        self.timestamp_scale
    }

    /// The index built from `Cues`, plus whatever packets have gone past.
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    // -------------------------------------------------------------- header

    fn read_ebml_header(&mut self) -> Result<()> {
        let header = ebml::read_header(&mut self.io, self.caps)?
            .ok_or(Error::InvalidData("empty stream"))?;
        if header.id != el::EBML {
            return Err(Error::InvalidData(
                "stream does not start with an EBML header",
            ));
        }
        let size = header
            .size
            .known()
            .ok_or(Error::InvalidData("EBML header is of unknown size"))?;
        let body = self.read_body(size, "ebml_header")?;
        let mut max_id_len = u64::from(ebml::MAX_ID_LEN);
        let mut max_size_len = u64::from(ebml::MAX_SIZE_LEN);
        let mut doc_type = String::new();
        let mut read_version = 1u64;
        for child in ebml::Slice::new(&body, self.caps).children() {
            match child.id {
                el::EBMLMAXIDLENGTH => max_id_len = ebml::as_uint(child.data).unwrap_or(4),
                el::EBMLMAXSIZELENGTH => max_size_len = ebml::as_uint(child.data).unwrap_or(8),
                el::DOCTYPE => ebml::as_str(child.data)
                    .unwrap_or_default()
                    .clone_into(&mut doc_type),
                el::DOCTYPEREADVERSION => read_version = ebml::as_uint(child.data).unwrap_or(1),
                _ => {}
            }
        }
        self.caps.adopt(max_id_len, max_size_len)?;
        if doc_type != "matroska" && doc_type != "webm" {
            return Err(Error::Unsupported("EBML DocType is not matroska or webm"));
        }
        // RFC 9559 section 7: a reader must refuse a DocTypeReadVersion above
        // the one it implements, because the file may use syntax it cannot skip.
        if read_version > 4 {
            return Err(Error::Unsupported("DocTypeReadVersion above 4"));
        }
        self.doc_type = doc_type;
        Ok(())
    }

    fn enter_segment(&mut self) -> Result<()> {
        loop {
            let header = ebml::read_header(&mut self.io, self.caps)?
                .ok_or(Error::InvalidData("no Segment element"))?;
            if header.id == el::SEGMENT {
                self.segment_data_pos = header.data_pos;
                self.segment_end = header.end();
                return Ok(());
            }
            // Anything else at the root is skipped, which covers `Void` padding
            // between the header and the segment.
            let size = header
                .size
                .known()
                .ok_or(Error::InvalidData("unknown-size element before Segment"))?;
            self.io.skip(size)?;
        }
    }

    /// Walk the `Segment`'s direct children, parsing every header element and
    /// stepping over cluster bodies.
    ///
    /// The scan is preferred over following `SeekHead` because it finds the
    /// first cluster as a side effect and because a `SeekHead` that lies is
    /// common enough that its positions have to be validated anyway. `SeekHead`
    /// is consulted only when the scan cannot continue — an unknown-size
    /// cluster, or a source that cannot seek — which is exactly the case it is
    /// needed for.
    fn scan_level1(&mut self, opts: &FormatOptions) -> Result<()> {
        let mut seek_entries: Vec<(u32, u64)> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        let mut steps = 0u32;
        let mut complete = false;
        let seekable = self.io.seekability() != Seekability::None;
        loop {
            steps = steps.saturating_add(1);
            if steps > MAX_LEVEL1_ELEMENTS {
                return Err(Error::LimitExceeded {
                    limit: "matroska_level1_elements",
                    requested: u64::from(steps),
                    cap: u64::from(MAX_LEVEL1_ELEMENTS),
                });
            }
            if self.segment_end.is_some_and(|e| self.io.pos() >= e) {
                complete = true;
                break;
            }
            let Some(header) = ebml::read_header(&mut self.io, self.caps)? else {
                complete = true;
                break;
            };
            if header.id == el::CLUSTER {
                if self.first_cluster.is_none() {
                    self.first_cluster = Some(header.pos);
                }
                match header.size.known() {
                    Some(n) if seekable => {
                        self.io.skip(n)?;
                        continue;
                    }
                    // An unknown-size cluster cannot be stepped over, and on a
                    // pipe nothing can be. Either way the scan stops here — and
                    // on a pipe the header has already been consumed, so the
                    // packet path has to resume *inside* this cluster rather
                    // than expect to read its header again.
                    _ => {
                        if !seekable {
                            self.stack.clear();
                            self.stack.push(el::CLUSTER, header.end())?;
                            self.in_cluster = true;
                            self.cluster_timestamp = 0;
                            self.cluster_pos = header.pos;
                        }
                        break;
                    }
                }
            }
            // A root element inside the segment means a new document or a
            // second segment; we read the first segment only.
            if ebml::is_root(header.id) {
                self.io.seek(header.pos)?;
                break;
            }
            let Some(size) = header.size.known() else {
                // RFC 8794 section 6.2 allows unknown sizes only on `Segment`
                // and `Cluster`; anything else is unrecoverable (plan 18 K3).
                return Err(Error::InvalidData("unknown-size element below Segment"));
            };
            // RFC 8794 section 4: an element's data lies inside its parent. A
            // size that runs past the `Segment` — or past the file — is the
            // classic corrupt-VINT shape, and skipping by it lands the scan at
            // end of input with nothing found. The reference refuses it too,
            // reporting "exceeds containing master element", so the scan stops
            // here and the `SeekHead` recovery below takes over.
            let overruns = header.end().is_none_or(|e| {
                self.segment_end.is_some_and(|seg| e > seg)
                    || self.io.size().is_some_and(|total| e > total)
            });
            if overruns {
                self.io.seek(header.pos)?;
                break;
            }
            if !matches!(
                header.id,
                el::INFO
                    | el::TRACKS
                    | el::CUES
                    | el::TAGS
                    | el::CHAPTERS
                    | el::ATTACHMENTS
                    | el::SEEKHEAD
            ) {
                self.io.skip(size)?;
                continue;
            }
            if size > MAX_HEADER_ELEMENT {
                self.io.skip(size)?;
                continue;
            }
            let body = self.read_body(size, "matroska_header_element")?;
            seen.push(header.id);
            self.parse_level1(header.id, &body, opts, &mut seek_entries)?;
        }
        // A scan that ran to the end of the `Segment` has seen everything, so
        // `SeekHead` can add nothing. One that stopped early — at an
        // unknown-size cluster, at a corrupt size, or at end of input — may have
        // missed `Tracks`, `Cues` or `Tags` sitting past the clusters, and
        // `SeekHead` is the only remaining record of where they are.
        if !complete && seekable {
            self.follow_seek_head(&seek_entries, &seen, opts)?;
        }
        if self.first_cluster.is_none() && seekable {
            self.first_cluster = self.first_cluster_from_cues();
        }
        // RFC 9559 section 5.1.2.9: TimestampScale is nanoseconds per tick, so
        // the time base is scale/1e9 — 1/1000 for the default 1 000 000, and
        // deliberately not the 1/1000 every implementation assumes.
        self.time_base = time_base_for(self.timestamp_scale)
            .ok_or(Error::InvalidData("TimestampScale has no usable time base"))?;
        self.finish_streams(opts);
        Ok(())
    }

    fn parse_level1(
        &mut self,
        id: u32,
        body: &[u8],
        opts: &FormatOptions,
        seek_entries: &mut Vec<(u32, u64)>,
    ) -> Result<()> {
        match id {
            el::INFO => self.parse_info(body),
            el::TRACKS => self.parse_tracks(body, opts)?,
            el::CUES => self.parse_cues(body),
            el::TAGS => self.parse_tags(body),
            el::CHAPTERS => self.parse_chapters(body),
            el::ATTACHMENTS => self.parse_attachments(body, opts)?,
            el::SEEKHEAD => parse_seek_head(body, self.caps, seek_entries),
            _ => {}
        }
        Ok(())
    }

    /// Follow `SeekHead` for the elements the scan never reached.
    ///
    /// Every position is validated by seeking to it and checking that the
    /// element there really has the claimed ID; a `SeekHead` that lies is
    /// ignored rather than trusted (plan 18 section 3.2.6).
    fn follow_seek_head(
        &mut self,
        entries: &[(u32, u64)],
        seen: &[u32],
        opts: &FormatOptions,
    ) -> Result<()> {
        let resume = self.io.pos();
        let mut extra = Vec::new();
        for &(id, offset) in entries {
            if seen.contains(&id)
                || !matches!(
                    id,
                    el::INFO | el::TRACKS | el::CUES | el::TAGS | el::CHAPTERS | el::ATTACHMENTS
                )
            {
                continue;
            }
            let Some(at) = self.segment_data_pos.checked_add(offset) else {
                continue;
            };
            if self.io.size().is_some_and(|s| at >= s) || self.io.seek(at).is_err() {
                continue;
            }
            let Ok(Some(header)) = ebml::read_header(&mut self.io, self.caps) else {
                continue;
            };
            if header.id != id {
                continue;
            }
            let Some(size) = header.size.known().filter(|&n| n <= MAX_HEADER_ELEMENT) else {
                continue;
            };
            let Ok(body) = self.read_body(size, "matroska_header_element") else {
                continue;
            };
            self.parse_level1(id, &body, opts, &mut extra)?;
        }
        self.io.seek(resume)?;
        Ok(())
    }

    /// The earliest `Cues` position that really holds a `Cluster`.
    ///
    /// The recovery path for a file whose level-1 scan could not reach a
    /// cluster — a corrupt element size stops it early, and then the only
    /// remaining record of where the clusters are is `Cues`. Validated the same
    /// way `SeekHead` entries are: read the header there and check the ID.
    fn first_cluster_from_cues(&mut self) -> Option<u64> {
        let resume = self.io.pos();
        let mut best: Option<u64> = None;
        let mut candidates: Vec<u64> = self.index.entries().iter().map(|e| e.pos).collect();
        candidates.sort_unstable();
        candidates.dedup();
        for at in candidates {
            if self.io.seek(at).is_err() {
                continue;
            }
            if let Ok(Some(h)) = ebml::read_header(&mut self.io, self.caps)
                && h.id == el::CLUSTER
            {
                best = Some(at);
                break;
            }
        }
        let _ = self.io.seek(resume);
        best
    }

    fn rewind_to_first_cluster(&mut self) -> Result<()> {
        if let Some(pos) = self.first_cluster
            && self.io.seekability() != Seekability::None
        {
            self.io.seek(pos)?;
        }
        Ok(())
    }

    /// Read `size` octets from the current position into a fresh buffer.
    ///
    /// Two-phase (plan 13 section 2.2.2 rule 3): the declared size is checked
    /// against the file and the budget first, and the buffer then grows from
    /// bytes that actually arrived, so a 16-octet file cannot cause a gigabyte
    /// allocation.
    fn read_body(&mut self, size: u64, limit: &'static str) -> Result<Vec<u8>> {
        if size > MAX_HEADER_ELEMENT.max(MAX_BLOCK) {
            return Err(Error::LimitExceeded {
                limit,
                requested: size,
                cap: MAX_HEADER_ELEMENT.max(MAX_BLOCK),
            });
        }
        if let Some(total) = self.io.size()
            && size > total.saturating_sub(self.io.pos())
        {
            return Err(Error::InvalidData("element claims more bytes than remain"));
        }
        self.budget.check(size)?;
        let declared = usize::try_from(size).unwrap_or(usize::MAX);
        let mut out = self.budget.incremental::<u8>(declared);
        let mut chunk = [0u8; 16 * 1024];
        let mut left = declared;
        while left > 0 {
            let want = left.min(chunk.len());
            let Some(dst) = chunk.get_mut(..want) else {
                break;
            };
            self.io.read_exact(dst)?;
            out.push_slice(&mut self.budget, dst)?;
            left -= want;
        }
        Ok(out.into_vec())
    }

    // ------------------------------------------------------------ Info

    fn parse_info(&mut self, body: &[u8]) {
        let mut duration_ticks: Option<f64> = None;
        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::TIMESTAMPSCALE => {
                    if let Some(v) = ebml::as_uint(child.data).filter(|&v| v > 0) {
                        self.timestamp_scale = v;
                    }
                }
                el::DURATION => duration_ticks = ebml::as_float(child.data),
                el::TITLE => {
                    if let Some(s) = ebml::as_str(child.data).filter(|s| !s.is_empty()) {
                        push_meta(&mut self.metadata, "title", s);
                    }
                }
                _ => {}
            }
        }
        // Held as ticks until `finish_streams` knows the time base.
        if let Some(d) = duration_ticks {
            self.duration = duration_from_ticks(d, self.timestamp_scale);
        }
    }

    // ------------------------------------------------------------ Tracks

    fn parse_tracks(&mut self, body: &[u8], opts: &FormatOptions) -> Result<()> {
        for entry in ebml::Slice::new(body, self.caps).children() {
            if entry.id != el::TRACKENTRY {
                continue;
            }
            self.check_stream_room(opts)?;
            self.parse_track_entry(entry.data);
        }
        Ok(())
    }

    /// Room for one more stream, under both the budget and `-max_streams`.
    fn check_stream_room(&self, opts: &FormatOptions) -> Result<()> {
        let want = self.streams.len() as u64 + 1;
        self.budget.check_streams(want)?;
        let cap = u64::try_from(opts.max_streams).unwrap_or(u64::MAX);
        if want > cap {
            return Err(Error::LimitExceeded {
                limit: "max_streams",
                requested: want,
                cap,
            });
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one flat match over a track's elements is clearer than splitting a\
                  single element list across helpers"
    )]
    fn parse_track_entry(&mut self, body: &[u8]) {
        let mut number = 0u64;
        let mut uid = 0u64;
        let mut track_type = 0u64;
        let mut codec_id = String::new();
        let mut private: Option<Vec<u8>> = None;
        let mut name: Option<String> = None;
        let mut language: Option<String> = None;
        let mut language_bcp47: Option<String> = None;
        let mut default_duration: Option<u64> = None;
        let mut codec_delay = 0u64;
        let mut video: Option<&[u8]> = None;
        let mut audio: Option<&[u8]> = None;
        let mut encodings: Option<&[u8]> = None;
        let mut disposition = Disposition::empty();
        // RFC 9559 section 5.1.4.1.9: FlagDefault defaults to 1.
        let mut flag_default = true;

        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::TRACKNUMBER => number = ebml::as_uint(child.data).unwrap_or(0),
                el::TRACKUID => uid = ebml::as_uint(child.data).unwrap_or(0),
                el::TRACKTYPE => track_type = ebml::as_uint(child.data).unwrap_or(0),
                el::CODECID => {
                    ebml::as_str(child.data)
                        .unwrap_or_default()
                        .clone_into(&mut codec_id);
                }
                el::CODECPRIVATE => private = Some(child.data.to_vec()),
                el::NAME => name = ebml::as_str(child.data).map(str::to_owned),
                el::LANGUAGE => language = ebml::as_str(child.data).map(str::to_owned),
                el::LANGUAGEBCP47 => {
                    language_bcp47 = ebml::as_str(child.data).map(str::to_owned);
                }
                el::DEFAULTDURATION => {
                    default_duration = ebml::as_uint(child.data).filter(|&v| v > 0);
                }
                el::CODECDELAY => codec_delay = ebml::as_uint(child.data).unwrap_or(0),
                el::FLAGDEFAULT => flag_default = ebml::as_uint(child.data) != Some(0),
                el::FLAGFORCED => {
                    disposition.set(Disposition::FORCED, ebml::as_uint(child.data) != Some(0));
                }
                el::FLAGHEARINGIMPAIRED => {
                    disposition.set(
                        Disposition::HEARING_IMPAIRED,
                        ebml::as_uint(child.data) == Some(1),
                    );
                }
                el::FLAGVISUALIMPAIRED => {
                    disposition.set(
                        Disposition::VISUAL_IMPAIRED,
                        ebml::as_uint(child.data) == Some(1),
                    );
                }
                el::FLAGTEXTDESCRIPTIONS => {
                    disposition.set(
                        Disposition::DESCRIPTIONS,
                        ebml::as_uint(child.data) == Some(1),
                    );
                }
                el::FLAGORIGINAL => {
                    disposition.set(Disposition::ORIGINAL, ebml::as_uint(child.data) == Some(1));
                }
                el::FLAGCOMMENTARY => {
                    disposition.set(Disposition::COMMENT, ebml::as_uint(child.data) == Some(1));
                }
                el::VIDEO => video = Some(child.data),
                el::AUDIO => audio = Some(child.data),
                el::CONTENTENCODINGS => encodings = Some(child.data),
                _ => {}
            }
        }
        disposition.set(Disposition::DEFAULT, flag_default);

        let Some(mapping) = codec::map(&codec_id) else {
            return;
        };
        // TrackType is authoritative where it disagrees with the codec id
        // (RFC 9559 section 5.1.4.1.3); an unknown value falls back to the codec.
        let media = media_of(track_type).unwrap_or(mapping.media);
        let mut params = CodecParameters::new(media);
        if let Some(id) = mapping.codec {
            params.codec_id = Some(id);
        }
        if let Some(p) = private.filter(|p| !p.is_empty())
            && codec::private_is_extradata(&codec_id)
        {
            params.extradata = Some(p);
        }
        if media == MediaType::Video {
            params.video = Some(video.map_or_else(VideoParameters::default, |d| {
                self.parse_video(d, default_duration)
            }));
        }
        let mut sample_rate = 0u32;
        if media == MediaType::Audio {
            let a = audio.map_or_else(AudioParameters::default, |d| self.parse_audio(d));
            sample_rate = a.sample_rate;
            params.audio = Some(a);
        }

        let index = self.streams.len() as u32;
        let mut stream = Stream::new(index, media, self.time_base);
        stream.id = Some(i64::try_from(number).unwrap_or(i64::MAX));
        stream.params = params;
        stream.disposition = disposition;
        // `DefaultDuration` is the only rate a Matroska track states, and it
        // answers **both** printed rates: `av.mkv`'s 40 000 000 ns track
        // reports `r_frame_rate=25/1` and `avg_frame_rate=25/1`. A track that
        // states none leaves both at `0/0` and `Discovery` estimates them —
        // which is the right split, because a rate derived from observed
        // packet deltas is not something this container stated.
        //
        // `duration_ts` is deliberately **not** set here. Measured: a Matroska
        // track's per-track `DURATION` tag is *not* what the reference prints
        // (`as2.mkv`'s subtitle tag says 1.0 s where the field says 2.008),
        // the value that is printed is the segment `Duration`, and it appears
        // only on a stream that has no timing of its own. That makes it a
        // container-wide rule, and it lives in `Discovery::finish`.
        if media == MediaType::Video
            && let Some(ns) = default_duration
        {
            let rate = frame_rate_from_duration(ns);
            stream.r_frame_rate = rate;
            stream.avg_frame_rate = rate;
        }
        if let Some(t) = name.filter(|s| !s.is_empty()) {
            push_meta(&mut stream.metadata, "title", &t);
        }
        // RFC 9559 section 12: LanguageBCP47 overrides Language when present.
        // "und" is the schema default and the reference does not print it.
        let lang = language_bcp47
            .filter(|s| !s.is_empty())
            .or(language)
            .unwrap_or_else(|| "und".to_owned());
        if lang != "und" && !lang.is_empty() {
            push_meta(&mut stream.metadata, "language", &lang);
        }

        let encodings = encodings.map_or_else(Vec::new, |d| self.parse_encodings(d));
        let droppable = encodings.contains(&Encoding::Unsupported);
        if droppable {
            push_meta(&mut stream.metadata, "encoding", "unsupported");
        }
        self.streams.push(stream);
        self.tracks.push(Track {
            number,
            uid,
            stream_index: index,
            default_duration_ns: default_duration,
            delay_ticks: 0,
            delay_samples: 0,
            sample_rate,
            emitted_delay: false,
            encodings,
            droppable,
        });
        // The delay needs the time base, which `Info` may not have supplied yet
        // if `Tracks` came first; it is finalised in `finish_streams`.
        if let Some(t) = self.tracks.last_mut() {
            t.delay_samples = u32::try_from(
                rescale_rnd(
                    i64::try_from(codec_delay).unwrap_or(i64::MAX),
                    i64::from(sample_rate),
                    NS,
                    Rounding::NearestAwayFromZero,
                )
                .unwrap_or(0),
            )
            .unwrap_or(0);
            t.delay_ticks = i64::try_from(codec_delay).unwrap_or(i64::MAX);
        }
    }

    fn parse_video(&self, body: &[u8], default_duration: Option<u64>) -> VideoParameters {
        let mut v = VideoParameters::default();
        let mut display_w = 0u64;
        let mut display_h = 0u64;
        let mut display_unit = 0u64;
        let (mut crop_t, mut crop_b, mut crop_l, mut crop_r) = (0u64, 0u64, 0u64, 0u64);
        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::PIXELWIDTH => v.coded_width = clamp_dim(ebml::as_uint(child.data)),
                el::PIXELHEIGHT => v.coded_height = clamp_dim(ebml::as_uint(child.data)),
                el::PIXELCROPTOP => crop_t = ebml::as_uint(child.data).unwrap_or(0),
                el::PIXELCROPBOTTOM => crop_b = ebml::as_uint(child.data).unwrap_or(0),
                el::PIXELCROPLEFT => crop_l = ebml::as_uint(child.data).unwrap_or(0),
                el::PIXELCROPRIGHT => crop_r = ebml::as_uint(child.data).unwrap_or(0),
                el::DISPLAYWIDTH => display_w = ebml::as_uint(child.data).unwrap_or(0),
                el::DISPLAYHEIGHT => display_h = ebml::as_uint(child.data).unwrap_or(0),
                el::DISPLAYUNIT => display_unit = ebml::as_uint(child.data).unwrap_or(0),
                el::FLAGINTERLACED => {
                    // RFC 9559 section 5.1.4.1.28: 0 undetermined, 1 interlaced,
                    // 2 progressive. FieldOrder refines an interlaced track.
                    v.field_order = if ebml::as_uint(child.data) == Some(1) {
                        FieldOrder::Unknown
                    } else {
                        FieldOrder::Progressive
                    };
                }
                el::FIELDORDER => {
                    // Section 5.1.4.1.29: 0 progressive, 1 tff, 6 bff,
                    // 9 bff-swapped, 14 tff-swapped.
                    v.field_order = match ebml::as_uint(child.data) {
                        Some(1) => FieldOrder::TopFirst,
                        Some(6) => FieldOrder::BottomFirst,
                        Some(9) => FieldOrder::BottomCodedFirst,
                        Some(14) => FieldOrder::TopCodedFirst,
                        _ => v.field_order,
                    };
                }
                el::COLOUR => v.color = self.parse_colour(child.data),
                _ => {}
            }
        }
        // Section 15.1: the cropping values are removed from the coded size to
        // give the size that is displayed.
        let crop_x = crop_l.saturating_add(crop_r);
        let crop_y = crop_t.saturating_add(crop_b);
        v.width = v
            .coded_width
            .saturating_sub(u32::try_from(crop_x).unwrap_or(u32::MAX));
        v.height = v
            .coded_height
            .saturating_sub(u32::try_from(crop_y).unwrap_or(u32::MAX));
        // RFC 9559 table 8: with DisplayUnit 0, DisplayWidth and DisplayHeight
        // default to the *cropped* pixel size — so a track that states neither
        // has square pixels, which is what the reference reports as 1:1.
        if display_unit == 0 {
            if display_w == 0 {
                display_w = u64::from(v.width);
            }
            if display_h == 0 {
                display_h = u64::from(v.height);
            }
        }
        // DisplayUnit 0 is pixels, which is the only unit an aspect ratio can be
        // derived from without knowing the display device.
        if display_unit == 0 && display_w > 0 && display_h > 0 && v.width > 0 && v.height > 0 {
            v.sample_aspect_ratio = sar(
                display_w,
                display_h,
                u64::from(v.width),
                u64::from(v.height),
            );
        }
        if let Some(ns) = default_duration {
            v.frame_rate = frame_rate_from_duration(ns);
        }
        v
    }

    fn parse_colour(&self, body: &[u8]) -> vaco_color::ColorInfo {
        let mut c = vaco_color::ColorInfo::default();
        let mut siting_h = 0u64;
        let mut siting_v = 0u64;
        for child in ebml::Slice::new(body, self.caps).children() {
            let val = ebml::as_uint(child.data).unwrap_or(0);
            let byte = u8::try_from(val).unwrap_or(u8::MAX);
            match child.id {
                // Sections 5.1.4.1.30.x: these carry H.273 code points directly.
                el::MATRIXCOEFFICIENTS => {
                    c.matrix = MatrixCoefficients::from_u8(byte).unwrap_or_default();
                }
                el::TRANSFERCHARACTERISTICS => {
                    c.transfer = TransferCharacteristic::from_u8(byte).unwrap_or_default();
                }
                el::PRIMARIES => c.primaries = ColorPrimaries::from_u8(byte).unwrap_or_default(),
                // Range is Matroska's own vocabulary, not H.273's:
                // 0 unspecified, 1 broadcast, 2 full, 3 defined by MatrixCoefficients.
                el::RANGE => {
                    c.range = match val {
                        1 => ColorRange::Limited,
                        2 => ColorRange::Full,
                        _ => ColorRange::Unspecified,
                    };
                }
                el::CHROMASITINGHORZ => siting_h = val,
                el::CHROMASITINGVERT => siting_v = val,
                _ => {}
            }
        }
        c.chroma_location = chroma_siting(siting_h, siting_v);
        c
    }

    fn parse_audio(&self, body: &[u8]) -> AudioParameters {
        let mut a = AudioParameters::default();
        let mut channels = 1u64;
        let mut rate = 8000.0f64;
        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::SAMPLINGFREQUENCY => {
                    rate = ebml::as_float(child.data).unwrap_or(rate);
                }
                el::CHANNELS => channels = ebml::as_uint(child.data).unwrap_or(1),
                el::BITDEPTH => {
                    // `BitDepth` is the container's stored depth, so it is
                    // `bits_per_coded_sample`. See the field's docs.
                    a.bits_per_coded_sample = ebml::as_uint(child.data)
                        .and_then(|v| u8::try_from(v).ok())
                        .filter(|&v| v > 0);
                }
                _ => {}
            }
        }
        // A float sampling frequency is the one place Matroska stores a rate
        // that is not an integer; the reference reports an integer, so round.
        a.sample_rate = if rate.is_finite() && rate > 0.0 && rate < 4_000_000_000.0 {
            rate.round() as u32
        } else {
            0
        };
        a.layout = u32::try_from(channels)
            .ok()
            .filter(|&n| n > 0)
            .and_then(ChannelLayout::default_for);
        a
    }

    fn parse_encodings(&self, body: &[u8]) -> Vec<Encoding> {
        let mut out: Vec<(u64, Encoding)> = Vec::new();
        for enc in ebml::Slice::new(body, self.caps).children() {
            if enc.id != el::CONTENTENCODING {
                continue;
            }
            let mut order = 0u64;
            let mut kind = 0u64;
            let mut algo = 0u64;
            let mut settings: Vec<u8> = Vec::new();
            let mut has_compression = false;
            for child in ebml::Slice::new(enc.data, self.caps).children() {
                match child.id {
                    el::CONTENTENCODINGORDER => order = ebml::as_uint(child.data).unwrap_or(0),
                    el::CONTENTENCODINGTYPE => kind = ebml::as_uint(child.data).unwrap_or(0),
                    el::CONTENTCOMPRESSION => {
                        has_compression = true;
                        for c in ebml::Slice::new(child.data, self.caps).children() {
                            match c.id {
                                el::CONTENTCOMPALGO => algo = ebml::as_uint(c.data).unwrap_or(0),
                                el::CONTENTCOMPSETTINGS => settings = c.data.to_vec(),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Section 5.1.4.1.31.2: type 0 is compression, 1 is encryption.
            let encoding = if kind == 1 {
                Encoding::Unsupported
            } else if has_compression || kind == 0 {
                match algo {
                    0 => Encoding::Zlib,
                    3 => Encoding::HeaderStrip(settings),
                    _ => Encoding::Unsupported,
                }
            } else {
                Encoding::Unsupported
            };
            out.push((order, encoding));
        }
        // Section 5.1.4.1.31.1: encodings are applied in descending
        // ContentEncodingOrder, so the highest order was applied last and must
        // be undone first.
        out.sort_by_key(|&(order, _)| core::cmp::Reverse(order));
        out.into_iter().map(|(_, e)| e).collect()
    }

    // ------------------------------------------------------------ Cues

    fn parse_cues(&mut self, body: &[u8]) {
        for point in ebml::Slice::new(body, self.caps).children() {
            if point.id != el::CUEPOINT {
                continue;
            }
            let mut time = None;
            let mut cluster_pos = None;
            for child in ebml::Slice::new(point.data, self.caps).children() {
                match child.id {
                    el::CUETIME => time = ebml::as_uint(child.data),
                    el::CUETRACKPOSITIONS => {
                        for c in ebml::Slice::new(child.data, self.caps).children() {
                            if c.id == el::CUECLUSTERPOSITION {
                                cluster_pos = ebml::as_uint(c.data);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let (Some(time), Some(rel)) = (time, cluster_pos) else {
                continue;
            };
            let Some(at) = self.segment_data_pos.checked_add(rel) else {
                continue;
            };
            // A cue pointing past the end of the file is dropped, not fatal
            // (plan 18 section 3.2.6).
            if self.io.size().is_some_and(|s| at >= s) {
                continue;
            }
            let Ok(ts) = i64::try_from(time) else {
                continue;
            };
            self.index.add(IndexEntry::keyframe(at, Timestamp::new(ts)));
        }
    }

    // ------------------------------------------------------------ Tags

    fn parse_tags(&mut self, body: &[u8]) {
        for tag in ebml::Slice::new(body, self.caps).children() {
            if tag.id != el::TAG {
                continue;
            }
            let mut track_uid = 0u64;
            let mut pairs: Vec<(String, String)> = Vec::new();
            for child in ebml::Slice::new(tag.data, self.caps).children() {
                match child.id {
                    el::TARGETS => {
                        for t in ebml::Slice::new(child.data, self.caps).children() {
                            if t.id == el::TAGTRACKUID {
                                track_uid = ebml::as_uint(t.data).unwrap_or(0);
                            }
                        }
                    }
                    el::SIMPLETAG => {
                        self.flatten_simple_tag(child.data, "", &mut pairs, 0);
                    }
                    _ => {}
                }
            }
            // A tag with no target is container metadata; one targeting a track
            // becomes that stream's. Chapter- and attachment-targeted tags are
            // read but not attached to anything yet.
            let target = (track_uid != 0)
                .then(|| self.tracks.iter().find(|t| t.uid == track_uid))
                .flatten()
                .map(|t| t.stream_index);
            match target.and_then(|i| self.streams.get_mut(i as usize)) {
                Some(stream) => {
                    for (k, v) in pairs {
                        push_meta(&mut stream.metadata, &k, &v);
                    }
                }
                None if track_uid == 0 => {
                    for (k, v) in pairs {
                        push_meta(&mut self.metadata, &k, &v);
                    }
                }
                None => {}
            }
        }
    }

    /// Flatten a `SimpleTag` tree into `PARENT/CHILD` keys.
    ///
    /// Recursion is bounded by [`ebml::MAX_DEPTH`]; `SimpleTag` is one of the
    /// two recursive elements in the schema, so a file can nominate any depth.
    fn flatten_simple_tag(
        &self,
        body: &[u8],
        prefix: &str,
        out: &mut Vec<(String, String)>,
        depth: u8,
    ) {
        if depth >= ebml::MAX_DEPTH {
            return;
        }
        let mut name = String::new();
        let mut value: Option<String> = None;
        let mut nested: Vec<&[u8]> = Vec::new();
        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::TAGNAME => ebml::as_str(child.data)
                    .unwrap_or_default()
                    .clone_into(&mut name),
                el::TAGSTRING => value = ebml::as_str(child.data).map(str::to_owned),
                el::SIMPLETAG => nested.push(child.data),
                _ => {}
            }
        }
        if name.is_empty() {
            return;
        }
        let key = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if let Some(v) = value {
            out.push((key.clone(), v));
        }
        for n in nested {
            self.flatten_simple_tag(n, &key, out, depth.saturating_add(1));
        }
    }

    // ------------------------------------------------------------ Chapters

    fn parse_chapters(&mut self, body: &[u8]) {
        // Multiple editions exist; the first is used and the rest ignored
        // (plan 18 section 3.2.4 step 13). Ordered chapters are not honoured.
        let Some(edition) = ebml::Slice::new(body, self.caps)
            .children()
            .find(|c| c.id == el::EDITIONENTRY)
        else {
            return;
        };
        let mut next_id = 1i64;
        for atom in ebml::Slice::new(edition.data, self.caps).children() {
            if atom.id != el::CHAPTERATOM {
                continue;
            }
            let mut start = 0u64;
            let mut end = 0u64;
            let mut uid = 0u64;
            let mut title: Option<String> = None;
            for child in ebml::Slice::new(atom.data, self.caps).children() {
                match child.id {
                    el::CHAPTERUID => uid = ebml::as_uint(child.data).unwrap_or(0),
                    el::CHAPTERTIMESTART => start = ebml::as_uint(child.data).unwrap_or(0),
                    el::CHAPTERTIMEEND => end = ebml::as_uint(child.data).unwrap_or(0),
                    el::CHAPTERDISPLAY => {
                        for d in ebml::Slice::new(child.data, self.caps).children() {
                            if d.id == el::CHAPSTRING {
                                title = ebml::as_str(d.data).map(str::to_owned);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let mut chapter = Chapter {
                // The reference numbers chapters from 1 when the file's own UID
                // does not fit, which is what a UID of 0 means.
                id: if uid == 0 {
                    next_id
                } else {
                    i64::try_from(uid).unwrap_or(next_id)
                },
                // RFC 9559 section 5.1.7.1.4.3: chapter timestamps are in
                // nanoseconds, not in TimestampScale ticks.
                time_base: NS_BASE,
                start: Timestamp::new(i64::try_from(start).unwrap_or(i64::MAX)),
                end: Timestamp::new(i64::try_from(end).unwrap_or(i64::MAX)),
                metadata: Vec::new(),
            };
            if let Some(t) = title {
                push_meta(&mut chapter.metadata, "title", &t);
            }
            next_id = next_id.saturating_add(1);
            self.chapters.push(chapter);
        }
    }

    // ------------------------------------------------------------ Attachments

    fn parse_attachments(&mut self, body: &[u8], opts: &FormatOptions) -> Result<()> {
        for file in ebml::Slice::new(body, self.caps).children() {
            if file.id != el::ATTACHEDFILE {
                continue;
            }
            let mut filename = String::new();
            let mut mime = String::new();
            for child in ebml::Slice::new(file.data, self.caps).children() {
                match child.id {
                    el::FILENAME => {
                        ebml::as_str(child.data)
                            .unwrap_or_default()
                            .clone_into(&mut filename);
                    }
                    el::FILEMEDIATYPE => {
                        ebml::as_str(child.data)
                            .unwrap_or_default()
                            .clone_into(&mut mime);
                    }
                    _ => {}
                }
            }
            self.check_stream_room(opts)?;
            let index = self.streams.len() as u32;
            let mut stream = Stream::new(index, MediaType::Attachment, self.time_base);
            if !filename.is_empty() {
                push_meta(&mut stream.metadata, "filename", &filename);
            }
            if !mime.is_empty() {
                push_meta(&mut stream.metadata, "mimetype", &mime);
            }
            self.streams.push(stream);
        }
        Ok(())
    }

    // ------------------------------------------------------------ finishing

    fn finish_streams(&mut self, _opts: &FormatOptions) {
        let scale = i64::try_from(self.timestamp_scale).unwrap_or(1_000_000);
        for stream in &mut self.streams {
            stream.time_base = self.time_base;
        }
        for track in &mut self.tracks {
            // `delay_ticks` still holds the raw nanosecond `CodecDelay`; convert
            // it once, now that the time base is settled. **Measured** against
            // the reference: the delay is rounded to the nearest tick *first*
            // and then subtracted from the integer block timestamp. An MP3 track
            // with `CodecDelay` 25 056 689 ns on a 1 ms base reports its first
            // packet at -25; converting in the nanosecond domain and flooring
            // would give -26, and the reference prints -25.
            track.delay_ticks =
                rescale_rnd(track.delay_ticks, 1, scale, Rounding::NearestAwayFromZero)
                    .unwrap_or(0);
            let Some(stream) = self
                .streams
                .get_mut(usize::try_from(track.stream_index).unwrap_or(usize::MAX))
            else {
                continue;
            };
            if let Some(audio) = stream.params.audio.as_mut() {
                audio.initial_padding = track.delay_samples;
            }
        }
    }

    // ------------------------------------------------------------ packets

    /// Advance the parse by exactly one element, queueing any packets it holds.
    ///
    /// Contract: every call either consumes at least one octet of input or sets
    /// `eof`. That is what makes [`Demuxer::read_packet`]'s loop terminate on
    /// any input at all.
    fn advance(&mut self) -> Result<()> {
        if self.eof {
            return Err(Error::Eof);
        }
        let pos = self.io.pos();
        if self.segment_end.is_some_and(|e| pos >= e) && !self.in_cluster {
            self.eof = true;
            return Err(Error::Eof);
        }
        self.stack.close_finished(pos);
        if self.stack.is_empty() {
            self.in_cluster = false;
        }
        let Some(header) = ebml::read_header(&mut self.io, self.caps)? else {
            self.eof = true;
            return Err(Error::Eof);
        };
        // RFC 8794 section 6.2: does this element close anything unknown-sized?
        // Closing costs no input, so it is done here rather than by returning —
        // `advance` must always consume at least one octet.
        if let Some(n) = self.stack.terminations_for(header.id)
            && n > 0
        {
            self.stack.truncate_by(n);
            if self.stack.is_empty() {
                self.in_cluster = false;
            }
        }
        self.handle_element(&header)
    }

    fn handle_element(&mut self, header: &ebml::Header) -> Result<()> {
        match header.id {
            el::CLUSTER => {
                self.stack.clear();
                self.stack.push(el::CLUSTER, header.end())?;
                self.in_cluster = true;
                self.cluster_timestamp = 0;
                self.cluster_pos = header.pos;
                Ok(())
            }
            el::TIMESTAMP if self.in_cluster => {
                let size = Self::bounded_size(header, 8)?;
                let body = self.read_body(size, "matroska_cluster_timestamp")?;
                self.cluster_timestamp = ebml::as_uint(&body)
                    .and_then(|v| i64::try_from(v).ok())
                    .unwrap_or(0);
                Ok(())
            }
            el::BLOCKGROUP if self.in_cluster => {
                let size = Self::bounded_size(header, MAX_BLOCK)?;
                let data_pos = header.data_pos;
                let body = self.read_body(size, "matroska_block_group")?;
                self.parse_block_group(&body, data_pos)
            }
            el::SIMPLEBLOCK if self.in_cluster => {
                let size = Self::bounded_size(header, MAX_BLOCK)?;
                let data_pos = header.data_pos;
                let body = self.read_body(size, "matroska_block")?;
                self.emit_block(&body, data_pos, BlockContext::simple())
            }
            el::EBML | el::SEGMENT => {
                // A second document or segment: we read the first only.
                self.eof = true;
                Err(Error::Eof)
            }
            _ => {
                let Some(size) = header.size.known() else {
                    // An unknown-size element that is not a Cluster is
                    // unrecoverable: nothing tells us where it ends.
                    self.eof = true;
                    return Err(Error::InvalidData(
                        "unknown-size element in the packet path",
                    ));
                };
                self.io.skip(size)?;
                Ok(())
            }
        }
    }

    fn bounded_size(header: &ebml::Header, cap: u64) -> Result<u64> {
        let size = header
            .size
            .known()
            .ok_or(Error::InvalidData("block element is of unknown size"))?;
        if size > cap {
            return Err(Error::LimitExceeded {
                limit: "matroska_block",
                requested: size,
                cap,
            });
        }
        Ok(size)
    }

    fn parse_block_group(&mut self, body: &[u8], data_pos: u64) -> Result<()> {
        let mut block: Option<(usize, usize)> = None;
        let mut duration: Option<u64> = None;
        let mut discard_padding: Option<i64> = None;
        let mut has_reference = false;
        for child in ebml::Slice::new(body, self.caps).children() {
            match child.id {
                el::BLOCK => block = Some((child.data_offset, child.data.len())),
                el::BLOCKDURATION => duration = ebml::as_uint(child.data),
                el::DISCARDPADDING => discard_padding = ebml::as_int(child.data),
                el::REFERENCEBLOCK => has_reference = true,
                _ => {}
            }
        }
        let Some((offset, len)) = block else {
            return Ok(());
        };
        let Some(block) = body.get(offset..).and_then(|b| b.get(..len)) else {
            return Ok(());
        };
        let block = block.to_vec();
        // `Packet::pos` is the block element's *data* offset — measured against
        // the reference, which reports 87 232 for a `Block` whose enclosing
        // `BlockGroup` data starts at 87 229 and whose own header is three
        // octets long.
        let block_pos = data_pos.saturating_add(offset as u64);
        // A `Block` carries no keyframe bit; RFC 9559 section 10.4 makes the
        // absence of `ReferenceBlock` the random-access signal instead.
        self.emit_block(
            &block,
            block_pos,
            BlockContext {
                simple: false,
                keyframe: !has_reference,
                block_duration: duration,
                discard_padding,
            },
        )
    }

    fn emit_block(&mut self, data: &[u8], data_pos: u64, ctx: BlockContext) -> Result<()> {
        // A corrupt block is skipped, not fatal: the rest of the cluster is
        // still readable and that is what makes a damaged file play.
        let Ok(header) = block::parse_header(data, ctx.simple) else {
            return Ok(());
        };
        let keyframe = if ctx.simple {
            header.keyframe
        } else {
            ctx.keyframe
        };
        let block_duration = ctx.block_duration;
        let discard_padding = ctx.discard_padding;
        let Some(track_idx) = self.tracks.iter().position(|t| t.number == header.track) else {
            // Blocks naming a track that `Tracks` never declared are dropped
            // (plan 18 section 3.2.6).
            return Ok(());
        };
        let Some(track) = self.tracks.get(track_idx) else {
            return Ok(());
        };
        if track.droppable {
            return Ok(());
        }
        let stream_index = track.stream_index;
        let delay_ticks = track.delay_ticks;
        let default_duration_ns = track.default_duration_ns;
        let sample_rate = track.sample_rate;
        let emitted_delay = track.emitted_delay;
        let delay_samples = track.delay_samples;
        let lacing = header.lacing;

        let Ok(frames) = block::frames(data, &header) else {
            return Ok(());
        };
        let count = frames.len();
        let base_ts = self
            .cluster_timestamp
            .saturating_add(i64::from(header.rel_timestamp))
            .saturating_sub(delay_ticks);
        // RFC 9559 section 10.3.5: the block timestamp applies to the first
        // frame of a lace and the rest are contiguous, so they are spaced by
        // `DefaultDuration` when the track states one and by `BlockDuration`
        // divided by the frame count when it does not. Rule MKV-L1.
        let step_ticks = if lacing == Lacing::None || count <= 1 {
            0
        } else if let Some(ns) = default_duration_ns {
            self.ns_to_ticks(i64::try_from(ns).unwrap_or(0))
        } else {
            block_duration
                .and_then(|d| i64::try_from(d).ok())
                .and_then(|d| d.checked_div(i64::try_from(count).unwrap_or(1)))
                .unwrap_or(0)
        };

        for (i, frame) in frames.iter().enumerate() {
            let Some(bytes) = data.get(frame.offset..).and_then(|b| b.get(..frame.len)) else {
                continue;
            };
            let payload = self.decode_frame(track_idx, bytes)?;
            let mut pkt = Packet::from_slice(&mut self.budget, &payload)?;
            pkt.stream_index = stream_index;
            pkt.pos = Some(data_pos);
            pkt.flags = if keyframe {
                PacketFlags::KEY
            } else {
                PacketFlags::empty()
            };
            if header.discardable {
                pkt.flags |= PacketFlags::DISCARD;
            }
            let offset_ticks = i64::try_from(i).unwrap_or(0).saturating_mul(step_ticks);
            pkt.pts = Timestamp::new(base_ts.saturating_add(offset_ticks));
            pkt.duration = self.packet_duration(default_duration_ns, block_duration, count);
            if i == 0 && !emitted_delay && delay_samples > 0 {
                pkt.set_side_data(PacketSideData::SkipSamples {
                    start: delay_samples,
                    end: 0,
                });
            }
            if i + 1 == count
                && let Some(pad) = discard_padding.filter(|&p| p > 0)
                && sample_rate > 0
            {
                let samples = rescale_rnd(
                    pad,
                    i64::from(sample_rate),
                    NS,
                    Rounding::NearestAwayFromZero,
                )
                .unwrap_or(0);
                // Only the side data: `BlockDuration` is already the trimmed
                // length. Measured — the reference reports duration 7 for the
                // block whose BlockDuration is 7 and whose DiscardPadding is
                // 13 500 000 ns, so subtracting the padding again would halve it.
                pkt.set_side_data(PacketSideData::SkipSamples {
                    start: 0,
                    end: u32::try_from(samples).unwrap_or(0),
                });
            }
            if keyframe && i == 0 {
                self.index
                    .add(IndexEntry::keyframe(self.cluster_pos, pkt.pts));
            }
            self.queue.push_back(pkt);
        }
        if let Some(t) = self.tracks.get_mut(track_idx)
            && delay_samples > 0
        {
            t.emitted_delay = true;
        }
        Ok(())
    }

    /// Nanoseconds into ticks of this segment's `TimestampScale`.
    fn ns_to_ticks(&self, ns: i64) -> i64 {
        rescale_rnd(
            ns,
            1,
            i64::try_from(self.timestamp_scale).unwrap_or(1_000_000),
            Rounding::NearestAwayFromZero,
        )
        .unwrap_or(0)
    }

    fn packet_duration(
        &self,
        default_duration_ns: Option<u64>,
        block_duration: Option<u64>,
        count: usize,
    ) -> Duration {
        // Quantised to the stream time base *before* becoming microseconds.
        // Measured: an MP3 track whose DefaultDuration is 26 122 448 ns reports
        // duration 26 ticks / 0.026000 s, not the 0.026122 s the nanosecond
        // value carries — the reference rescales into the time base once and
        // everything downstream reads the tick count.
        if let Some(ns) = default_duration_ns {
            let ticks = self.ns_to_ticks(i64::try_from(ns).unwrap_or(i64::MAX));
            return Duration::from_micros(ticks_to_micros(ticks, self.timestamp_scale));
        }
        if let Some(ticks) = block_duration {
            let ticks = i64::try_from(ticks).unwrap_or(i64::MAX);
            let count = i64::try_from(count.max(1)).unwrap_or(1);
            let per = ticks.checked_div(count).unwrap_or(0);
            return Duration::from_micros(ticks_to_micros(per, self.timestamp_scale));
        }
        Duration::ZERO
    }

    /// Undo the track's content encodings, innermost first.
    fn decode_frame(&mut self, track_idx: usize, bytes: &[u8]) -> Result<Vec<u8>> {
        let Some(track) = self.tracks.get(track_idx) else {
            return Ok(bytes.to_vec());
        };
        if track.encodings.is_empty() {
            return Ok(bytes.to_vec());
        }
        let encodings = track.encodings.clone();
        let mut data = bytes.to_vec();
        for enc in encodings {
            match enc {
                Encoding::Zlib => {
                    self.budget.check(data.len() as u64)?;
                    data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(
                        &data,
                        usize::try_from(self.budget.available().min(MAX_BLOCK)).unwrap_or(0),
                    )
                    .map_err(|_| Error::InvalidData("zlib content encoding did not inflate"))?;
                    self.budget.charge(data.len() as u64)?;
                }
                Encoding::HeaderStrip(prefix) => {
                    self.budget.check((prefix.len() + data.len()) as u64)?;
                    let mut out = prefix.clone();
                    out.extend_from_slice(&data);
                    data = out;
                }
                Encoding::Unsupported => {
                    return Err(Error::Unsupported("matroska content encoding"));
                }
            }
        }
        Ok(data)
    }

    // ------------------------------------------------------------ seeking

    fn seek_timestamp(&mut self, ts: Timestamp, flags: SeekFlags) -> Result<()> {
        let seekable = self.io.seekability() != Seekability::None;
        if !seekable {
            return Err(Error::NotSeekable);
        }
        let strategy = SeekStrategy::choose(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts,
            },
            flags,
            FLAGS,
            !self.index.is_empty(),
            seekable,
        );
        let pos = match strategy {
            SeekStrategy::Index => self
                .index
                .search(ts, flags)
                .map(|e| e.pos)
                .ok_or(Error::NotSeekable)?,
            // Without cues, restarting from the first cluster is correct if
            // coarse: every cluster begins with a keyframe in practice, and the
            // caller discards packets before the target.
            SeekStrategy::BinarySearch | SeekStrategy::Byte => {
                self.first_cluster.ok_or(Error::NotSeekable)?
            }
            SeekStrategy::Unsupported => return Err(Error::NotSeekable),
        };
        self.io.seek(pos)?;
        self.reset_stream_state();
        Ok(())
    }

    fn reset_stream_state(&mut self) {
        self.stack.clear();
        self.in_cluster = false;
        self.cluster_timestamp = 0;
        self.cluster_pos = 0;
        self.queue.clear();
        self.eof = false;
    }
}

impl Demuxer for MatroskaDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        loop {
            if let Some(pkt) = self.queue.pop_front() {
                return Ok(pkt);
            }
            self.advance()?;
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        match target {
            SeekTarget::Byte(pos) => {
                if self.io.seekability() == Seekability::None {
                    return Err(Error::NotSeekable);
                }
                self.io.seek(pos.max(self.segment_data_pos))?;
                self.reset_stream_state();
                Ok(())
            }
            SeekTarget::Timestamp { stream_index, ts } => {
                let Some(stream) = self
                    .streams
                    .get(usize::try_from(stream_index).unwrap_or(usize::MAX))
                else {
                    return Err(Error::InvalidData("seek names an unknown stream"));
                };
                let _ = stream;
                self.seek_timestamp(ts, flags)
            }
            SeekTarget::Frame { stream_index, .. } => {
                let Some(stream) = self
                    .streams
                    .get(usize::try_from(stream_index).unwrap_or(usize::MAX))
                else {
                    return Err(Error::InvalidData("seek names an unknown stream"));
                };
                let rate = stream
                    .params
                    .video
                    .as_ref()
                    .map_or(Rational::ZERO, |v| v.frame_rate);
                let tb = stream.time_base;
                match target.resolve_frames(rate, tb)? {
                    SeekTarget::Timestamp { ts, .. } => self.seek_timestamp(ts, flags),
                    _ => Err(Error::Unsupported("unresolved frame seek")),
                }
            }
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }
}

// ------------------------------------------------------------------ helpers

/// The stream time base for a `TimestampScale` in nanoseconds per tick.
fn time_base_for(scale_ns: u64) -> Option<Rational> {
    let num = i32::try_from(scale_ns).ok()?;
    let tb = Rational::new(num, 1_000_000_000).checked_reduced()?;
    tb.is_defined().then_some(tb)
}

/// `Info\Duration` is a float in `TimestampScale` ticks (RFC 9559 §5.1.2.10).
fn duration_from_ticks(ticks: f64, scale_ns: u64) -> Option<Duration> {
    if !ticks.is_finite() || ticks < 0.0 {
        return None;
    }
    // DD3: convert once, deterministically, half away from zero.
    let rounded = ticks.round();
    if rounded > 9.0e18 {
        return None;
    }
    let micros = rescale_rnd(
        rounded as i64,
        i64::try_from(scale_ns).ok()?,
        1000,
        Rounding::NearestAwayFromZero,
    )?;
    Some(Duration::from_micros(micros))
}

fn ticks_to_micros(ticks: i64, scale_ns: u64) -> i64 {
    rescale_rnd(
        ticks,
        i64::try_from(scale_ns).unwrap_or(1_000_000),
        1000,
        Rounding::NearestAwayFromZero,
    )
    .unwrap_or(0)
}

/// What the enclosing element says about a block that the block itself cannot.
#[derive(Debug, Clone, Copy)]
struct BlockContext {
    /// Whether this is a `SimpleBlock`, which changes the flags layout.
    simple: bool,
    /// For a `Block`, whether the `BlockGroup` lacked a `ReferenceBlock`.
    keyframe: bool,
    block_duration: Option<u64>,
    discard_padding: Option<i64>,
}

impl BlockContext {
    const fn simple() -> Self {
        Self {
            simple: true,
            keyframe: false,
            block_duration: None,
            discard_padding: None,
        }
    }
}

/// Collect `SeekID`/`SeekPosition` pairs from a `SeekHead`.
///
/// The position is relative to the `Segment`'s data start (RFC 9559 §16), and it
/// is recorded unvalidated here — [`MatroskaDemuxer::follow_seek_head`] proves
/// each one by reading the element it claims to point at.
fn parse_seek_head(body: &[u8], caps: ebml::Caps, out: &mut Vec<(u32, u64)>) {
    for seek in ebml::Slice::new(body, caps).children() {
        if seek.id != el::SEEK {
            continue;
        }
        let mut id = None;
        let mut pos = None;
        for child in ebml::Slice::new(seek.data, caps).children() {
            match child.id {
                // `SeekID` is the target's element ID stored as binary, marker
                // included, so it decodes exactly like one read from the stream.
                el::SEEKID => {
                    id = ebml::read_id(child.data, ebml::MAX_ID_LEN)
                        .ok()
                        .map(|(v, _)| v);
                }
                el::SEEKPOSITION => pos = ebml::as_uint(child.data),
                _ => {}
            }
        }
        if let (Some(id), Some(pos)) = (id, pos) {
            out.push((id, pos));
        }
    }
}

/// RFC 9559 section 5.1.4.1.3.
fn media_of(track_type: u64) -> Option<MediaType> {
    match track_type {
        1 => Some(MediaType::Video),
        2 => Some(MediaType::Audio),
        17 => Some(MediaType::Subtitle),
        3 | 16 | 18 | 32 | 33 => Some(MediaType::Data),
        _ => None,
    }
}

fn clamp_dim(v: Option<u64>) -> u32 {
    v.and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}

/// The sample aspect ratio implied by a display size in pixels.
fn sar(display_w: u64, display_h: u64, width: u64, height: u64) -> Rational {
    let num = display_w.saturating_mul(height);
    let den = display_h.saturating_mul(width);
    let (Ok(num), Ok(den)) = (i32::try_from(num), i32::try_from(den)) else {
        return Rational::ONE;
    };
    Rational::new(num, den)
        .checked_reduced()
        .unwrap_or(Rational::ONE)
}

/// `DefaultDuration` is nanoseconds per frame, so the rate is its reciprocal.
fn frame_rate_from_duration(ns: u64) -> Rational {
    let Ok(ns) = i32::try_from(ns) else {
        return Rational::UNDEFINED;
    };
    Rational::new(1_000_000_000, ns)
        .checked_reduced()
        .unwrap_or(Rational::UNDEFINED)
}

/// RFC 9559 sections 5.1.4.1.30.8 and .9: 0 unspecified, 1 collocated with the
/// luma sample, 2 halfway between.
fn chroma_siting(horz: u64, vert: u64) -> ChromaLocation {
    match (horz, vert) {
        (1, 1) => ChromaLocation::TopLeft,
        (1, 2) => ChromaLocation::Left,
        (2, 1) => ChromaLocation::Top,
        (2, 2) => ChromaLocation::Center,
        _ => ChromaLocation::Unspecified,
    }
}

/// Append `key = value`, preserving both insertion order and duplicates.
fn push_meta(into: &mut Vec<(String, String)>, key: &str, value: &str) {
    into.push((key.to_owned(), value.to_owned()));
}
