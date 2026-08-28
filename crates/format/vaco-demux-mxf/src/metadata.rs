//! The structural-metadata graph (SMPTE ST 377-1 §7): `Preface` →
//! `ContentStorage` → `Package` (Material/Source) → `Track` → `Sequence` →
//! `StructuralComponent` (`SourceClip`/`TimecodeComponent`) → `Descriptor`.
//!
//! # Why this is a graph, not a tree, and what that costs
//!
//! Every cross-set reference — `ContentStorage.Packages`, `Package.Tracks`,
//! `Track.Sequence`, `Sequence.StructuralComponents`, and critically
//! `SourceClip.SourcePackageID` — is a 16-byte Instance UID or UMID pointing
//! at another set, resolved by lookup rather than by nesting. That is what
//! makes multi-generation essence (a package whose `SourceClip` points at
//! *another* source package, tracing history through re-digitisations or
//! transcodes) representable at all, and it is also what makes a cycle
//! representable: nothing in the encoding stops a hostile file from making a
//! `SourceClip` name its own package, or two packages name each other.
//!
//! # How this crate bounds that
//!
//! [`resolve_essence`] — the function that walks Material Package → Track →
//! Sequence → `SourceClip` → (by UMID) Source Package, repeating for however
//! many generations a file's history chain has — carries two independent
//! bounds, deliberately redundant with each other:
//!
//! 1. **A visited-UMID set.** Every package UMID visited is recorded before
//!    its Track/Sequence/SourceClip are followed; if the next hop's target
//!    UMID is already in the set, that is a cycle by definition (the state
//!    space is finite and we have proof of a repeat), and resolution stops
//!    with [`Error::InvalidData`] rather than recursing.
//! 2. **[`MAX_CHAIN_DEPTH`].** Independent of the visited set, because a
//!    *very large but finite* number of distinct generations is a different
//!    failure shape (slow, not infinite) and deserves its own cap — the same
//!    "two independent bounds" pattern `vaco-limits` uses for allocation
//!    (declared-size check *and* budget cap).
//!
//! Everything else in this module (parsing one set's properties, building
//! the instance-UID-keyed arena) is a flat pass with no recursion at all —
//! only the source-reference chase in [`resolve_essence`] can cycle, so only
//! it needs guarding.

use std::collections::{HashMap, HashSet};

use vaco_core::{Error, Result};
use vaco_io::IoContext;
use vaco_limits::Budget;

use crate::klv;
use crate::localset;
use crate::properties::{PropertyId, Resolver};
use crate::ul::{PartitionFamilyKind, StructuralClass, Ul};

pub type InstanceUid = [u8; 16];

/// One structural-metadata set, with only the properties this crate
/// recognises retained (see [`Resolver`] for why an unrecognised local tag
/// is dropped rather than guessed at).
#[derive(Debug, Clone)]
pub struct MetadataSet {
    pub class: StructuralClass,
    pub instance_uid: Option<InstanceUid>,
    pub props: HashMap<PropertyId, Vec<u8>>,
}

impl MetadataSet {
    fn get(&self, p: PropertyId) -> Option<&[u8]> {
        self.props.get(&p).map(Vec::as_slice)
    }
    fn u32(&self, p: PropertyId) -> Option<u32> {
        localset::u32_be(self.get(p)?)
    }
    fn u16(&self, p: PropertyId) -> Option<u16> {
        localset::u16_be(self.get(p)?)
    }
    fn u8(&self, p: PropertyId) -> Option<u8> {
        localset::u8_(self.get(p)?)
    }
    fn i64(&self, p: PropertyId) -> Option<i64> {
        localset::i64_be(self.get(p)?)
    }
    fn rational(&self, p: PropertyId) -> Option<vaco_core::Rational> {
        localset::rational_be(self.get(p)?)
    }
    fn uid(&self, p: PropertyId) -> Option<InstanceUid> {
        localset::uid16(self.get(p)?)
    }
    /// A 32-byte UMID, stored across two consecutive property slots is never
    /// the case in this crate's table (UMIDs are always one 32-byte value);
    /// kept separate from [`MetadataSet::uid`] only for the type difference.
    fn umid(&self, p: PropertyId) -> Option<[u8; 32]> {
        self.get(p)?.get(0..32)?.try_into().ok()
    }
    fn instance_array(&self, p: PropertyId, budget: &Budget) -> Vec<InstanceUid> {
        let Some(v) = self.get(p) else {
            return Vec::new();
        };
        let Ok(b) = localset::batch(v, budget) else {
            return Vec::new();
        };
        b.iter().filter_map(localset::uid16).collect()
    }
}

/// The instance-UID-keyed arena: every structural-metadata set in the
/// header, indexed for lookup. Building it is a single forward pass over
/// the header metadata KLVs; nothing here recurses.
#[derive(Debug, Clone, Default)]
pub struct MetadataGraph {
    by_instance: HashMap<InstanceUid, MetadataSet>,
    /// The first `Preface` set seen — there is exactly one per spec, but a
    /// hostile file could include more, in which case the first wins and the
    /// rest are ignored rather than causing an error (D6: a demuxer is
    /// forgiving; a second Preface does not stop the file from being read).
    preface: Option<InstanceUid>,
}

impl MetadataGraph {
    #[must_use]
    pub fn get(&self, id: InstanceUid) -> Option<&MetadataSet> {
        self.by_instance.get(&id)
    }

    #[must_use]
    pub fn preface(&self) -> Option<&MetadataSet> {
        self.preface.and_then(|id| self.get(id))
    }

    /// Every set of a given class, in the order they were parsed.
    pub fn of_class(&self, class: StructuralClass) -> impl Iterator<Item = &MetadataSet> {
        self.by_instance.values().filter(move |s| s.class == class)
    }
}

/// Widest plausible single structural-metadata set. The corpus this crate
/// measured against has none over 500 bytes; four orders of magnitude of
/// headroom over that still refuses a hostile length before allocating.
const MAX_SET_BYTES: u64 = 1024 * 1024;

/// KLVs read in one region-scan before giving up on ever finding a
/// partition boundary — fuel for the *loop*, independent of any one KLV's
/// declared length. A real header metadata block has a few dozen sets.
const MAX_REGION_KLVS: u64 = 1 << 20;

/// Read header-metadata and Index Table Segment KLVs starting at the
/// current I/O position, until a partition-pack-family key (a new
/// partition, or the Random Index Pack) is found.
///
/// This is the region-scanner every partition's post-primer content goes
/// through: rather than trusting the arithmetic of `HeaderByteCount` +
/// `IndexByteCount` against the partition's stated offsets — which this
/// crate could not fully reconcile against a real footer partition's byte
/// layout during development, see `docs/format/vaco-demux-mxf.md` — it reads
/// forward classifying each KLV by its own key and stops at the first key
/// that is unambiguously the start of the *next* thing (a partition pack or
/// the RIP). On return, `io`'s position is exactly that next key's offset,
/// so the caller can carry straight on.
///
/// # Errors
/// As [`klv::read_header`], [`klv::read_value`] and [`crate::index::parse`].
#[allow(
    clippy::implicit_hasher,
    reason = "internal API; every caller in this crate uses the standard HashMap"
)]
pub fn scan_region(
    io: &mut IoContext,
    budget: &mut Budget,
    primer: &HashMap<u16, Ul>,
    resolver: &Resolver,
    graph: &mut MetadataGraph,
    index_segments: &mut Vec<crate::index::IndexTableSegment>,
) -> Result<()> {
    for _ in 0..MAX_REGION_KLVS {
        budget.consume_fuel(1)?;
        let start = io.pos();
        let header = match klv::read_header(io) {
            Ok(h) => h,
            // Ran off the end of the file (e.g. a header partition with no
            // footer yet, still being written): that is the natural end of
            // this region, not a failure.
            Err(Error::UnexpectedEof) => return Ok(()),
            Err(e) => return Err(e),
        };
        if header.key.is_any_partition_pack()
            || header.key.partition_family_kind() == Some(PartitionFamilyKind::RandomIndexPack)
        {
            io.seek(start)?;
            return Ok(());
        }
        if header.key.is_essence_element() || header.key.is_generic_container_system_item() {
            // Measured against a single-partition D-10 file (`ffmpeg -f
            // mxf_d10`): a header partition can carry its own Index Table
            // Segment and then run straight into the essence body with no
            // intervening body partition pack at all. Before this check,
            // nothing here stopped the scan at the essence boundary, so it
            // walked forward treating every System Item and essence element
            // as an unrecognised, skippable KLV all the way to the footer
            // partition near EOF — the demuxer then started reading packets
            // from a position near the end of the file and found none. See
            // `planning/TECH-DEBT.md`/this crate's closing report for the
            // measurement.
            io.seek(start)?;
            return Ok(());
        }
        if header.key == crate::ul::KLV_FILL_ITEM {
            klv::skip_value(io, &header)?;
            continue;
        }
        if header.key.is_index_table_segment() {
            let value = klv::read_value(io, budget, &header, MAX_SET_BYTES)?;
            index_segments.push(crate::index::parse(&value, primer, resolver, budget)?);
            continue;
        }
        if let Some(class) = header.key.structural_class() {
            let value = klv::read_value(io, budget, &header, MAX_SET_BYTES)?;
            let set = parse_set(class, &value, primer, resolver, budget)?;
            if class == StructuralClass::Preface
                && let Some(id) = set.instance_uid
            {
                graph.preface.get_or_insert(id);
            }
            if let Some(id) = set.instance_uid {
                graph.by_instance.insert(id, set);
            }
            continue;
        }
        // An unrecognised top-level KLV (a vendor extension, or a structure
        // this crate has not identified — see the module docs for one such
        // key this crate's corpus contains and does not interpret). Skipped,
        // not fatal: demuxing is lenient (see the project-wide
        // detection/demuxing split), and the file may still be fully usable.
        klv::skip_value(io, &header)?;
    }
    Err(Error::InvalidData(
        "mxf: header metadata region did not terminate within the KLV budget",
    ))
}

fn parse_set(
    class: StructuralClass,
    value: &[u8],
    primer: &HashMap<u16, Ul>,
    resolver: &Resolver,
    budget: &mut Budget,
) -> Result<MetadataSet> {
    let mut props = HashMap::new();
    let mut instance_uid = None;
    localset::for_each_item(value, budget, |item| {
        if let Some(p) = resolver.resolve(primer, item.tag) {
            if p == PropertyId::InstanceUid {
                instance_uid = localset::uid16(item.value);
            }
            props.insert(p, item.value.to_vec());
        }
        Ok(())
    })?;
    Ok(MetadataSet {
        class,
        instance_uid,
        props,
    })
}

// ------------------------------------------------------------- typed access

/// One package's essence description, resolved from the graph: which track
/// carries it, its edit rate, and its descriptor.
#[derive(Debug, Clone)]
pub struct ResolvedTrack {
    pub track_id: Option<u32>,
    /// The raw bytes of the Generic Container track number (matched against
    /// an essence element key's last 4 bytes — see [`crate::essence`]).
    pub track_number: Option<u32>,
    pub edit_rate: Option<vaco_core::Rational>,
    pub descriptor: Option<InstanceUid>,
    pub is_timecode: bool,
    pub timecode: Option<Timecode>,
}

#[derive(Debug, Clone, Copy)]
pub struct Timecode {
    pub start: i64,
    pub base: u16,
    pub drop_frame: bool,
}

/// How many source-package generations [`resolve_essence`] will chase
/// before refusing to go further, independent of the visited-set cycle
/// check (see the module docs).
const MAX_CHAIN_DEPTH: usize = 64;

/// Walk from a Material Package's tracks down to the Source Package that
/// actually carries essence, following `SourceClip.SourcePackageID` through
/// as many generations as the file has — bounded per the module docs.
///
/// Returns every track on the *terminal* source package (the one with a
/// `Descriptor`), plus every Timecode track met on a Material Package along
/// the way (for `start_timecode`).
///
/// # Errors
/// [`Error::InvalidData`] if the chain cycles (a package's UMID is visited
/// twice) or exceeds [`MAX_CHAIN_DEPTH`].
pub fn resolve_essence(
    graph: &MetadataGraph,
    material_package: InstanceUid,
    budget: &mut Budget,
) -> Result<(Vec<ResolvedTrack>, Vec<Timecode>)> {
    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut timecodes = Vec::new();
    let mut current = material_package;
    for depth in 0..MAX_CHAIN_DEPTH {
        budget.consume_fuel(4)?;
        let package = graph.get(current).ok_or(Error::InvalidData(
            "mxf: package reference does not resolve",
        ))?;
        let umid = package.umid(PropertyId::PackageUmid);
        if let Some(umid) = umid
            && !visited.insert(umid)
        {
            return Err(Error::InvalidData(
                "mxf: source-package reference cycle detected",
            ));
        }
        let track_ids = package.instance_array(PropertyId::PackageTracks, budget);
        budget.check_count("mxf_tracks_per_package", track_ids.len() as u64, 4096)?;

        let mut next_source_umid = None;
        let mut resolved = Vec::new();
        for tid in &track_ids {
            let Some(track) = graph.get(*tid) else {
                continue;
            };
            if track.class != StructuralClass::Track {
                continue;
            }
            let sequence_id = track.uid(PropertyId::TrackSequence);
            let sequence = sequence_id.and_then(|id| graph.get(id));
            let mut this_timecode = None;
            let mut source_package_ref = None;
            if let Some(seq) = sequence {
                let component_ids = seq.instance_array(PropertyId::SequenceComponents, budget);
                budget.check_count(
                    "mxf_components_per_sequence",
                    component_ids.len() as u64,
                    4096,
                )?;
                for cid in component_ids.iter().chain(sequence_id.iter()) {
                    let Some(comp) = graph.get(*cid) else {
                        continue;
                    };
                    match comp.class {
                        StructuralClass::TimecodeComponent => {
                            this_timecode = Some(Timecode {
                                start: comp.i64(PropertyId::TimecodeStart).unwrap_or(0),
                                base: comp.u16(PropertyId::TimecodeRoundedBase).unwrap_or(25),
                                drop_frame: comp.u8(PropertyId::TimecodeDropFrame).unwrap_or(0)
                                    != 0,
                            });
                        }
                        StructuralClass::SourceClip => {
                            source_package_ref = comp.umid(PropertyId::SourceClipSourcePackageId);
                        }
                        _ => {}
                    }
                }
                // A `Sequence` whose own class matched `SourceClip` directly
                // (some writers put a lone StructuralComponent straight on
                // the Track without an intervening Sequence array entry) is
                // covered by the `.chain(sequence_id.iter())` above.
            }
            if let Some(tc) = this_timecode {
                timecodes.push(tc);
            }
            if let Some(src) = source_package_ref {
                next_source_umid = Some(src);
            }
            resolved.push(ResolvedTrack {
                track_id: track.u32(PropertyId::TrackId),
                track_number: track.u32(PropertyId::TrackNumber),
                edit_rate: track.rational(PropertyId::TrackEditRate),
                descriptor: package.uid(PropertyId::PackageDescriptor),
                is_timecode: this_timecode.is_some(),
                timecode: this_timecode,
            });
        }

        // This package already carries a Descriptor: it is a Source
        // Package with essence in *this* file. Stop here.
        if package.uid(PropertyId::PackageDescriptor).is_some() {
            return Ok((resolved, timecodes));
        }
        let Some(next_umid) = next_source_umid else {
            // No descriptor and nothing to chase further: nothing to
            // resolve for this package (e.g. a Material Package with only
            // a timecode track, or a dangling reference).
            return Ok((Vec::new(), timecodes));
        };
        let Some(next_id) = find_package_by_umid(graph, next_umid) else {
            return Ok((Vec::new(), timecodes));
        };
        current = next_id;
        let _ = depth;
    }
    Err(Error::InvalidData(
        "mxf: source-package chain exceeded the maximum depth",
    ))
}

fn find_package_by_umid(graph: &MetadataGraph, umid: [u8; 32]) -> Option<InstanceUid> {
    graph
        .by_instance
        .values()
        .find(|s| {
            matches!(
                s.class,
                StructuralClass::MaterialPackage | StructuralClass::SourcePackage
            ) && s.umid(PropertyId::PackageUmid) == Some(umid)
        })
        .and_then(|s| s.instance_uid)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn set_bytes(class_byte: u8, items: &[(u16, Vec<u8>)]) -> (Ul, Vec<u8>) {
        let ul = Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01, 0x01,
            class_byte, 0x00,
        ]);
        let mut value = Vec::new();
        for (tag, v) in items {
            value.extend_from_slice(&tag.to_be_bytes());
            value.extend_from_slice(&(v.len() as u16).to_be_bytes());
            value.extend_from_slice(v);
        }
        (ul, value)
    }

    fn instance_uid_item(id: InstanceUid) -> (u16, Vec<u8>) {
        (0x3c0a, id.to_vec())
    }

    fn primer_and_resolver() -> (HashMap<u16, Ul>, Resolver) {
        let resolver = Resolver::new();
        // Build a primer mapping every tag this test uses to its real UL by
        // asking the resolver's own table — a shortcut that is exactly
        // equivalent to a well-formed file's primer pack, since it uses the
        // same measured ULs.
        let mut primer = HashMap::new();
        for (tag, prop) in [
            (0x3c0a, PropertyId::InstanceUid),
            (0x4401, PropertyId::PackageUmid),
            (0x4403, PropertyId::PackageTracks),
            (0x4701, PropertyId::PackageDescriptor),
            (0x4801, PropertyId::TrackId),
            (0x4803, PropertyId::TrackSequence),
            (0x1001, PropertyId::SequenceComponents),
            (0x1101, PropertyId::SourceClipSourcePackageId),
        ] {
            let ul = crate::properties::TABLE
                .iter()
                .find(|&&(p, _)| p == prop)
                .map(|&(_, ul)| ul)
                .unwrap();
            primer.insert(tag, ul);
        }
        (primer, resolver)
    }

    #[test]
    fn a_two_package_chain_resolves_without_cycling() {
        let (primer, resolver) = primer_and_resolver();
        let mut budget = Budget::new(Limits::permissive());
        let mut graph = MetadataGraph::default();

        let material_id = [1u8; 16];
        let source_id = [2u8; 16];
        let track_id = [3u8; 16];
        let sequence_id = [4u8; 16];
        let clip_id = [5u8; 16];
        let material_umid = [0xAA; 32];
        let source_umid = [0xBB; 32];

        // Material Package: one track, pointing at `sequence_id`.
        let mut tracks_batch = 1u32.to_be_bytes().to_vec();
        tracks_batch.extend_from_slice(&16u32.to_be_bytes());
        tracks_batch.extend_from_slice(&track_id);
        let (_, mp_bytes) = set_bytes(
            0x36,
            &[
                instance_uid_item(material_id),
                (0x4401, material_umid.to_vec()),
                (0x4403, tracks_batch.clone()),
            ],
        );
        graph.by_instance.insert(
            material_id,
            parse_set(
                StructuralClass::MaterialPackage,
                &mp_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        // Track -> Sequence.
        let (_, track_bytes) = set_bytes(
            0x3b,
            &[instance_uid_item(track_id), (0x4803, sequence_id.to_vec())],
        );
        graph.by_instance.insert(
            track_id,
            parse_set(
                StructuralClass::Track,
                &track_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        // Sequence -> [SourceClip].
        let mut comps_batch = 1u32.to_be_bytes().to_vec();
        comps_batch.extend_from_slice(&16u32.to_be_bytes());
        comps_batch.extend_from_slice(&clip_id);
        let (_, seq_bytes) = set_bytes(
            0x0f,
            &[instance_uid_item(sequence_id), (0x1001, comps_batch)],
        );
        graph.by_instance.insert(
            sequence_id,
            parse_set(
                StructuralClass::Sequence,
                &seq_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        // SourceClip -> Source Package (by UMID).
        let (_, clip_bytes) = set_bytes(
            0x11,
            &[instance_uid_item(clip_id), (0x1101, source_umid.to_vec())],
        );
        graph.by_instance.insert(
            clip_id,
            parse_set(
                StructuralClass::SourceClip,
                &clip_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        // Source Package: has a Descriptor, so resolution stops here.
        let descriptor_id = [6u8; 16];
        let (_, sp_bytes) = set_bytes(
            0x37,
            &[
                instance_uid_item(source_id),
                (0x4401, source_umid.to_vec()),
                (0x4701, descriptor_id.to_vec()),
                (0x4403, tracks_batch),
            ],
        );
        graph.by_instance.insert(
            source_id,
            parse_set(
                StructuralClass::SourcePackage,
                &sp_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        let (tracks, _tc) = resolve_essence(&graph, material_id, &mut budget).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].descriptor, Some(descriptor_id));
    }

    #[test]
    fn a_source_clip_naming_its_own_package_terminates_instead_of_looping() {
        let (primer, resolver) = primer_and_resolver();
        let mut budget = Budget::new(Limits::permissive());
        let mut graph = MetadataGraph::default();

        let package_id = [9u8; 16];
        let track_id = [10u8; 16];
        let sequence_id = [11u8; 16];
        let clip_id = [12u8; 16];
        let self_umid = [0xCCu8; 32];

        let mut tracks_batch = 1u32.to_be_bytes().to_vec();
        tracks_batch.extend_from_slice(&16u32.to_be_bytes());
        tracks_batch.extend_from_slice(&track_id);
        let (_, pkg_bytes) = set_bytes(
            0x37,
            &[
                instance_uid_item(package_id),
                (0x4401, self_umid.to_vec()),
                (0x4403, tracks_batch),
                // Deliberately no Descriptor: this package only has a
                // SourceClip pointing at *itself*, which is the cycle.
            ],
        );
        graph.by_instance.insert(
            package_id,
            parse_set(
                StructuralClass::SourcePackage,
                &pkg_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );
        let (_, track_bytes) = set_bytes(
            0x3b,
            &[instance_uid_item(track_id), (0x4803, sequence_id.to_vec())],
        );
        graph.by_instance.insert(
            track_id,
            parse_set(
                StructuralClass::Track,
                &track_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );
        let mut comps_batch = 1u32.to_be_bytes().to_vec();
        comps_batch.extend_from_slice(&16u32.to_be_bytes());
        comps_batch.extend_from_slice(&clip_id);
        let (_, seq_bytes) = set_bytes(
            0x0f,
            &[instance_uid_item(sequence_id), (0x1001, comps_batch)],
        );
        graph.by_instance.insert(
            sequence_id,
            parse_set(
                StructuralClass::Sequence,
                &seq_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );
        let (_, clip_bytes) = set_bytes(
            0x11,
            &[instance_uid_item(clip_id), (0x1101, self_umid.to_vec())],
        );
        graph.by_instance.insert(
            clip_id,
            parse_set(
                StructuralClass::SourceClip,
                &clip_bytes,
                &primer,
                &resolver,
                &mut budget,
            )
            .unwrap(),
        );

        let err = resolve_essence(&graph, package_id, &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }
}
