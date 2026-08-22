//! The `moov` tree: `ftyp`, `mvhd`, `trak ▸ tkhd/edts/mdia ▸ minf ▸ stbl`.
//!
//! ISO/IEC 14496-12 §8.2 to §8.4. Assembly is a fixed nest of `for` loops, not
//! a recursive descent — the shape of the tree is a compile-time fact, so no
//! input can deepen it (see [`crate::boxes`]).
//!
//! Everything borrows. A [`Movie`] holds no copy of any table; it is a set of
//! offsets and small headers over the `moov` buffer the caller supplied. The
//! only allocations are the per-track `Vec`s of edit entries, track references
//! and run summaries, all of which are bounded by constants rather than by
//! declared counts.
//!
//! # The two time bases
//!
//! `mvhd.timescale` is the **movie** timescale; `mdhd.timescale` is the
//! **media** timescale and is the one a stream's timestamps are counted in.
//! `tkhd.duration` and every `elst.segment_duration` are in the movie
//! timescale; `mdhd.duration` and every `elst.media_time` are in the media
//! timescale. [`Track::time_base`] returns the media one, because that is what
//! a `Stream` needs, and every conversion goes through
//! [`crate::edit::rescale_movie_to_media`] so the direction is stated at the
//! call site.

use vaco_core::{Error, MediaType, Rational, Result};

use crate::boxes::{BoxIter, IsoBox};
use crate::edit::{EditList, Timeline, rescale_movie_to_media};
use crate::fixed::{DisplayMatrix, fp8, fp16, fp16u};
use crate::fourcc::{FourCc, boxes};
use crate::frag::{MovieFragment, SegmentIndex, TrackExtends, TrackFragmentRandomAccess};
use crate::lang::Language;
use crate::stbl::SampleTable;

/// Seconds between the ISOBMFF epoch (1904-01-01 UTC) and the Unix epoch.
pub const EPOCH_OFFSET_SECS: i64 = 2_082_844_800;

/// Largest number of tracks kept from one `moov`.
///
/// Each track costs a few hundred bytes of headers plus its run summaries, so
/// this bounds a `moov` full of empty `trak`s at a few tens of megabytes rather
/// than at whatever the file's size allows.
pub const MAX_TRACKS: usize = 4096;
/// Largest number of track references kept per `tref` type.
pub const MAX_TRACK_REFERENCES: usize = 4096;
/// Largest number of `moof` boxes collected by [`IsoFile::parse`].
pub const MAX_FRAGMENTS: usize = 65_536;
/// Largest number of compatible brands kept from `ftyp`.
pub const MAX_BRANDS: usize = 1024;

/// `ftyp` or `styp` (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileType {
    /// `major_brand`.
    pub major_brand: FourCc,
    /// `minor_version`.
    pub minor_version: u32,
    /// `compatible_brands`, in file order.
    pub compatible_brands: Vec<FourCc>,
}

impl FileType {
    /// Parse an `ftyp`/`styp` payload.
    #[must_use]
    pub fn parse(b: &IsoBox<'_>) -> Self {
        let mut r = vaco_bitstream::ByteReader::new(b.payload);
        let major_brand = FourCc(<[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4]));
        let minor_version = r.be32();
        let mut compatible_brands = Vec::new();
        while compatible_brands.len() < MAX_BRANDS && r.remaining() >= 4 {
            compatible_brands.push(FourCc(<[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4])));
        }
        Self {
            major_brand,
            minor_version,
            compatible_brands,
        }
    }

    /// Whether `brand` is the major brand or among the compatible ones.
    #[must_use]
    pub fn has_brand(&self, brand: FourCc) -> bool {
        self.major_brand == brand || self.compatible_brands.contains(&brand)
    }
}

/// `mvhd` (§8.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieHeader {
    /// Seconds since 1904-01-01 UTC.
    pub creation_time: u64,
    /// Seconds since 1904-01-01 UTC.
    pub modification_time: u64,
    /// The movie timescale, in ticks per second.
    pub timescale: u32,
    /// Movie duration in movie ticks.
    pub duration: u64,
    /// Preferred playback rate, 16.16.
    pub rate: Rational,
    /// Preferred volume, 8.8.
    pub volume: Rational,
    /// The display matrix.
    pub matrix: DisplayMatrix,
    /// `next_track_ID`.
    pub next_track_id: u32,
}

impl Default for MovieHeader {
    fn default() -> Self {
        Self {
            creation_time: 0,
            modification_time: 0,
            timescale: 0,
            duration: 0,
            rate: Rational::new(1, 1),
            volume: Rational::new(1, 1),
            matrix: DisplayMatrix::default(),
            next_track_id: 0,
        }
    }
}

impl MovieHeader {
    /// Parse an `mvhd` full box.
    #[must_use]
    pub fn parse(full: &crate::boxes::FullBox<'_>) -> Self {
        let mut r = full.reader();
        let (creation_time, modification_time, timescale, duration) = if full.version == 1 {
            (r.be64(), r.be64(), r.be32(), r.be64())
        } else {
            (
                u64::from(r.be32()),
                u64::from(r.be32()),
                r.be32(),
                u64::from(r.be32()),
            )
        };
        let rate = fp16(r.be32());
        let volume = fp8(r.be16());
        let _reserved = r.be16();
        let _reserved2 = r.be64();
        let matrix = DisplayMatrix::parse(&mut r);
        let _pre_defined = r.bytes(24);
        let next_track_id = r.be32();
        Self {
            creation_time,
            modification_time,
            timescale,
            duration,
            rate,
            volume,
            matrix,
            next_track_id,
        }
    }

    /// The movie time base, `1 / timescale`.
    ///
    /// Undefined when the timescale is zero, which §8.2.2.3 forbids and files
    /// nevertheless contain.
    #[must_use]
    pub fn time_base(&self) -> Rational {
        Rational::new(1, i32::try_from(self.timescale).unwrap_or(i32::MAX))
    }
}

/// `tkhd` (§8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrackHeader {
    /// Seconds since 1904-01-01 UTC.
    pub creation_time: u64,
    /// Seconds since 1904-01-01 UTC.
    pub modification_time: u64,
    /// `track_ID`, the container's own stream identifier.
    pub track_id: u32,
    /// Duration in **movie** ticks.
    pub duration: u64,
    /// Compositing layer; lower is nearer the viewer.
    pub layer: i16,
    /// Alternate group; tracks in one group are alternatives.
    pub alternate_group: i16,
    /// Volume, 8.8. Nonzero only for audio tracks.
    pub volume: Rational,
    /// The display matrix, from which rotation is derived.
    pub matrix: DisplayMatrix,
    /// Display width, 16.16 — *not* the coded width.
    pub width: Rational,
    /// Display height, 16.16.
    pub height: Rational,
    /// The `tkhd` flags word.
    pub flags: u32,
}

impl TrackHeader {
    /// `track_enabled`.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.flags & 0x1 != 0
    }

    /// `track_in_movie`.
    #[must_use]
    pub const fn is_in_movie(&self) -> bool {
        self.flags & 0x2 != 0
    }

    /// `track_in_preview`.
    #[must_use]
    pub const fn is_in_preview(&self) -> bool {
        self.flags & 0x4 != 0
    }

    /// Parse a `tkhd` full box.
    #[must_use]
    pub fn parse(full: &crate::boxes::FullBox<'_>) -> Self {
        let mut r = full.reader();
        let (creation_time, modification_time) = if full.version == 1 {
            (r.be64(), r.be64())
        } else {
            (u64::from(r.be32()), u64::from(r.be32()))
        };
        let track_id = r.be32();
        let _reserved = r.be32();
        let duration = if full.version == 1 {
            r.be64()
        } else {
            u64::from(r.be32())
        };
        let _reserved2 = r.be64();
        let layer = r.be16().cast_signed();
        let alternate_group = r.be16().cast_signed();
        let volume = fp8(r.be16());
        let _reserved3 = r.be16();
        let matrix = DisplayMatrix::parse(&mut r);
        let width = fp16u(r.be32());
        let height = fp16u(r.be32());
        Self {
            creation_time,
            modification_time,
            track_id,
            duration,
            layer,
            alternate_group,
            volume,
            matrix,
            width,
            height,
            flags: full.flags,
        }
    }
}

/// `mdhd` (§8.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaHeader {
    /// Seconds since 1904-01-01 UTC.
    pub creation_time: u64,
    /// Seconds since 1904-01-01 UTC.
    pub modification_time: u64,
    /// The **media** timescale — the unit every timestamp on this track uses.
    pub timescale: u32,
    /// Duration in media ticks.
    pub duration: u64,
    /// The packed language field.
    pub language: Language,
}

impl MediaHeader {
    /// Parse an `mdhd` full box.
    #[must_use]
    pub fn parse(full: &crate::boxes::FullBox<'_>) -> Self {
        let mut r = full.reader();
        let (creation_time, modification_time, timescale, duration) = if full.version == 1 {
            (r.be64(), r.be64(), r.be32(), r.be64())
        } else {
            (
                u64::from(r.be32()),
                u64::from(r.be32()),
                r.be32(),
                u64::from(r.be32()),
            )
        };
        let language = Language::unpack(r.be16());
        Self {
            creation_time,
            modification_time,
            timescale,
            duration,
            language,
        }
    }
}

/// One `tref` group: a reference type and the track ids it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackReference {
    /// The reference type, e.g. `chap`, `cdsc`, `tmcd`.
    pub kind: FourCc,
    /// The referenced `track_ID`s.
    pub track_ids: Vec<u32>,
}

/// What `dref` says about where a track's samples live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataReferences {
    /// Entries declared.
    pub count: u32,
    /// Whether every entry is self-contained.
    ///
    /// An entry with the `self_contained` flag clear points at *another file*.
    /// `planning/18-formats.md` §3.1.10 refuses those by default, and rightly:
    /// following one is a file-system read chosen by file content. This crate
    /// reports the fact and reads nothing.
    pub all_self_contained: bool,
}

/// One `trak`.
#[derive(Debug, Clone)]
pub struct Track<'a> {
    /// `tkhd`.
    pub header: TrackHeader,
    /// `mdhd`.
    pub media: MediaHeader,
    /// `hdlr.handler_type`.
    pub handler: FourCc,
    /// `hdlr`'s trailing name field, raw.
    pub handler_name: &'a [u8],
    /// `elng`'s BCP-47 tag, which wins over `mdhd`'s packed language.
    pub extended_language: Option<&'a [u8]>,
    /// `edts ▸ elst`.
    pub edits: Option<EditList>,
    /// The sample tables.
    pub sample_table: SampleTable<'a>,
    /// `tref` groups.
    pub references: Vec<TrackReference>,
    /// What `dref` declares.
    pub data_references: DataReferences,
}

impl<'a> Track<'a> {
    /// The media time base, `1 / mdhd.timescale`.
    ///
    /// This is a stream's `time_base`. `ffprobe 8.1` printed `1/12800` for the
    /// video track and `1/44100` for the audio track of the calibration file,
    /// matching each track's `mdhd`.
    #[must_use]
    pub fn time_base(&self) -> Rational {
        Rational::new(1, i32::try_from(self.media.timescale).unwrap_or(i32::MAX))
    }

    /// Whether the track's timescale is usable.
    ///
    /// A zero `mdhd.timescale` is a division by zero waiting to happen and a
    /// guaranteed fuzz finding; §3.1.10 of the format plan says to drop such a
    /// track, and this is the predicate for it.
    #[must_use]
    pub const fn has_usable_timescale(&self) -> bool {
        self.media.timescale != 0
    }

    /// The media type the handler implies.
    #[must_use]
    pub fn media_type(&self) -> Option<MediaType> {
        crate::stsd::handler_media_type(self.handler)
    }

    /// The `hdlr` name as text, when it is valid UTF-8.
    ///
    /// Both conventions occur: a null-terminated C string (MP4) and a
    /// length-prefixed Pascal string (`QuickTime`). The disambiguation is
    /// heuristic and stated as such — if the first byte equals the remaining
    /// length, it is read as Pascal.
    #[must_use]
    pub fn handler_name_str(&self) -> Option<&'a str> {
        let raw = self.handler_name;
        let first = raw.first().copied()?;
        let body = if usize::from(first).saturating_add(1) == raw.len() {
            raw.get(1..)?
        } else {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            raw.get(..end)?
        };
        core::str::from_utf8(body).ok()
    }

    /// The language tag to report: `elng` when present, else `mdhd`.
    #[must_use]
    pub fn language_tag(&self) -> &str {
        if let Some(raw) = self.extended_language {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            if let Some(s) = raw.get(..end).and_then(|b| core::str::from_utf8(b).ok())
                && !s.is_empty()
            {
                return s;
            }
        }
        self.media.language.tag()
    }

    /// Whether every sample lives in this file.
    #[must_use]
    pub const fn is_self_contained(&self) -> bool {
        self.data_references.all_self_contained
    }

    /// The resolved edit timeline, in media ticks.
    #[must_use]
    pub fn timeline(&self, movie_timescale: u32) -> Timeline {
        let media_duration = i64::try_from(self.media.duration).unwrap_or(i64::MAX);
        match &self.edits {
            Some(e) if !e.entries.is_empty() => {
                e.resolve(movie_timescale, self.media.timescale, media_duration)
            }
            _ => Timeline::identity(media_duration),
        }
    }

    /// The shift a demuxer applies to every PTS and DTS on this track, in media
    /// ticks.
    ///
    /// Zero when there is no edit list. See [`EditList::simple_shift`] for the
    /// measurements this reproduces.
    #[must_use]
    pub fn edit_shift(&self, movie_timescale: u32) -> i64 {
        self.edits
            .as_ref()
            .map_or(0, |e| e.simple_shift(movie_timescale, self.media.timescale))
    }

    /// Duration in media ticks, as `ffprobe` reports `duration_ts`.
    ///
    /// # Measured
    ///
    /// `ffprobe 8.1` prints the **played** edit duration rescaled into the
    /// media timescale, not `mdhd.duration`:
    ///
    /// | Track | `mdhd.duration` | `elst` played | `duration_ts` |
    /// |---|---:|---:|---:|
    /// | `prog.mp4` video | 26 112 | 2000 movie @12800/1000 | **25 600** |
    /// | `prog.mp4` audio | 89 224 | 2000 movie @44100/1000 | **88 200** |
    /// | `delay.mp4` video | 25 600 | 2000 (empty edit excluded) | **25 600** |
    ///
    /// The third row is the discriminating one: had the empty edit counted, it
    /// would read 32 256. With no edit list at all, `mdhd.duration` is used.
    #[must_use]
    pub fn reported_duration(&self, movie_timescale: u32) -> u64 {
        match &self.edits {
            Some(e) if !e.entries.is_empty() => {
                let played = i64::try_from(e.played_duration_movie()).unwrap_or(i64::MAX);
                rescale_movie_to_media(played, movie_timescale, self.media.timescale)
                    .try_into()
                    .unwrap_or(0)
            }
            _ => self.media.duration,
        }
    }

    /// Parse one `trak`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed child or a missing `mdia ▸ minf ▸
    /// stbl`.
    pub fn parse(trak: &IsoBox<'a>) -> Result<Self> {
        let mut header = TrackHeader::default();
        let mut edits = None;
        let mut references = Vec::new();
        let mut mdia = None;
        for child in trak.children() {
            let child = child?;
            match child.kind() {
                boxes::TKHD => header = TrackHeader::parse(&child.full()?),
                boxes::EDTS => {
                    if let Some(elst) = child.children().find(boxes::ELST) {
                        edits = Some(EditList::parse(&elst.full()?));
                    }
                }
                boxes::TREF => {
                    for r in child.children() {
                        let r = r?;
                        let mut ids = Vec::new();
                        let mut rd = vaco_bitstream::ByteReader::new(r.payload);
                        while ids.len() < MAX_TRACK_REFERENCES && rd.remaining() >= 4 {
                            ids.push(rd.be32());
                        }
                        references.push(TrackReference {
                            kind: r.kind(),
                            track_ids: ids,
                        });
                    }
                }
                boxes::MDIA => mdia = Some(child),
                _ => {}
            }
        }
        let mdia = mdia.ok_or(Error::InvalidData("isom: trak without an mdia"))?;

        let mut media = MediaHeader::default();
        let mut handler = FourCc([0; 4]);
        let mut handler_name: &[u8] = &[];
        let mut extended_language = None;
        let mut minf = None;
        for child in mdia.children() {
            let child = child?;
            match child.kind() {
                boxes::MDHD => media = MediaHeader::parse(&child.full()?),
                boxes::HDLR => {
                    let full = child.full()?;
                    let mut r = full.reader();
                    let _pre_defined = r.be32();
                    handler = FourCc(<[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4]));
                    let _reserved = r.bytes(12);
                    handler_name = full.body.get(r.pos()..).unwrap_or(&[]);
                }
                boxes::ELNG => {
                    extended_language = Some(child.full()?.body);
                }
                boxes::MINF => minf = Some(child),
                _ => {}
            }
        }
        let minf = minf.ok_or(Error::InvalidData("isom: mdia without a minf"))?;

        let mut data_references = DataReferences::default();
        let mut stbl = None;
        for child in minf.children() {
            let child = child?;
            match child.kind() {
                boxes::DINF => {
                    if let Some(dref) = child.children().find(boxes::DREF) {
                        data_references = parse_dref(&dref)?;
                    }
                }
                boxes::STBL => stbl = Some(child),
                _ => {}
            }
        }
        let stbl = stbl.ok_or(Error::InvalidData("isom: minf without an stbl"))?;

        Ok(Self {
            header,
            media,
            handler,
            handler_name,
            extended_language,
            edits,
            sample_table: SampleTable::parse(&stbl)?,
            references,
            data_references,
        })
    }
}

fn parse_dref(dref: &IsoBox<'_>) -> Result<DataReferences> {
    let full = dref.full()?;
    let count = full
        .body
        .first_chunk::<4>()
        .map_or(0, |b| u32::from_be_bytes(*b));
    let mut all_self_contained = true;
    let mut seen = 0u32;
    for entry in dref.children_after(8) {
        let entry = entry?;
        seen = seen.saturating_add(1);
        let f = entry.full()?;
        if f.flags & 0x1 == 0 {
            all_self_contained = false;
        }
    }
    Ok(DataReferences {
        count: count.min(seen),
        // A `dref` with no entries at all is self-contained by convention:
        // every sample offset is a position in this file.
        all_self_contained,
    })
}

/// A parsed `moov`.
#[derive(Debug, Clone)]
pub struct Movie<'a> {
    /// `mvhd`.
    pub header: MovieHeader,
    /// The tracks, in file order.
    pub tracks: Vec<Track<'a>>,
    /// `mvex ▸ trex` rows; non-empty means the file is fragmented.
    pub extends: Vec<TrackExtends>,
    /// `mvex ▸ mehd.fragment_duration`, in movie ticks.
    pub fragment_duration: Option<u64>,
    /// The `udta` box, unparsed. Metadata mapping is the demuxer's, not the box
    /// layer's; see the crate doc file's *Deferred* section.
    pub udta: Option<IsoBox<'a>>,
}

impl<'a> Movie<'a> {
    /// Parse a `moov` container.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed child. A `trak` that fails to
    /// parse is **skipped**, not fatal: a file with one broken track and three
    /// good ones is worth opening, and that is what the reference does.
    pub fn parse(moov: &IsoBox<'a>) -> Result<Self> {
        let mut header = MovieHeader::default();
        let mut tracks = Vec::new();
        let mut extends = Vec::new();
        let mut fragment_duration = None;
        let mut udta = None;
        for child in moov.children() {
            let child = child?;
            match child.kind() {
                boxes::MVHD => header = MovieHeader::parse(&child.full()?),
                boxes::TRAK => {
                    if tracks.len() < MAX_TRACKS
                        && let Ok(t) = Track::parse(&child)
                    {
                        tracks.push(t);
                    }
                }
                boxes::MVEX => {
                    extends = crate::frag::parse_mvex(&child)?;
                    if let Some(mehd) = child.children().find(boxes::MEHD) {
                        let full = mehd.full()?;
                        let mut r = full.reader();
                        fragment_duration = Some(if full.version == 1 {
                            r.be64()
                        } else {
                            u64::from(r.be32())
                        });
                    }
                }
                boxes::UDTA => udta = Some(child),
                _ => {}
            }
        }
        Ok(Self {
            header,
            tracks,
            extends,
            fragment_duration,
            udta,
        })
    }

    /// Whether the file declares fragments.
    #[must_use]
    pub fn is_fragmented(&self) -> bool {
        !self.extends.is_empty()
    }

    /// The `trex` row for `track_id`, or a zeroed one.
    #[must_use]
    pub fn extends_for(&self, track_id: u32) -> TrackExtends {
        self.extends
            .iter()
            .find(|t| t.track_id == track_id)
            .copied()
            .unwrap_or_default()
    }

    /// The track with `track_id`.
    #[must_use]
    pub fn track_by_id(&self, track_id: u32) -> Option<&Track<'a>> {
        self.tracks.iter().find(|t| t.header.track_id == track_id)
    }
}

/// Everything a whole-file parse finds.
///
/// Suitable for a file small enough to hold in memory — a CMAF segment, a
/// fuzz input, a test fixture. A real demuxer uses [`crate::scan`] to locate
/// `moov` without reading `mdat`, then calls [`Movie::parse`] on it.
#[derive(Debug, Clone, Default)]
pub struct IsoFile<'a> {
    /// `ftyp`, or `styp` for a segment.
    pub file_type: Option<FileType>,
    /// The `moov`, when there is one.
    pub movie: Option<Movie<'a>>,
    /// Every `moof`, in file order.
    pub fragments: Vec<MovieFragment<'a>>,
    /// Every top-level `sidx`.
    pub segment_indexes: Vec<SegmentIndex>,
    /// `mfra ▸ tfra` tables.
    pub random_access: Vec<TrackFragmentRandomAccess>,
    /// Absolute offset and size of each `mdat`.
    pub media_data: Vec<(u64, u64)>,
}

impl<'a> IsoFile<'a> {
    /// Walk the top-level boxes of `data`, whose first byte is at file offset
    /// `base`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed top-level box chain.
    pub fn parse(data: &'a [u8], base: u64) -> Result<Self> {
        let mut me = Self::default();
        for b in BoxIter::new(data, base) {
            let b = b?;
            match b.kind() {
                boxes::FTYP | boxes::STYP if me.file_type.is_none() => {
                    me.file_type = Some(FileType::parse(&b));
                }
                // "The first `moov` wins; the second is skipped with a
                // warning" (§3.1.10). Skipping silently here is deliberate:
                // this layer has no logger, and the caller can see there was
                // more than one by walking the boxes itself.
                boxes::MOOV if me.movie.is_none() => me.movie = Some(Movie::parse(&b)?),
                boxes::MOOF => {
                    if me.fragments.len() < MAX_FRAGMENTS {
                        me.fragments.push(MovieFragment::parse(&b)?);
                    }
                }
                boxes::SIDX => {
                    if let Some(s) = SegmentIndex::parse(&b) {
                        me.segment_indexes.push(s);
                    }
                }
                boxes::MFRA => me.random_access = crate::frag::parse_mfra(&b),
                boxes::MDAT => me
                    .media_data
                    .push((b.payload_offset(), b.header.payload_len())),
                _ => {}
            }
        }
        Ok(me)
    }
}

/// Convert an ISOBMFF timestamp to seconds since the Unix epoch.
///
/// `None` for a value that would not fit `i64` after the shift. Note that
/// `-fflags +bitexact` suppresses these fields entirely on the mux side, so a
/// conformance comparison should not depend on them.
#[must_use]
pub fn to_unix_time(iso: u64) -> Option<i64> {
    i64::try_from(iso).ok()?.checked_sub(EPOCH_OFFSET_SECS)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::build::{StblSpec, TrackSpec};
    use crate::testutil::{bx, first_box, fullbx};

    fn calibration_file() -> Vec<u8> {
        // prog.mp4's shape: mvhd ts 1000 dur 2000; video track mdhd ts 12800
        // dur 26112, elst (2000, 1024); audio track mdhd ts 44100 dur 89224,
        // elst (2000, 1024).
        let video = TrackSpec {
            track_id: 1,
            track_duration: 2000,
            handler: *b"vide",
            timescale: 12_800,
            media_duration: 26_112,
            elst: vec![(2000, 1024, 1)],
            stbl: StblSpec {
                stts: vec![(50, 512)],
                ctts_v0: vec![(1, 1024), (1, 2048), (48, 512)],
                stss: vec![1, 16, 31, 46],
                stsc: vec![(1, 2, 1), (2, 1, 1)],
                stsz: vec![4822, 1668, 1011, 629],
                stco: vec![3017, 9765, 11181],
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let audio = TrackSpec {
            track_id: 2,
            track_duration: 2000,
            handler: *b"soun",
            timescale: 44_100,
            media_duration: 89_224,
            elst: vec![(2000, 1024, 1)],
            stbl: StblSpec {
                stts: vec![(87, 1024), (1, 136)],
                stsc: vec![(1, 1, 1)],
                stsz: vec![258, 258],
                stco: vec![9507, 10776],
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        crate::build::file(b"isom", 1000, 2000, &[video, audio])
    }

    #[test]
    fn the_calibration_file_parses_into_two_tracks() {
        let raw = calibration_file();
        let f = IsoFile::parse(&raw, 0).unwrap();
        let ft = f.file_type.unwrap();
        assert_eq!(ft.major_brand, FourCc::new(b"isom"));
        assert!(ft.has_brand(FourCc::new(b"isom")));
        let m = f.movie.unwrap();
        assert_eq!(m.header.timescale, 1000);
        assert_eq!(m.header.duration, 2000);
        assert_eq!(m.tracks.len(), 2);
        assert!(!m.is_fragmented());
        assert_eq!(f.media_data.len(), 1);
    }

    #[test]
    fn each_track_reports_the_time_base_ffprobe_printed() {
        let raw = calibration_file();
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        assert_eq!(m.tracks[0].time_base(), Rational::new(1, 12_800));
        assert_eq!(m.tracks[1].time_base(), Rational::new(1, 44_100));
        assert_eq!(m.tracks[0].media_type(), Some(MediaType::Video));
        assert_eq!(m.tracks[1].media_type(), Some(MediaType::Audio));
        assert_eq!(m.tracks[0].header.track_id, 1);
        assert!(m.tracks[0].header.is_enabled());
        assert!(m.tracks[0].header.is_in_movie());
    }

    #[test]
    fn duration_ts_matches_the_measured_reference_values() {
        let raw = calibration_file();
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        let ts = m.header.timescale;
        // ffprobe printed 25600 and 88200 respectively, not 26112 and 89224.
        assert_eq!(m.tracks[0].reported_duration(ts), 25_600);
        assert_eq!(m.tracks[1].reported_duration(ts), 88_200);
    }

    #[test]
    fn the_edit_shift_matches_the_measured_packet_timestamps() {
        let raw = calibration_file();
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        let ts = m.header.timescale;
        assert_eq!(m.tracks[0].edit_shift(ts), -1024);
        // Sample 0: raw dts 0, ctts 1024 -> pts 1024, dts 0; after the shift,
        // ffprobe printed pts=0 dts=-1024.
        let s = m.tracks[0].sample_table.sample(0).unwrap();
        assert_eq!(s.pts() + m.tracks[0].edit_shift(ts), 0);
        assert_eq!(s.dts + m.tracks[0].edit_shift(ts), -1024);
        assert_eq!(s.offset, 3017);
    }

    #[test]
    fn a_missing_edit_list_leaves_timestamps_alone() {
        let spec = TrackSpec {
            media_duration: 1000,
            elst: vec![],
            stbl: StblSpec {
                stts: vec![(2, 100)],
                stsc: vec![(1, 2, 1)],
                stco: vec![10],
                stsz: vec![1, 1],
                stss: vec![1],
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let raw = crate::build::file(b"isom", 1000, 1000, &[spec]);
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        assert_eq!(m.tracks[0].edit_shift(1000), 0);
        assert_eq!(m.tracks[0].reported_duration(1000), 1000);
        assert_eq!(m.tracks[0].timeline(1000).start_offset(), 0);
    }

    #[test]
    fn a_zero_media_timescale_is_reported_not_divided_by() {
        let spec = TrackSpec {
            timescale: 0,
            ..TrackSpec::default()
        };
        let raw = crate::build::file(b"isom", 1000, 0, &[spec]);
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        assert!(!m.tracks[0].has_usable_timescale());
        // The time base is undefined rather than a panic.
        let _ = m.tracks[0].time_base();
    }

    #[test]
    fn a_trak_without_an_mdia_is_skipped_not_fatal() {
        let good = crate::build::trak(&TrackSpec::default());
        let bad = bx(b"trak", &fullbx(b"tkhd", 0, 3, &[0; 80]));
        let mut moov = fullbx(b"mvhd", 0, 0, &[0; 96]);
        moov.extend_from_slice(&bad);
        moov.extend_from_slice(&good);
        let raw = bx(b"moov", &moov);
        let m = Movie::parse(&first_box(&raw)).unwrap();
        assert_eq!(m.tracks.len(), 1);
    }

    #[test]
    fn language_comes_from_mdhd_and_elng_wins() {
        let raw = calibration_file();
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        assert_eq!(m.tracks[0].language_tag(), "und");
        assert_eq!(m.tracks[0].handler_name_str(), Some("Fixture"));
    }

    #[test]
    fn handler_names_parse_in_both_conventions() {
        // C string.
        let mut t = TrackSpec::default();
        t.stbl.stts = vec![(1, 1)];
        let raw = crate::build::file(b"isom", 1000, 0, &[t]);
        let m = IsoFile::parse(&raw, 0).unwrap().movie.unwrap();
        assert_eq!(m.tracks[0].handler_name_str(), Some("Fixture"));
    }

    #[test]
    fn a_fragmented_moov_reports_its_trex_rows() {
        let mut trex_body = 1u32.to_be_bytes().to_vec();
        for v in [1u32, 512, 4822, 0] {
            trex_body.extend_from_slice(&v.to_be_bytes());
        }
        let mvex = bx(b"mvex", &fullbx(b"trex", 0, 0, &trex_body));
        let mut moov = fullbx(b"mvhd", 0, 0, &[0; 96]);
        moov.extend_from_slice(&crate::build::trak(&TrackSpec::default()));
        moov.extend_from_slice(&mvex);
        let raw = bx(b"moov", &moov);
        let m = Movie::parse(&first_box(&raw)).unwrap();
        assert!(m.is_fragmented());
        assert_eq!(m.extends_for(1).default_sample_duration, 512);
        assert_eq!(m.extends_for(99), TrackExtends::default());
    }

    #[test]
    fn a_dref_pointing_at_another_file_is_reported() {
        // A `url ` entry with flags == 0 is external.
        let dref_body = {
            let mut b = 1u32.to_be_bytes().to_vec();
            b.extend_from_slice(&fullbx(b"url ", 0, 0, b"other.mp4\0"));
            b
        };
        let raw = fullbx(b"dref", 0, 0, &dref_body);
        let d = parse_dref(&first_box(&raw)).unwrap();
        assert!(!d.all_self_contained);
        assert_eq!(d.count, 1);
    }

    #[test]
    fn a_self_contained_dref_is_the_normal_case() {
        let dref_body = {
            let mut b = 1u32.to_be_bytes().to_vec();
            b.extend_from_slice(&fullbx(b"url ", 0, 1, &[]));
            b
        };
        let raw = fullbx(b"dref", 0, 0, &dref_body);
        assert!(parse_dref(&first_box(&raw)).unwrap().all_self_contained);
    }

    #[test]
    fn track_references_are_grouped_by_type() {
        let mut tref_body = bx(b"chap", &2u32.to_be_bytes());
        tref_body.extend_from_slice(&bx(b"tmcd", &3u32.to_be_bytes()));
        let base = crate::build::trak(&TrackSpec::default());
        let built = first_box(&base);
        let mdia = built
            .children()
            .flatten()
            .find(|b| b.kind() == boxes::MDIA)
            .unwrap();
        let mut trak_body = fullbx(b"tkhd", 0, 3, &[0; 80]);
        trak_body.extend_from_slice(&bx(b"tref", &tref_body));
        trak_body.extend_from_slice(&bx(b"mdia", mdia.payload));
        let raw = bx(b"trak", &trak_body);
        let t = Track::parse(&first_box(&raw)).unwrap();
        assert_eq!(t.references.len(), 2);
        assert_eq!(t.references[0].kind, boxes::CHAP);
        assert_eq!(t.references[0].track_ids, vec![2]);
    }

    #[test]
    fn the_first_moov_wins() {
        let raw = calibration_file();
        let mut doubled = raw.clone();
        doubled.extend_from_slice(&bx(b"moov", &fullbx(b"mvhd", 0, 0, &[0; 96])));
        let f = IsoFile::parse(&doubled, 0).unwrap();
        assert_eq!(f.movie.unwrap().header.timescale, 1000);
    }

    #[test]
    fn epoch_conversion_lands_on_the_unix_epoch() {
        assert_eq!(to_unix_time(EPOCH_OFFSET_SECS as u64), Some(0));
        assert_eq!(to_unix_time(0), Some(-EPOCH_OFFSET_SECS));
        assert_eq!(to_unix_time(u64::MAX), None);
    }

    #[test]
    fn an_ftyp_with_a_ragged_tail_keeps_the_whole_brands_it_has() {
        let raw = bx(b"ftyp", b"isom\0\0\x02\0isomiso2av");
        let ft = FileType::parse(&first_box(&raw));
        assert_eq!(ft.major_brand, FourCc::new(b"isom"));
        assert_eq!(ft.minor_version, 512);
        assert_eq!(ft.compatible_brands.len(), 2);
    }

    #[test]
    fn an_empty_file_yields_an_empty_parse() {
        let f = IsoFile::parse(&[], 0).unwrap();
        assert!(f.movie.is_none());
        assert!(f.file_type.is_none());
        assert!(f.fragments.is_empty());
    }
}
