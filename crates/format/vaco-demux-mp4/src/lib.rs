//! The MP4 / MOV / 3GP / fragmented-MP4 demuxer.
//!
//! This crate turns a file into streams and packets. It is **not** the box
//! parser — [`vaco_format_isom`] owns the box grammar, the sample tables, the
//! edit list, the fragment boxes and the four-character-code tables, and
//! nothing here re-parses what that crate already parses. What is here is
//! demuxing policy: which tracks become streams, what numbers they report, in
//! what order packets come out, and where a seek lands.
//!
//! ```no_run
//! use vaco_format_core::{Demuxer, discovery::NoParsers, FormatOptions};
//! use vaco_io::{MediaSource, MemorySource};
//!
//! let bytes = std::fs::read("clip.mp4")?;
//! let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
//! let mut demux = vaco_demux_mp4::Mp4Demuxer::open(
//!     src,
//!     &NoParsers,
//!     &FormatOptions::default(),
//!     vaco_demux_mp4::Mp4Options::default(),
//! )?;
//! for s in demux.streams() {
//!     println!("{} {:?}", s.index, s.params.codec_id);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # The fields `ffprobe` prints, and where each comes from
//!
//! Everything below was **measured** against `ffprobe 8.1` on files written by
//! `ffmpeg 8.1`, per plan 13 §1b — the commands are in
//! `docs/format/vaco-demux-mp4.md`. Three of them contradict what the plans and
//! the box layer say, which is why they are stated here rather than inferred:
//!
//! | Field | Rule |
//! |---|---|
//! | `duration_ts` | `min(non-empty `elst` total, `min(mdhd.duration, Σ sample durations)`)`. **Not** `mdhd.duration`, and not the `elst` alone. |
//! | `bit_rate` | `Σ sample sizes × 8 × timescale / min(mdhd.duration, Σ sample durations)`, truncated. The divisor is *not* `duration_ts`. |
//! | `start_pts` | the edit list's leading empty-edit offset when there is an edit list; the first sample's presentation time when there is not. |
//! | timestamp shift | `pts = dts_raw + ctts + shift`, `dts = dts_raw + dts_shift + shift`, where `shift = empty_offset − max(media_time, min PTS)`. The `max` is the part no specification states. |
//!
//! # Bounding a uniform `stsz`
//!
//! `vaco-format-isom` left exactly one gap for its caller: a uniform `stsz` has
//! no payload to clamp its declared count against, so twelve bytes can declare
//! four billion samples. [`read::sample_limit`] closes it with the source's own
//! size rather than a magic number — distinct samples occupy disjoint byte
//! ranges, so an `n`-byte file holds at most `n` samples.

#![forbid(unsafe_code)]

mod items;
mod meta;
mod options;
mod read;
mod track;

pub use options::{DecryptionKey, Mp4Options};

use std::collections::VecDeque;

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Error, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_format_core::seek::{IndexEntry, PacketIndex, SeekFlags, SeekStrategy, SeekTarget};
use vaco_format_core::{
    Chapter, Demuxer, DemuxerDesc, Disposition, FormatFlags, FormatOptions, ParserProvider,
    ProbeData, ProbeScore, Stream, StreamGroup,
};
use vaco_format_isom::boxes::{BoxHeader, IsoBox};
use vaco_format_isom::fourcc::boxes as bt;
use vaco_format_isom::frag::{
    MovieFragment, SegmentIndex, TrackExtends, TrackFragmentRandomAccess, parse_mfra,
};
use vaco_format_isom::scan::{BoxSpan, ScanError, TopLevelScanner};
use vaco_format_isom::stbl::SampleTable;
use vaco_format_isom::{FileType, FourCc, Movie, Track, probe as isom_probe, stsd};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags, PacketSideData};

use read::{FragEntry, Pending, Reader, Source};
use track::MediaTotals;

/// The registry name of this family. One component, six spellings.
pub const FORMAT_NAME: &str = vaco_format_isom::FORMAT_NAME;
/// The long name `ffprobe` prints as `format_long_name`.
pub const FORMAT_LONG_NAME: &str = vaco_format_isom::FORMAT_LONG_NAME;

/// Largest `moov` this demuxer will hold in memory.
///
/// The whole box is resident for the life of the demuxer because every sample
/// table borrows from it. Sixteen mebibytes is a `stbl` of about four million
/// samples, i.e. a thirty-hour recording.
pub const MAX_MOOV_BYTES: u64 = 16 << 20;

/// Largest top-level `meta` this demuxer will hold, for a HEIF/AVIF file
/// whose pictures are items rather than tracks. The same bound as `moov`:
/// `iloc`/`ipma` entries are a few bytes each, and `idat` holds only small
/// derived-item descriptors in every file this crate has seen.
pub const MAX_META_BYTES: u64 = MAX_MOOV_BYTES;

/// Largest `ftyp` this demuxer will hold.
///
/// A `ftyp` is a brand, a version and a list of compatible brands;
/// `vaco-format-isom` keeps at most 1024 of the latter, so four kibibytes is
/// already generous and this is sixteen times that.
pub const MAX_FTYP_BYTES: u64 = 64 << 10;

/// Largest `mdat` buffered for a source that cannot seek.
pub const MAX_BUFFERED_MDAT_BYTES: u64 = 64 << 20;

/// Read granularity for a payload whose declared size is not yet believed.
const READ_CHUNK: usize = 8 * 1024;

/// Largest number of top-level boxes inspected while scanning for fragments.
pub const MAX_TOP_LEVEL_BOXES: u32 = 1 << 20;

/// Largest number of movie fragments collected.
pub const MAX_FRAGMENTS: usize = 1 << 17;

/// Largest number of `sidx` boxes collected between `moov` and the first
/// `moof`.
///
/// Each is small — `reference_count` is sixteen bits — so this bounds count
/// rather than bytes; a real DASH-style file has exactly one.
pub const MAX_SIDX_BOXES: usize = 4096;

/// Largest `sidx` payload read.
///
/// `reference_count` is 16 bits, so the whole reference table is at most
/// `65535 * 12` bytes; this leaves generous room for the fixed header on top
/// without trusting the declared box size any further than that.
pub const MAX_SIDX_BYTES: u64 = 1 << 20;

/// Largest number of top-level `pssh` boxes collected beside `moof`
/// (ISO/IEC 23001-7 §8.1's fragmented-file location — `moov`-level `pssh` is
/// collected separately, by the box layer's own whole-file scan).
pub const MAX_TOP_LEVEL_PSSH: usize = 256;

/// Largest top-level `pssh` payload read. `pssh`'s `Data` field carries a
/// DRM system's opaque init data, which real files keep small; this is a
/// generous bound, not a measured maximum.
pub const MAX_PSSH_BYTES: u64 = 1 << 20;

/// Fixed size of an `mfro` box: an 8-byte header, a 4-byte version/flags word
/// and the 4-byte `size` field (ISO/IEC 14496-12 §8.8.11). Unlike almost every
/// other box in the format, it never grows, which is what makes reading the
/// last sixteen bytes of the file a reliable way to find `mfra` without a
/// linear scan.
const MFRO_BOX_LEN: u64 = 16;

/// Samples examined when estimating a frame rate or the minimum presentation
/// time. The reference analyses a prefix too (`fpsprobesize`); this is ours.
pub const ANALYSE_SAMPLES: u32 = 4096;

/// How far apart two tracks' decode times may be before decode time, rather
/// than file position, decides which packet comes next.
///
/// One second, in microseconds. See [`Mp4Demuxer::pick`] for the measurement.
pub const INTERLEAVE_WINDOW_US: u64 = 1_000_000;

/// Time base of a chapter list, which `chpl` states in 100 ns units.
const CHAPTER_TIME_BASE: Rational = Rational {
    num: 1,
    den: 10_000_000,
};

/// Time base given to a cover-art stream, which has no timeline of its own.
///
/// **Measured**: `ffprobe` reports `time_base=1/90000` and
/// `r_frame_rate=90000/1` for a `covr` image.
const ATTACHED_PIC_TIME_BASE: Rational = Rational {
    num: 1,
    den: 90_000,
};

/// The descriptor a registry holds. Named by `vaco-component.toml`.
/// Behavioural flags, reachable through `DemuxerDesc::flags`.
///
/// MP4 states its own timestamps and has a real index, so none of the
/// discontinuity or generic-index flags apply. `SHOW_IDS` because a `trak`
/// carries a `track_ID` the reference prints, and `SEEK_TO_PTS` because the
/// sample tables are indexed by composition time.
///
/// `format_flags()` returns this; the const exists so `DemuxerDesc::flags` can
/// carry it, and the function delegates so there is one definition (D19).
pub const FLAGS: FormatFlags = FormatFlags::SEEK_TO_PTS.union(FormatFlags::SHOW_IDS);

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: FORMAT_NAME,
    long_name: FORMAT_LONG_NAME,
    extensions: isom_probe::EXTENSIONS,
    mime_types: isom_probe::MIME_TYPES,
    flags: FLAGS,
    probe,
    open: open_boxed,
};

/// What this container declares it can do.
///
/// Neither `GENERIC_INDEX` nor `NOBINSEARCH`: the demuxer seeks through the
/// sample tables itself, so the generic paths never run for a progressive file,
/// and bisection stays available for a fragmented one whose index was
/// discarded.
#[must_use]
pub fn format_flags() -> FormatFlags {
    FLAGS
}

/// Content probe, delegated to the box layer.
///
/// Kept as a one-line forward deliberately: the scoring was calibrated by
/// mutating a real file four ways and reading `probe_score` back, and having
/// two implementations of it is how they drift.
fn probe(data: &ProbeData<'_>) -> ProbeScore {
    isom_probe::probe(data)
}

fn open_boxed(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(Mp4Demuxer::open(
        src,
        parsers,
        &FormatOptions::default(),
        Mp4Options::default(),
    )?))
}

/// One movie fragment, held as bytes because the structures that read it
/// borrow them.
#[derive(Debug)]
struct Fragment {
    /// File offset of the `moof` box header.
    offset: u64,
    /// Its header length, 8 or 16.
    header_len: u64,
    /// Its payload.
    data: Vec<u8>,
}

/// Where a file's streams come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// A `moov` with tracks and sample tables.
    Movie,
    /// A HEIF/AVIF file: no `moov` at all, and the streams came from a
    /// top-level `meta` box's items (`items::build`). Nothing that reads
    /// `self.moov` runs for such a file.
    ImageItems,
}

/// The MP4 demuxer.
#[derive(Debug)]
pub struct Mp4Demuxer {
    io: IoContext,
    budget: Budget,
    /// Payload allocation is charged here and released immediately: the
    /// demuxer retains no packet, and a cumulative cap would otherwise refuse
    /// to read a file larger than the cap.
    packet_budget: Budget,
    mp4: Mp4Options,

    /// The `moov` payload. Every sample table borrows from it.
    moov: Vec<u8>,
    moov_offset: u64,
    moov_header_len: u64,
    movie_timescale: u32,
    fragmented: bool,
    layout: Layout,
    groups: Vec<StreamGroup>,
    /// `Movie::tracks` slot for each stream, or `None` for a cover image.
    slots: Vec<Option<usize>>,
    extends: Vec<TrackExtends>,

    fragments: Vec<Fragment>,
    /// `mfra ▸ tfra`, read from the file's trailer once at open, if present.
    ///
    /// [`Mp4Demuxer::place_fragment`] tries this fast path first — a direct
    /// lookup by presentation time instead of the linear scan over collected
    /// fragments that is the only option without it — falling back to that
    /// scan whenever the lookup does not resolve to something already known
    /// or fetchable.
    tfra: Vec<TrackFragmentRandomAccess>,
    /// `sidx` boxes seen between `moov` and the first `moof`.
    ///
    /// Not yet consulted for seeking — see the crate doc's *Deferred*
    /// section — but collected because the box-layer parse is free once the
    /// scan is already walking past them to find `moof`.
    sidx: Vec<SegmentIndex>,
    /// Where the top-level scan for further fragments has reached.
    scan_pos: u64,
    scan_end: u64,
    scan_done: bool,
    boxes_seen: u32,
    /// A buffered `mdat` for a source that cannot seek: (start, bytes).
    mdat_buf: Option<(u64, Vec<u8>)>,

    streams: Vec<Stream>,
    readers: Vec<Reader>,
    chapters: Vec<Chapter>,
    metadata: Vec<(String, String)>,
    duration: Option<Duration>,
    index: PacketIndex,
    eof: bool,
}

impl Mp4Demuxer {
    /// Open a source, read its `moov`, and build one [`Stream`] per track.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when there is no usable `moov`,
    /// [`Error::Unsupported`] when the `moov` follows the media data on a
    /// source that cannot seek, and [`Error::LimitExceeded`] when the `moov`
    /// exceeds [`MAX_MOOV_BYTES`] or the budget.
    pub fn open(
        src: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
        opts: &FormatOptions,
        mp4: Mp4Options,
    ) -> Result<Self> {
        let _ = parsers;
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());
        let seekable = io.seekability() != Seekability::None;
        let scan_end = io.size().unwrap_or(u64::MAX);

        let mut file_type: Option<FileType> = None;
        let mut moov_span: Option<BoxSpan> = None;
        let mut moov = Vec::new();
        let mut meta: Option<(BoxSpan, Vec<u8>)> = None;
        let mut saw_mdat = false;
        // One scanner per box rather than one for the walk, so that a payload
        // can be read as it is passed. On a source that cannot seek there is no
        // going back for it afterwards.
        let mut pos = io.pos();
        for _ in 0..MAX_TOP_LEVEL_BOXES {
            let span = {
                let mut sc = TopLevelScanner::range(&mut io, pos, scan_end);
                sc.next_box(&mut budget)?
            };
            let Some(span) = span else { break };
            pos = span.end();
            match span.kind {
                bt::FTYP | bt::STYP
                    if file_type.is_none() && span.payload_len() <= MAX_FTYP_BYTES =>
                {
                    let payload =
                        read_payload_incremental(&mut io, &mut budget, span, MAX_FTYP_BYTES)?;
                    file_type = Some(FileType::parse(&reassemble(span, &payload)));
                }
                bt::MOOV if moov_span.is_none() => {
                    if saw_mdat && !seekable {
                        return Err(Error::Unsupported(
                            "mp4: moov follows mdat and the source cannot seek; \
                             rewrite the file with -movflags +faststart",
                        ));
                    }
                    // Read **incrementally**: a declared box size is a claim,
                    // and believing it is the classic amplification. A
                    // fifteen-byte input declaring a huge `ftyp` asked for a
                    // 512 MiB allocation before this was two-phase, and the
                    // `dem_mp4_chunked` fuzz target found it in twenty-three
                    // executions — on a source that could not state its own
                    // size, so nothing else bounded the claim.
                    moov = read_payload_incremental(&mut io, &mut budget, span, MAX_MOOV_BYTES)?;
                    moov_span = Some(span);
                    break;
                }
                bt::MDAT => saw_mdat = true,
                // A HEIF/AVIF file has no `moov`; its pictures are items in a
                // top-level `meta`. Kept only until `moov` settles the
                // question — a `moov` file with a stray top-level `meta` is
                // read as tracks, exactly as before.
                bt::META if meta.is_none() && span.payload_len() <= MAX_META_BYTES => {
                    if saw_mdat && !seekable {
                        return Err(Error::Unsupported(
                            "mp4: meta follows mdat and the source cannot seek",
                        ));
                    }
                    let payload =
                        read_payload_incremental(&mut io, &mut budget, span, MAX_META_BYTES)?;
                    meta = Some((span, payload));
                }
                _ => {}
            }
        }
        let layout = if moov_span.is_none() && meta.is_some() {
            Layout::ImageItems
        } else {
            Layout::Movie
        };
        let (moov_span, moov) = match (moov_span, meta) {
            (Some(span), _) => (span, moov),
            (None, Some((span, payload))) => (span, payload),
            (None, None) => {
                return Err(Error::InvalidData(if saw_mdat {
                    ScanError::MoovAfterMediaData.message()
                } else {
                    ScanError::NoMovie.message()
                }));
            }
        };

        let mut me = Self {
            io,
            budget,
            packet_budget: Budget::new(Limits::permissive()),
            mp4,
            moov,
            moov_offset: moov_span.offset,
            moov_header_len: moov_span.header_len,
            movie_timescale: 0,
            fragmented: false,
            layout,
            groups: Vec::new(),
            slots: Vec::new(),
            extends: Vec::new(),
            fragments: Vec::new(),
            tfra: Vec::new(),
            sidx: Vec::new(),
            scan_pos: moov_span.end(),
            scan_end,
            scan_done: false,
            boxes_seen: 0,
            mdat_buf: None,
            streams: Vec::new(),
            readers: Vec::new(),
            chapters: Vec::new(),
            metadata: Vec::new(),
            duration: None,
            index: PacketIndex::with_options(opts),
            eof: false,
        };
        me.metadata = file_type
            .as_ref()
            .map(meta::file_type_tags)
            .unwrap_or_default();
        me.build(seekable)?;
        Ok(me)
    }

    // `duration_ts`, `frame_rates` and `display_matrix` used to live here as
    // inherent accessors over a private `TrackFacts` table, because `Stream`
    // could hold none of the three. They are `Stream::duration_ts`,
    // `Stream::r_frame_rate`/`avg_frame_rate` and
    // `Stream::side_data` now, so a caller holding a `dyn Demuxer` can reach
    // them — which is the whole point, since `DemuxerDesc::open` hands back a
    // trait object and `vaco-probe` reads `.streams()` off it.

    /// Whether the file is fragmented (`mvex` present).
    #[must_use]
    pub const fn is_fragmented(&self) -> bool {
        self.fragmented
    }

    /// The index built from the container's own tables.
    #[must_use]
    pub const fn index(&self) -> &PacketIndex {
        &self.index
    }

    /// The `mfra ▸ tfra` tables read from the file's trailer, if it had one.
    ///
    /// Empty for a fragmented file with no `mfra`, for a non-seekable source
    /// (nothing to seek back to read a trailer with), and for every
    /// non-fragmented file.
    #[must_use]
    pub fn fragment_random_access(&self) -> &[TrackFragmentRandomAccess] {
        &self.tfra
    }

    /// The `sidx` boxes seen between `moov` and the first `moof`.
    ///
    /// Collected because the scan that finds `moof` passes over them for
    /// free; not yet consulted by [`Mp4Demuxer::seek`] — see the crate doc's
    /// *Deferred* section.
    #[must_use]
    pub fn segment_index(&self) -> &[SegmentIndex] {
        &self.sidx
    }

    // ------------------------------------------------------------------ open

    fn build(&mut self, seekable: bool) -> Result<()> {
        if self.layout == Layout::ImageItems {
            return self.build_items();
        }
        // Two parses of the same bytes, deliberately. The first settles whether
        // the file is fragmented, which decides whether the `moof` chain has to
        // be walked; the second happens once that walk is done, because the
        // walk needs `&mut self` and a parsed `Movie` borrows `self.moov`.
        {
            let bx = moov_box(&self.moov, self.moov_offset, self.moov_header_len);
            let movie = Movie::parse(&bx)?;
            self.movie_timescale = movie.header.timescale;
            self.fragmented = movie.is_fragmented();
            self.extends = movie.extends.clone();
        }
        if self.fragmented {
            // Cheap regardless of file size — a fixed seek to the last
            // sixteen bytes and one more box read — so it happens before the
            // fragment scan can, rather than depending on the scan reaching
            // the end. A file with more fragments than `MAX_FRAGMENTS` is
            // exactly the case this exists for: the scan below gives up long
            // before `mfra`, so if this ran after it, it would never run.
            if seekable {
                self.tfra = self.read_mfra_trailer().unwrap_or_default();
            }
            self.collect_fragments(seekable)?;
        }

        let size = self.io.size();
        let (streams, readers, slots, found, qt_chapter_track, pssh_tags, timecode_tracks) = {
            let bx = moov_box(&self.moov, self.moov_offset, self.moov_header_len);
            let movie = Movie::parse(&bx)?;
            let mut streams = Vec::new();
            let mut readers = Vec::new();
            let mut slots = Vec::new();
            for (slot, trak) in movie.tracks.iter().enumerate() {
                let index = streams.len() as u32;
                if u64::from(index) >= u64::from(self.budget.limits().max_streams) {
                    break;
                }
                let Some((stream, reader)) = self.build_track(trak, slot, index, size) else {
                    continue;
                };
                streams.push(stream);
                readers.push(reader);
                slots.push(Some(slot));
            }
            // Metadata, chapters and cover art all live in `udta`, which the
            // box layer hands over unparsed.
            let found = movie
                .udta
                .as_ref()
                .map(|u| meta::parse_udta(u, !self.mp4.ignore_chapters));
            // A QuickTime chapter *track*: some other track's `tref ▸ chap`
            // names it, and its samples are the simple Apple `text`
            // sample format (a big-endian length then that many UTF-8 bytes).
            // Structural data only, extracted here because it borrows `movie`
            // — the actual sample bytes need `self.io`, which cannot be
            // borrowed at the same time, and are read once this block ends.
            let qt_chapter_track = (!self.mp4.ignore_chapters)
                .then(|| find_qt_chapter_track(&movie, size))
                .flatten();
            // `tmcd` tracks: structural only here, for the same reason as the
            // chapter track above — the sample byte needs `self.io`, which
            // cannot be borrowed while `movie` still is.
            let timecode_tracks = find_timecode_tracks(&movie);
            // `pssh` under `moov` is the progressive-file location (§8.1). A
            // fragmented file's copy is a top-level box next to `moof`, which
            // `collect_fragments` scans through the same `pssh_tags` helper.
            let pssh_tags: Vec<(String, String)> = movie.pssh.iter().flat_map(pssh_tags).collect();
            (
                streams,
                readers,
                slots,
                found,
                qt_chapter_track,
                pssh_tags,
                timecode_tracks,
            )
        };
        self.streams = streams;
        self.readers = readers;
        self.slots = slots;
        self.metadata.extend(pssh_tags);

        if let Some(found) = found {
            self.metadata.extend(found.tags);
            for (i, (start, title)) in found.chapters.iter().enumerate() {
                self.chapters.push(Chapter {
                    id: i64::try_from(i).unwrap_or(0),
                    time_base: CHAPTER_TIME_BASE,
                    start: Timestamp::new(*start),
                    end: Timestamp::NONE,
                    metadata: vec![("title".to_owned(), title.clone())],
                });
            }
            if let Some(cover) = found.cover {
                let index = self.streams.len() as u32;
                let (stream, reader) = cover_stream(index, cover);
                self.streams.push(stream);
                self.readers.push(reader);
                self.slots.push(None);
            }
        }
        // Nero `chpl` wins when both are present. Plan 18's VERIFY-M4 names
        // this as the deciding case and records it as unmeasured; it still is
        // — no reference file combining both was available this pass — so
        // this is an assumption, not a measurement, and is documented as one.
        if self.chapters.is_empty()
            && let Some((track_tb, samples)) = qt_chapter_track
        {
            self.load_qt_chapter_track(track_tb, &samples);
        }
        self.load_timecode_tracks(timecode_tracks);
        self.finish_durations();
        self.seed_index();
        Ok(())
    }

    /// Turn a `QuickTime` chapter track's samples into [`Chapter`]s.
    ///
    /// Each sample is read through [`Mp4Demuxer::payload`], the same path
    /// ordinary packets use, so it costs nothing extra to support: a
    /// non-seekable source simply yields no chapters from this path, the same
    /// way `payload` already refuses a backward read on one.
    fn load_qt_chapter_track(&mut self, track_tb: Rational, samples: &[(i64, u64, u32)]) {
        for (i, &(dts, offset, size)) in samples.iter().enumerate() {
            let Ok(mut pkt) = self.payload(offset, size) else {
                continue;
            };
            let bytes = pkt.payload_mut();
            let Some(len) = bytes.first_chunk::<2>().map(|b| u16::from_be_bytes(*b)) else {
                continue;
            };
            let text = bytes
                .get(2..2usize.saturating_add(usize::from(len)))
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let start = Timestamp::new(dts)
                .checked_rescale(track_tb, CHAPTER_TIME_BASE, Rounding::NearestAwayFromZero)
                .unwrap_or(Timestamp::new(dts));
            let end = samples
                .get(i.saturating_add(1))
                .and_then(|&(next_dts, ..)| {
                    Timestamp::new(next_dts).checked_rescale(
                        track_tb,
                        CHAPTER_TIME_BASE,
                        Rounding::NearestAwayFromZero,
                    )
                })
                .unwrap_or(Timestamp::NONE);
            self.chapters.push(Chapter {
                id: i64::try_from(i).unwrap_or(0),
                time_base: CHAPTER_TIME_BASE,
                start,
                end,
                metadata: vec![("title".to_owned(), text)],
            });
        }
    }

    /// Turn each `tmcd` track's first sample into a `timecode` tag.
    ///
    /// **Measured** (`ffmpeg -timecode 01:00:00:00`, real `.mov`): the tag
    /// lands on the `tmcd` track's own stream *and* on every other track
    /// whose `tref ▸ tmcd` names it — a video track carries the same
    /// `TAG:timecode` value the reference prints for its data stream.
    fn load_timecode_tracks(&mut self, tracks: Vec<TimecodeTrack>) {
        for t in tracks {
            let Ok(mut pkt) = self.payload(t.offset, t.size) else {
                continue;
            };
            let Some(count) = pkt
                .payload_mut()
                .first_chunk::<4>()
                .map(|b| u32::from_be_bytes(*b))
            else {
                continue;
            };
            let Some(text) = t.entry.format(count) else {
                continue;
            };
            let mut targets: Vec<i64> = t.referenced_by.iter().map(|&id| i64::from(id)).collect();
            targets.push(i64::from(t.track_id));
            for s in &mut self.streams {
                if s.id.is_some_and(|id| targets.contains(&id)) {
                    s.metadata.push(("timecode".to_owned(), text.clone()));
                }
            }
        }
    }

    /// Container duration: the largest `start_time + duration` over the
    /// streams. **Measured** — patching `mvhd.duration` to 5 s left
    /// `format.duration` at 2 s, so `mvhd` is not the source.
    /// The HEIF/AVIF path: `self.moov` holds the top-level `meta` box.
    fn build_items(&mut self) -> Result<()> {
        let size = self.io.size();
        let built = {
            let bx = reassemble_at(bt::META, self.moov_offset, self.moov_header_len, &self.moov);
            let (io, packet_budget) = (&mut self.io, &mut self.packet_budget);
            let mut read = |extents: &[(u64, u64)]| -> Option<Vec<u8>> {
                let mut out = Vec::new();
                for &(offset, len) in extents {
                    let n = usize::try_from(len).ok()?;
                    let mut pkt = Packet::alloc(packet_budget, n).ok()?;
                    packet_budget.release(len);
                    io.seek(offset).ok()?;
                    io.read_exact(pkt.payload_mut()).ok()?;
                    out.extend_from_slice(pkt.payload());
                }
                Some(out)
            };
            items::build(&bx, &self.budget, size, &mut read)
        };
        if built.streams.is_empty() {
            return Err(Error::InvalidData(
                "mp4: meta box holds no image item this demuxer can read",
            ));
        }
        self.slots = built.streams.iter().map(|_| None).collect();
        self.streams = built.streams;
        self.readers = built.readers;
        self.groups = built.groups;
        Ok(())
    }

    fn finish_durations(&mut self) {
        let mut best: Option<Duration> = None;
        for s in &self.streams {
            let Some(dur) = s.duration_ts else { continue };
            let start = s.start_time.ticks().unwrap_or(0);
            let end = Timestamp::new(start.saturating_add(dur)).to_duration(s.time_base);
            if let Some(end) = end {
                best = Some(best.map_or(end, |b| b.max(end)));
            }
        }
        self.duration = best;
        // A cover image has no timeline; the reference gives it the container's
        // duration in its own 1/90000 base.
        let total = self.duration;
        for s in &mut self.streams {
            if !s.is_attached_pic() {
                continue;
            }
            s.duration_ts = total.and_then(|d| attached_picture_duration_ticks(d, s.time_base));
        }
    }

    /// Record the first sample of every track so a seek to zero is exact even
    /// before any packet has been read.
    fn seed_index(&mut self) {
        for r in &self.readers {
            if let Some(e) = r.entries.first() {
                self.index.add(IndexEntry::keyframe(
                    e.first_offset,
                    Timestamp::new(e.start_dts),
                ));
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "one track, one field at a time")]
    fn build_track(
        &self,
        trak: &Track<'_>,
        slot: usize,
        index: u32,
        size: Option<u64>,
    ) -> Option<(Stream, Reader)> {
        // A zero timescale is a division by zero waiting to happen; plan 18
        // §3.1.10 drops the track.
        if !trak.has_usable_timescale() {
            return None;
        }
        // An unrecognized `hdlr` handler used to drop the whole track. A
        // corrupted `moov` that still carries a readable `stsd` sample entry
        // (measured against the reference) gets classified from that instead
        // — `codec_parameters` already falls back from `codec_id`'s own media
        // type when the handler does not resolve one, which this used to
        // foreclose by resolving `media_type` before that fallback could run.
        let handler_media_type = trak.media_type();
        let table = &trak.sample_table;

        let entries = table
            .sample_descriptions
            .as_ref()
            .and_then(|stsd| stsd::parse_stsd(stsd, trak.handler).ok())
            .unwrap_or_default();
        let entry = entries.first();

        let totals = if self.fragmented {
            self.fragment_totals(trak.header.track_id)
        } else {
            table_totals(table)
        };
        let limit = track::media_limit(trak.media.duration, &totals);
        let timescale = trak.media.timescale;

        let mut params = entry.map_or_else(
            || vaco_codec_core::CodecParameters::new(handler_media_type.unwrap_or(MediaType::Data)),
            |e| track::codec_parameters(e, handler_media_type, trak),
        );
        // Neither the handler nor the sample entry's own codec said what this
        // track is (both `handler_media_type` and `codec_parameters`'s own
        // codec-id fallback came up empty): salvaged as `Data` rather than
        // dropped, the same fallback the handler table already uses for a
        // *recognized* non-AV handler (`meta`, `tmcd`).
        let media_type = params.media_type.unwrap_or(MediaType::Data);
        params.media_type = Some(media_type);
        params.bit_rate = track::bit_rate(totals.bytes, timescale, limit, self.fragmented);
        if params.validate(&self.budget).is_err() {
            return None;
        }

        // The edit list.
        let (edit_shift, trim_point, start_pts, trim_end) = self.edit_shift(trak, table);
        let duration_ts = self.duration_ticks(trak, limit);

        let mut stream = track::shell(index, trak, media_type);
        stream.params = params;
        stream.disposition = track::disposition(trak);
        stream.start_time = Timestamp::new(start_pts);
        stream.duration_ts = duration_ts;
        stream.frame_count = (totals.count > 0).then_some(u64::from(totals.count));
        let vendor = entry.and_then(|_| vendor_id(table));
        let compressor = entry.and_then(|e| e.visual.as_ref().and_then(|v| v.compressor()));
        stream.metadata = track::track_metadata(trak, vendor, compressor);

        // Common Encryption (ISO/IEC 23001-7): report the scheme and key id.
        // `codec_name` already reads as the *original* codec via
        // `SampleEntry::effective_format`, which is `vaco-format-isom`'s job
        // and not repeated here.
        let cenc = entry
            .and_then(vaco_format_isom::stsd::SampleEntry::cenc)
            .filter(|c| !c.is_empty());
        if let Some(cenc) = &cenc {
            if let Some(scheme) = cenc.scheme {
                stream.metadata.push((
                    "encryption_scheme".to_owned(),
                    String::from_utf8_lossy(&scheme.scheme_type.as_bytes()).into_owned(),
                ));
            }
            if let Some(te) = cenc.track_encryption {
                stream
                    .metadata
                    .push(("encryption_key_id".to_owned(), hex16(&te.default_kid)));
                stream.metadata.push((
                    "encryption_iv_size".to_owned(),
                    te.per_sample_iv_size.to_string(),
                ));
            }
        }
        // Only literal `cenc` reaches AES-CTR. A version-1 `sgpd(seig)` plus
        // `sbgp` may replace the key and IV size sample by sample; progressive
        // tracks resolve it here, while fragmented tracks retain track-level
        // descriptions and resolve each `traf` during refill.
        let (decryptor, encryption_error) = match cenc.as_ref() {
            None => (None, None),
            Some(cenc) => match cenc.scheme.map(|scheme| scheme.scheme_type.as_bytes()) {
                Some(kind) if kind == *b"cenc" => match cenc.track_encryption {
                    None => (None, Some("mp4: cenc track is missing its tenc parameters")),
                    Some(defaults) => {
                        let result = if self.fragmented {
                            read::Decryptor::fragmented(
                                &self.mp4,
                                defaults,
                                &table.sample_group_descriptions,
                            )
                        } else {
                            read::Decryptor::progressive(&self.mp4, defaults, table)
                        };
                        match result {
                            Ok(decryptor) => (Some(decryptor), None),
                            Err(error) => (None, Some(error)),
                        }
                    }
                },
                Some(kind) if kind == *b"cens" => (
                    None,
                    Some("mp4: cens pattern-CTR decryption is not implemented"),
                ),
                Some(kind) if kind == *b"cbc1" => (
                    None,
                    Some("mp4: cbc1 AES-CBC decryption is not implemented"),
                ),
                Some(kind) if kind == *b"cbcs" => (
                    None,
                    Some("mp4: cbcs pattern-CBC decryption is not implemented"),
                ),
                Some(_) => (None, Some("mp4: unknown Common Encryption scheme")),
                None => (None, Some("mp4: protected sample entry has no schm scheme")),
            },
        };

        let (r_rate, avg_rate) = self.frame_rate_estimate(trak, table, &totals, limit);
        if media_type == MediaType::Video {
            stream.r_frame_rate = r_rate;
            stream.avg_frame_rate = avg_rate;
            if let Some(v) = stream.params.video.as_mut() {
                v.frame_rate = avg_rate;
            }
        }
        if let Some(matrix) = track::display_matrix(trak) {
            stream
                .side_data
                .push(vaco_format_core::StreamSideData::DisplayMatrix(matrix));
        }

        let source = if self.fragmented {
            Source::Fragments {
                entry: 0,
                next_in_entry: 0,
            }
        } else {
            Source::Table {
                slot,
                next: 0,
                limit: read::sample_limit(table.sample_count(), size),
            }
        };
        // An absent `dinf ▸ dref` is not an external reference: the box layer's
        // `DataReferences::default()` reports `all_self_contained = false`
        // because it has seen nothing, and a `trak` with no `dref` at all is
        // perfectly ordinary. Only a *declared* external entry is refused
        // (plan 18 §3.1.10 — following it is a file-system read triggered by
        // file content).
        let external =
            trak.data_references.count > 0 && !trak.is_self_contained() && !self.mp4.enable_drefs;
        let mut reader = Reader {
            stream_index: index,
            time_base: stream.time_base,
            media_type,
            dts_shift: table.dts_shift(),
            edit_shift,
            trim_point,
            trim_end,
            // Only for a codec whose decoder emits a fixed number of samples
            // per packet no matter what the container says. ALAC's final
            // frame is genuinely short — its own frame header carries the
            // count — so a short final `stts` there is the truth, not
            // padding, and trimming to it deleted 1912 real samples from
            // every `ffmpeg -c:a alac` file before this guard existed.
            // `fixed_frame_size` is the predicate; the *value* still comes
            // from `stts`, so a 960-sample AAC profile trims by 960 rather
            // than by the table's 1024.
            frame_samples: if media_type == MediaType::Audio
                && stream
                    .params
                    .codec_id
                    .and_then(vaco_codec_core::CodecId::fixed_frame_size)
                    .is_some()
            {
                common_delta(table)
            } else {
                0
            },
            source,
            entries: Vec::new(),
            queue: VecDeque::new(),
            batch: read::BATCH_MIN,
            finished: external,
            blocked: external,
            encryption_error,
            raw_pcm: media_type == MediaType::Audio
                && decryptor.is_none()
                && stream
                    .params
                    .codec_id
                    .is_some_and(|id| id.name().starts_with("pcm_")),
            decrypt: decryptor,
        };
        if self.fragmented {
            reader.entries = self.fragment_entries(trak.header.track_id, size);
            // Empty means two different things depending on whether
            // `collect_fragments` already ran to completion. On a seekable
            // source it did (`Mp4Demuxer::build` calls it before any track is
            // built), so an empty result here means the track truly has no
            // fragments anywhere in the file — finished. On a source that
            // cannot seek, `collect_fragments` never eagerly scans at all
            // (see its own doc comment), so an empty result here means only
            // "nothing pulled yet" — marking it finished would make every
            // fragmented track on a non-seekable source permanently empty,
            // since `ensure_head` checks `finished` before ever calling
            // `refill`, and `refill`'s own retry-by-pulling-one-more-`moof`
            // loop would then never run. Found by
            // `a_non_seekable_source_gets_no_fast_path_but_still_demuxes`.
            if reader.entries.is_empty() && self.io.seekability() != Seekability::None {
                reader.finished = true;
            }
        }
        Some((stream, reader))
    }

    /// `duration_ts`: the edit list, clamped by what the media actually holds.
    fn duration_ticks(&self, trak: &Track<'_>, limit: i64) -> Option<i64> {
        if limit <= 0 {
            return None;
        }
        if self.mp4.ignore_editlist {
            return Some(limit);
        }
        match trak.edits.as_ref() {
            Some(e) if !e.entries.is_empty() => {
                let played = i64::try_from(e.played_duration_movie()).unwrap_or(i64::MAX);
                let rescaled = vaco_format_isom::edit::rescale_movie_to_media(
                    played,
                    self.movie_timescale,
                    trak.media.timescale,
                );
                Some(rescaled.min(limit).max(0))
            }
            _ => Some(limit),
        }
    }

    /// The shift applied to every timestamp, the point before which samples are
    /// trimmed, and the value `start_pts` reports.
    ///
    /// The `max(media_time, min PTS)` in the middle is the part no
    /// specification states and the part plan 18's MP4-T1 got wrong. Measured
    /// by patching one file's `elst.media_time` to 0, 512, 1024 and 2048: the
    /// first three all produced `dts = -1024` and only the fourth followed the
    /// edit, and 1024 is that track's minimum raw presentation time.
    fn edit_shift(&self, trak: &Track<'_>, table: &SampleTable<'_>) -> (i64, i64, i64, i64) {
        let track_id = trak.header.track_id;
        if self.mp4.ignore_editlist {
            let first = self.first_presentation(table, track_id).unwrap_or(0);
            return (0, i64::MIN, first, i64::MAX);
        }
        let Some(edits) = trak.edits.as_ref().filter(|e| !e.entries.is_empty()) else {
            let first = self.first_presentation(table, track_id).unwrap_or(0);
            return (0, i64::MIN, first, i64::MAX);
        };
        let empty = vaco_format_isom::edit::rescale_movie_to_media(
            i64::try_from(edits.empty_offset_movie()).unwrap_or(i64::MAX),
            self.movie_timescale,
            trak.media.timescale,
        );
        let media_time = edits.initial_media_time().max(0);
        let min_pts = self.min_presentation(table, track_id).unwrap_or(0);
        let base = media_time.max(min_pts);
        let shift = empty.saturating_sub(base);
        // The far end of the presented window, in the same shifted coordinate
        // `trim_point` is in. A `played_duration_movie` of 0 means every
        // non-empty edit said "to the end of the media" (§8.6.6.1), so
        // nothing is trimmed off the tail.
        let played = vaco_format_isom::edit::rescale_movie_to_media(
            i64::try_from(edits.played_duration_movie()).unwrap_or(i64::MAX),
            self.movie_timescale,
            trak.media.timescale,
        );
        let trim_end = if played > 0 {
            empty.saturating_add(played)
        } else {
            i64::MAX
        };
        (shift, empty, empty, trim_end)
    }

    /// Presentation time of the first sample, in raw media ticks.
    fn first_presentation(&self, table: &SampleTable<'_>, track_id: u32) -> Option<i64> {
        if self.fragmented {
            return self.fragment_first_presentation(track_id);
        }
        let s = table.sample(0)?;
        Some(s.dts.saturating_add(i64::from(s.cts_offset)))
    }

    /// The same, from the first `traf` a fragmented track has.
    fn fragment_first_presentation(&self, track_id: u32) -> Option<i64> {
        let defaults = self.extends_for(track_id);
        for frag in &self.fragments {
            let parsed = parse_fragment(frag)?;
            for (i, traf) in parsed.tracks.iter().enumerate() {
                if traf.header.track_id != track_id {
                    continue;
                }
                let base = parsed
                    .track_base(i, |id| self.extends_for(id))
                    .unwrap_or(frag.offset);
                let start = traf
                    .base_media_decode_time
                    .map_or(0, |t| i64::try_from(t).unwrap_or(i64::MAX));
                return traf.samples(base, start, &defaults).next().map(|s| s.pts());
            }
        }
        None
    }

    /// The smallest `dts + ctts` over the track.
    ///
    /// Bounded: decode times only increase, so once `dts + min(ctts)` is at or
    /// above the best value found, nothing later can beat it.
    fn min_presentation(&self, table: &SampleTable<'_>, track_id: u32) -> Option<i64> {
        if self.fragmented {
            return self.fragment_first_presentation(track_id);
        }
        let floor = table
            .composition_offsets
            .as_ref()
            .map_or(0, |c| i64::from(c.min_offset()));
        let mut best: Option<i64> = None;
        for s in table.cursor().take(ANALYSE_SAMPLES as usize) {
            let pts = s.dts.saturating_add(i64::from(s.cts_offset));
            best = Some(best.map_or(pts, |b: i64| b.min(pts)));
            if s.dts.saturating_add(floor) >= best.unwrap_or(i64::MAX) {
                break;
            }
        }
        best
    }

    /// `r_frame_rate` and `avg_frame_rate`.
    ///
    /// **Measured** on a file whose `stts` is `[(9, 60), (1, 20)]` at timescale
    /// 600: `r_frame_rate=10/1` (the timescale over the *most common* delta,
    /// not the smallest, which would have been 30) and `avg_frame_rate=75/7`
    /// (the sample count over the media limit).
    fn frame_rate_estimate(
        &self,
        trak: &Track<'_>,
        table: &SampleTable<'_>,
        totals: &MediaTotals,
        limit: i64,
    ) -> (Rational, Rational) {
        let _ = limit;
        let ts = i32::try_from(trak.media.timescale).unwrap_or(i32::MAX);
        // **Measured**: the divisor is the sample table's own total, not the
        // media limit `duration_ts` and `bit_rate` use. Patching `mdhd.duration`
        // to 20 000 on a track whose `stts` totals 25 600 moved `duration_ts`
        // and `bit_rate` but left `avg_frame_rate` at 25/1.
        let samples = if totals.count > 0 {
            i64::from(totals.count)
        } else {
            i64::try_from(totals.timed).unwrap_or(i64::MAX)
        };
        let avg = if totals.duration > 0 && samples > 0 {
            reduce(
                samples.saturating_mul(i64::from(trak.media.timescale)),
                totals.duration,
            )
        } else {
            Rational::UNDEFINED
        };
        let common = if self.fragmented {
            totals.common_delta
        } else {
            common_delta(table)
        };
        let r = if common > 0 {
            reduce(i64::from(ts), i64::from(common))
        } else {
            avg
        };
        (r, avg)
    }

    // ------------------------------------------------------------- fragments

    fn collect_fragments(&mut self, seekable: bool) -> Result<()> {
        if !seekable {
            return Ok(());
        }
        while self.fragments.len() < MAX_FRAGMENTS {
            if !self.pull_next_fragment()? {
                break;
            }
        }
        Ok(())
    }

    /// Read `mfra` from the file's trailer, if it is there.
    ///
    /// `mfro`, the very last box in a file that has one, states `mfra`'s own
    /// size (ISO/IEC 14496-12 §8.8.11) — the one place this format lets a
    /// random-access structure be found without a linear scan. Absent,
    /// truncated, or malformed input all resolve to "no fast path", not an
    /// error: a demuxer that cannot seek any faster than by scanning is still
    /// a working demuxer.
    fn read_mfra_trailer(&mut self) -> Option<Vec<TrackFragmentRandomAccess>> {
        let size = self.io.size()?;
        let mfro_offset = size.checked_sub(MFRO_BOX_LEN)?;
        let mfro_span = {
            let mut sc = TopLevelScanner::range(&mut self.io, mfro_offset, size);
            sc.next_box(&mut self.budget).ok().flatten()?
        };
        if mfro_span.kind != bt::MFRO || mfro_span.offset != mfro_offset {
            return None;
        }
        let mfro_payload =
            read_payload_incremental(&mut self.io, &mut self.budget, mfro_span, MFRO_BOX_LEN)
                .ok()?;
        self.budget.release(mfro_payload.len() as u64);
        // version(8) + flags(24) + size(32), all inside the 8-byte body.
        let mfra_size = u64::from(u32::from_be_bytes(
            *mfro_payload.get(4..8)?.first_chunk::<4>()?,
        ));
        if mfra_size < MFRO_BOX_LEN {
            return None;
        }
        let mfra_offset = size.checked_sub(mfra_size)?;
        let mfra_span = {
            let mut sc = TopLevelScanner::range(&mut self.io, mfra_offset, size);
            sc.next_box(&mut self.budget).ok().flatten()?
        };
        if mfra_span.kind != bt::MFRA
            || mfra_span.offset != mfra_offset
            || mfra_span.payload_len() > MAX_MOOV_BYTES
        {
            return None;
        }
        let payload =
            read_payload_incremental(&mut self.io, &mut self.budget, mfra_span, MAX_MOOV_BYTES)
                .ok()?;
        self.budget.release(payload.len() as u64);
        Some(parse_mfra(&reassemble(mfra_span, &payload)))
    }

    /// Fetch and append the single `moof` at `offset`, beyond every fragment
    /// already known.
    ///
    /// Deliberately refuses anything that is not strictly past the current
    /// tail of [`Mp4Demuxer::fragments`]: that vector is kept in ascending
    /// file-offset order everywhere else in this crate (`FragEntry::fragment`
    /// is an index into it, and those indices are handed out once and never
    /// renumbered), so inserting into the middle would silently invalidate
    /// every entry recorded past the insertion point. Appending past the tail
    /// has no such hazard — it is exactly what the ordinary scan does, just
    /// out of the file's own order, which `mfra` is what makes safe: it names
    /// an exact `moof` offset instead of asking this function to guess where
    /// one starts.
    ///
    /// This is what lets a seek reach a fragment beyond
    /// [`MAX_FRAGMENTS`] without rescanning everything before it — the case
    /// the ordinary eager scan gives up on.
    fn fetch_fragment_at(&mut self, offset: u64) -> Result<Option<usize>> {
        if self.fragments.last().is_some_and(|f| f.offset >= offset) {
            return Ok(None);
        }
        let span = {
            let mut sc = TopLevelScanner::range(&mut self.io, offset, self.scan_end);
            sc.next_box(&mut self.budget).ok().flatten()
        };
        let Some(span) = span else { return Ok(None) };
        if span.kind != bt::MOOF || span.offset != offset || span.payload_len() > MAX_MOOV_BYTES {
            return Ok(None);
        }
        let data = read_payload_incremental(&mut self.io, &mut self.budget, span, MAX_MOOV_BYTES)?;
        self.budget.release(data.len() as u64);
        self.fragments.push(Fragment {
            offset: span.offset,
            header_len: span.header_len,
            data,
        });
        self.extend_entries();
        Ok(Some(self.fragments.len().saturating_sub(1)))
    }

    /// The `mfra`-backed fast path for [`Mp4Demuxer::place_fragment`].
    ///
    /// Resolves straight to the index in `reader.entries` a seek should start
    /// from, using `tfra`'s own `(time, moof_offset)` pairs instead of
    /// scanning every entry the slow path has to. Returns `None` whenever the
    /// fast path cannot answer — no `tfra` for this track, a target before
    /// its first entry, or a `moof_offset` this demuxer has neither collected
    /// nor can fetch — and [`Mp4Demuxer::place_fragment`] falls back to its
    /// own scan in every one of those cases, so a wrong or absent `tfra` never
    /// costs correctness, only the speedup.
    fn tfra_locate(&mut self, slot: usize, media: i64) -> Option<usize> {
        let track_id = u32::try_from(self.streams.get(slot)?.id?).ok()?;
        let media = u64::try_from(media).ok()?;
        let entry = self
            .tfra
            .iter()
            .find(|t| t.track_id == track_id)?
            .at_or_before(media)?;
        let fi = match self
            .fragments
            .binary_search_by_key(&entry.moof_offset, |f| f.offset)
        {
            Ok(fi) => fi,
            Err(_) => self.fetch_fragment_at(entry.moof_offset).ok()??,
        };
        self.readers
            .get(slot)?
            .entries
            .iter()
            .position(|e| e.fragment == fi)
    }

    /// Advance the top-level scan to the next `moof`, reading its payload.
    ///
    /// Returns `false` at the end of the source. On a source that cannot seek
    /// the `mdat` that follows is buffered, because a fragment's samples are
    /// interleaved by decode time and therefore not read in file order.
    fn pull_next_fragment(&mut self) -> Result<bool> {
        if self.scan_done {
            return Ok(false);
        }
        loop {
            self.boxes_seen = self.boxes_seen.saturating_add(1);
            if self.boxes_seen > MAX_TOP_LEVEL_BOXES || self.fragments.len() >= MAX_FRAGMENTS {
                self.scan_done = true;
                return Ok(false);
            }
            // A malformed tail is the end of the fragments, not a reason to
            // refuse the whole file.
            let next = {
                let mut sc = TopLevelScanner::range(&mut self.io, self.scan_pos, self.scan_end);
                sc.next_box(&mut self.budget).ok().flatten()
            };
            let Some(span) = next else {
                self.scan_done = true;
                return Ok(false);
            };
            self.scan_pos = span.end();
            if span.kind == bt::SIDX && self.sidx.len() < MAX_SIDX_BOXES {
                // Best-effort: a `sidx` that fails to parse or that declares
                // more than `MAX_SIDX_BYTES` is skipped rather than treated
                // as a reason to give up on the fragments after it.
                if span.payload_len() <= MAX_SIDX_BYTES
                    && let Ok(data) = read_payload_incremental(
                        &mut self.io,
                        &mut self.budget,
                        span,
                        MAX_SIDX_BYTES,
                    )
                {
                    self.budget.release(data.len() as u64);
                    if let Some(sidx) = SegmentIndex::parse(&reassemble(span, &data)) {
                        self.sidx.push(sidx);
                    }
                }
                continue;
            }
            if span.kind == bt::PSSH {
                // A fragmented file's `pssh` sits here, beside `moof` — not
                // under `moov`, which is the box layer's own whole-file
                // `IsoFile::top_level_pssh` covers instead. Best-effort, the
                // same shape as `sidx` just above: a `pssh` that fails to
                // parse or exceeds `MAX_PSSH_BYTES` is skipped, not treated
                // as a reason to give up on the fragments after it.
                let seen = self
                    .metadata
                    .iter()
                    .filter(|(k, _)| k == "encryption_system_id")
                    .count();
                if seen < MAX_TOP_LEVEL_PSSH
                    && span.payload_len() <= MAX_PSSH_BYTES
                    && let Ok(data) = read_payload_incremental(
                        &mut self.io,
                        &mut self.budget,
                        span,
                        MAX_PSSH_BYTES,
                    )
                {
                    self.budget.release(data.len() as u64);
                    if let Ok(pssh) = vaco_format_isom::cenc::Pssh::parse(&reassemble(span, &data))
                    {
                        self.metadata.extend(pssh_tags(&pssh));
                    }
                }
                continue;
            }
            if span.kind == bt::MOOF {
                if span.payload_len() > MAX_MOOV_BYTES {
                    self.scan_done = true;
                    return Ok(false);
                }
                let data =
                    read_payload_incremental(&mut self.io, &mut self.budget, span, MAX_MOOV_BYTES)?;
                self.budget.release(data.len() as u64);
                self.fragments.push(Fragment {
                    offset: span.offset,
                    header_len: span.header_len,
                    data,
                });
                if self.io.seekability() == Seekability::None {
                    self.buffer_next_mdat()?;
                }
                return Ok(true);
            }
        }
    }

    /// Buffer the `mdat` at the scan position, for a source that cannot seek.
    fn buffer_next_mdat(&mut self) -> Result<()> {
        let span = {
            let mut sc = TopLevelScanner::range(&mut self.io, self.scan_pos, self.scan_end);
            match sc.next_box(&mut self.budget) {
                Ok(Some(s)) => s,
                _ => return Ok(()),
            }
        };
        if span.kind != bt::MDAT {
            return Ok(());
        }
        let buf = read_payload_incremental(
            &mut self.io,
            &mut self.budget,
            span,
            MAX_BUFFERED_MDAT_BYTES,
        )?;
        if let Some((_, old)) = self.mdat_buf.take() {
            self.budget.release(old.len() as u64);
        }
        self.mdat_buf = Some((span.payload_offset(), buf));
        self.scan_pos = span.end();
        Ok(())
    }

    /// Totals a fragmented track's `moof` chain implies.
    fn fragment_totals(&self, track_id: u32) -> MediaTotals {
        let mut totals = MediaTotals::default();
        let mut deltas: Vec<(u32, u32)> = Vec::new();
        let defaults = self.extends_for(track_id);
        let mut dts = 0i64;
        for frag in &self.fragments {
            let Some(parsed) = parse_fragment(frag) else {
                continue;
            };
            for (i, traf) in parsed.tracks.iter().enumerate() {
                if traf.header.track_id != track_id {
                    continue;
                }
                let base = parsed
                    .track_base(i, |id| self.extends_for(id))
                    .unwrap_or(frag.offset);
                let start = if self.mp4.use_tfdt {
                    traf.base_media_decode_time
                        .map_or(dts, |t| i64::try_from(t).unwrap_or(i64::MAX))
                } else {
                    dts
                };
                let mut n = 0u32;
                for s in traf
                    .samples(base, start, &defaults)
                    .take(read::MAX_SAMPLES_PER_FRAGMENT as usize)
                {
                    totals.bytes = totals.bytes.saturating_add(u64::from(s.size));
                    totals.duration = totals.duration.saturating_add(i64::from(s.duration));
                    totals.timed = totals.timed.saturating_add(1);
                    n = n.saturating_add(1);
                    if deltas.len() < 32 {
                        bump(&mut deltas, s.duration);
                    } else if let Some(slot) = deltas.iter_mut().find(|(d, _)| *d == s.duration) {
                        slot.1 = slot.1.saturating_add(1);
                    }
                }
                dts = start.saturating_add(i64::from(
                    i32::try_from(totals.duration).unwrap_or(i32::MAX),
                ));
                let _ = n;
            }
        }
        totals.common_delta = deltas.iter().max_by_key(|(_, n)| *n).map_or(0, |(d, _)| *d);
        totals
    }

    /// Per-fragment entries for one track, in file order.
    fn fragment_entries(&self, track_id: u32, size: Option<u64>) -> Vec<FragEntry> {
        let defaults = self.extends_for(track_id);
        let mut out = Vec::new();
        let mut dts = 0i64;
        for (fi, frag) in self.fragments.iter().enumerate() {
            let Some(parsed) = parse_fragment(frag) else {
                continue;
            };
            for (i, traf) in parsed.tracks.iter().enumerate() {
                if traf.header.track_id != track_id {
                    continue;
                }
                let base = parsed
                    .track_base(i, |id| self.extends_for(id))
                    .unwrap_or(frag.offset);
                let start = if self.mp4.use_tfdt {
                    traf.base_media_decode_time
                        .map_or(dts, |t| i64::try_from(t).unwrap_or(i64::MAX))
                } else {
                    dts
                };
                let declared = u32::try_from(traf.sample_count()).unwrap_or(u32::MAX);
                let samples =
                    read::sample_limit(declared, size).min(read::MAX_SAMPLES_PER_FRAGMENT);
                let mut end = start;
                let mut first_offset = base;
                for (n, s) in traf
                    .samples(base, start, &defaults)
                    .take(samples as usize)
                    .enumerate()
                {
                    if n == 0 {
                        first_offset = s.offset;
                    }
                    end = s.dts.saturating_add(i64::from(s.duration));
                }
                out.push(FragEntry {
                    fragment: fi,
                    traf: i,
                    start_dts: start,
                    samples,
                    first_offset,
                });
                dts = end;
            }
        }
        out
    }

    fn extends_for(&self, track_id: u32) -> TrackExtends {
        self.extends
            .iter()
            .find(|t| t.track_id == track_id)
            .copied()
            .unwrap_or_default()
    }

    // ---------------------------------------------------------------- reading

    /// Make sure `slot`'s queue has a head, or that the reader is finished.
    fn ensure_head(&mut self, slot: usize) -> Result<()> {
        let mut guard = vaco_limits::ProgressGuard::new();
        loop {
            let Some(reader) = self.readers.get(slot) else {
                return Ok(());
            };
            // Reported, not decoded (see `Reader::encryption_error`): a clear refusal
            // rather than either silence or the encrypted bytes themselves.
            if let Some(reason) = reader.encryption_error {
                return Err(Error::Unsupported(reason));
            }
            if reader.finished || !reader.queue.is_empty() {
                return Ok(());
            }
            let progressed = self.refill(slot)?;
            guard.tick(progressed)?;
        }
    }

    /// One refill step. Returns whether it made progress.
    fn refill(&mut self, slot: usize) -> Result<bool> {
        let size = self.io.size();
        match self.readers.get(slot).map(|r| &r.source) {
            None => return Ok(false),
            Some(Source::AttachedPic {
                offset,
                size,
                emitted,
            }) => {
                let (offset, size, emitted) = (*offset, *size, *emitted);
                let Some(reader) = self.readers.get_mut(slot) else {
                    return Ok(false);
                };
                if emitted {
                    reader.finished = true;
                    return Ok(true);
                }
                // A cover image has no timeline: the reference prints
                // `pts=N/A dts=N/A` for its single packet.
                reader.queue.push_back(Pending {
                    offset,
                    size,
                    dts: i64::MIN,
                    pts: i64::MIN,
                    duration: 0,
                    key: true,
                    discard: false,
                    skip: 0,
                    skip_end: 0,
                    index: 0,
                });
                if let Source::AttachedPic { emitted, .. } = &mut reader.source {
                    *emitted = true;
                }
                return Ok(true);
            }
            Some(Source::Item { extents, emitted }) => {
                let (emitted, offset, total) = (
                    *emitted,
                    extents.first().map_or(0, |e| e.0),
                    extents.iter().map(|e| e.1).sum::<u64>(),
                );
                let Some(reader) = self.readers.get_mut(slot) else {
                    return Ok(false);
                };
                if emitted {
                    reader.finished = true;
                    return Ok(true);
                }
                // **Measured**: `ffprobe -show_packets` on an AVIF prints one
                // packet per item stream, `pts=0 dts=0 duration=1 flags=K`,
                // `pos` at the first extent.
                reader.queue.push_back(Pending {
                    offset,
                    size: u32::try_from(total).unwrap_or(u32::MAX),
                    dts: 0,
                    pts: 0,
                    duration: 1,
                    key: true,
                    discard: false,
                    skip: 0,
                    skip_end: 0,
                    index: 0,
                });
                if let Source::Item { emitted, .. } = &mut reader.source {
                    *emitted = true;
                }
                return Ok(true);
            }
            Some(Source::Table { .. }) => {
                // Disjoint field borrows: the tables read `self.moov`, the
                // queue lives in `self.readers`.
                let (moov, offset, header_len) =
                    (&self.moov, self.moov_offset, self.moov_header_len);
                let Some(reader) = self.readers.get_mut(slot) else {
                    return Ok(false);
                };
                let Source::Table {
                    slot: track_slot, ..
                } = reader.source
                else {
                    reader.finished = true;
                    return Ok(false);
                };
                let bx = moov_box(moov, offset, header_len);
                let Ok(movie) = Movie::parse(&bx) else {
                    reader.finished = true;
                    return Ok(true);
                };
                let Some(trak) = movie.tracks.get(track_slot) else {
                    reader.finished = true;
                    return Ok(true);
                };
                return Ok(read::refill_table(reader, &trak.sample_table, size) || reader.finished);
            }
            Some(Source::Fragments { .. }) => {}
        }

        // Fragmented: advance within the current `traf`, moving to the next
        // entry — and pulling one more `moof` — when it is exhausted.
        let mut guard = vaco_limits::ProgressGuard::new();
        loop {
            let Some(reader) = self.readers.get_mut(slot) else {
                return Ok(false);
            };
            if !read::advance_fragment(reader) {
                if !self.pull_next_fragment()? {
                    if let Some(reader) = self.readers.get_mut(slot) {
                        reader.finished = true;
                    }
                    return Ok(true);
                }
                self.extend_entries();
                guard.tick(true)?;
                continue;
            }
            let Some(e) = self.readers.get(slot).and_then(|r| match r.source {
                Source::Fragments { entry, .. } => r.entries.get(entry).copied(),
                _ => None,
            }) else {
                return Ok(false);
            };
            let Some(frag) = self.fragments.get(e.fragment) else {
                return Ok(false);
            };
            let Some(parsed) = parse_fragment(frag) else {
                if let Some(reader) = self.readers.get_mut(slot) {
                    reader.finished = true;
                }
                return Ok(true);
            };
            let Some(traf) = parsed.tracks.get(e.traf) else {
                return Ok(false);
            };
            let base = parsed
                .track_base(e.traf, |id| self.extends_for(id))
                .unwrap_or(frag.offset);
            let defaults = self.extends_for(traf.header.track_id);
            let Some(reader) = self.readers.get_mut(slot) else {
                return Ok(false);
            };
            if let Some(decryptor) = reader.decrypt.as_mut() {
                decryptor
                    .replace_fragment(traf, e.samples)
                    .map_err(Error::Unsupported)?;
            }
            return Ok(
                read::refill_fragment(reader, traf, base, &defaults, size) || reader.finished
            );
        }
    }

    /// After a lazily pulled `moof`, give every track its new entry.
    fn extend_entries(&mut self) {
        let Some(fi) = self.fragments.len().checked_sub(1) else {
            return;
        };
        let size = self.io.size();
        let Some(frag) = self.fragments.get(fi) else {
            return;
        };
        let Some(parsed) = parse_fragment(frag) else {
            return;
        };
        for (i, traf) in parsed.tracks.iter().enumerate() {
            let track_id = traf.header.track_id;
            let base = parsed
                .track_base(i, |id| self.extends_for(id))
                .unwrap_or(frag.offset);
            let defaults = self.extends_for(track_id);
            let Some(slot) = self.stream_slot_for_track(track_id) else {
                continue;
            };
            let previous_end = self
                .readers
                .get(slot)
                .and_then(|r| r.entries.last())
                .map_or(0, |e| e.start_dts);
            let start = if self.mp4.use_tfdt {
                traf.base_media_decode_time
                    .map_or(previous_end, |t| i64::try_from(t).unwrap_or(i64::MAX))
            } else {
                previous_end
            };
            let declared = u32::try_from(traf.sample_count()).unwrap_or(u32::MAX);
            let samples = read::sample_limit(declared, size).min(read::MAX_SAMPLES_PER_FRAGMENT);
            let first_offset = traf
                .samples(base, start, &defaults)
                .next()
                .map_or(base, |s| s.offset);
            if let Some(reader) = self.readers.get_mut(slot) {
                reader.entries.push(FragEntry {
                    fragment: fi,
                    traf: i,
                    start_dts: start,
                    samples,
                    first_offset,
                });
            }
        }
    }

    fn stream_slot_for_track(&self, track_id: u32) -> Option<usize> {
        self.streams
            .iter()
            .position(|s| s.id == Some(i64::from(track_id)))
    }

    /// The next reader to emit from.
    ///
    /// **Rule MP4-O1, corrected against the binary.** Plan 18 says "smallest
    /// DTS, ties broken by file offset". Measured, the reference is the other
    /// way round within a window: while two tracks' decode times are within
    /// [`INTERLEAVE_WINDOW_US`] of each other it emits in **file order**, and
    /// only outside that window does decode time decide.
    ///
    /// The discriminating file is a fragmented MP4 whose first `moof` holds
    /// thirteen video samples spanning 0.56 s and twenty-two audio samples
    /// starting at 0.000000. Pure DTS order interleaves them; the reference
    /// emits all thirteen video packets first, because the audio data sits
    /// later in the `mdat`. Both orders are "correct" in the sense that both
    /// are monotonic per track, but `-show_packets` prints the order, so only
    /// one of them matches.
    ///
    /// A packet with no decode time — a cover image — never wins against one
    /// that has one, so it is emitted after every timed track has run out.
    fn pick(&self) -> Option<usize> {
        let mut best: Option<(usize, Option<i64>, u64)> = None;
        for (i, r) in self.readers.iter().enumerate() {
            let Some(head) = r.head().copied() else {
                continue;
            };
            let micros = (head.dts != i64::MIN)
                .then(|| {
                    Timestamp::new(head.dts)
                        .to_duration(r.time_base)
                        .map(vaco_core::Duration::as_micros)
                })
                .flatten();
            let Some((_, best_us, best_pos)) = best else {
                best = Some((i, micros, head.offset));
                continue;
            };
            let wins = match (micros, best_us) {
                // A packet with no decode time is the attached picture, and
                // the reference pre-loads it: it comes out first, before any
                // timed packet. Measured on an `.m4a` with a `covr` atom.
                (None, Some(_)) => true,
                (None | Some(_), None) => false,
                (Some(a), Some(b)) => {
                    if a.abs_diff(b) <= INTERLEAVE_WINDOW_US {
                        head.offset < best_pos
                    } else {
                        a < b
                    }
                }
            };
            if wins {
                best = Some((i, micros, head.offset));
            }
        }
        best.map(|(i, _, _)| i)
    }

    /// Read a sample's bytes into a packet.
    /// Several byte ranges, concatenated into one packet.
    fn payload_extents(&mut self, extents: &[(u64, u64)]) -> Result<Packet> {
        let total = extents.iter().map(|e| e.1).sum::<u64>();
        let len = usize::try_from(total).map_err(|_| Error::LimitExceeded {
            limit: "mp4_item_bytes",
            requested: total,
            cap: u64::from(u32::MAX),
        })?;
        let mut pkt = Packet::alloc(&mut self.packet_budget, len)?;
        self.packet_budget.release(total);
        let mut at = 0usize;
        for &(offset, size) in extents {
            let n = usize::try_from(size).unwrap_or(usize::MAX);
            let part = self.payload(offset, u32::try_from(size).unwrap_or(u32::MAX))?;
            let end = at.saturating_add(n);
            if let Some(dst) = pkt.payload_mut().get_mut(at..end) {
                dst.copy_from_slice(part.payload());
            }
            at = end;
        }
        Ok(pkt)
    }

    fn payload(&mut self, offset: u64, size: u32) -> Result<Packet> {
        let len = usize::try_from(size).unwrap_or(usize::MAX);
        if let Some(total) = self.io.size()
            && offset.saturating_add(u64::from(size)) > total
        {
            return Err(Error::InvalidData("mp4: sample lies past the end of file"));
        }
        let mut pkt = Packet::alloc(&mut self.packet_budget, len)?;
        self.packet_budget.release(len as u64);
        if let Some((start, buf)) = self.mdat_buf.as_ref()
            && offset >= *start
            && offset.saturating_add(u64::from(size)) <= start.saturating_add(buf.len() as u64)
        {
            let at = (offset - start) as usize;
            if let Some(src) = buf.get(at..at.saturating_add(len)) {
                pkt.payload_mut().copy_from_slice(src);
                return Ok(pkt);
            }
        }
        if self.io.seekability() == Seekability::None && offset < self.io.pos() {
            return Err(Error::Unsupported(
                "mp4: sample lies behind the read position on a source that cannot seek",
            ));
        }
        self.io.seek(offset)?;
        self.io.read_exact(pkt.payload_mut())?;
        Ok(pkt)
    }

    fn next_packet(&mut self) -> Result<Packet> {
        loop {
            if self.eof {
                return Err(Error::Eof);
            }
            for slot in 0..self.readers.len() {
                self.ensure_head(slot)?;
            }
            let Some(slot) = self.pick() else {
                self.eof = true;
                return Err(Error::Eof);
            };
            let (sample, stream_index, time_base, audio, subtitle, decrypt) = {
                let Some(reader) = self.readers.get_mut(slot) else {
                    self.eof = true;
                    return Err(Error::Eof);
                };
                let Some(sample) = reader.queue.pop_front() else {
                    self.eof = true;
                    return Err(Error::Eof);
                };
                (
                    sample,
                    reader.stream_index,
                    reader.time_base,
                    reader.is_audio(),
                    matches!(reader.media_type, MediaType::Subtitle),
                    reader.decrypt.clone(),
                )
            };
            // A zero-duration *subtitle* sample never reaches a caller
            // through real ffmpeg 9.0.1's own MP4 demuxer — measured
            // directly: a real `mov_text` file's trailing "clear the
            // subtitle" sample (`stts` declares its own delta `0`, a
            // standard trailing entry many `mov_text` writers, including
            // ffmpeg's own, append after the last real cue) is invisible to
            // `ffprobe -show_packets` on the reference, though it is *not* a
            // zero-*size* sample — its `stsz` entry is a real 2 bytes
            // (mov_text's own big-endian `u16` zero-length-string encoding),
            // so a size-based check would have missed it entirely.
            //
            // Gated on the media type as well as the duration, because
            // duration alone was measured **wrong** on video: a file whose
            // final `stts` run is `(1, 0)` — the shape this repository's own
            // MP4 muxer wrote for the last sample of every progressive file
            // until this commit — has that sample reported by `ffprobe
            // -count_packets` (20 of 20, with `duration=100` substituted for
            // the declared 0), while this crate silently dropped the last
            // frame of every such track. Skipped here, not filtered out when
            // the sample table is first read, so every other consumer of
            // that table (seeking, duration accounting) is unaffected — only
            // the packet stream a caller actually reads from changes.
            if subtitle && sample.duration == 0 {
                continue;
            }
            // An item split across several `iloc` extents is one packet:
            // `sample.size` is their sum, `sample.offset` only the first.
            let split = self.readers.get(slot).and_then(|r| match &r.source {
                Source::Item { extents, .. } if extents.len() > 1 => Some(extents.clone()),
                _ => None,
            });
            let mut pkt = match split {
                Some(extents) => self.payload_extents(&extents)?,
                None => self.payload(sample.offset, sample.size)?,
            };
            if let Some(dec) = &decrypt
                && !dec.decrypt(sample.index, pkt.payload_mut())
            {
                return Err(Error::Unsupported(
                    "mp4: cenc: senc has no usable record for this sample (none declared, or \
                     a subsample table that does not fit the sample)",
                ));
            }
            pkt.stream_index = stream_index;
            // `i64::MIN` is the "no timeline" marker a cover image carries.
            pkt.pts = if sample.pts == i64::MIN {
                Timestamp::NONE
            } else {
                Timestamp::new(sample.pts)
            };
            pkt.dts = if sample.dts == i64::MIN {
                Timestamp::NONE
            } else {
                Timestamp::new(sample.dts)
            };
            pkt.pos = Some(sample.offset);
            pkt.duration = Timestamp::new(i64::from(sample.duration))
                .to_duration(time_base)
                .unwrap_or(Duration::ZERO);
            pkt.set_duration_ts(i64::from(sample.duration));
            pkt.flags = PacketFlags::empty();
            if sample.key {
                pkt.flags |= PacketFlags::KEY;
            }
            if sample.discard {
                pkt.flags |= PacketFlags::DISCARD;
            }
            if audio && (sample.skip > 0 || sample.skip_end > 0) {
                pkt.side_data.push(PacketSideData::SkipSamples {
                    start: sample.skip,
                    end: sample.skip_end,
                    // D17: measured 0 on every MP4 file tried so far — an
                    // `elst`-derived leading skip carries no reason of its own.
                    skip_reason: 0,
                    discard_reason: 0,
                });
            }
            if sample.key {
                self.index.add(IndexEntry::keyframe(sample.offset, pkt.dts));
            }
            return Ok(pkt);
        }
    }

    // ---------------------------------------------------------------- seeking

    fn seek_to(&mut self, stream_index: u32, ts: Timestamp, flags: SeekFlags) -> Result<()> {
        let Some(ticks) = ts.ticks() else {
            return Err(Error::InvalidData("mp4: seek target has no timestamp"));
        };
        // Resolve the reference track first, then place the others at their own
        // nearest preceding sample, which is what `-seek_streams_individually`
        // controls.
        let reference = usize::try_from(stream_index)
            .unwrap_or(0)
            .min(self.readers.len().saturating_sub(1));
        let ref_base = self
            .readers
            .get(reference)
            .map_or(Rational::ONE, |r| r.time_base);
        let landed = self.place(reference, ticks, flags)?;
        for slot in 0..self.readers.len() {
            if slot == reference {
                continue;
            }
            let base = self.readers.get(slot).map_or(ref_base, |r| r.time_base);
            let target = Timestamp::new(landed)
                .rescale(ref_base, base, Rounding::Down)
                .ticks()
                .unwrap_or(0);
            // `-seek_streams_individually` off means every track lands at the
            // same instant rather than each at its own nearest sync sample.
            let per_stream = if self.mp4.seek_streams_individually {
                flags | SeekFlags::BACKWARD
            } else {
                flags | SeekFlags::BACKWARD | SeekFlags::ANY
            };
            self.place(slot, target, per_stream)?;
        }
        self.eof = false;
        Ok(())
    }

    /// Position one track at or before `ticks`, returning where it landed.
    fn place(&mut self, slot: usize, ticks: i64, flags: SeekFlags) -> Result<i64> {
        let forward = !flags.contains(SeekFlags::BACKWARD);
        let any = flags.contains(SeekFlags::ANY);
        let (shift, edit) = self
            .readers
            .get(slot)
            .map_or((0, 0), |r| (r.dts_shift, r.edit_shift));
        // The target is in presented time; invert the edit to reach media time.
        let media = ticks.saturating_sub(edit);
        if self.fragmented {
            return Ok(self.place_fragment(slot, media, any));
        }
        let (moov, offset, header_len) = (&self.moov, self.moov_offset, self.moov_header_len);
        let track_slot = self.readers.get(slot).and_then(|r| match r.source {
            Source::Table { slot, .. } => Some(slot),
            _ => None,
        });
        let Some(track_slot) = track_slot else {
            if let Some(r) = self.readers.get_mut(slot) {
                r.queue.clear();
                // A still image has one frame at 0: any seek lands on it.
                if let Source::Item { emitted, .. } = &mut r.source {
                    *emitted = false;
                    r.finished = false;
                }
            }
            return Ok(ticks);
        };
        let bx = moov_box(moov, offset, header_len);
        let movie = Movie::parse(&bx)?;
        let Some(trak) = movie.tracks.get(track_slot) else {
            return Ok(ticks);
        };
        let table = &trak.sample_table;
        let raw = media.saturating_sub(shift);
        let at = table.sample_at_dts(raw).unwrap_or(0);
        let target = if any {
            at
        } else {
            let before = table.sync_at_or_before(at);
            if forward {
                before.or_else(|| table.sync_at_or_after(at)).unwrap_or(0)
            } else {
                before.unwrap_or(0)
            }
        };
        let landed = table
            .sample(target)
            .map_or(ticks, |s| s.dts.saturating_add(shift).saturating_add(edit));
        if let Some(reader) = self.readers.get_mut(slot) {
            reader.queue.clear();
            reader.finished = reader.blocked;
            reader.batch = read::BATCH_MIN;
            if let Source::Table { next, limit, .. } = &mut reader.source {
                *next = target.min(*limit);
                if *next >= *limit {
                    reader.finished = true;
                }
            }
        }
        Ok(landed)
    }

    /// Position one fragmented track at or before `media`.
    ///
    /// Two steps, and the second is the one that is easy to leave out: pick the
    /// `moof` whose first sample is at or before the target, **then** walk that
    /// fragment's samples for the last sync sample at or before it. Stopping at
    /// the fragment boundary lands a whole fragment early on any file whose
    /// fragments are longer than its keyframe interval — measured against the
    /// reference, which lands mid-fragment.
    fn place_fragment(&mut self, slot: usize, media: i64, any: bool) -> i64 {
        // `tfra` first, when it resolves — see `tfra_locate`'s own docs for
        // why a miss here is never a correctness problem, only a missed
        // speedup.
        //
        // Otherwise: scanned rather than bisected, and every entry is
        // examined rather than stopping at the first one past the target:
        // `tfdt` is written by the file, so a corrupt one can make the
        // fragment start times non-monotonic, and an early `break` would then
        // land somewhere arbitrary. Choosing the latest fragment at or before
        // the target, and otherwise the earliest fragment there is, is well
        // defined for any ordering — which is what makes "a backward seek
        // lands at or before the target, or at the earliest packet the track
        // has" true rather than true-for-well-formed-files.
        let Some(chosen) = self.tfra_locate(slot, media).or_else(|| {
            self.readers.get(slot).and_then(|r| {
                let mut at_or_before: Option<(usize, i64)> = None;
                let mut earliest: Option<(usize, i64)> = None;
                for (i, e) in r.entries.iter().enumerate() {
                    if earliest.is_none_or(|(_, d)| e.start_dts < d) {
                        earliest = Some((i, e.start_dts));
                    }
                    if e.start_dts <= media && at_or_before.is_none_or(|(_, d)| e.start_dts > d) {
                        at_or_before = Some((i, e.start_dts));
                    }
                }
                at_or_before.or(earliest).map(|(i, _)| i)
            })
        }) else {
            if let Some(reader) = self.readers.get_mut(slot) {
                reader.queue.clear();
                reader.finished = true;
            }
            return media;
        };
        let Some(e) = self
            .readers
            .get(slot)
            .and_then(|r| r.entries.get(chosen).copied())
        else {
            return media;
        };
        let (within, dts) = self.locate_in_fragment(e, media, any);
        let Some(reader) = self.readers.get_mut(slot) else {
            return media;
        };
        reader.queue.clear();
        reader.finished = reader.blocked;
        if let Source::Fragments {
            entry,
            next_in_entry,
        } = &mut reader.source
        {
            *entry = chosen;
            *next_in_entry = within;
        }
        dts.saturating_add(reader.edit_shift)
    }

    /// The last usable sample at or before `media` inside one track fragment.
    fn locate_in_fragment(&self, e: FragEntry, media: i64, any: bool) -> (u32, i64) {
        let Some(frag) = self.fragments.get(e.fragment) else {
            return (0, e.start_dts);
        };
        let Some(parsed) = parse_fragment(frag) else {
            return (0, e.start_dts);
        };
        let Some(traf) = parsed.tracks.get(e.traf) else {
            return (0, e.start_dts);
        };
        let base = parsed
            .track_base(e.traf, |id| self.extends_for(id))
            .unwrap_or(frag.offset);
        let defaults = self.extends_for(traf.header.track_id);
        let mut best = (0u32, e.start_dts);
        for (i, s) in traf
            .samples(base, e.start_dts, &defaults)
            .take(e.samples as usize)
            .enumerate()
        {
            if s.dts > media {
                break;
            }
            if any || s.is_sync() {
                best = (u32::try_from(i).unwrap_or(u32::MAX), s.dts);
            }
        }
        best
    }
}

impl Demuxer for Mp4Demuxer {
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
        self.next_packet()
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        if self.io.seekability() == Seekability::None {
            return Err(Error::NotSeekable);
        }
        let reference = target.stream_index().unwrap_or(0);
        let rate = self
            .streams
            .get(usize::try_from(reference).unwrap_or(0))
            .and_then(|s| s.params.video.as_ref().map(|v| v.frame_rate))
            .unwrap_or(Rational::UNDEFINED);
        let base = self
            .streams
            .get(usize::try_from(reference).unwrap_or(0))
            .map_or(Rational::ONE, |s| s.time_base);
        let target = target.resolve_frames(rate, base)?;
        let strategy =
            SeekStrategy::choose(target, flags, format_flags(), !self.index.is_empty(), true);
        match target {
            SeekTarget::Timestamp { stream_index, ts } => self.seek_to(stream_index, ts, flags),
            SeekTarget::Byte(_) if strategy == SeekStrategy::Unsupported => Err(Error::NotSeekable),
            SeekTarget::Byte(_) => Err(Error::Unsupported(
                "mp4: byte seeking is not meaningful for a sample-table container",
            )),
            SeekTarget::Frame { .. } => Err(Error::Unsupported("mp4: frame seek needs a rate")),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn stream_groups(&self) -> &[StreamGroup] {
        &self.groups
    }
}

// --------------------------------------------------------------------- helpers

/// Read a located box's payload, growing the buffer as the bytes actually
/// arrive rather than reserving the size the box claims.
///
/// Plan 13 §2.2.2 rule 3: never allocate a declared size before the bytes
/// exist. `vaco-format-isom`'s `TopLevelScanner::read_payload` reserves up
/// front, which is right for a caller that has already bounded the claim
/// against something; on a source that cannot state its own size there is
/// nothing to bound it against, so the growth has to be the bound.
///
/// A payload the source runs out of is returned short rather than refused: a
/// truncated file must still report its streams.
///
/// # Errors
///
/// [`Error::LimitExceeded`] when the declared size exceeds `cap` or the budget
/// refuses the growth, and whatever the transport reports.
fn read_payload_incremental(
    io: &mut IoContext,
    budget: &mut Budget,
    span: BoxSpan,
    cap: u64,
) -> Result<Vec<u8>> {
    let declared = span.payload_len();
    if declared > cap {
        return Err(Error::LimitExceeded {
            limit: "mp4_box_payload",
            requested: declared,
            cap,
        });
    }
    let want = usize::try_from(declared).unwrap_or(usize::MAX);
    io.seek(span.payload_offset())?;
    let mut out = budget.incremental::<u8>(want);
    let mut chunk = [0u8; READ_CHUNK];
    let mut left = want;
    while left > 0 {
        let take = left.min(READ_CHUNK);
        let Some(dst) = chunk.get_mut(..take) else {
            break;
        };
        let n = io.read_partial(dst)?;
        if n == 0 {
            break;
        }
        let Some(src) = chunk.get(..n) else { break };
        out.push_slice(budget, src)?;
        left = left.saturating_sub(n);
    }
    Ok(out.into_vec())
}

/// Rebuild an [`IsoBox`] over a payload the scanner read separately.
fn reassemble(span: BoxSpan, payload: &[u8]) -> IsoBox<'_> {
    reassemble_at(span.kind, span.offset, span.header_len, payload)
}

fn reassemble_at(kind: FourCc, offset: u64, header_len: u64, payload: &[u8]) -> IsoBox<'_> {
    IsoBox {
        header: BoxHeader {
            kind,
            size: header_len.saturating_add(payload.len() as u64),
            header_len,
            usertype: None,
            to_end: false,
        },
        payload,
        offset,
    }
}

/// Parse one collected `moof`.
///
/// A free function rather than a method so that the returned structure borrows
/// only the fragment list, leaving the reader list free to be borrowed mutably
/// in the same expression.
fn parse_fragment(frag: &Fragment) -> Option<MovieFragment<'_>> {
    let bx = reassemble_at(bt::MOOF, frag.offset, frag.header_len, &frag.data);
    MovieFragment::parse(&bx).ok()
}

/// One `tmcd` track, located but not yet read — see [`find_timecode_tracks`].
struct TimecodeTrack {
    track_id: u32,
    entry: stsd::TimecodeSampleEntry,
    offset: u64,
    size: u32,
    /// Track ids of other tracks whose `tref ▸ tmcd` names this one.
    referenced_by: Vec<u32>,
}

/// Every `tmcd` track with a readable first sample, plus which other tracks
/// reference it. Structural only, for the same borrow-splitting reason as
/// [`find_qt_chapter_track`].
fn find_timecode_tracks(movie: &Movie<'_>) -> Vec<TimecodeTrack> {
    let mut out = Vec::new();
    for trak in &movie.tracks {
        if trak.handler != bt::TMCD || !trak.has_usable_timescale() {
            continue;
        }
        let Some(entry) = trak
            .sample_table
            .sample_descriptions
            .as_ref()
            .and_then(|d| stsd::parse_stsd(d, trak.handler).ok())
            .and_then(|v| v.first().and_then(|e| e.tmcd))
        else {
            continue;
        };
        let Some(sample) = trak.sample_table.cursor_at(0).next() else {
            continue;
        };
        let mut referenced_by = Vec::new();
        for other in &movie.tracks {
            if other.header.track_id == trak.header.track_id {
                continue;
            }
            for r in &other.references {
                if r.kind == FourCc::new(b"tmcd") && r.track_ids.contains(&trak.header.track_id) {
                    referenced_by.push(other.header.track_id);
                }
            }
        }
        out.push(TimecodeTrack {
            track_id: trak.header.track_id,
            entry,
            offset: sample.offset,
            size: sample.size,
            referenced_by,
        });
    }
    out
}

/// Find a `QuickTime` chapter track: some other track's `tref ▸ chap`
/// names it, and it carries the simple Apple `text` sample format (a
/// big-endian length then that many bytes) rather than 3GPP `tx3g`, which is a
/// different sample shape and is not read here.
///
/// Returns the chapter track's time base and `(dts, offset, size)` for each of
/// its samples, in file order — structural only, so the caller can read the
/// actual sample bytes once it is free to borrow `self.io` again.
fn find_qt_chapter_track(
    movie: &Movie<'_>,
    size: Option<u64>,
) -> Option<(Rational, Vec<(i64, u64, u32)>)> {
    for trak in &movie.tracks {
        for r in &trak.references {
            if r.kind != FourCc::new(b"chap") {
                continue;
            }
            for &chap_id in &r.track_ids {
                let Some(chap_trak) = movie.track_by_id(chap_id) else {
                    continue;
                };
                if !chap_trak.has_usable_timescale() {
                    continue;
                }
                let entries = chap_trak
                    .sample_table
                    .sample_descriptions
                    .as_ref()
                    .and_then(|d| stsd::parse_stsd(d, chap_trak.handler).ok())
                    .unwrap_or_default();
                let is_apple_text = entries
                    .first()
                    .is_some_and(|e| e.format == FourCc::new(b"text"));
                if !is_apple_text {
                    continue;
                }
                let limit = read::sample_limit(u32::MAX, size);
                let samples: Vec<(i64, u64, u32)> = chap_trak
                    .sample_table
                    .cursor_at(0)
                    .take_while(|s| s.index < limit)
                    .take(meta::MAX_ENTRIES)
                    .map(|s| (s.dts, s.offset, s.size))
                    .collect();
                if !samples.is_empty() {
                    return Some((chap_trak.time_base(), samples));
                }
            }
        }
    }
    None
}

/// The `vendor` field of the first sample entry, which `ffprobe` prints as the
/// `vendor_id` stream tag on `.mov` files.
///
/// Read here rather than through [`stsd::SampleEntry`] because that type does
/// not expose it — the field is `pre_defined`/`reserved` in ISO/IEC 14496-12
/// and only Apple's `QuickTime` specification gives it a meaning.
fn vendor_id(table: &SampleTable<'_>) -> Option<[u8; 4]> {
    let stsd = table.sample_descriptions.as_ref()?;
    let full = stsd.full().ok()?;
    let body = full.body.get(4..)?;
    let first = vaco_format_isom::boxes::BoxIter::new(body, 0)
        .next()?
        .ok()?;
    // Sample-entry header is 6 reserved + 2 data_reference_index; the
    // `QuickTime` body then reads version(2), revision(2), vendor(4).
    first.payload.get(12..16)?.first_chunk::<4>().copied()
}

fn moov_box(data: &[u8], offset: u64, header_len: u64) -> IsoBox<'_> {
    reassemble_at(bt::MOOV, offset, header_len, data)
}

/// Lower-case hex, for a `default_KID` or a `pssh` system id.
fn hex16(bytes: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(32);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Container metadata carried by one `pssh`, preserving its version-1 KID
/// declaration order after the DRM system id.
fn pssh_tags(pssh: &vaco_format_isom::cenc::Pssh) -> impl Iterator<Item = (String, String)> + '_ {
    std::iter::once(("encryption_system_id".to_owned(), hex16(&pssh.system_id))).chain(
        pssh.kids
            .iter()
            .map(|kid| ("encryption_key_id".to_owned(), hex16(kid))),
    )
}

/// Sample-table totals: what every derived field divides by.
fn table_totals(table: &SampleTable<'_>) -> MediaTotals {
    let count = table.sample_count();
    MediaTotals {
        duration: table.total_duration(),
        bytes: table.sample_sizes.cumulative(count),
        count,
        common_delta: 0,
        timed: u64::from(count),
    }
}

/// The most common `stts` delta over a bounded prefix.
fn common_delta(table: &SampleTable<'_>) -> u32 {
    let mut deltas: Vec<(u32, u32)> = Vec::new();
    for s in table.cursor().take(ANALYSE_SAMPLES as usize) {
        if let Some(slot) = deltas.iter_mut().find(|(d, _)| *d == s.duration) {
            slot.1 = slot.1.saturating_add(1);
        } else if deltas.len() < 32 {
            deltas.push((s.duration, 1));
        }
    }
    deltas
        .iter()
        .filter(|(d, _)| *d > 0)
        .max_by_key(|(_, n)| *n)
        .map_or(0, |(d, _)| *d)
}

fn bump(deltas: &mut Vec<(u32, u32)>, delta: u32) {
    if let Some(slot) = deltas.iter_mut().find(|(d, _)| *d == delta) {
        slot.1 = slot.1.saturating_add(1);
    } else {
        deltas.push((delta, 1));
    }
}

/// A reduced rational, or [`Rational::UNDEFINED`] when it cannot be formed.
fn reduce(num: i64, den: i64) -> Rational {
    if den == 0 || num == 0 {
        return Rational::UNDEFINED;
    }
    Rational::reduce(num, den, i64::from(i32::MAX)).0
}

/// The stream a `covr` image becomes.
fn cover_stream(index: u32, cover: meta::CoverArt) -> (Stream, Reader) {
    let mut stream = Stream::new(index, MediaType::Video, ATTACHED_PIC_TIME_BASE);
    stream.disposition = Disposition::ATTACHED_PIC;
    stream.start_time = Timestamp::ZERO;
    // **Measured**: a `covr` image reports `r_frame_rate=90000/1` — the
    // reciprocal of its own time base — and `avg_frame_rate=0/0`.
    stream.r_frame_rate = ATTACHED_PIC_TIME_BASE.inverse();
    stream.params = vaco_codec_core::CodecParameters::video();
    // The reference prints `codec_tag_string=[0][0][0][0]`: a cover image is
    // not a sample entry, so it has no four-character code at all.
    stream.params.codec_tag = Some([0; 4]);
    stream.params.codec_id = match cover.data_type {
        13 => Some(CodecId::Jpeg),
        14 => Some(CodecId::Png),
        _ => None,
    };
    let reader = Reader {
        stream_index: index,
        time_base: ATTACHED_PIC_TIME_BASE,
        media_type: vaco_core::MediaType::Video,
        dts_shift: 0,
        edit_shift: 0,
        trim_point: i64::MIN,
        trim_end: i64::MAX,
        frame_samples: 0,
        source: Source::AttachedPic {
            offset: cover.offset,
            size: cover.size,
            emitted: false,
        },
        entries: Vec::new(),
        queue: VecDeque::new(),
        batch: 1,
        finished: false,
        blocked: false,
        encryption_error: None,
        raw_pcm: false,
        decrypt: None,
    };
    (stream, reader)
}

/// Rescale the aggregate duration straight from its rational representation.
fn attached_picture_duration_ticks(duration: Duration, time_base: Rational) -> Option<i64> {
    duration.to_ticks_rounding(time_base, Rounding::NearestAwayFromZero)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code, and this module needs private fields the integration tests cannot reach"
)]
mod tests {
    use super::*;
    use vaco_format_isom::build::{StblSpec, TrackSpec, bx, fullbx, trak};

    #[test]
    fn attached_picture_duration_rescales_the_exact_container_span() {
        let duration = Duration::from_fraction(783, 28_224_000).unwrap();
        assert_eq!(
            attached_picture_duration_ticks(duration, ATTACHED_PIC_TIME_BASE),
            Some(2)
        );
    }

    const TRACK_ID: u32 = 7;

    fn moov() -> Vec<u8> {
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0; 8]);
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0; 10]);
        for v in vaco_format_isom::fixed::IDENTITY_MATRIX {
            mvhd.extend_from_slice(&v.to_be_bytes());
        }
        mvhd.extend_from_slice(&[0; 24]);
        mvhd.extend_from_slice(&2u32.to_be_bytes());
        let mut out = fullbx(b"mvhd", 0, 0, &mvhd);
        let spec = TrackSpec {
            track_id: TRACK_ID,
            track_duration: 0,
            handler: *b"vide",
            timescale: 1_000,
            media_duration: 0,
            language: 0x55C4,
            elst: Vec::new(),
            stbl: StblSpec {
                stsd_box: Some({
                    let mut entry = vec![0u8; 6];
                    entry.extend_from_slice(&1u16.to_be_bytes());
                    entry.extend_from_slice(&[0; 16]);
                    entry.extend_from_slice(&160u16.to_be_bytes());
                    entry.extend_from_slice(&120u16.to_be_bytes());
                    entry.extend_from_slice(&[0; 14]);
                    entry.extend_from_slice(&1u16.to_be_bytes());
                    entry.extend_from_slice(&[0; 32]);
                    entry.extend_from_slice(&24u16.to_be_bytes());
                    entry.extend_from_slice(&0xFFFFu16.to_be_bytes());
                    let mut body = 1u32.to_be_bytes().to_vec();
                    body.extend_from_slice(&bx(b"avc1", &entry));
                    fullbx(b"stsd", 0, 0, &body)
                }),
                ..StblSpec::default()
            },
            tref: Vec::new(),
        };
        out.extend_from_slice(&trak(&spec));
        let mut trex = TRACK_ID.to_be_bytes().to_vec();
        trex.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&bx(b"mvex", &fullbx(b"trex", 0, 0, &trex)));
        out
    }

    /// One `moof` + `mdat` for `TRACK_ID`, `sizes.len()` samples of 1000
    /// ticks each, `default-base-is-moof`. Mirrors
    /// `tests/common/mod.rs::frag_unit` — duplicated rather than shared,
    /// because a unit test module cannot depend on the integration tests'
    /// support crate.
    fn unit(sequence: u32, tfdt: u64, sizes: &[u32]) -> Vec<u8> {
        let mfhd = fullbx(b"mfhd", 0, 0, &sequence.to_be_bytes());
        let tfhd = fullbx(b"tfhd", 0, 0x02_0000, &TRACK_ID.to_be_bytes());
        let tfdt_box = fullbx(b"tfdt", 1, 0, &tfdt.to_be_bytes());
        let mut trun_body = u32::try_from(sizes.len())
            .unwrap_or(0)
            .to_be_bytes()
            .to_vec();
        let data_offset_at = trun_body.len();
        trun_body.extend_from_slice(&0i32.to_be_bytes());
        for (i, &size) in sizes.iter().enumerate() {
            trun_body.extend_from_slice(&1_000u32.to_be_bytes());
            trun_body.extend_from_slice(&size.to_be_bytes());
            trun_body.extend_from_slice(
                &(if i == 0 { 0x0200_0000u32 } else { 0x0101_0000 }).to_be_bytes(),
            );
        }
        let trun = fullbx(b"trun", 0, 0x1 | 0x100 | 0x200 | 0x400, &trun_body);
        let mut traf_body = tfhd;
        traf_body.extend_from_slice(&tfdt_box);
        let trun_pos = traf_body.len();
        traf_body.extend_from_slice(&trun);
        let traf = bx(b"traf", &traf_body);
        let mut moof_body = mfhd;
        let traf_pos = moof_body.len();
        moof_body.extend_from_slice(&traf);
        let mut moof = bx(b"moof", &moof_body);
        let pos = 8 + traf_pos + 8 + trun_pos + 8 + 4 + data_offset_at;
        let data_offset = i32::try_from(moof.len() as u64 + 8).unwrap_or(i32::MAX);
        moof[pos..pos + 4].copy_from_slice(&data_offset.to_be_bytes());
        let mdat: Vec<u8> = sizes
            .iter()
            .flat_map(|&s| std::iter::repeat_n(0xCDu8, s as usize))
            .collect();
        moof.extend_from_slice(&bx(b"mdat", &mdat));
        moof
    }

    fn file(n: u32) -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&512u32.to_be_bytes());
        ftyp.extend_from_slice(b"isom");
        let mut out = bx(b"ftyp", &ftyp);
        out.extend_from_slice(&bx(b"moov", &moov()));
        for i in 0..n {
            out.extend_from_slice(&unit(i + 1, u64::from(i) * 2000, &[50, 50]));
        }
        out
    }

    fn open(data: Vec<u8>) -> Mp4Demuxer {
        let src: Box<dyn MediaSource> = Box::new(vaco_io::MemorySource::new(data));
        Mp4Demuxer::open(
            src,
            &vaco_format_core::discovery::NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .unwrap()
    }

    /// The exact capability the fast path exists for: a target beyond every
    /// fragment `collect_fragments` was allowed to keep (simulated here by
    /// truncating `self.fragments`/`self.readers[0].entries` and setting
    /// `scan_done`, standing in for hitting [`MAX_FRAGMENTS`] without
    /// building a file that large) is still reachable, by fetching exactly
    /// the one fragment `tfra` names rather than re-scanning everything
    /// before it.
    #[test]
    fn a_seek_past_the_collected_tail_is_fetched_via_tfra() {
        let mut demux = open(file(4));
        assert_eq!(demux.fragments.len(), 4);
        let offsets: Vec<u64> = demux.fragments.iter().map(|f| f.offset).collect();
        demux.tfra = vec![TrackFragmentRandomAccess {
            track_id: TRACK_ID,
            entries: (0..4u64)
                .map(|i| vaco_format_isom::frag::RandomAccessEntry {
                    time: i * 2000,
                    moof_offset: offsets[i as usize],
                    traf_number: 1,
                    trun_number: 1,
                    sample_number: 1,
                })
                .collect(),
        }];

        // Simulate "only the first fragment was ever collected", as
        // `MAX_FRAGMENTS` would if the file had that many fragments —
        // *after* recording `tfra` from the complete scan, matching how a
        // real file's `mfra` trailer (read once, up front, regardless of how
        // far the eager scan gets) would already know about fragments the
        // scan itself has not reached yet.
        demux.fragments.truncate(1);
        demux.readers[0].entries.truncate(1);
        demux.scan_done = true;

        // Land inside fragment 2 (media time in [4000, 6000)).
        let landed = demux.place_fragment(0, 4500, false);
        assert_eq!(landed, 4000, "lands on fragment 2's sync sample");
        // Exactly one fragment was fetched to answer this — not fragment 1,
        // which the fast path had no reason to visit.
        assert_eq!(demux.fragments.len(), 2);
        assert_eq!(demux.fragments[1].offset, offsets[2]);
    }

    /// The fast path must never answer differently than the fallback scan —
    /// only faster. Checked across every fragment boundary of a small file,
    /// once with `tfra` available and once with it cleared on the same
    /// instance (so everything else about the demuxer is held fixed).
    #[test]
    fn tfra_and_the_fallback_scan_agree_on_every_landing() {
        let mut demux = open(file(6));
        assert!(
            demux.tfra.is_empty(),
            "fixture file carries no mfra trailer yet"
        );
        // This fixture has no `mfra` trailer (`file` appends none), so build
        // one directly from what `collect_fragments` already found — the
        // fast path is then exercised exactly as a real file's `mfra` would
        // drive it.
        let offsets: Vec<u64> = demux.fragments.iter().map(|f| f.offset).collect();
        demux.tfra = vec![TrackFragmentRandomAccess {
            track_id: TRACK_ID,
            entries: (0..offsets.len() as u64)
                .map(|i| vaco_format_isom::frag::RandomAccessEntry {
                    time: i * 2000,
                    moof_offset: offsets[i as usize],
                    traf_number: 1,
                    trun_number: 1,
                    sample_number: 1,
                })
                .collect(),
        }];
        for target in [-1i64, 0, 500, 1999, 2000, 5999, 11999, 100_000] {
            let with_fast_path = demux.place_fragment(0, target, false);
            demux.tfra.clear();
            let with_fallback = demux.place_fragment(0, target, false);
            // Restore `tfra` for the next iteration.
            demux.tfra = vec![TrackFragmentRandomAccess {
                track_id: TRACK_ID,
                entries: (0..offsets.len() as u64)
                    .map(|i| vaco_format_isom::frag::RandomAccessEntry {
                        time: i * 2000,
                        moof_offset: offsets[i as usize],
                        traf_number: 1,
                        trun_number: 1,
                        sample_number: 1,
                    })
                    .collect(),
            }];
            assert_eq!(with_fast_path, with_fallback, "target {target}");
        }
    }
}
