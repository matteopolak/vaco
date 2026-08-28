//! `ImfDemuxer`: the seam between a parsed [`Cpl`]'s virtual tracks
//! (`cpl.rs`) and the OP-Atom MXF essence they name, resolved through a
//! [`Package`] (`package.rs`) and read frame-by-frame via
//! [`vaco_demux_mxf::MxfDemuxer::read_edit_unit`].
//!
//! # The two-call open this format needs
//!
//! [`ImfDemuxer::open`] only ever sees the Composition Playlist itself — the
//! `Box<dyn MediaSource>` a normal registry lookup opens from the URL the
//! caller named. That is enough to parse every `Segment`/`Sequence`/
//! `Resource` and build one [`vaco_format_core::Stream`] per virtual track,
//! but **not** enough to know real codec parameters (resolution, sample
//! rate — anything that lives in an essence file's own header, not the
//! CPL's XML), because a CPL never restates them and this crate has no
//! second `MediaSource` for the track files it references.
//!
//! [`ImfDemuxer::bind_url`] is where the real work happens: given the CPL's
//! own path (`vaco_format_core::Demuxer::bind_url`'s docs, and gap 7 in
//! `planning/INTERFACE-GAPS.md`, name exactly this shape — "a sidecar file
//! whose name is a convention relative to this one"), it resolves the
//! ASSETMAP and opens the first resource of every virtual track to fill in
//! real [`vaco_core::CodecParameters`]. The `vaco-cli` input path
//! (`crates/app/vaco-cli/src/input.rs`) already calls `bind_url` once,
//! immediately after `open`, for every non-`NEEDNUMBER` format — this is a
//! `NEEDNUMBER`-shaped need without being one, since `open` itself always
//! succeeds (there is always a real CPL file to open), only `streams()`'s
//! *content* is incomplete until `bind_url` runs. A caller that skips
//! `bind_url` (a fuzz target driving `(desc.open)(..)` directly, say) gets a
//! demuxer whose `streams()` answers with bare video/audio placeholders and
//! whose `read_packet` reports [`Error::NotSeekable`] rather than a panic.
//!
//! # A known scope limit: one edit rate assumed shared with every essence file
//!
//! `cpl.rs` already rejects a `Resource` whose own `EditRate` differs from
//! its `Sequence`'s (`Error::Unsupported`) — SMPTE ST 2067-3 allows a
//! `Resource`-level override for exactly the case where a track file's own
//! frame rate does not match the CPL's, which this crate does not attempt
//! to retime. This module goes one step further, silently rather than by a
//! checked rejection: it assumes every track file's own index entries
//! (`vaco_demux_mxf`'s `PacketIndex`) enumerate edit units in the *same*
//! units as `Cpl::edit_rate`, using `EntryPoint`/`SourceDuration` directly
//! as zero-based indices into that file's own [`vaco_format_core::seek::PacketIndex`].
//! No IMF reference implementation was available on this machine to check
//! that assumption against (see this crate's top-level docs,
//! "Verification") — recorded in `planning/TECH-DEBT.md` rather than left
//! implicit.

use std::path::PathBuf;

use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_format_core::{
    Demuxer, DemuxerDesc, FormatFlags, ParserProvider, ProbeData, ProbeScore, SeekFlags,
    SeekTarget, Stream,
};
use vaco_io::{MediaSource, PeekSource};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

use crate::cpl::{self, Cpl, Resource, SequenceKind};
use crate::fsio::FileRawSource;
use crate::package::Package;

/// A Composition Playlist has no extension of its own — real deliveries
/// name the file freely (`CPL_<uuid>.xml` is the common convention, not a
/// requirement) — so this format is found by content alone, the same
/// posture `vaco-demux-dash::probe` takes for `<MPD`. Allowing for a
/// leading XML declaration/BOM, `<CompositionPlaylist` should appear within
/// the first few hundred bytes of any conforming file.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.find(b"<CompositionPlaylist", 0, 4096).is_some() {
        ProbeScore::MAGIC
    } else {
        ProbeScore::NONE
    }
}

/// See `vaco-demux-dash::FLAGS`'s own doc comment for the identical
/// reasoning: an edit list over other containers' own essence has no native
/// byte index or byte-seek relationship of its own to offer.
pub const FLAGS: FormatFlags = FormatFlags::GENERIC_INDEX
    .union(FormatFlags::NOBINSEARCH)
    .union(FormatFlags::NO_BYTE_SEEK);

/// The descriptor `vaco-registry` holds (`vaco-component.toml`'s own
/// `ctor = "vaco_format_imf::demux::DEMUXER"`).
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "imf",
    long_name: "IMF (SMPTE ST 2067) Composition Playlist",
    extensions: &[],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_boxed,
};

/// Largest Composition Playlist this demuxer will buffer. CPLs are XML
/// text describing an edit list, not carrying essence, so this is generous
/// without being unbounded — matching the spirit of
/// `vaco-demux-image2::multi::MAX_SINGLE_SOURCE` for the same "must have
/// *some* cap, chosen for headroom rather than measured" reasoning.
const MAX_CPL_BYTES: u64 = 64 << 20;

struct OpenResource {
    demux: vaco_demux_mxf::MxfDemuxer,
    mxf_stream_index: u32,
}

/// One virtual track's read cursor over its own flattened resource list
/// (already in CPL segment order — see [`Cpl::virtual_tracks`]).
struct TrackCursor {
    stream_index: u32,
    resources: Vec<Resource>,
    /// This track's own total length, in edit units — the sum of every
    /// resource's `source_duration * repeat_count`. Computed once; a
    /// `Resource` list is never mutated after `open`.
    total_units: u64,
    resource_idx: usize,
    /// Offset within the *current* resource's own repeated play range
    /// (`0..source_duration * repeat_count`), not yet reduced modulo
    /// `source_duration` — see [`TrackCursor::file_edit_unit`].
    unit_in_resource: u64,
    /// This track's own next output position, in composition-timeline edit
    /// units — the `pts`/`dts` [`ImfDemuxer::read_packet`] assigns.
    composition_unit: u64,
    open: Option<OpenResource>,
}

impl TrackCursor {
    fn new(stream_index: u32, resources: Vec<Resource>) -> Self {
        let total_units = resources
            .iter()
            .map(|r| r.source_duration.saturating_mul(u64::from(r.repeat_count.max(1))))
            .fold(0u64, u64::saturating_add);
        Self {
            stream_index,
            resources,
            total_units,
            resource_idx: 0,
            unit_in_resource: 0,
            composition_unit: 0,
            open: None,
        }
    }

    fn is_done(&self) -> bool {
        self.resource_idx >= self.resources.len()
    }

    fn current_resource(&self) -> Option<&Resource> {
        self.resources.get(self.resource_idx)
    }

    /// The position to hand `MxfDemuxer::read_edit_unit`: `EntryPoint` plus
    /// how far into the current repeat we are, wrapped modulo
    /// `SourceDuration` — ST 2067-3's own account of `RepeatCount`, replaying
    /// the same `EntryPoint..EntryPoint+SourceDuration` range that many
    /// times before the sequence moves to its next `Resource`.
    fn file_edit_unit(&self) -> Option<u64> {
        let res = self.current_resource()?;
        let span = res.source_duration.max(1);
        Some(res.entry_point.saturating_add(self.unit_in_resource % span))
    }

    /// Move one edit unit forward, crossing into the next resource (closing
    /// whatever essence file the one just finished had open) when the
    /// current one's repeated range is exhausted.
    fn advance(&mut self) {
        self.composition_unit = self.composition_unit.saturating_add(1);
        self.unit_in_resource = self.unit_in_resource.saturating_add(1);
        if let Some(res) = self.current_resource() {
            let total = res.source_duration.saturating_mul(u64::from(res.repeat_count.max(1)));
            if self.unit_in_resource >= total {
                self.resource_idx += 1;
                self.unit_in_resource = 0;
                self.open = None;
            }
        }
    }

    /// Reposition to composition-timeline edit unit `target`, clamped to
    /// `[0, total_units)` (or to 0 if the track is empty). Does not itself
    /// open anything — [`ImfDemuxer::seek`] does that once the right
    /// resource is known.
    fn seek_to(&mut self, target: u64) {
        if self.resources.is_empty() {
            self.resource_idx = 0;
            self.unit_in_resource = 0;
            self.composition_unit = 0;
            self.open = None;
            return;
        }
        let clamped = target.min(self.total_units.saturating_sub(1));
        let mut remaining = clamped;
        for (idx, res) in self.resources.iter().enumerate() {
            let total = res.source_duration.saturating_mul(u64::from(res.repeat_count.max(1)));
            if remaining < total || idx + 1 == self.resources.len() {
                self.resource_idx = idx;
                self.unit_in_resource = remaining.min(total.saturating_sub(1));
                self.composition_unit = clamped;
                self.open = None;
                return;
            }
            remaining -= total;
        }
    }
}

/// IMF (SMPTE ST 2067) demuxer. See the module docs for the two-call open
/// this format needs and the shared-edit-rate assumption it makes.
pub struct ImfDemuxer {
    /// `Some` until [`ImfDemuxer::bind_url`] consumes it into a [`Package`];
    /// `None` afterward, and also the "already bound" / "never bound"
    /// discriminant.
    cpl: Option<Cpl>,
    package: Option<Package>,
    streams: Vec<Stream>,
    tracks: Vec<TrackCursor>,
    metadata: Vec<(String, String)>,
}

impl std::fmt::Debug for ImfDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `vaco_demux_mxf::MxfDemuxer` (held per open track resource) does
        // not itself implement `Debug`, so this reports only the shape a
        // caller can already see through `Demuxer::streams`.
        f.debug_struct("ImfDemuxer")
            .field("bound", &self.package.is_some())
            .field("streams", &self.streams.len())
            .finish_non_exhaustive()
    }
}

impl ImfDemuxer {
    /// Parse the Composition Playlist `source` was opened from and build
    /// one placeholder [`Stream`] per virtual track. Real codec parameters
    /// are not available until [`ImfDemuxer::bind_url`] resolves the
    /// package and opens each track's first essence file — see the module
    /// docs.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a CPL that is not valid UTF-8 or does not
    /// parse as ST 2067-3; [`Error::LimitExceeded`] past
    /// [`MAX_CPL_BYTES`]. Propagates whatever reading `source` reports.
    pub fn open(source: Box<dyn MediaSource>, _parsers: &dyn ParserProvider) -> Result<Self> {
        let bytes = read_source_to_end(source)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::InvalidData("imf: Composition Playlist is not valid UTF-8"))?;
        let mut budget = Budget::new(Limits::permissive());
        let cpl = cpl::parse(&text, &mut budget)?;

        let metadata = cpl
            .content_title
            .as_ref()
            .map(|t| vec![("title".to_owned(), t.clone())])
            .unwrap_or_default();

        let mut streams = Vec::new();
        let mut tracks = Vec::new();
        for vt in cpl.virtual_tracks() {
            let media_type = match vt.kind {
                SequenceKind::MainImage => MediaType::Video,
                SequenceKind::MainAudio => MediaType::Audio,
            };
            let index = u32::try_from(streams.len()).unwrap_or(u32::MAX);
            // The CPL states one edit rate for the whole composition
            // (`Cpl::edit_rate`); a stream's `time_base` is its reciprocal,
            // matching how every other demuxer in this workspace derives
            // `time_base` from a frame rate.
            let time_base = Rational::new(cpl.edit_rate.den, cpl.edit_rate.num);
            streams.push(Stream::new(index, media_type, time_base));
            tracks.push(TrackCursor::new(index, vt.resources));
        }

        Ok(Self {
            cpl: Some(cpl),
            package: None,
            streams,
            tracks,
            metadata,
        })
    }

    /// Whichever not-yet-exhausted track's next composition-unit position
    /// is smallest — the interleaving order [`ImfDemuxer::read_packet`]
    /// reads in, since every track shares one edit-unit domain (the CPL's
    /// own edit rate; see the module docs).
    fn next_track_index(&self) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_done())
            .min_by_key(|(_, t)| t.composition_unit)
            .map(|(i, _)| i)
    }

    fn ensure_open<'a>(package: &Package, cursor: &'a mut TrackCursor) -> Result<&'a mut OpenResource> {
        if cursor.open.is_none() {
            let res = cursor
                .current_resource()
                .ok_or(Error::InvalidData("imf: track cursor has no current resource"))?;
            cursor.open = Some(open_resource(package, &res.track_file_id)?);
        }
        cursor
            .open
            .as_mut()
            .ok_or(Error::InvalidData("imf: track cursor failed to open its resource"))
    }
}

impl Demuxer for ImfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    fn read_packet(&mut self) -> Result<Packet> {
        let package = self.package.as_ref().ok_or(Error::NotSeekable)?;
        let Some(idx) = self.next_track_index() else {
            return Err(Error::Eof);
        };
        let Some(cursor) = self.tracks.get_mut(idx) else {
            return Err(Error::Eof);
        };
        let Some(file_unit) = cursor.file_edit_unit() else {
            return Err(Error::Eof);
        };
        let opened = Self::ensure_open(package, cursor)?;
        let mut pkt = opened.demux.read_edit_unit(opened.mxf_stream_index, file_unit)?;
        pkt.stream_index = cursor.stream_index;
        let ticks = i64::try_from(cursor.composition_unit).unwrap_or(i64::MAX);
        pkt.pts = Timestamp::new(ticks);
        pkt.dts = Timestamp::new(ticks);
        cursor.advance();
        Ok(pkt)
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let _ = flags;
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::Unsupported(
                "imf: only timestamp seeks are implemented",
            ));
        };
        if self.package.is_none() {
            return Err(Error::NotSeekable);
        }
        let Some(cursor) = self.tracks.iter_mut().find(|t| t.stream_index == stream_index) else {
            return Err(Error::NotSeekable);
        };
        let Some(target_unit) = ts.ticks() else {
            return Err(Error::NotSeekable);
        };
        cursor.seek_to(target_unit.max(0).unsigned_abs());
        Ok(())
    }

    fn bind_url(&mut self, url: &str) -> Result<()> {
        let cpl = self
            .cpl
            .take()
            .ok_or(Error::Unsupported("this IMF demuxer is already bound"))?;
        let package = Package::for_cpl(cpl, url)?;
        for cursor in &mut self.tracks {
            let Some(res) = cursor.current_resource() else {
                continue;
            };
            let track_file_id = res.track_file_id.clone();
            let opened = open_resource(&package, &track_file_id)?;
            if let Some(stream) = self
                .streams
                .iter_mut()
                .find(|s| s.index == cursor.stream_index)
                && let Some(real) = opened
                    .demux
                    .streams()
                    .iter()
                    .find(|s| s.index == opened.mxf_stream_index)
            {
                stream.params = real.params.clone();
            }
            cursor.open = Some(opened);
        }
        self.package = Some(package);
        Ok(())
    }
}

fn open_resource(package: &Package, track_file_id: &str) -> Result<OpenResource> {
    let path: PathBuf = package.resolve_track_file(track_file_id)?;
    let raw = FileRawSource::open(&path)?;
    let src: Box<dyn MediaSource> = Box::new(PeekSource::with_limits(raw, Limits::permissive()));
    let demux = vaco_demux_mxf::MxfDemuxer::open(src, &vaco_format_core::NoParsers)?;
    let mxf_stream_index = demux
        .streams()
        .first()
        .map(|s| s.index)
        .ok_or(Error::InvalidData(
            "imf: OP-Atom track file has no essence stream",
        ))?;
    Ok(OpenResource {
        demux,
        mxf_stream_index,
    })
}

fn read_source_to_end(mut src: Box<dyn MediaSource>) -> Result<Vec<u8>> {
    let mut budget = Budget::new(Limits::permissive());
    let mut out = Vec::new();
    let mut chunk = budget.alloc::<u8>(64 * 1024)?;
    loop {
        let n = src.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let Some(taken) = chunk.get(..n) else {
            return Err(Error::InvalidData(
                "imf: short read reported more bytes than taken",
            ));
        };
        budget.charge(taken.len() as u64)?;
        if out.len() as u64 + taken.len() as u64 > MAX_CPL_BYTES {
            return Err(Error::LimitExceeded {
                limit: "imf_cpl_buffer",
                requested: out.len() as u64 + taken.len() as u64,
                cap: MAX_CPL_BYTES,
            });
        }
        out.extend_from_slice(taken);
    }
    Ok(out)
}

/// Registered as `"imf"` in `vaco-registry`.
///
/// # Errors
/// See [`ImfDemuxer::open`].
pub fn open_boxed(
    source: Box<dyn MediaSource>,
    parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(ImfDemuxer::open(source, parsers)?))
}
