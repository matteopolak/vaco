//! [`MxfDemuxer`]: the four layers wired together into a [`Demuxer`].
//!
//! # Scope, stated plainly
//!
//! This demuxer supports **one essence track per `BodySID`** — the shape
//! every file in this crate's corpus has (`OP1a` and `OP-Atom` alike, both
//! single-video-track). MXF's index tables can describe several interleaved
//! tracks sharing one `BodySID` via `DeltaEntryArray`'s slice numbers; that
//! interleaving is not implemented, so a multi-essence-track `OP1a` file will
//! have its packets demuxed (the KLV walk is track-agnostic) but only the
//! first recognised track gets a seek index. See this crate's closing
//! report for the exact status of each layer.

use std::collections::HashMap;

use vaco_core::MediaType;
use vaco_core::{Duration, Error, ExactDuration, Rational, Result, Timestamp};
use vaco_format_core::seek::{
    IndexEntry as FcIndexEntry, IndexFlags, PacketIndex, SeekFlags, SeekTarget,
};
use vaco_format_core::{Demuxer, ParserProvider, Stream};
use vaco_io::{IoContext, IoOptions, MediaSource, Seekability};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

use crate::descriptor;
use crate::index::IndexTableSegment;
use crate::klv::{self, KlvHeader};
use crate::metadata::{self, MetadataGraph};
use crate::partition::{self, PartitionKind};
use crate::primer;
use crate::properties::Resolver;
use crate::ul::PartitionFamilyKind;

/// One packet's worth of essence, at most: a real-world frame this crate
/// has seen tops out around 100 KB; 256 MiB is generous headroom for an
/// uncompressed or very high-bitrate frame while still refusing a
/// declared-length attack before it is read.
const MAX_PACKET_BYTES: u64 = 256 * 1024 * 1024;

/// The most CBE index entries `build_indices` will synthesise for one
/// track. Same order of magnitude as `index::MAX_INDEX_ENTRIES` (the VBE
/// array-length cap): a CBE segment's entry count is computed, not read
/// from a declared array length, but the computation now (see
/// `effective_index_duration`) can be driven by a real file's own size
/// divided by a small `EditUnitByteCount` — capped here rather than left as
/// an unbounded `for n in 0..count` loop over an attacker-influenceable
/// count.
const MAX_CBE_INDEX_ENTRIES: u64 = 16 * 1024 * 1024;

/// One recognised essence track: which stream it feeds and its Generic
/// Container track number.
struct TrackBinding {
    stream_index: u32,
    track_number: u32,
    edit_rate: Rational,
    next_edit_unit: i64,
}

pub struct MxfDemuxer {
    io: IoContext,
    budget: Budget,
    /// Essence-payload allocation is charged here and released immediately
    /// (mirroring `vaco-demux-mp4`'s `packet_budget`): this demuxer retains
    /// no packet after handing it back, so a cumulative cap on `budget`
    /// would otherwise refuse to read a file larger than the cap.
    packet_budget: Budget,
    streams: Vec<Stream>,
    format_metadata: Vec<(String, String)>,
    duration_exact: Option<ExactDuration>,
    bindings: Vec<TrackBinding>,
    /// Per-stream seek index, built from a measured file's Index Table
    /// Segment(s) — see `index` and `essence` module docs for how a
    /// `StreamOffset` becomes an absolute file position.
    indices: HashMap<u32, PacketIndex>,
    eof: bool,
}

impl std::fmt::Debug for MxfDemuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MxfDemuxer")
            .field("streams", &self.streams.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl MxfDemuxer {
    /// Open an MXF source.
    ///
    /// `parsers` is accepted for interface symmetry with every other
    /// demuxer (D14.1: a format crate reaches bitstream parsers only through
    /// the injected [`ParserProvider`]) but is not yet called — this crate
    /// derives everything it reports about a stream from container-stated
    /// descriptor properties, never from parsing the elementary stream, so
    /// there is currently nothing to ask a parser for.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if the source does not begin with a Header
    /// Partition Pack, or if required header metadata cannot be resolved.
    pub fn open(src: Box<dyn MediaSource>, parsers: &dyn ParserProvider) -> Result<Self> {
        let _ = parsers;
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(Limits::permissive());

        let header_klv = klv::read_header(&mut io)?;
        let header_pp = partition::parse(&mut io, &mut budget, &header_klv)?;
        if header_pp.kind != PartitionKind::Header {
            return Err(Error::InvalidData(
                "mxf: file does not begin with a header partition pack",
            ));
        }

        let primer_klv = find_primer_pack(&mut io, &mut budget)?;
        let primer = primer::parse(&mut io, &mut budget, &primer_klv)?;
        let resolver = Resolver::new();

        let mut graph = MetadataGraph::default();
        let mut index_segments: Vec<IndexTableSegment> = Vec::new();
        metadata::scan_region(
            &mut io,
            &mut budget,
            &primer,
            &resolver,
            &mut graph,
            &mut index_segments,
        )?;
        let body_start = io.pos();

        let seekable = io.seekability() != Seekability::None;
        if seekable
            && header_pp.footer_partition != 0
            && header_pp.footer_partition != header_pp.this_partition
        {
            if let Ok(footer_klv) = seek_and_read_header(&mut io, header_pp.footer_partition)
                && let Ok(footer_pp) = partition::parse(&mut io, &mut budget, &footer_klv)
                && footer_pp.kind == PartitionKind::Footer
            {
                // Best-effort: a truncated or unusual footer must not stop
                // the file from opening with what the header already gave
                // us.
                let _ = metadata::scan_region(
                    &mut io,
                    &mut budget,
                    &primer,
                    &resolver,
                    &mut graph,
                    &mut index_segments,
                );
            }
            io.seek(body_start)?;
        }

        // Find the first essence element without consuming it, so its
        // offset can anchor every Index Table Segment's `StreamOffset` (see
        // `essence` module docs) before any packet is read. Only attempted
        // on a seekable source: the lookahead consumes bytes a forward-only
        // transport could never give back.
        let first_essence = if seekable {
            let found = find_first_essence_offset(&mut io, &mut budget)?;
            io.seek(body_start)?;
            found
        } else {
            None
        };
        let essence_origin = first_essence.as_ref().map(|e| e.key_pos);

        let (streams, bindings, format_metadata) = build_streams(&graph, &mut budget)?;

        let total_essence_len =
            essence_origin.and_then(|origin| io.size().map(|size| size.saturating_sub(origin)));
        let duration_of =
            |seg: &IndexTableSegment| -> i64 { effective_index_duration(seg, total_essence_len) };

        let indices = first_essence
            .as_ref()
            .map(|e| build_indices(&bindings, &index_segments, e, &duration_of))
            .unwrap_or_default();

        let duration_exact = bindings
            .iter()
            .zip(streams.iter())
            .filter_map(|(b, s)| {
                let seg = index_segments
                    .iter()
                    .find(|seg| seg.index_edit_rate == Some(b.edit_rate))?;
                ExactDuration::from_ticks(duration_of(seg), s.time_base)
            })
            .max();

        Ok(Self {
            io,
            budget,
            packet_budget: Budget::new(Limits::permissive()),
            streams,
            format_metadata,
            duration_exact,
            bindings,
            indices,
            eof: false,
        })
    }

    /// Read edit unit `n` of `stream_index`'s essence directly through its
    /// parsed index, rather than `read_packet`'s own "read the next KLV
    /// header" walk.
    ///
    /// Added for `vaco-format-imf`'s essence integration: an IMF virtual
    /// track's `Resource` names an exact edit-unit range
    /// (`EntryPoint..EntryPoint+SourceDuration`) out of one track file, not
    /// "the next packet in storage order" — for a frame-wrapped file this
    /// is equivalent to calling `read_packet` after seeking to the right
    /// edit unit, but for a **clip-wrapped** file (every OP-Atom essence
    /// element this crate has measured) it is the *only* way to reach one:
    /// after the first edit unit there is no second KLV header for
    /// `read_packet`'s walk to find at all, since the whole track lives in
    /// one Generic Container element. Each call is independent (seeks,
    /// reads, and does not disturb `read_packet`'s own sequential state) —
    /// calling this and `read_packet` on the same [`MxfDemuxer`] in any
    /// interleaved order is not a supported combination, since both leave
    /// `self.io`'s cursor wherever their own last read ended.
    ///
    /// # Errors
    /// [`Error::NotSeekable`] if `stream_index` has no parsed index (the
    /// source was not seekable at `open` time, or `stream_index` is
    /// unknown). [`Error::InvalidData`] if `n` is past the index's own
    /// entry count.
    pub fn read_edit_unit(&mut self, stream_index: u32, n: u64) -> Result<Packet> {
        let Some(index) = self.indices.get(&stream_index) else {
            return Err(Error::NotSeekable);
        };
        let entry = usize::try_from(n)
            .ok()
            .and_then(|i| index.entries().get(i))
            .copied()
            .ok_or(Error::InvalidData(
                "mxf: edit unit index past this stream's own index entry count",
            ))?;
        self.io.seek(entry.pos)?;
        let len = usize::try_from(entry.size).unwrap_or(0);
        let mut pkt = Packet::alloc(&mut self.packet_budget, len)?;
        self.packet_budget.release(len as u64);
        self.io.read_exact(pkt.payload_mut())?;
        pkt.stream_index = stream_index;
        let ticks = i64::try_from(n).unwrap_or(i64::MAX);
        pkt.pts = Timestamp::new(ticks);
        pkt.dts = Timestamp::new(ticks);
        pkt.pos = Some(entry.pos);
        pkt.flags = if entry.flags.contains(IndexFlags::KEYFRAME) {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        Ok(pkt)
    }
}

/// Read forward past any KLV Fill Item(s) to find the Primer Pack.
///
/// Measured: a real header partition pack is followed by a Filler (KAG
/// alignment padding) before the Primer Pack proper, not by the Primer Pack
/// directly — see `out.mxf`'s byte layout in the `ul` module docs. Bounded
/// by a small fixed iteration cap; a real file needs at most one or two
/// fillers here.
fn find_primer_pack(io: &mut IoContext, budget: &mut Budget) -> Result<KlvHeader> {
    for _ in 0..64u32 {
        budget.consume_fuel(1)?;
        let header = klv::read_header(io)?;
        if header.key.partition_family_kind() == Some(PartitionFamilyKind::Primer) {
            return Ok(header);
        }
        if header.key == crate::ul::KLV_FILL_ITEM {
            klv::skip_value(io, &header)?;
            continue;
        }
        return Err(Error::InvalidData(
            "mxf: expected a primer pack (or filler) after the header partition pack",
        ));
    }
    Err(Error::InvalidData(
        "mxf: too many fillers before a primer pack was found",
    ))
}

fn seek_and_read_header(io: &mut IoContext, offset: u64) -> Result<KlvHeader> {
    io.seek(offset)?;
    klv::read_header(io)
}

/// Walk forward from the current position, skipping partition packs, filler
/// and unrecognised KLVs, until an essence element is found. Does not
/// consume it — the caller seeks back.
/// The first essence element's own three positions: its key (`essence_origin`
/// proper — where a frame-wrapped file's `IndexEntryArray::StreamOffset`s
/// are anchored, since spec-conformant `StreamOffset`s for *that* shape are
/// measured from one essence element's key to the next), its value (where a
/// **clip-wrapped** file's `StreamOffset`s are anchored instead — see
/// [`ClipShape::detect`]'s own doc comment for how this crate tells the two
/// apart empirically rather than trusting the item-type byte, which
/// `essence.rs`'s own module docs already found unreliable), and that
/// value's own declared length.
struct FirstEssenceElement {
    key_pos: u64,
    value_offset: u64,
    value_len: u64,
}

fn find_first_essence_offset(
    io: &mut IoContext,
    budget: &mut Budget,
) -> Result<Option<FirstEssenceElement>> {
    for _ in 0..1_000_000u32 {
        budget.consume_fuel(1)?;
        let header = match klv::read_header(io) {
            Ok(h) => h,
            Err(Error::UnexpectedEof) => return Ok(None),
            Err(e) => return Err(e),
        };
        if header.key.is_essence_element() {
            return Ok(Some(FirstEssenceElement {
                key_pos: header.offset,
                value_offset: header.value_offset,
                value_len: header.length,
            }));
        }
        if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack) {
            return Ok(None);
        }
        klv::skip_value(io, &header)?;
    }
    Ok(None)
}

fn build_streams(
    graph: &MetadataGraph,
    budget: &mut Budget,
) -> Result<(Vec<Stream>, Vec<TrackBinding>, Vec<(String, String)>)> {
    let mut streams = Vec::new();
    let mut bindings = Vec::new();
    let mut format_metadata = Vec::new();

    let Some(material) = graph
        .of_class(crate::ul::StructuralClass::MaterialPackage)
        .next()
        .and_then(|s| s.instance_uid)
    else {
        // No Material Package resolved: an empty but valid demux (some
        // partial/growing files legitimately have no complete header yet).
        return Ok((streams, bindings, format_metadata));
    };

    if let Some(pref) = graph.preface()
        && let Some(op) = pref.get_ul(crate::properties::PropertyId::PrefaceOperationalPattern)
    {
        format_metadata.push(("operational_pattern_ul".to_owned(), op.to_string()));
    }

    let (tracks, timecodes) = metadata::resolve_essence(graph, material, budget)?;
    if let Some(tc) = timecodes.first() {
        format_metadata.push(("timecode".to_owned(), format_timecode(*tc)));
    }

    for track in tracks {
        if track.is_timecode {
            continue;
        }
        let Some(descriptor_id) = track.descriptor else {
            continue;
        };
        let Some(desc) = graph.get(descriptor_id) else {
            continue;
        };
        // A MultipleDescriptor that `metadata::resolve_essence`'s
        // per-track expansion could not resolve (no `SubDescriptorUIDs`
        // match for this track, or a hostile file's array pointing nowhere
        // useful): still a `Descriptor(0x44)` here, still carrying none of
        // a real essence descriptor's properties. Skipped rather than
        // built with no parameters at all.
        if matches!(desc.class, crate::ul::StructuralClass::Descriptor(0x44)) {
            continue;
        }
        let is_picture = desc
            .get_u32(crate::properties::PropertyId::StoredWidth)
            .is_some()
            || desc
                .get_ul(crate::properties::PropertyId::PictureEssenceCoding)
                .is_some();
        // A sound descriptor is recognised by carrying the properties only
        // sound descriptors have (`descriptor::sound_parameters`'s module
        // docs) — checked instead of by class byte, since both
        // `AES3PCMDescriptor` and `GenericSoundEssenceDescriptor` fold into
        // the same `StructuralClass::Descriptor` arm as every picture
        // descriptor kind (see `ul.rs`).
        let is_sound = !is_picture
            && (desc
                .get_u32(crate::properties::PropertyId::AudioChannelCount)
                .is_some()
                || desc
                    .get_u32(crate::properties::PropertyId::AudioQuantizationBits)
                    .is_some());
        let (params, media_type) = if is_picture {
            (descriptor::picture_parameters(desc), MediaType::Video)
        } else if is_sound {
            (descriptor::sound_parameters(desc), MediaType::Audio)
        } else {
            // A data descriptor, or a picture/sound descriptor kind this
            // crate has not measured and so cannot tell apart from
            // "nothing recognised" — skipped rather than reported with
            // guessed parameters (D6/D17).
            continue;
        };
        let edit_rate = track.edit_rate.unwrap_or(Rational { num: 25, den: 1 });
        let time_base = Rational {
            num: edit_rate.den,
            den: edit_rate.num,
        };
        let index = streams.len() as u32;
        let mut stream = Stream::new(index, media_type, time_base);
        stream.params = params;
        if media_type == MediaType::Video {
            stream.r_frame_rate = edit_rate;
            stream.avg_frame_rate = edit_rate;
        }
        if let Some(id) = track.track_id {
            stream.id = Some(i64::from(id));
        }
        streams.push(stream);
        bindings.push(TrackBinding {
            stream_index: index,
            track_number: track.track_number.unwrap_or(0),
            edit_rate,
            next_edit_unit: 0,
        });
    }
    Ok((streams, bindings, format_metadata))
}

#[allow(
    clippy::integer_division,
    reason = "base is clamped to at least 1 above; this is timecode arithmetic, not a rounding bug"
)]
fn format_timecode(tc: metadata::Timecode) -> String {
    let base = i64::from(tc.base.max(1));
    let total_frames = tc.start.max(0);
    let hours = total_frames / (base * 3600);
    let minutes = (total_frames / (base * 60)) % 60;
    let seconds = (total_frames / base) % 60;
    let frames = total_frames % base;
    let sep = if tc.drop_frame { ';' } else { ':' };
    format!("{hours:02}:{minutes:02}:{seconds:02}{sep}{frames:02}")
}

/// Whether the first (in this crate's single-essence-track-per-`BodySID`
/// scope, only) essence element holds one edit unit or the whole track.
///
/// The item-type byte cannot tell the two apart reliably —
/// `essence::Wrapping`'s own doc comment records a real D-10 file using a
/// byte ST 379-1's own table calls "clip-wrapped" for essence that is, in
/// every operational sense, frame-wrapped (twenty-five separate KLVs, one
/// per edit unit). What *does* tell them apart: whether the one element
/// found already reaches as far as the index's own last entry claims a
/// later edit unit starts. A frame-wrapped element's own declared length is
/// one edit unit's worth of bytes — never more than the second entry's own
/// `StreamOffset`, since that is where the *next* element's key begins. A
/// clip-wrapped element's declared length is the whole track — at least as
/// large as the last entry's `StreamOffset`, since every edit unit lives
/// inside this one element's value.
///
/// Measured directly against a real `ffmpeg`-produced OP-Atom fixture this
/// session (`vaco-format-imf`'s own essence-integration work): the single
/// essence element's key sits 25 bytes before its first edit unit's own
/// `00 00 01` MPEG start code (16-byte key + a 9-byte wide-form BER length
/// prefix, `88 ...` — OP-Atom's own clip-wrapped element always uses that
/// wide form regardless of size, per `vaco-mux-mxf::ber`'s own doc comment
/// on the write side), which the pre-existing `essence_origin`-relative
/// computation below did not account for: `IndexEntryArray::StreamOffset`
/// for a clip-wrapped file is relative to the element's **value**, not its
/// key, so `pos = essence_origin + stream_offset` landed inside the
/// *previous* edit unit's own tail bytes for every entry past the first,
/// not on a real edit-unit boundary. Not previously exercised by any test:
/// every prior seek/index test used a frame-wrapped fixture, where
/// `essence_origin` (the first element's key) and its own value-relative
/// zero point coincide for entry 0 and are supposed to advance by whole
/// elements thereafter, hiding the bug completely.
fn is_clip_wrapped(first: &FirstEssenceElement, seg: &IndexTableSegment) -> bool {
    if seg.is_cbe() {
        // The same reasoning as the VBE branch below, in CBE terms:
        // `EditUnitByteCount` is one whole edit unit's size (D-10's own
        // measured value bundles a System Item and KAG padding around the
        // essence itself -- `vaco-mux-mxf`'s own D-10 write-side doc
        // comments have the exact arithmetic -- so it is always at least as
        // large as the essence element's own declared length there). The
        // one element already found reaching that far means it holds more
        // than one edit unit, i.e. is clip-wrapped; every real D-10 fixture
        // measured has `value_len` strictly less than `EditUnitByteCount`
        // (the essence alone is smaller than essence-plus-overhead) and so
        // correctly evaluates to frame-wrapped here.
        return first.value_len >= u64::from(seg.edit_unit_byte_count);
    }
    seg.entries
        .get(1)
        .is_some_and(|second| first.value_len >= second.stream_offset)
}

/// How many edit units `seg` covers, trusted only as far as the essence
/// container's own measured size backs it up.
///
/// A CBE Index Table Segment's own `IndexDuration` is not trusted blindly in
/// either direction:
///
/// * **Zero is not "empty".** Measured against a real single-partition D-10
///   file (`ffmpeg -f mxf_d10`), `IndexDuration` states `0` even though the
///   file has a definite, 25-frame essence container — the real count is
///   only recoverable from the essence container's own measured size
///   divided by `EditUnitByteCount`, the same computation
///   `essence::clip_wrapped_spans`'s CBE branch already does. `0` most
///   likely means "unknown/growing" in a live-capture writer, not "empty".
/// * **A large positive value is not ground truth either.** `IndexDuration`
///   is an attacker-controlled `i64` read straight off the wire
///   (`localset::i64_be`), and [`build_indices`]'s CBE branch runs a loop of
///   exactly this length. A 9,934-byte file declaring `IndexDuration =
///   144,115,188,075,855,872` against a real 212,992-byte
///   `EditUnitByteCount` sailed past the old "0 means unknown" check (the
///   value is very much not 0) and fell through to `MAX_CBE_INDEX_ENTRIES`
///   as the only cap — 16,777,216 loop iterations, ~500ms, for a file that
///   cannot possibly contain more than a handful of real edit units. Found
///   by `fuzz/fuzz_targets/mxf_demux.rs`,
///   `slow-unit-a4c0af443812c5c6e4cc5601feb1ab8b163d65b7`.
///
/// So a size-derived bound — `total_essence_len / EditUnitByteCount` — is
/// applied on *both* sides: it substitutes for a `0` and it clamps a stated
/// value that overshoots it. This makes the bound scale with the input
/// actually given, rather than sitting at a fixed ceiling regardless of it;
/// `MAX_CBE_INDEX_ENTRIES` remains the fallback only when `total_essence_len`
/// itself is unknown (a non-seekable source, where no size-derived bound is
/// possible at all).
fn effective_index_duration(seg: &IndexTableSegment, total_essence_len: Option<u64>) -> i64 {
    let size_bound = if seg.is_cbe() {
        total_essence_len
    } else {
        None
    }
    .map(|total| {
        #[allow(
            clippy::integer_division,
            reason = "edit_unit_byte_count is checked non-zero via is_cbe() above"
        )]
        let count = total / u64::from(seg.edit_unit_byte_count);
        count
    });
    if seg.index_duration > 0 {
        let stated = u64::try_from(seg.index_duration).unwrap_or(0);
        let bounded = match size_bound {
            // A measured size is ground truth; a stated duration can only
            // ever be trusted down to it, never up past it.
            Some(bound) => stated.min(bound),
            // No real size to measure against (non-seekable source): fall
            // back to the fixed ceiling `build_indices` already enforces, so
            // this function's own return value can never exceed what the
            // loop it feeds will actually run.
            None => stated.min(MAX_CBE_INDEX_ENTRIES),
        };
        return i64::try_from(bounded).unwrap_or(0);
    }
    size_bound.and_then(|b| i64::try_from(b).ok()).unwrap_or(0)
}

fn build_indices(
    bindings: &[TrackBinding],
    segments: &[IndexTableSegment],
    first_essence: &FirstEssenceElement,
    effective_index_duration: &dyn Fn(&IndexTableSegment) -> i64,
) -> HashMap<u32, PacketIndex> {
    let mut out = HashMap::new();
    // Single-track-per-BodySID (see module docs): pair each binding with the
    // first segment whose edit rate matches, in order. Good enough for this
    // crate's corpus (one video track, one segment) and stated as a scope
    // limit rather than pretended to be a general BodySID/track match.
    for binding in bindings {
        let Some(seg) = segments
            .iter()
            .find(|s| s.index_edit_rate == Some(binding.edit_rate))
        else {
            continue;
        };
        // The zero point `StreamOffset` is measured from: a clip-wrapped
        // element's own value start, a frame-wrapped one's own key
        // (`essence_origin` proper) -- see `is_clip_wrapped`'s own doc
        // comment for the real bug this distinction fixes.
        let origin = if is_clip_wrapped(first_essence, seg) {
            first_essence.value_offset
        } else {
            first_essence.key_pos
        };
        let mut idx = PacketIndex::new();
        if seg.is_cbe() {
            let unit = u64::from(seg.edit_unit_byte_count);
            if unit == 0 {
                continue;
            }
            let count = u64::try_from(effective_index_duration(seg))
                .unwrap_or(0)
                .min(MAX_CBE_INDEX_ENTRIES);
            for n in 0..count {
                let Some(rel) = seg.cbe_offset(n) else { break };
                let Ok(ticks) = i64::try_from(n) else { break };
                idx.add(FcIndexEntry {
                    pos: origin.saturating_add(rel),
                    timestamp: Timestamp::new(ticks),
                    flags: IndexFlags::KEYFRAME,
                    size: unit.try_into().unwrap_or(u32::MAX),
                    min_distance: 0,
                });
            }
        } else {
            let clip_wrapped = is_clip_wrapped(first_essence, seg);
            for (i, entry) in seg.entries.iter().enumerate() {
                let Ok(ticks) = i64::try_from(i) else { break };
                // The last entry has no "next" to diff against for its own
                // size. `read_packet` never needed one -- it always reads a
                // fresh KLV header at the current position, discovering the
                // real length itself -- so this stayed `0` and harmless
                // until `read_edit_unit` (below) needed a real byte count to
                // read without a header to read it from. Fixable exactly,
                // not just approximately, for the clip-wrapped case: the one
                // element's own declared `value_len` *is* the end of the
                // last edit unit, by construction (there is nothing else in
                // that element). Left at the pre-existing `0` for a
                // frame-wrapped file's last entry -- this crate has no
                // general "where does the essence region end" figure that
                // would not risk over-reading into a footer partition or
                // RIP past the real last frame, and `read_packet` (which
                // does not have this problem) remains every frame-wrapped
                // caller's own correct path regardless.
                let size = seg.entries.get(i + 1).map_or_else(
                    || {
                        if clip_wrapped {
                            first_essence.value_len.saturating_sub(entry.stream_offset)
                        } else {
                            0
                        }
                    },
                    |next| next.stream_offset.saturating_sub(entry.stream_offset),
                );
                idx.add(FcIndexEntry {
                    pos: origin.saturating_add(entry.stream_offset),
                    timestamp: Timestamp::new(ticks),
                    flags: if entry.is_key_frame() {
                        IndexFlags::KEYFRAME
                    } else {
                        IndexFlags::empty()
                    },
                    size: size.try_into().unwrap_or(u32::MAX),
                    min_distance: 0,
                });
            }
        }
        out.insert(binding.stream_index, idx);
    }
    out
}

impl Demuxer for MxfDemuxer {
    fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.format_metadata
    }

    fn duration(&self) -> Option<Duration> {
        self.duration_exact
    }

    fn duration_exact(&self) -> Option<ExactDuration> {
        self.duration_exact
    }

    fn read_packet(&mut self) -> Result<Packet> {
        if self.eof {
            return Err(Error::Eof);
        }
        loop {
            let start = self.io.pos();
            let header = match klv::read_header(&mut self.io) {
                Ok(h) => h,
                Err(Error::UnexpectedEof) => {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                Err(e) => return Err(e),
            };
            if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack)
                || header.key.is_any_partition_pack()
            {
                if header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack)
                {
                    self.eof = true;
                    return Err(Error::Eof);
                }
                // A new partition: this crate's single-essence-track scope
                // (module docs) means there is nothing partition-specific to
                // do here beyond skipping past its own fixed-layout value;
                // essence elements are recognised by key regardless of which
                // partition carries them.
                self.budget.consume_fuel(1)?;
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if header.key == crate::ul::KLV_FILL_ITEM {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if header.key.is_index_table_segment() {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            if !header.key.is_essence_element() {
                // Filler, the Generic Container System Item, or a vendor
                // extension this crate does not interpret — skipped, not
                // fatal (D6: demuxing is lenient).
                klv::skip_value(&mut self.io, &header)?;
                continue;
            }
            let track_number = header.key.track_number();
            let Some(binding_idx) = self
                .bindings
                .iter()
                .position(|b| b.track_number == track_number)
            else {
                klv::skip_value(&mut self.io, &header)?;
                continue;
            };
            if header.length > MAX_PACKET_BYTES {
                return Err(Error::LimitExceeded {
                    limit: "mxf_packet_bytes",
                    requested: header.length,
                    cap: MAX_PACKET_BYTES,
                });
            }
            let len = usize::try_from(header.length).map_err(|_| Error::LimitExceeded {
                limit: "mxf_packet_bytes",
                requested: header.length,
                cap: MAX_PACKET_BYTES,
            })?;
            let mut pkt = Packet::alloc(&mut self.packet_budget, len)?;
            self.packet_budget.release(len as u64);
            self.io.read_exact(pkt.payload_mut())?;
            let Some(binding) = self.bindings.get_mut(binding_idx) else {
                // Unreachable in practice (`binding_idx` came from `.position()`
                // on this same, unmutated vector a few lines up), but a direct
                // index would violate the workspace's `indexing_slicing` deny
                // regardless of that invariant, so this is the honest spelling.
                continue;
            };
            pkt.stream_index = binding.stream_index;
            let ticks = binding.next_edit_unit;
            binding.next_edit_unit = binding.next_edit_unit.saturating_add(1);
            pkt.pts = Timestamp::new(ticks);
            pkt.dts = Timestamp::new(ticks);
            pkt.pos = Some(start);
            pkt.flags = PacketFlags::empty();
            if let Some(idx) = self.indices.get(&binding.stream_index) {
                if let Some(e) = idx.entries().iter().find(|e| e.pos == start)
                    && e.is_key()
                {
                    pkt.flags |= PacketFlags::KEY;
                }
            } else if ticks == 0 {
                // No index available: the conservative default is to trust
                // only the very first packet as a keyframe.
                pkt.flags |= PacketFlags::KEY;
            }
            return Ok(pkt);
        }
    }

    fn seek(&mut self, target: SeekTarget, flags: SeekFlags) -> Result<()> {
        let SeekTarget::Timestamp { stream_index, ts } = target else {
            return Err(Error::Unsupported(
                "mxf: only timestamp seeks are implemented",
            ));
        };
        let Some(index) = self.indices.get(&stream_index) else {
            return Err(Error::NotSeekable);
        };
        let Some(entry) = index.search(ts, flags) else {
            return Err(Error::NotSeekable);
        };
        self.io.seek(entry.pos)?;
        self.eof = false;
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|b| b.stream_index == stream_index)
        {
            binding.next_edit_unit = entry.timestamp.ticks().unwrap_or(0);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_format_core::discovery::NoParsers;
    use vaco_io::MemorySource;

    fn open_fixture(bytes: &'static [u8]) -> MxfDemuxer {
        let src = Box::new(MemorySource::new(bytes.to_vec()));
        MxfDemuxer::open(src, &NoParsers).unwrap()
    }

    #[test]
    fn op1a_fixture_reports_the_measured_stream_shape() {
        let demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        assert_eq!(demux.streams().len(), 1);
        let s = &demux.streams()[0];
        assert_eq!(s.media_type(), Some(MediaType::Video));
        let v = s.params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (720, 576));
        assert_eq!(
            s.params.codec_id,
            Some(vaco_codec_core::CodecId::Mpeg2video)
        );
    }

    #[test]
    fn ntsc_op1a_fixture_preserves_one_edit_unit_at_the_native_rate() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_ntsc_one_frame.mxf"));

        // Black-box reference: ffprobe 9.0.1 reports time_base=1001/30000,
        // duration_ts=1, duration=0.033367, and nb_read_packets=1.
        assert_eq!(demux.duration().map(Duration::as_ratio), Some((1001, 30_000)));
        assert_eq!(
            demux
                .duration_exact()
                .map(vaco_core::ExactDuration::as_ratio),
            Some((1001, 30_000))
        );
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.pts, Timestamp::ZERO);
        assert_eq!(packet.len, 2035);
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn op1a_fixture_demuxes_three_packets_matching_measured_positions_and_sizes() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        // Measured with `ffprobe -show_packets`: pos 6144/32768/47616,
        // sizes 26049/13853/1907.
        let expected = [(6144u64, 26049usize), (32768, 13853), (47616, 1907)];
        for (pos, size) in expected {
            let pkt = demux.read_packet().unwrap();
            assert_eq!(pkt.pos, Some(pos));
            assert_eq!(pkt.len, size);
        }
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn opatom_fixture_opens_and_demuxes() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/opatom_mpeg2_sample.mxf"));
        assert_eq!(demux.streams().len(), 1);
        let pkt = demux.read_packet().unwrap();
        assert!(pkt.len > 0);
    }

    #[test]
    fn opatom_read_edit_unit_lands_on_a_real_mpeg_start_code_for_every_entry() {
        // The bug this session found and fixed: a clip-wrapped (OP-Atom)
        // file's `IndexEntryArray::StreamOffset` is relative to the one
        // essence element's own *value*, not its key -- `is_clip_wrapped`'s
        // own doc comment has the full account, including the exact 25-byte
        // (16-byte key + 9-byte wide-form BER length) offset measured
        // directly against this real fixture. Before the fix, `read_edit_unit`
        // (or a `seek` to any edit unit past the first) landed inside the
        // *previous* edit unit's own tail bytes -- this test would have
        // failed the very first `assert_eq!` below with a garbage prefix
        // instead of `00 00 01`.
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/opatom_mpeg2_sample.mxf"));
        let stream_index = demux.streams()[0].index;
        for n in 0..3u64 {
            let pkt = demux.read_edit_unit(stream_index, n).unwrap();
            assert_eq!(
                &pkt.payload()[..3],
                &[0x00, 0x00, 0x01],
                "edit unit {n} does not start on a real MPEG-2 start code"
            );
        }
        // Past the last real entry: a clean, typed error, not a panic or a
        // silently-wrong read.
        assert!(matches!(
            demux.read_edit_unit(stream_index, 3),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn seeking_to_the_second_edit_unit_lands_on_its_measured_position() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        // Measured: this 3-frame clip's Index Table Segment marks only edit
        // unit 0 as a keyframe (long-GOP MPEG-2 with one I-frame), so a seek
        // to edit unit 1 needs `ANY` — without it, `search` correctly reports
        // no usable keyframe at or after that point, which is not a seek
        // failure, it is an accurate answer about this GOP.
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(1),
                },
                SeekFlags::ANY,
            )
            .unwrap();
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.pos, Some(32768));
    }

    #[test]
    fn seeking_backward_to_the_only_keyframe_works_without_any() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/op1a_mpeg2_sample.mxf"));
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(2),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.pos, Some(6144));
    }

    #[test]
    fn d10_fixture_reports_the_measured_stream_shape() {
        let demux = open_fixture(include_bytes!("../tests/fixtures/d10_mpeg2_sample.mxf"));
        assert_eq!(demux.streams().len(), 1);
        let s = &demux.streams()[0];
        assert_eq!(s.media_type(), Some(MediaType::Video));
        // Measured: `StoredHeight`/`SampledHeight`/`DisplayHeight` all read
        // 288 in this file's descriptor (`FrameLayout` 1, "Separate
        // Fields"), and `ffprobe` reports the frame itself as 720x576 —
        // double the stated height. See `descriptor::picture_parameters`.
        let v = s.params.video.as_ref().unwrap();
        assert_eq!((v.width, v.height), (720, 576));
        assert_eq!(
            s.params.codec_id,
            Some(vaco_codec_core::CodecId::Mpeg2video)
        );
    }

    #[test]
    fn d10_fixture_demuxes_three_packets_matching_measured_positions_and_sizes() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/d10_mpeg2_sample.mxf"));
        // Measured with `ffprobe -show_packets`: this is a single-partition
        // file (header partition pack states a nonzero `body_sid`, essence
        // follows its own metadata directly with no separate body
        // partition pack in between) -- the case that exposed
        // `metadata::scan_region` walking straight through the essence
        // body to the footer instead of stopping at it.
        let expected = [
            (6144u64, 150_000usize),
            (157_184, 150_000),
            (308_224, 150_000),
        ];
        for (pos, size) in expected {
            let pkt = demux.read_packet().unwrap();
            assert_eq!(pkt.pos, Some(pos));
            assert_eq!(pkt.len, size);
        }
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn d10_fixture_reports_the_measured_duration_despite_a_zero_index_duration() {
        let demux = open_fixture(include_bytes!("../tests/fixtures/d10_mpeg2_sample.mxf"));
        // Measured: this file's Index Table Segment states `IndexDuration =
        // 0` even though it plainly has three real frames -- the real count
        // has to come from the essence container's own measured size
        // divided by `EditUnitByteCount` (see `MxfDemuxer::open`'s
        // `effective_index_duration`). `ffprobe -show_entries
        // format=duration` reports `0.12` for this file (3 frames at 25
        // fps).
        assert_eq!(demux.duration().map(Duration::as_micros), Some(120_000));
    }

    #[test]
    fn seeking_the_d10_fixture_lands_on_its_measured_cbe_position() {
        let mut demux = open_fixture(include_bytes!("../tests/fixtures/d10_mpeg2_sample.mxf"));
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(2),
                },
                SeekFlags::empty(),
            )
            .unwrap();
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.pos, Some(308_224));
    }

    #[test]
    fn op1a_video_audio_fixture_reports_both_streams_with_measured_sound_parameters() {
        let demux = open_fixture(include_bytes!(
            "../tests/fixtures/op1a_mpeg2_pcm_sample.mxf"
        ));
        // A two-essence-track package: before `metadata::resolve_track_descriptor`
        // expanded the package's `MultipleDescriptor` via `SubDescriptorUIDs`,
        // every track resolved to the same descriptor id (one with none of
        // the properties either stream needs), and this file produced zero
        // streams at all.
        assert_eq!(demux.streams().len(), 2);
        let video = &demux.streams()[0];
        assert_eq!(video.media_type(), Some(MediaType::Video));
        assert_eq!(
            video.params.codec_id,
            Some(vaco_codec_core::CodecId::Mpeg2video)
        );
        let audio = &demux.streams()[1];
        assert_eq!(audio.media_type(), Some(MediaType::Audio));
        // Measured against `ffprobe`: `AES3PCMDescriptor` (class `0x47`)
        // carries raw, tightly-interleaved `pcm_s16le` verbatim, so this
        // class is the one this crate claims a `CodecId` for.
        assert_eq!(
            audio.params.codec_id,
            Some(vaco_codec_core::CodecId::PcmS16le)
        );
        let a = audio.params.audio.as_ref().unwrap();
        assert_eq!(a.sample_rate, 48_000);
        assert_eq!(a.layout.as_ref().unwrap().channels, 2);
        assert_eq!(a.format, Some(vaco_sampfmt::SampleFmt::S16));
    }

    #[test]
    fn op1a_video_audio_fixture_demuxes_interleaved_packets_matching_measured_positions_and_sizes()
    {
        let mut demux = open_fixture(include_bytes!(
            "../tests/fixtures/op1a_mpeg2_pcm_sample.mxf"
        ));
        // Measured with `ffprobe -show_packets`: video and audio essence
        // elements alternate, and the audio packet's own `len` (`7680`)
        // matches `ffprobe`'s reported size exactly -- unlike the D-10 AES3
        // case below, this descriptor class's bytes are already the real
        // interleaved PCM `ffprobe` reports, with nothing to unpack.
        let expected = [
            (0u32, 7168u64, 8063usize),
            (1, 15360, 7680),
            (0, 24064, 4598),
            (1, 29184, 7680),
            (0, 37888, 1106),
            (1, 39424, 7680),
        ];
        for (stream_index, pos, size) in expected {
            let pkt = demux.read_packet().unwrap();
            assert_eq!(pkt.stream_index, stream_index);
            assert_eq!(pkt.pos, Some(pos));
            assert_eq!(pkt.len, size);
        }
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn d10_audio_fixture_reports_measured_channels_and_sample_rate_but_no_codec_id() {
        let demux = open_fixture(include_bytes!(
            "../tests/fixtures/d10_mpeg2_aes3_sample.mxf"
        ));
        assert_eq!(demux.streams().len(), 2);
        let audio = &demux.streams()[1];
        assert_eq!(audio.media_type(), Some(MediaType::Audio));
        let a = audio.params.audio.as_ref().unwrap();
        // Descriptor-stated facts are accurate and match `ffprobe` exactly
        // (this fixture was muxed with `-d10_channelcount 8`, the
        // SMPTE-386M-compliant channel count).
        assert_eq!(a.sample_rate, 48_000);
        assert_eq!(a.layout.as_ref().unwrap().channels, 8);
        assert_eq!(a.format, Some(vaco_sampfmt::SampleFmt::S16));
        // But the packet bytes are not literal `pcm_s16le` (see
        // `descriptor::sound_parameters`'s module docs for the measured
        // AES3-bundle layout this crate does not unpack) -- claiming a
        // `CodecId` here would be actively wrong, not just incomplete.
        assert_eq!(audio.params.codec_id, None);
    }

    #[test]
    fn d10_audio_fixture_packet_length_is_the_real_aes3_bundle_not_ffprobes_unpacked_size() {
        let mut demux = open_fixture(include_bytes!(
            "../tests/fixtures/d10_mpeg2_aes3_sample.mxf"
        ));
        demux.read_packet().unwrap(); // video, stream 0
        let audio_pkt = demux.read_packet().unwrap();
        assert_eq!(audio_pkt.stream_index, 1);
        assert_eq!(audio_pkt.pos, Some(157_696));
        // The raw KLV genuinely declares 61444 bytes (confirmed with `xxd`
        // against the file directly) -- `ffprobe` reports `30720`
        // (`1920 samples * 8 channels * 2 bytes`) for the same packet, which
        // is `ffmpeg`'s own AES3-to-PCM unpacking, not the container's
        // declared essence length. `4 + 1920 * 8 * 4 == 61444`: a 4-byte
        // element header of undetermined meaning, then 1920 sample instants
        // of 8 fixed channel slots, each a 4-byte word (1 tag byte + a
        // 24-bit sample left-shifted 4 bits). This crate reports the real,
        // unmodified length rather than silently substituting the smaller
        // unpacked size.
        assert_eq!(audio_pkt.len, 61_444);
    }

    #[test]
    fn a_huge_cbe_entry_count_is_capped_not_looped_over_unbounded() {
        // A hostile or merely huge file could drive `effective_index_duration`
        // to a very large value (a real file's size divided by a small
        // `EditUnitByteCount`) -- `build_indices`'s `for n in 0..count` loop
        // must not run `i64::MAX` times before `PacketIndex::add`'s own
        // decimation ever gets a chance to bound memory. `MAX_CBE_INDEX_ENTRIES`
        // bounds the loop itself; `PacketIndex`'s own `max_entries` then
        // decimates that down further, which is why the final stored count
        // is well under the loop cap, not equal to it -- this test's job is
        // only to confirm the whole call returns promptly and does not try
        // to iterate `i64::MAX` times.
        let bindings = vec![TrackBinding {
            stream_index: 0,
            track_number: 1,
            edit_rate: Rational { num: 25, den: 1 },
            next_edit_unit: 0,
        }];
        let segments = vec![IndexTableSegment {
            index_edit_rate: Some(Rational { num: 25, den: 1 }),
            edit_unit_byte_count: 1,
            ..Default::default()
        }];
        let huge = |_seg: &IndexTableSegment| i64::MAX;
        let first = FirstEssenceElement {
            key_pos: 0,
            value_offset: 0,
            value_len: 0,
        };
        let indices = build_indices(&bindings, &segments, &first, &huge);
        let idx = indices.get(&0).unwrap();
        assert!(!idx.entries().is_empty());
        assert!((idx.entries().len() as u64) < MAX_CBE_INDEX_ENTRIES);
    }

    #[test]
    fn a_non_mxf_source_is_rejected_not_panicked_on() {
        let src = Box::new(MemorySource::new(
            b"not an mxf file at all, just prose".to_vec(),
        ));
        assert!(MxfDemuxer::open(src, &NoParsers).is_err());
    }

    /// The exact shape of `slow-unit-a4c0af443812c5c6e4cc5601feb1ab8b163d65b7`:
    /// a CBE segment whose stated `IndexDuration` wildly overshoots what a
    /// 9,934-byte essence container could hold at a 212,992-byte
    /// `EditUnitByteCount`. The old code trusted any positive
    /// `IndexDuration` outright and fell through to the fixed
    /// `MAX_CBE_INDEX_ENTRIES` ceiling (16,777,216) instead of the real,
    /// much smaller, size-derived count.
    #[test]
    fn a_stated_index_duration_is_clamped_to_the_measured_essence_size() {
        let seg = IndexTableSegment {
            index_duration: 144_115_188_075_855_872,
            edit_unit_byte_count: 212_992,
            ..Default::default()
        };
        // 9_934 / 212_992 == 0: the file cannot hold even one full edit unit.
        assert_eq!(effective_index_duration(&seg, Some(9_934)), 0);
        assert!(
            effective_index_duration(&seg, Some(9_934))
                < i64::try_from(MAX_CBE_INDEX_ENTRIES).unwrap(),
            "must not fall through to the fixed ceiling when a real size is known"
        );
    }

    #[test]
    fn a_stated_index_duration_within_the_measured_size_passes_through() {
        let seg = IndexTableSegment {
            index_duration: 25,
            edit_unit_byte_count: 1_000,
            ..Default::default()
        };
        assert_eq!(effective_index_duration(&seg, Some(1_000_000)), 25);
    }

    /// The pre-existing "`IndexDuration == 0` means unknown, not empty" case
    /// (a real D-10 fixture) must still recover the size-derived count.
    #[test]
    fn a_zero_index_duration_falls_back_to_the_measured_size() {
        let seg = IndexTableSegment {
            index_duration: 0,
            edit_unit_byte_count: 1_000,
            ..Default::default()
        };
        assert_eq!(effective_index_duration(&seg, Some(25_000)), 25);
    }

    #[test]
    fn without_a_measured_size_a_stated_duration_still_hits_the_fixed_ceiling() {
        let seg = IndexTableSegment {
            index_duration: i64::MAX,
            edit_unit_byte_count: 1,
            ..Default::default()
        };
        assert_eq!(
            effective_index_duration(&seg, None),
            i64::try_from(MAX_CBE_INDEX_ENTRIES).unwrap()
        );
    }

    #[test]
    fn a_non_cbe_segment_is_not_size_bounded() {
        // `edit_unit_byte_count: 0` makes `is_cbe()` false; the size-derived
        // bound only applies to CBE segments (a VBE segment's own `entries`
        // already physically bound `build_indices`'s loop over it).
        let seg = IndexTableSegment {
            index_duration: 25,
            edit_unit_byte_count: 0,
            ..Default::default()
        };
        assert_eq!(effective_index_duration(&seg, Some(1)), 25);
    }
}
