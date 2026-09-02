//! The 16-byte SMPTE Universal Label, and the well-known keys built from it.
//!
//! # Source
//!
//! SMPTE ST 336 (the KLV encoding protocol) defines the label itself; SMPTE
//! ST 377-1 registers the specific keys below (partition packs, the primer
//! pack, the random index pack, and the structural-metadata set family).
//! Clean-room from those documents (D7/D15).
//!
//! Every constant below was cross-checked against real files written by the
//! installed `ffmpeg 8.1` (`ffmpeg -f lavfi -i testsrc=... -f mxf out.mxf`,
//! `-f mxf_opatom`, and, for D-10/SMPTE 386M specifically, `-f mxf_d10`
//! with `-c:v mpeg2video -pix_fmt yuv422p -intra_vlc 1 -qmax 12 -qmin 1
//! -non_linear_quant 1 -flags +ildct -g 1 -bf 0` plus matched
//! `-b:v`/`-minrate`/`-maxrate`/`-bufsize`/`-rc_init_occupancy` at one of
//! the three standard bitrates), byte-for-byte, per D6/D17 — this is
//! recording the observed bytes of a shipped binary's *output*, not
//! reading its source, which is what keeps it clean-room. Where a value
//! could not be confirmed against a real file it says so in its own doc
//! comment.
//!
//! # Layout
//!
//! A UL is 16 bytes, always read and written big-endian/network order (no
//! byte-swapping, unlike a Microsoft GUID). The canonical text form groups
//! them in fours: `060e2b34.02050101.0d010201.01020400`.

use core::fmt;

/// A 16-byte SMPTE Universal Label, stored exactly as it appears on disk.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ul(pub [u8; 16]);

impl Ul {
    pub const LEN: usize = 16;

    /// Build from a byte array.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Read the first 16 bytes of `data`. `None` if `data` is shorter.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        data.first_chunk::<16>().copied().map(Self)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }

    /// The registry designator byte (index 4): what family of label this is
    /// (`0x02` = defined lengths, i.e. everything this crate cares about).
    #[must_use]
    pub const fn designator(self) -> u8 {
        self.0[4]
    }

    /// Whether `self` and `other` agree on every byte the mask marks `true`.
    ///
    /// Used to match a family of keys that differ only in one or two bytes —
    /// the partition-pack keys differ only in their "kind" and "status" bytes,
    /// for instance — without writing out every combination.
    #[must_use]
    pub fn matches_prefix(self, prefix: &[u8]) -> bool {
        self.0.get(..prefix.len()) == Some(prefix)
    }
}

impl fmt::Debug for Ul {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ul({self})")
    }
}

impl fmt::Display for Ul {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}.{:02x}{:02x}{:02x}{:02x}.{:02x}{:02x}{:02x}{:02x}.{:02x}{:02x}{:02x}{:02x}",
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

// ------------------------------------------------------------- well-known keys

/// The 13-byte prefix every SMPTE-labelled KLV key in this file shares:
/// `06.0e.2b.34` (the SMPTE UL registry root) plus the registry-category
/// bytes common to partition packs, the primer pack and the random index
/// pack. Verified against a real header partition pack (see module docs).
const PARTITION_FAMILY_PREFIX: [u8; 13] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01,
];

/// Byte 13 (0-indexed) of a partition-family key: which structure this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionFamilyKind {
    Header,
    Body,
    Footer,
    Primer,
    /// The Random Index Pack, at the tail of the file.
    RandomIndexPack,
    /// Recognised prefix, unrecognised discriminator byte.
    Other(u8),
}

impl Ul {
    /// Classify a key sharing [`PARTITION_FAMILY_PREFIX`], if it does.
    ///
    /// The discriminator lives at byte 13: `0x02` Header, `0x03` Body, `0x04`
    /// Footer, `0x05` Primer Pack, `0x11` Random Index Pack. All five were
    /// read directly off a real file (module docs); nothing here is guessed.
    #[must_use]
    pub fn partition_family_kind(self) -> Option<PartitionFamilyKind> {
        if !self.matches_prefix(&PARTITION_FAMILY_PREFIX) {
            return None;
        }
        Some(match self.0[13] {
            0x02 => PartitionFamilyKind::Header,
            0x03 => PartitionFamilyKind::Body,
            0x04 => PartitionFamilyKind::Footer,
            0x05 => PartitionFamilyKind::Primer,
            0x11 => PartitionFamilyKind::RandomIndexPack,
            other => PartitionFamilyKind::Other(other),
        })
    }

    /// Whether this key is *some* partition pack (header, body or footer) —
    /// the question the top-level scanner needs to decide "stop and read a
    /// partition pack" without caring which kind yet.
    #[must_use]
    pub const fn is_any_partition_pack(self) -> bool {
        // Manual prefix + discriminator check so this can be `const`.
        let b = self.0;
        b[0] == 0x06
            && b[1] == 0x0e
            && b[2] == 0x2b
            && b[3] == 0x34
            && b[4] == 0x02
            && b[5] == 0x05
            && b[6] == 0x01
            && b[7] == 0x01
            && b[8] == 0x0d
            && b[9] == 0x01
            && b[10] == 0x02
            && b[11] == 0x01
            && b[12] == 0x01
            && matches!(b[13], 0x02..=0x04)
    }
}

/// The 14-byte prefix every KLV Fill Item shares in practice; the full key is
/// a constant (below), this is kept only for documentation symmetry.
pub const KLV_FILL_ITEM: Ul = Ul([
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x02, 0x03, 0x01, 0x02, 0x10, 0x01, 0x00, 0x00, 0x00,
]);

/// The 14-byte prefix shared by every header-metadata structural-metadata
/// set this crate interprets (`Preface`, `Identification`, `ContentStorage`,
/// packages, tracks, sequences, structural components, descriptors): the
/// group-2 byte `0x53` marks "local set, 2-byte tag, 2-byte length"; byte 14
/// (the 15th byte) is the class discriminator [`StructuralClass`] reads.
///
/// Verified against a real file (module docs): every set the demuxer walked
/// in `out.mxf`'s header metadata shared exactly these 14 bytes.
pub const STRUCTURAL_SET_PREFIX: [u8; 14] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01, 0x01,
];

/// The Index Table Segment's own key family: same 2-byte-tag/2-byte-length
/// local-set encoding as [`STRUCTURAL_SET_PREFIX`], but registered under the
/// partition-pack branch (`0d.01.02.01`) rather than the structural-metadata
/// branch (`0d.01.01.01`) — measured directly off a footer partition's Index
/// Table Segment, byte for byte.
pub const INDEX_TABLE_SEGMENT_PREFIX: [u8; 14] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x10,
];

/// Class discriminator (byte 14 of a [`STRUCTURAL_SET_PREFIX`] key) for every
/// structural-metadata set this crate understands.
///
/// Every value here was read directly off a real header partition; see the
/// module docs. `#[non_exhaustive]`-style handling lives in the caller: an
/// unrecognised byte does not stop the walk, it just means that set's
/// properties are kept raw and not folded into the typed graph (D6's
/// "detection is strict, demuxing is lenient" applied to metadata, not
/// packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StructuralClass {
    Preface,
    Identification,
    ContentStorage,
    MaterialPackage,
    SourcePackage,
    Track,
    Sequence,
    SourceClip,
    TimecodeComponent,
    /// Any essence descriptor: `FileDescriptor`, `GenericPictureEssenceDescriptor`,
    /// `CDCIEssenceDescriptor`, `MPEGVideoDescriptor`, `GenericSoundEssenceDescriptor`,
    /// `WaveAudioDescriptor`, `MultipleDescriptor`. Distinguished by which
    /// properties are present, not by class byte (RP210 registers a
    /// descriptor subclass per essence family; this crate folds the ones it
    /// has evidence for into one enum arm and reads whichever properties
    /// showed up — see [`crate::descriptor`]).
    Descriptor(u8),
    Unknown(u8),
}

impl Ul {
    /// Classify a structural-metadata set key.
    #[must_use]
    pub fn structural_class(self) -> Option<StructuralClass> {
        if !self.matches_prefix(&STRUCTURAL_SET_PREFIX) {
            return None;
        }
        Some(match self.0[14] {
            0x2f => StructuralClass::Preface,
            0x30 => StructuralClass::Identification,
            0x18 => StructuralClass::ContentStorage,
            0x36 => StructuralClass::MaterialPackage,
            0x37 => StructuralClass::SourcePackage,
            0x3b => StructuralClass::Track,
            0x0f => StructuralClass::Sequence,
            0x11 => StructuralClass::SourceClip,
            0x14 => StructuralClass::TimecodeComponent,
            // Descriptor subclasses measured in the corpus: 0x24 FileDescriptor,
            // 0x27 GenericPictureEssenceDescriptor, 0x28 CDCIEssenceDescriptor,
            // 0x51 MPEGVideoDescriptor, 0x42 GenericSoundEssenceDescriptor,
            // 0x48 WaveAudioDescriptor, 0x44 MultipleDescriptor, 0x47
            // AES3PCMDescriptor. 0x51, 0x44 and 0x47 were actually produced
            // by the reference in this crate's corpus (0x47 is what a real
            // `ffmpeg -f mxf`/`mxf_d10` PCM audio track's descriptor class
            // turned out to be, not 0x48 `WaveAudioDescriptor` — measured,
            // not assumed); 0x42/0x48 remain spec-derived (ST377-1 Annex,
            // RP210) and unexercised — see docs/format/vaco-demux-mxf.md.
            b @ (0x24 | 0x27 | 0x28 | 0x51 | 0x42 | 0x47 | 0x48 | 0x44) => {
                StructuralClass::Descriptor(b)
            }
            other => StructuralClass::Unknown(other),
        })
    }

    /// Whether this is the Index Table Segment key.
    #[must_use]
    pub fn is_index_table_segment(self) -> bool {
        self.matches_prefix(&INDEX_TABLE_SEGMENT_PREFIX) && self.0[14] == 0x01
    }
}

/// Operational pattern labels, measured against real files (module docs).
pub mod op {
    use super::Ul;

    /// `OP1a`: one material package, one file. `out.mxf`'s
    /// `operational_pattern_ul`.
    pub const OP1A: Ul = Ul([
        0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x01, 0x09,
        0x00,
    ]);

    /// OP-Atom: one essence track per file. `opatom.mxf`'s
    /// `operational_pattern_ul`.
    pub const OP_ATOM: Ul = Ul([
        0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x02, 0x0d, 0x01, 0x02, 0x01, 0x10, 0x03, 0x00,
        0x00,
    ]);
}

/// Essence container labels, measured against real files.
pub mod essence_container {
    use super::Ul;

    /// "MXF-GC Generic MPEG-2 frame-wrapped picture", the label `out.mxf`
    /// carries in its partition pack, Preface and File Descriptor alike.
    pub const MPEG_GENERIC_FRAME_WRAPPED: Ul = Ul([
        0x06, 0x0e, 0x2b, 0x34, 0x04, 0x01, 0x01, 0x02, 0x0d, 0x01, 0x03, 0x01, 0x02, 0x04, 0x60,
        0x01,
    ]);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_dotted_hex_form() {
        let ul = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x02,
            0x04, 0x00,
        ]);
        assert_eq!(ul.to_string(), "060e2b34.02050101.0d010201.01020400");
    }

    #[test]
    fn header_partition_key_classifies_as_header() {
        let ul = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x02,
            0x04, 0x00,
        ]);
        assert_eq!(
            ul.partition_family_kind(),
            Some(PartitionFamilyKind::Header)
        );
        assert!(ul.is_any_partition_pack());
    }

    #[test]
    fn primer_and_rip_keys_classify_correctly() {
        let primer = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x05,
            0x01, 0x00,
        ]);
        assert_eq!(
            primer.partition_family_kind(),
            Some(PartitionFamilyKind::Primer)
        );
        assert!(!primer.is_any_partition_pack());

        let rip = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x11,
            0x01, 0x00,
        ]);
        assert_eq!(
            rip.partition_family_kind(),
            Some(PartitionFamilyKind::RandomIndexPack)
        );
    }

    #[test]
    fn preface_key_classifies_and_unrelated_key_does_not() {
        let preface = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x2f, 0x00,
        ]);
        assert_eq!(preface.structural_class(), Some(StructuralClass::Preface));
        assert_eq!(op::OP1A.structural_class(), None);
    }

    #[test]
    fn index_table_segment_key_is_recognised() {
        let its = Ul([
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x10,
            0x01, 0x00,
        ]);
        assert!(its.is_index_table_segment());
        assert!(!op::OP1A.is_index_table_segment());
    }
}
