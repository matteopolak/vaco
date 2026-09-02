//! The 16-byte SMPTE Universal Label, and the well-known keys this crate
//! writes.
//!
//! # Source
//!
//! Every constant below reuses this workspace's own already-published,
//! clean-room measurement of the same bytes: `vaco-demux-mxf`'s `ul.rs`/
//! `properties.rs`/`essence.rs` (D7/D15 — that crate read no `ffmpeg`
//! source either, only its shipped output, per `provenance/sources.toml`'s
//! `ffmpeg-mxf-probe` family and this crate's own
//! `ffmpeg-mxf-mux-header-probe` entry, which re-measured tag *numbers*
//! directly against a real header this session, since the demux crate only
//! needed the resolved ULs, not which local tag conventionally carries
//! each one). Reusing a sibling crate's own prior measurement is not
//! reading the reference's source; it is this project's own record.

/// Which of the three real `ffmpeg` MXF muxers this crate is imitating —
/// `-f mxf` (`OP1a`), `-f mxf_d10` (D-10 / SMPTE 386M), `-f mxf_opatom`
/// (OP-Atom / SMPTE 390) — each a distinct registered muxer name in the
/// reference (`ffmpeg -muxers | grep mxf`), not an option of one muxer.
/// `mux.rs` threads this through `write_header`/`write_packet`/
/// `write_trailer`; `metadata.rs` threads it through descriptor/essence-
/// container/operational-pattern selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MxfVariant {
    Op1a,
    D10,
    OpAtom,
}

/// A 16-byte SMPTE Universal Label, stored exactly as it will appear on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Ul(pub [u8; 16]);

impl Ul {
    #[must_use]
    pub(crate) const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub(crate) const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// The 14-byte prefix shared by every structural-metadata set this crate
/// writes; byte 14 (the 15th byte) is the class discriminator, byte 15 is
/// always `0x00` in every real file this workspace has measured.
const STRUCTURAL_SET_PREFIX: [u8; 14] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01, 0x01,
];

/// Build a structural-metadata set's own 16-byte key from its class byte.
#[must_use]
pub(crate) const fn structural_set_key(class: u8) -> [u8; 16] {
    let p = STRUCTURAL_SET_PREFIX;
    [
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], p[13],
        class, 0x00,
    ]
}

pub(crate) mod class {
    pub(crate) const PREFACE: u8 = 0x2f;
    pub(crate) const IDENTIFICATION: u8 = 0x30;
    pub(crate) const CONTENT_STORAGE: u8 = 0x18;
    pub(crate) const MATERIAL_PACKAGE: u8 = 0x36;
    pub(crate) const SOURCE_PACKAGE: u8 = 0x37;
    pub(crate) const TRACK: u8 = 0x3b;
    pub(crate) const SEQUENCE: u8 = 0x0f;
    pub(crate) const SOURCE_CLIP: u8 = 0x11;
    pub(crate) const TIMECODE_COMPONENT: u8 = 0x14;
    /// `MPEGVideoDescriptor` — measured (see `vaco-demux-mxf::ul`) as the
    /// real descriptor class an `ffmpeg -f mxf` MPEG-2 picture track uses.
    pub(crate) const MPEG_VIDEO_DESCRIPTOR: u8 = 0x51;
    /// `AES3PCMDescriptor` — measured as the real class an `ffmpeg -f mxf`
    /// `pcm_s16le` audio track uses (not `WaveAudioDescriptor`).
    pub(crate) const AES3_PCM_DESCRIPTOR: u8 = 0x47;
    pub(crate) const MULTIPLE_DESCRIPTOR: u8 = 0x44;
    /// `CDCIEssenceDescriptor` — measured this session against a real
    /// `ffmpeg -f mxf_d10` file's video descriptor (a real single-track D-10
    /// file's structural-set class bytes, decoded in sequence, land on
    /// `0x28` where an `OP1a` MPEG-2 track lands on `0x51` instead). D-10's
    /// constrained 4:2:2 profile evidently gets the more general CDCI
    /// descriptor, not `MPEGVideoDescriptor`.
    pub(crate) const CDCI_ESSENCE_DESCRIPTOR: u8 = 0x28;
    /// `EssenceContainerData` — identified this session, not merely
    /// inferred: decoded a real file's own class-`0x23` set directly (a
    /// real `ffmpeg -f mxf`/`-f mxf_d10`/`-f mxf_opatom` file writes one
    /// in every case checked) and cross-validated two ways rather than
    /// pattern-matching a spec table. Its `InstanceUID` is exactly the one
    /// `ContentStorage`'s own second batch property (tag `0x1902`,
    /// unnamed before this session) references — the same
    /// strong-reference shape `ContentStorage.Packages` (`0x1901`) already
    /// uses for the two `Package`s. Its `LinkedPackageUID` property (tag
    /// `0x2701`) is a 32-byte value starting `06 0a 2b 34...` (the SMPTE
    /// UMID designator root, distinct from the `06 0e 2b 34` Universal
    /// Label root every other property here uses) and is byte-for-byte
    /// identical to the `SourcePackage`'s own UMID used elsewhere in the
    /// same file. Alongside `BodySID`/`IndexSID` (tags `0x3f07`/`0x3f06`,
    /// both already known from the Index Table Segment), this is exactly
    /// ST 377-1's `EssenceContainerData` class: one per essence-carrying
    /// `BodySID`, linking it back to the package and its index.
    pub(crate) const ESSENCE_CONTAINER_DATA: u8 = 0x23;
}

/// Partition Pack family: `06.0e.2b.34.02.05.01.01.0d.01.02.01.01` plus a
/// kind byte (`0x02` Header, `0x04` Footer, `0x05` Primer Pack, `0x11`
/// Random Index Pack) plus `0x04.0x00` — the exact suffix a real "closed,
/// complete" `ffmpeg` partition/primer pack carries, measured in
/// `vaco-demux-mxf::ul`.
const PARTITION_FAMILY_PREFIX: [u8; 13] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01,
];

/// The KLV Fill Item key, used to pad structures out to a KAG (KLV
/// Alignment Grid) boundary — measured this session: a real `ffmpeg -f mxf`
/// file uses `KAGSize = 512` and pads the header region (partition packs,
/// primer, structural metadata, the first System Item) out to 512-byte
/// boundaries with Fill Items, but not between subsequent essence elements
/// (measured directly: one frame's KLV ends and the next item begins with
/// no gap at all).
pub(crate) const FILL_ITEM: [u8; 16] = [
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x03, 0x01, 0x02, 0x10, 0x01, 0x00, 0x00, 0x00,
];

#[must_use]
pub(crate) const fn header_partition_key() -> [u8; 16] {
    partition_family_key(0x02)
}

#[must_use]
pub(crate) const fn footer_partition_key() -> [u8; 16] {
    partition_family_key(0x04)
}

/// Measured to matter for a real `ffmpeg -i` cross-check (not this crate's
/// own reader, which does not care): a real two-essence-track `ffmpeg -f
/// mxf` file inserts a genuine Body Partition Pack (`kind = 0x03`) right
/// before its essence begins, distinct from the single-essence-track shape
/// (`vaco-demux-mxf`'s own D-10 corpus) where the header partition carries
/// essence directly with no separate body pack at all.
#[must_use]
pub(crate) const fn body_partition_key() -> [u8; 16] {
    partition_family_key(0x03)
}

#[must_use]
pub(crate) const fn primer_pack_key() -> [u8; 16] {
    let p = PARTITION_FAMILY_PREFIX;
    [
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], 0x05,
        0x01, 0x00,
    ]
}

#[must_use]
pub(crate) const fn random_index_pack_key() -> [u8; 16] {
    let p = PARTITION_FAMILY_PREFIX;
    [
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], 0x11,
        0x01, 0x00,
    ]
}

const fn partition_family_key(kind: u8) -> [u8; 16] {
    let p = PARTITION_FAMILY_PREFIX;
    [
        p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12], kind,
        0x04, 0x00,
    ]
}

/// The Index Table Segment's own key, measured in
/// `vaco-demux-mxf::ul::INDEX_TABLE_SEGMENT_PREFIX` plus its `0x01`
/// discriminator.
pub(crate) const INDEX_TABLE_SEGMENT: [u8; 16] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x10, 0x01, 0x00,
];

/// The Generic Container System Item, measured in
/// `vaco-demux-mxf::essence::GC_SYSTEM_ITEM_PREFIX` plus the trailing
/// `04.01.01.00` a real single-partition `OP1a`/D-10 file carries.
pub(crate) const GC_SYSTEM_ITEM: [u8; 16] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x04, 0x01, 0x01, 0x00,
];

/// The 12-byte prefix every Generic Container essence element key shares
/// (`vaco-demux-mxf::essence::GC_ESSENCE_PREFIX`). The last 4 bytes are the
/// per-track "track number", written to equal `Track.EssenceTrackNumber`
/// exactly (`essence::track_number_for`).
pub(crate) const GC_ESSENCE_PREFIX: [u8; 12] = [
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01,
];

/// `OP1a`: one material package, one file. Measured in
/// `vaco-demux-mxf::ul::op::OP1A`.
pub(crate) const OPERATIONAL_PATTERN_OP1A: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x01, 0x09, 0x00,
]);

/// "MXF-GC Generic MPEG-2 frame-wrapped picture", measured in
/// `vaco-demux-mxf::ul::essence_container::MPEG_GENERIC_FRAME_WRAPPED`.
pub(crate) const ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x02, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x04, 0x60, 0x01,
]);

/// "MXF-GC Generic Sound essence" (AES3, frame-wrapped) — measured this
/// session off a real `ffmpeg -f mxf` file's `AES3PCMDescriptor`. Distinct
/// from the picture label above: getting this wrong (an earlier version of
/// this crate reused the picture label for the audio track's own
/// `EssenceContainer` property) did not stop `vaco-demux-mxf` from reading
/// the file — that crate does not interpret this property's value at all —
/// but caused a real `ffmpeg -i` to guess `mp2` instead of `pcm_s16le` for
/// the audio stream, since `ffmpeg`'s own codec-from-container-label table
/// evidently does key off it.
pub(crate) const ESSENCE_CONTAINER_SOUND_FRAME_WRAPPED: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x06, 0x03, 0x00,
]);

/// "Multiple wrappings" — the generic container label a `MultipleDescriptor`
/// and the Preface/Partition Pack's own `EssenceContainer`(s) carry when a
/// package has more than one essence kind, measured the same way. Each
/// individual essence descriptor still states its own specific container
/// (the two constants above); this one is the package-level placeholder for
/// "more than one, see the sub-descriptors."
pub(crate) const ESSENCE_CONTAINER_MULTIPLE_WRAPPINGS: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x7f, 0x01, 0x00,
]);

/// MPEG-2 4:2:2 Long GOP `PictureEssenceCoding`, measured in
/// `vaco-demux-mxf::descriptor::PICTURE_ESSENCE_CODING`'s first (and only
/// non-D-10) row.
pub(crate) const PICTURE_ESSENCE_CODING_MPEG2_LONG_GOP: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x04, 0x01, 0x02, 0x02, 0x01, 0x01, 0x11, 0x00,
]);

/// D-10 (SMPTE 386M)'s own video `EssenceContainer` label — distinct from
/// `OP1a`'s (differs starting at byte 7: `01` vs `04`, and again from byte 12
/// on), measured this session directly off a real `ffmpeg -f mxf_d10`
/// file's header partition, `Preface` and `CDCIEssenceDescriptor` alike (all
/// three states carry the identical bytes).
pub(crate) const ESSENCE_CONTAINER_D10_VIDEO: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x01, 0x05, 0x01,
]);

/// D-10 (SMPTE 386M)'s three fixed-bitrate `PictureEssenceCoding` labels,
/// reused from `vaco-demux-mxf::descriptor::PICTURE_ESSENCE_CODING`'s
/// already-measured D-10 rows (that crate measured all three against real
/// `ffmpeg -f mxf_d10 -b:v <rate>` files at 50/40/30 Mbit/s; this crate adds
/// no new measurement here, only reuses it for the write side).
pub(crate) const PICTURE_ESSENCE_CODING_D10_50MBIT: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02, 0x01, 0x01,
]);
pub(crate) const PICTURE_ESSENCE_CODING_D10_40MBIT: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02, 0x01, 0x03,
]);
pub(crate) const PICTURE_ESSENCE_CODING_D10_30MBIT: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x04, 0x01, 0x02, 0x02, 0x01, 0x02, 0x01, 0x05,
]);

/// OP-Atom: one essence track per file. Reused from
/// `vaco-demux-mxf::ul::op::OP_ATOM`, which measured it against that
/// crate's own `opatom.mxf` corpus file; re-confirmed this session against
/// a freshly generated `ffmpeg -f mxf_opatom` file (`opatom_test.mxf`),
/// byte for byte.
pub(crate) const OPERATIONAL_PATTERN_OP_ATOM: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x02, 0x0d, 0x01, 0x02, 0x01, 0x10, 0x03, 0x00, 0x00,
]);

/// The Operational Pattern label every partition pack and the `Preface`
/// itself state, by variant. D-10 uses the same `OP1a` label as the `OP1a`
/// muxer (measured this session against a real `ffmpeg -f mxf_d10` file's
/// own header partition — byte for byte identical to
/// [`OPERATIONAL_PATTERN_OP1A`]); only OP-Atom's differs.
#[must_use]
pub(crate) const fn operational_pattern_for(variant: MxfVariant) -> Ul {
    match variant {
        MxfVariant::OpAtom => OPERATIONAL_PATTERN_OP_ATOM,
        MxfVariant::Op1a | MxfVariant::D10 => OPERATIONAL_PATTERN_OP1A,
    }
}

/// `AspectRatio` (the display aspect ratio, e.g. `5/4`) — a property
/// `vaco-demux-mxf::properties::PropertyId::AspectRatio` already reads (via
/// `descriptor::picture_parameters`, into `sample_aspect_ratio`) but this
/// crate never wrote: a real functional gap, not a byte-identity nicety —
/// confirmed against three real fixtures this session (two different
/// resolutions of `OP1a`/`-f mxf` plus one D-10/`-f mxf_d10` file, all
/// carrying this exact UL at local tag `0x320e`).
pub(crate) const ASPECT_RATIO: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x04, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00,
]);

// OP-Atom's own video `EssenceContainer` label, measured this session
// against `opatom_test.mxf`: byte-for-byte identical to
// [`ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED`] above (both are "MXF-GC Generic
// MPEG-2 frame-wrapped picture" — the label's own name does not change for
// OP-Atom even though OP-Atom's essence is actually clip-wrapped: one
// Generic Container element for the whole file, not one per frame; see
// `mux.rs`'s `MxfVariant::OpAtom` docs). Reusing the `OP1a` constant
// directly rather than duplicating it under a second name.

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn structural_set_key_matches_a_real_preface_key() {
        assert_eq!(
            structural_set_key(class::PREFACE),
            [
                0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01, 0x01,
                0x2f, 0x00,
            ]
        );
    }

    #[test]
    fn header_partition_key_matches_a_real_header() {
        assert_eq!(
            header_partition_key(),
            [
                0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x02,
                0x04, 0x00,
            ]
        );
    }
}
