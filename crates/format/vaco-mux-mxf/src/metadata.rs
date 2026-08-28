//! Building the structural-metadata graph `vaco-demux-mxf` reads:
//! `Preface` -> `ContentStorage` -> `Package` (Material and Source) ->
//! `Track` -> `Sequence` -> `SourceClip` -> `Descriptor`.
//!
//! # Scope
//!
//! One video track, at most one audio track (SMPTE ST 377-1's `OP1a`
//! genuinely allows either shape). Two essence tracks means the Source
//! Package's descriptor is a `MultipleDescriptor` whose `SubDescriptorUIDs`
//! name each track's real descriptor, matched back by `LinkedTrackId` — the
//! exact mechanism `vaco-demux-mxf::metadata::resolve_track_descriptor`
//! expands on the read side (this session's own fix there), now constructed
//! on the write side.
//!
//! Every tag number and property UL below reuses this workspace's own
//! prior clean-room measurement (`vaco-demux-mxf::properties::TABLE`); see
//! `ul.rs`'s module docs and `provenance/sources.toml`'s
//! `ffmpeg-mxf-mux-header-probe` entry for the handful of tag numbers this
//! session measured freshly (the demux crate only needed the resolved UL,
//! not which conventional local tag carries it).

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Rational};

use crate::localset::{
    push_batch16, push_i64, push_item, push_rational, push_u16, push_u32, push_u64, push_u8,
    push_uid16, push_umid32,
};
use crate::uid::IdGenerator;
use crate::ul::{self, Ul, class, structural_set_key};

/// One essence track's `InstanceUID`s, generated once at `init()` and reused
/// for both the header and footer copies of the graph.
#[derive(Debug, Clone)]
pub(crate) struct TrackIds {
    pub mp_track: [u8; 16],
    pub mp_sequence: [u8; 16],
    pub mp_clip: [u8; 16],
    pub sp_track: [u8; 16],
    pub sp_sequence: [u8; 16],
    pub sp_clip: [u8; 16],
    pub descriptor: [u8; 16],
}

impl TrackIds {
    pub(crate) fn new(idgen: &mut IdGenerator) -> Self {
        Self {
            mp_track: idgen.instance_uid(),
            mp_sequence: idgen.instance_uid(),
            mp_clip: idgen.instance_uid(),
            sp_track: idgen.instance_uid(),
            sp_sequence: idgen.instance_uid(),
            sp_clip: idgen.instance_uid(),
            descriptor: idgen.instance_uid(),
        }
    }
}

/// One planned essence track: what `add_stream` recorded plus the ids and
/// Generic Container track number it was assigned.
#[derive(Debug, Clone)]
pub(crate) struct TrackPlan {
    pub media_type: MediaType,
    pub params: CodecParameters,
    pub track_id: u32,
    pub gc_track_number: [u8; 4],
    pub ids: TrackIds,
}

/// Every id generated once at `init()`: package- and file-level ids plus
/// each track's own. Reused unchanged for both the header and footer
/// partition's copy of the graph — a real reader matching `InstanceUID`s
/// (or a `SourceClip`'s `SourcePackageID` UMID) across the two copies needs
/// them to agree.
#[derive(Debug, Clone)]
pub(crate) struct GraphIds {
    pub preface: [u8; 16],
    pub identification: [u8; 16],
    pub content_storage: [u8; 16],
    pub material_package: [u8; 16],
    pub material_umid: [u8; 32],
    pub source_package: [u8; 16],
    pub source_umid: [u8; 32],
    /// Present only when more than one essence track is written.
    pub multiple_descriptor: Option<[u8; 16]>,
    /// `TrackID = 1` on both packages — see `mux.rs`'s own comment on why
    /// essence tracks start at `TrackID = 2` instead.
    pub timecode: TimecodeIds,
}

impl GraphIds {
    pub(crate) fn new(idgen: &mut IdGenerator, track_count: usize) -> Self {
        Self {
            preface: idgen.instance_uid(),
            identification: idgen.instance_uid(),
            content_storage: idgen.instance_uid(),
            material_package: idgen.instance_uid(),
            material_umid: idgen.package_umid(),
            source_package: idgen.instance_uid(),
            source_umid: idgen.package_umid(),
            multiple_descriptor: (track_count > 1).then(|| idgen.instance_uid()),
            timecode: TimecodeIds::new(idgen),
        }
    }
}

/// A timecode track's own ids, one pair of (Track, Sequence,
/// `TimecodeComponent`) per package — no descriptor, no essence: this track
/// carries no Generic Container element at all, matching a real file's own
/// shape (measured this session: `vaco-demux-mxf`'s own read side already
/// recognises and skips a timecode track's `StructuralClass::TimecodeComponent`
/// via `metadata::resolve_essence`'s `this_timecode` handling).
#[derive(Debug, Clone)]
pub(crate) struct TimecodeIds {
    pub mp_track: [u8; 16],
    pub mp_sequence: [u8; 16],
    pub mp_component: [u8; 16],
    pub sp_track: [u8; 16],
    pub sp_sequence: [u8; 16],
    pub sp_component: [u8; 16],
}

impl TimecodeIds {
    fn new(idgen: &mut IdGenerator) -> Self {
        Self {
            mp_track: idgen.instance_uid(),
            mp_sequence: idgen.instance_uid(),
            mp_component: idgen.instance_uid(),
            sp_track: idgen.instance_uid(),
            sp_sequence: idgen.instance_uid(),
            sp_component: idgen.instance_uid(),
        }
    }
}

/// `Timecode`/`Picture`/`Sound` `DataDefinition` labels, measured directly
/// off real `ffmpeg -f mxf` files' `Sequence` sets this session
/// (`provenance/sources.toml`'s `ffmpeg-mxf-mux-header-probe` entry).
///
/// Getting these three swapped was this crate's own real bug, caught by
/// cross-checking against a real `ffmpeg -i`, not by this crate's own
/// demuxer: a single-video-track file whose lone essence track's
/// `Sequence` carried [`DATA_DEFINITION_TIMECODE`] (mislabelled as
/// "picture" in an earlier version of this file) opened fine under
/// `vaco-demux-mxf` — which never interprets a `DataDefinition` value at
/// all, so it could not have noticed — but `ffmpeg -i` reported the stream
/// as `Data: mpeg2video` ("Codec type or id mismatches"), because its own
/// demuxer *does* key media type off this property. The three values share
/// an 11-byte prefix and differ only in bytes 11 and 12: byte 11
/// distinguishes "timecode" (`0x01`) from "essence" (`0x02`); within
/// "essence", byte 12 distinguishes picture (`0x01`) from sound (`0x02`).
const DATA_DEFINITION_TIMECODE: Ul = Ul::new([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00,
    0x00,
]);
const DATA_DEFINITION_PICTURE: Ul = Ul::new([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x01, 0x00, 0x00,
    0x00,
]);
const DATA_DEFINITION_SOUND: Ul = Ul::new([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x02, 0x00, 0x00,
    0x00,
]);

fn data_definition(media_type: MediaType) -> Ul {
    if media_type == MediaType::Audio {
        DATA_DEFINITION_SOUND
    } else {
        DATA_DEFINITION_PICTURE
    }
}

/// Duration in edit units for a track, or `-1` ("not known when written",
/// the SMPTE ST 377-1 convention this session's header copy relies on —
/// see `mux.rs`'s module docs on why no backpatch is needed).
fn duration_value(duration: Option<i64>) -> i64 {
    duration.unwrap_or(-1)
}

/// Build one full structural-metadata set (`Tag Length Value` items) and
/// wrap it in its own KLV-ready `(key, value)` pair.
fn build_set(class: u8, items: Vec<u8>) -> ([u8; 16], Vec<u8>) {
    (structural_set_key(class), items)
}

/// Everything the Primer Pack must map: `(tag, UL)` for every property this
/// module writes. Conventional RP210 tag numbers, reused from
/// `vaco-demux-mxf::properties::TABLE` (the same UL, in every case that
/// crate also registers) or measured fresh this session for the handful
/// (structural-class-set-only tags like `PackageUmid`'s `0x4401`) that
/// crate never needed a tag number for.
#[must_use]
pub(crate) fn primer_entries() -> Vec<(u16, Ul)> {
    vec![
        (0x3c0a, Ul::new(iu())),
        (0x3b03, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x02, 0x01,
            0x00, 0x00,
        ])),
        (0x3b06, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x06, 0x04,
            0x00, 0x00,
        ])),
        (0x3b09, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x02, 0x02, 0x03, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3b0a, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x02, 0x02, 0x10, 0x02, 0x01,
            0x00, 0x00,
        ])),
        (0x1901, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x05, 0x01,
            0x00, 0x00,
        ])),
        (0x4401, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x15, 0x10, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x4403, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x06, 0x05,
            0x00, 0x00,
        ])),
        (0x4701, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x02, 0x03,
            0x00, 0x00,
        ])),
        (0x4801, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x01, 0x07, 0x01, 0x01, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x4804, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x01, 0x04, 0x01, 0x03, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x4b01, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x05, 0x30, 0x04, 0x05, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x4803, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x02, 0x04,
            0x00, 0x00,
        ])),
        (0x0201, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x04, 0x07, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x0202, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x07, 0x02, 0x02, 0x01, 0x01, 0x03,
            0x00, 0x00,
        ])),
        (0x1001, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x06, 0x09,
            0x00, 0x00,
        ])),
        (0x1201, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x07, 0x02, 0x01, 0x03, 0x01, 0x04,
            0x00, 0x00,
        ])),
        (0x1101, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x03, 0x01, 0x00,
            0x00, 0x00,
        ])),
        (0x1102, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x03, 0x02, 0x00,
            0x00, 0x00,
        ])),
        (0x3006, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x06, 0x01, 0x01, 0x03, 0x05, 0x00,
            0x00, 0x00,
        ])),
        (0x3001, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x06, 0x01, 0x01, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3004, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x06, 0x01, 0x01, 0x04, 0x01, 0x02,
            0x00, 0x00,
        ])),
        (0x3202, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x02, 0x01, 0x00,
            0x00, 0x00,
        ])),
        (0x3203, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x02, 0x02, 0x00,
            0x00, 0x00,
        ])),
        (0x3204, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x01, 0x07, 0x00,
            0x00, 0x00,
        ])),
        (0x3205, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x01, 0x08, 0x00,
            0x00, 0x00,
        ])),
        (0x3208, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x01, 0x0b, 0x00,
            0x00, 0x00,
        ])),
        (0x3209, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x05, 0x01, 0x0c, 0x00,
            0x00, 0x00,
        ])),
        (0x320c, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x03, 0x01, 0x04, 0x00,
            0x00, 0x00,
        ])),
        (0x3201, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x04, 0x01, 0x06, 0x01, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3f01, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x06, 0x01, 0x01, 0x04, 0x06, 0x0b,
            0x00, 0x00,
        ])),
        (0x3d07, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x04, 0x02, 0x01, 0x01, 0x04, 0x00,
            0x00, 0x00,
        ])),
        (0x3d01, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x04, 0x02, 0x03, 0x03, 0x04, 0x00,
            0x00, 0x00,
        ])),
        (0x3d0a, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x04, 0x02, 0x03, 0x02, 0x01, 0x00,
            0x00, 0x00,
        ])),
        (0x3d03, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x04, 0x02, 0x03, 0x01, 0x01, 0x01,
            0x00, 0x00,
        ])),
        (0x3f0b, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x05, 0x30, 0x04, 0x06, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3f0c, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x07, 0x02, 0x01, 0x03, 0x01, 0x0a,
            0x00, 0x00,
        ])),
        (0x3f0d, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x07, 0x02, 0x02, 0x01, 0x01, 0x02,
            0x00, 0x00,
        ])),
        (0x3f05, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x04, 0x06, 0x02, 0x01, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3f06, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x01, 0x03, 0x04, 0x05, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3f07, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x01, 0x03, 0x04, 0x04, 0x00, 0x00,
            0x00, 0x00,
        ])),
        (0x3f08, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x04, 0x04, 0x04, 0x04, 0x01, 0x01, 0x00,
            0x00, 0x00,
        ])),
        (0x3f0a, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x05, 0x04, 0x04, 0x04, 0x02, 0x05, 0x00,
            0x00, 0x00,
        ])),
        (0x1501, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x07, 0x02, 0x01, 0x03, 0x01, 0x05,
            0x00, 0x00,
        ])),
        (0x1502, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x04, 0x04, 0x01, 0x01, 0x02, 0x06,
            0x00, 0x00,
        ])),
        (0x1503, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x04, 0x01, 0x01, 0x05, 0x00,
            0x00, 0x00,
        ])),
        (0x4b02, Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x07, 0x02, 0x01, 0x03, 0x01, 0x03,
            0x00, 0x00,
        ])),
    ]
}

/// `InstanceUid`'s own property UL — pulled out so [`primer_entries`]'s
/// first row and every set's own `push_uid16(0x3c0a, ..)` agree by
/// construction.
const fn iu() -> [u8; 16] {
    [
        0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x15, 0x02, 0x00, 0x00, 0x00,
        0x00,
    ]
}

/// Build every structural-metadata set for one copy of the graph (used
/// identically for the header, with `duration: None`, and the footer, with
/// the real final duration).
#[must_use]
pub(crate) fn build_sets(
    ids: &GraphIds,
    tracks: &[TrackPlan],
    edit_rate: Rational,
    duration: Option<i64>,
) -> Vec<([u8; 16], Vec<u8>)> {
    let mut sets = Vec::new();

    // -------------------------------------------------------------- Preface
    let mut preface = Vec::new();
    push_uid16(&mut preface, 0x3c0a, ids.preface);
    push_uid16(&mut preface, 0x3b03, ids.content_storage);
    push_batch16(&mut preface, 0x3b06, &[ids.identification]);
    push_item(&mut preface, 0x3b09, &ul::OPERATIONAL_PATTERN_OP1A.as_bytes());
    push_batch16(&mut preface, 0x3b0a, &essence_containers_used(tracks));
    sets.push(build_set(class::PREFACE, preface));

    // ------------------------------------------------------- Identification
    let mut ident = Vec::new();
    push_uid16(&mut ident, 0x3c0a, ids.identification);
    sets.push(build_set(class::IDENTIFICATION, ident));

    // ------------------------------------------------------- ContentStorage
    let mut cs = Vec::new();
    push_uid16(&mut cs, 0x3c0a, ids.content_storage);
    push_batch16(&mut cs, 0x1901, &[ids.material_package, ids.source_package]);
    sets.push(build_set(class::CONTENT_STORAGE, cs));

    // ------------------------------------------------------ Material Package
    let mut mp_track_uids: Vec<[u8; 16]> = vec![ids.timecode.mp_track];
    mp_track_uids.extend(tracks.iter().map(|t| t.ids.mp_track));
    let mut mp = Vec::new();
    push_uid16(&mut mp, 0x3c0a, ids.material_package);
    push_umid32(&mut mp, 0x4401, ids.material_umid);
    push_batch16(&mut mp, 0x4403, &mp_track_uids);
    sets.push(build_set(class::MATERIAL_PACKAGE, mp));

    // ----------------------------------------------------- Source Package
    let mut sp_track_uids: Vec<[u8; 16]> = vec![ids.timecode.sp_track];
    sp_track_uids.extend(tracks.iter().map(|t| t.ids.sp_track));
    let descriptor_ref = ids.multiple_descriptor.unwrap_or_else(|| {
        tracks
            .first()
            .map_or(ids.source_package, |t| t.ids.descriptor)
    });
    let mut sp = Vec::new();
    push_uid16(&mut sp, 0x3c0a, ids.source_package);
    push_umid32(&mut sp, 0x4401, ids.source_umid);
    push_batch16(&mut sp, 0x4403, &sp_track_uids);
    push_uid16(&mut sp, 0x4701, descriptor_ref);
    sets.push(build_set(class::SOURCE_PACKAGE, sp));

    let dur = duration_value(duration);

    // ------------------------------------------------------- Timecode track
    //
    // `TrackID = 1` on both packages (see `mux.rs`'s comment on why essence
    // tracks start at 2): a real `ffmpeg -f mxf` file's own demuxer keys
    // off this convention structurally, not just off `DataDefinition` —
    // see [`DATA_DEFINITION_TIMECODE`]'s doc comment for how this was
    // found. `TimecodeStart`/`Base`/`DropFrame` are the same fixed values
    // (`0`, this file's edit rate rounded, not drop-frame) on both
    // packages, matching every real fixture measured this session.
    #[allow(
        clippy::integer_division,
        reason = "edit_rate.den is checked non-zero immediately above"
    )]
    let tc_base = if edit_rate.den > 0 {
        (edit_rate.num / edit_rate.den).max(1) as u16
    } else {
        25
    };
    for (track_uid, seq_uid, comp_uid, is_material) in [
        (ids.timecode.mp_track, ids.timecode.mp_sequence, ids.timecode.mp_component, true),
        (ids.timecode.sp_track, ids.timecode.sp_sequence, ids.timecode.sp_component, false),
    ] {
        let _ = is_material;
        let mut track = Vec::new();
        push_uid16(&mut track, 0x3c0a, track_uid);
        push_u32(&mut track, 0x4801, 1);
        push_u32(&mut track, 0x4804, 0);
        push_rational(&mut track, 0x4b01, edit_rate.num, edit_rate.den);
        push_u64(&mut track, 0x4b02, 0);
        push_uid16(&mut track, 0x4803, seq_uid);
        sets.push(build_set(class::TRACK, track));

        let mut seq = Vec::new();
        push_uid16(&mut seq, 0x3c0a, seq_uid);
        push_item(&mut seq, 0x0201, &DATA_DEFINITION_TIMECODE.as_bytes());
        push_i64(&mut seq, 0x0202, dur);
        push_batch16(&mut seq, 0x1001, &[comp_uid]);
        sets.push(build_set(class::SEQUENCE, seq));

        let mut comp = Vec::new();
        push_uid16(&mut comp, 0x3c0a, comp_uid);
        push_item(&mut comp, 0x0201, &DATA_DEFINITION_TIMECODE.as_bytes());
        push_i64(&mut comp, 0x0202, dur);
        push_i64(&mut comp, 0x1501, 0); // TimecodeStart.
        push_u16(&mut comp, 0x1502, tc_base); // TimecodeRoundedBase.
        push_u8(&mut comp, 0x1503, 0); // TimecodeDropFrame.
        sets.push(build_set(class::TIMECODE_COMPONENT, comp));
    }

    for (i, t) in tracks.iter().enumerate() {
        let dd = data_definition(t.media_type);

        // Material Package side: Track -> Sequence -> SourceClip.
        let mut mtrack = Vec::new();
        push_uid16(&mut mtrack, 0x3c0a, t.ids.mp_track);
        push_u32(&mut mtrack, 0x4801, t.track_id);
        push_u32(&mut mtrack, 0x4804, 0); // Material Package track number is conventionally 0.
        push_rational(&mut mtrack, 0x4b01, edit_rate.num, edit_rate.den);
        push_u64(&mut mtrack, 0x4b02, 0);
        push_uid16(&mut mtrack, 0x4803, t.ids.mp_sequence);
        sets.push(build_set(class::TRACK, mtrack));

        let mut mseq = Vec::new();
        push_uid16(&mut mseq, 0x3c0a, t.ids.mp_sequence);
        push_item(&mut mseq, 0x0201, &dd.as_bytes());
        push_i64(&mut mseq, 0x0202, dur);
        push_batch16(&mut mseq, 0x1001, &[t.ids.mp_clip]);
        sets.push(build_set(class::SEQUENCE, mseq));

        let mut mclip = Vec::new();
        push_uid16(&mut mclip, 0x3c0a, t.ids.mp_clip);
        push_item(&mut mclip, 0x0201, &dd.as_bytes());
        push_i64(&mut mclip, 0x0202, dur);
        push_u64(&mut mclip, 0x1201, 0);
        push_umid32(&mut mclip, 0x1101, ids.source_umid);
        push_u32(&mut mclip, 0x1102, t.track_id);
        sets.push(build_set(class::SOURCE_CLIP, mclip));

        // Source Package side: Track -> Sequence (SourceClip omitted — this
        // is where real essence terminates; `resolve_essence` stops at a
        // package whose own `PackageDescriptor` is set, which the Source
        // Package already carries).
        let gc_track_number = u32::from_be_bytes(t.gc_track_number);
        let mut strack = Vec::new();
        push_uid16(&mut strack, 0x3c0a, t.ids.sp_track);
        push_u32(&mut strack, 0x4801, t.track_id);
        push_u32(&mut strack, 0x4804, gc_track_number);
        push_rational(&mut strack, 0x4b01, edit_rate.num, edit_rate.den);
        push_u64(&mut strack, 0x4b02, 0);
        push_uid16(&mut strack, 0x4803, t.ids.sp_sequence);
        sets.push(build_set(class::TRACK, strack));

        let mut sseq = Vec::new();
        push_uid16(&mut sseq, 0x3c0a, t.ids.sp_sequence);
        push_item(&mut sseq, 0x0201, &dd.as_bytes());
        push_i64(&mut sseq, 0x0202, dur);
        push_batch16(&mut sseq, 0x1001, &[t.ids.sp_clip]);
        sets.push(build_set(class::SEQUENCE, sseq));

        let mut sclip = Vec::new();
        push_uid16(&mut sclip, 0x3c0a, t.ids.sp_clip);
        push_item(&mut sclip, 0x0201, &dd.as_bytes());
        push_i64(&mut sclip, 0x0202, dur);
        push_u64(&mut sclip, 0x1201, 0);
        // A Source Package's own SourceClip names itself: this is where
        // essence actually lives, one generation, no further chase.
        push_umid32(&mut sclip, 0x1101, ids.source_umid);
        push_u32(&mut sclip, 0x1102, t.track_id);
        sets.push(build_set(class::SOURCE_CLIP, sclip));

        // Descriptor.
        sets.push(build_descriptor(t, i, edit_rate));
    }

    if let Some(md_id) = ids.multiple_descriptor {
        let sub_ids: Vec<[u8; 16]> = tracks.iter().map(|t| t.ids.descriptor).collect();
        let mut md = Vec::new();
        push_uid16(&mut md, 0x3c0a, md_id);
        push_rational(&mut md, 0x3001, edit_rate.num, edit_rate.den);
        push_item(&mut md, 0x3004, &ul::ESSENCE_CONTAINER_MULTIPLE_WRAPPINGS.as_bytes());
        push_batch16(&mut md, 0x3f01, &sub_ids);
        sets.push(build_set(class::MULTIPLE_DESCRIPTOR, md));
    }

    sets
}

/// The Generic Container essence-container label a track's own descriptor
/// states — distinct per media type, measured this session (see
/// `ul::ESSENCE_CONTAINER_SOUND_FRAME_WRAPPED`'s doc comment for what
/// reusing the picture label for audio broke).
fn essence_container_for(media_type: MediaType) -> Ul {
    if media_type == MediaType::Audio {
        ul::ESSENCE_CONTAINER_SOUND_FRAME_WRAPPED
    } else {
        ul::ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED
    }
}

/// The distinct essence-container labels `Preface.EssenceContainers` and
/// both partition packs' own `EssenceContainers` batch must list: one per
/// media type actually present, plus the "multiple wrappings" label when
/// more than one essence track is written — measured off a real two-track
/// file (three labels, in that order) rather than assumed.
pub(crate) fn essence_containers_used(tracks: &[TrackPlan]) -> Vec<[u8; 16]> {
    let mut out = Vec::new();
    if tracks.iter().any(|t| t.media_type != MediaType::Audio) {
        out.push(ul::ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED.as_bytes());
    }
    if tracks.iter().any(|t| t.media_type == MediaType::Audio) {
        out.push(ul::ESSENCE_CONTAINER_SOUND_FRAME_WRAPPED.as_bytes());
    }
    if tracks.len() > 1 {
        out.push(ul::ESSENCE_CONTAINER_MULTIPLE_WRAPPINGS.as_bytes());
    }
    out
}

fn build_descriptor(t: &TrackPlan, _index: usize, edit_rate: Rational) -> ([u8; 16], Vec<u8>) {
    let essence_container = essence_container_for(t.media_type);
    let mut d = Vec::new();
    push_uid16(&mut d, 0x3c0a, t.ids.descriptor);
    push_u32(&mut d, 0x3006, t.track_id); // LinkedTrackId
    push_rational(&mut d, 0x3001, edit_rate.num, edit_rate.den); // SampleRate (edit rate)
    push_item(&mut d, 0x3004, &essence_container.as_bytes());

    match (t.media_type, &t.params.video) {
        (MediaType::Video, Some(v)) => {
            push_u32(&mut d, 0x3202, v.height);
            push_u32(&mut d, 0x3203, v.width);
            push_u32(&mut d, 0x3204, v.height);
            push_u32(&mut d, 0x3205, v.width);
            push_u32(&mut d, 0x3208, v.height);
            push_u32(&mut d, 0x3209, v.width);
            push_u8(&mut d, 0x320c, 0); // FrameLayout: FullFrame (progressive).
            // Only MPEG-2 long-GOP is measured against a real file (see
            // `ul::PICTURE_ESSENCE_CODING_MPEG2_LONG_GOP`'s own doc comment)
            // — this is the only `CodecId` this crate's `add_stream` accepts
            // for video today, so there is no second arm to guess at yet.
            push_item(&mut d, 0x3201, &ul::PICTURE_ESSENCE_CODING_MPEG2_LONG_GOP.as_bytes());
            build_set(class::MPEG_VIDEO_DESCRIPTOR, d)
        }
        (MediaType::Audio, _) => {
            if let Some(a) = &t.params.audio {
                push_rational(&mut d, 0x3d03, a.sample_rate.cast_signed(), 1);
                let channels = a.layout.as_ref().map_or(0, |l| l.channels);
                push_u32(&mut d, 0x3d07, channels);
                let bits = a.bits_per_coded_sample.unwrap_or(16);
                push_u32(&mut d, 0x3d01, u32::from(bits));
                let bytes_per_sample = u32::from(bits) >> 3;
                let block_align = (bytes_per_sample * channels).min(u32::from(u16::MAX)) as u16;
                push_u16(&mut d, 0x3d0a, block_align);
            }
            build_set(class::AES3_PCM_DESCRIPTOR, d)
        }
        _ => build_set(class::MPEG_VIDEO_DESCRIPTOR, d),
    }
}

