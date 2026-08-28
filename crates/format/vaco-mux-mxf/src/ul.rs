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
}

/// Partition Pack family: `06.0e.2b.34.02.05.01.01.0d.01.02.01.01` plus a
/// kind byte (`0x02` Header, `0x04` Footer, `0x05` Primer Pack, `0x11`
/// Random Index Pack) plus `0x04.0x00` — the exact suffix a real "closed,
/// complete" `ffmpeg` partition/primer pack carries, measured in
/// `vaco-demux-mxf::ul`.
const PARTITION_FAMILY_PREFIX: [u8; 13] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01,
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
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x10, 0x01,
    0x00,
];

/// The Generic Container System Item, measured in
/// `vaco-demux-mxf::essence::GC_SYSTEM_ITEM_PREFIX` plus the trailing
/// `04.01.01.00` a real single-partition OP1a/D-10 file carries.
pub(crate) const GC_SYSTEM_ITEM: [u8; 16] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x04, 0x01, 0x01,
    0x00,
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
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x01, 0x09,
    0x00,
]);

/// "MXF-GC Generic MPEG-2 frame-wrapped picture", measured in
/// `vaco-demux-mxf::ul::essence_container::MPEG_GENERIC_FRAME_WRAPPED`.
pub(crate) const ESSENCE_CONTAINER_MPEG_FRAME_WRAPPED: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x02, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x04, 0x60,
    0x01,
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
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x06, 0x03,
    0x00,
]);

/// "Multiple wrappings" — the generic container label a `MultipleDescriptor`
/// and the Preface/Partition Pack's own `EssenceContainer`(s) carry when a
/// package has more than one essence kind, measured the same way. Each
/// individual essence descriptor still states its own specific container
/// (the two constants above); this one is the package-level placeholder for
/// "more than one, see the sub-descriptors."
pub(crate) const ESSENCE_CONTAINER_MULTIPLE_WRAPPINGS: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x7f, 0x01,
    0x00,
]);

/// MPEG-2 4:2:2 Long GOP `PictureEssenceCoding`, measured in
/// `vaco-demux-mxf::descriptor::PICTURE_ESSENCE_CODING`'s first (and only
/// non-D-10) row.
pub(crate) const PICTURE_ESSENCE_CODING_MPEG2_LONG_GOP: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x03, 0x04, 0x01, 0x02, 0x02, 0x01, 0x01, 0x11,
    0x00,
]);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn structural_set_key_matches_a_real_preface_key() {
        assert_eq!(
            structural_set_key(class::PREFACE),
            [
                0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01,
                0x01, 0x2f, 0x00,
            ]
        );
    }

    #[test]
    fn header_partition_key_matches_a_real_header() {
        assert_eq!(
            header_partition_key(),
            [
                0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01,
                0x02, 0x04, 0x00,
            ]
        );
    }
}
