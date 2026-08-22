//! NAL unit types, ITU-T H.264 Table 7-1.

use vaco_format_nalu::{HeaderKind, NalHeader};

/// `nal_unit_type`, ITU-T H.264 Table 7-1.
///
/// The numeric value is the specification's, so a bitstream value casts
/// directly and back. Reserved and unspecified ranges are kept as their raw
/// number rather than collapsed, because a parser has to make different
/// decisions about them: unspecified types (0, 24-31) may carry anything and
/// must be ignored, while reserved types (17, 18, 22, 23) are future syntax and
/// are also ignored — but a diagnostic that says which is which is worth the
/// two extra variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum NalUnitType {
    /// 0 — unspecified.
    Unspecified,
    /// 1 — coded slice of a non-IDR picture.
    NonIdrSlice,
    /// 2 — coded slice data partition A.
    SlicePartitionA,
    /// 3 — coded slice data partition B.
    SlicePartitionB,
    /// 4 — coded slice data partition C.
    SlicePartitionC,
    /// 5 — coded slice of an IDR picture.
    IdrSlice,
    /// 6 — supplemental enhancement information.
    Sei,
    /// 7 — sequence parameter set.
    Sps,
    /// 8 — picture parameter set.
    Pps,
    /// 9 — access unit delimiter.
    AccessUnitDelimiter,
    /// 10 — end of sequence.
    EndOfSequence,
    /// 11 — end of stream.
    EndOfStream,
    /// 12 — filler data.
    Filler,
    /// 13 — sequence parameter set extension.
    SpsExtension,
    /// 14 — prefix NAL unit (Annex G/H).
    Prefix,
    /// 15 — subset sequence parameter set (Annex G/H).
    SubsetSps,
    /// 16 — depth parameter set (Annex I).
    DepthParameterSet,
    /// 19 — coded slice of an auxiliary coded picture without partitioning.
    AuxiliarySlice,
    /// 20 — coded slice extension (Annex G/H).
    SliceExtension,
    /// 21 — coded slice extension for a depth view or 3D-AVC texture view.
    SliceExtensionDepth,
    /// 17, 18, 22, 23 — reserved by the specification for future use.
    Reserved(u8),
    /// 24-31 — unspecified; outside the specification's scope entirely.
    UnspecifiedHigh(u8),
}

impl NalUnitType {
    /// Classify a raw five-bit `nal_unit_type`.
    ///
    /// Values above 31 cannot occur — the field is five bits — and are mapped
    /// to [`NalUnitType::UnspecifiedHigh`] rather than rejected, so a caller
    /// that masks incorrectly gets a wrong answer instead of a panic.
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Unspecified,
            1 => Self::NonIdrSlice,
            2 => Self::SlicePartitionA,
            3 => Self::SlicePartitionB,
            4 => Self::SlicePartitionC,
            5 => Self::IdrSlice,
            6 => Self::Sei,
            7 => Self::Sps,
            8 => Self::Pps,
            9 => Self::AccessUnitDelimiter,
            10 => Self::EndOfSequence,
            11 => Self::EndOfStream,
            12 => Self::Filler,
            13 => Self::SpsExtension,
            14 => Self::Prefix,
            15 => Self::SubsetSps,
            16 => Self::DepthParameterSet,
            19 => Self::AuxiliarySlice,
            20 => Self::SliceExtension,
            21 => Self::SliceExtensionDepth,
            17 | 18 | 22 | 23 => Self::Reserved(v),
            _ => Self::UnspecifiedHigh(v),
        }
    }

    /// The five-bit value this type is coded as.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Unspecified => 0,
            Self::NonIdrSlice => 1,
            Self::SlicePartitionA => 2,
            Self::SlicePartitionB => 3,
            Self::SlicePartitionC => 4,
            Self::IdrSlice => 5,
            Self::Sei => 6,
            Self::Sps => 7,
            Self::Pps => 8,
            Self::AccessUnitDelimiter => 9,
            Self::EndOfSequence => 10,
            Self::EndOfStream => 11,
            Self::Filler => 12,
            Self::SpsExtension => 13,
            Self::Prefix => 14,
            Self::SubsetSps => 15,
            Self::DepthParameterSet => 16,
            Self::AuxiliarySlice => 19,
            Self::SliceExtension => 20,
            Self::SliceExtensionDepth => 21,
            Self::Reserved(v) | Self::UnspecifiedHigh(v) => v,
        }
    }

    /// Whether this unit carries coded slice data of the *primary* coded
    /// picture — types 1 to 5.
    ///
    /// This is the set §7.4.1.2.4 calls "VCL NAL units" for the purpose of
    /// detecting the first slice of a new access unit, and it deliberately
    /// excludes 19, 20 and 21: an auxiliary picture or a view-extension slice
    /// does not start a new primary picture.
    #[must_use]
    pub const fn is_vcl(self) -> bool {
        matches!(
            self,
            Self::NonIdrSlice
                | Self::SlicePartitionA
                | Self::SlicePartitionB
                | Self::SlicePartitionC
                | Self::IdrSlice
        )
    }

    /// Whether this unit begins with a slice header — types 1, 2, 5, 19, 20 and
    /// 21. Partitions B and C do not; they carry residual data only.
    #[must_use]
    pub const fn has_slice_header(self) -> bool {
        matches!(
            self,
            Self::NonIdrSlice
                | Self::SlicePartitionA
                | Self::IdrSlice
                | Self::AuxiliarySlice
                | Self::SliceExtension
                | Self::SliceExtensionDepth
        )
    }

    /// `IdrPicFlag` — §7.4.1: the unit belongs to an IDR picture.
    #[must_use]
    pub const fn is_idr(self) -> bool {
        matches!(self, Self::IdrSlice)
    }

    /// Whether this unit is a parameter set the parser must retain.
    #[must_use]
    pub const fn is_parameter_set(self) -> bool {
        matches!(self, Self::Sps | Self::Pps | Self::SubsetSps)
    }

    /// The name the specification's Table 7-1 gives this type.
    ///
    /// Used in diagnostics only; nothing in the output contract depends on it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unspecified | Self::UnspecifiedHigh(_) => "unspecified",
            Self::NonIdrSlice => "coded slice of a non-IDR picture",
            Self::SlicePartitionA => "coded slice data partition A",
            Self::SlicePartitionB => "coded slice data partition B",
            Self::SlicePartitionC => "coded slice data partition C",
            Self::IdrSlice => "coded slice of an IDR picture",
            Self::Sei => "supplemental enhancement information",
            Self::Sps => "sequence parameter set",
            Self::Pps => "picture parameter set",
            Self::AccessUnitDelimiter => "access unit delimiter",
            Self::EndOfSequence => "end of sequence",
            Self::EndOfStream => "end of stream",
            Self::Filler => "filler data",
            Self::SpsExtension => "sequence parameter set extension",
            Self::Prefix => "prefix NAL unit",
            Self::SubsetSps => "subset sequence parameter set",
            Self::DepthParameterSet => "depth parameter set",
            Self::AuxiliarySlice => "coded slice of an auxiliary coded picture",
            Self::SliceExtension => "coded slice extension",
            Self::SliceExtensionDepth => "coded slice extension for a depth view",
            Self::Reserved(_) => "reserved",
        }
    }
}

/// An H.264 NAL unit header: §7.3.1's three fields, decoded.
///
/// A thin projection of [`vaco_format_nalu::NalHeader`] onto H.264's own
/// vocabulary, so H.264 code says `header.nal_unit_type == NalUnitType::Sps`
/// rather than comparing a `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264NalHeader {
    /// Must be 0. A 1 means these bytes are not a conforming NAL unit.
    pub forbidden_zero_bit: bool,
    /// 0 means the unit is not used for reference by any other picture.
    pub nal_ref_idc: u8,
    /// The unit's type.
    pub nal_unit_type: NalUnitType,
}

impl H264NalHeader {
    /// Decode the first byte of a NAL unit.
    #[must_use]
    pub fn parse(nal: &[u8]) -> Option<Self> {
        let h = NalHeader::parse(HeaderKind::H264, nal)?;
        Some(Self {
            forbidden_zero_bit: h.forbidden_zero_bit,
            nal_ref_idc: h.nal_ref_idc,
            nal_unit_type: NalUnitType::from_u8(h.nal_unit_type),
        })
    }

    /// `IdrPicFlag`.
    #[must_use]
    pub const fn is_idr(self) -> bool {
        self.nal_unit_type.is_idr()
    }

    /// Whether the unit is marked as used for reference.
    #[must_use]
    pub const fn is_reference(self) -> bool {
        self.nal_ref_idc != 0
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn every_five_bit_value_round_trips() {
        for v in 0..32u8 {
            assert_eq!(NalUnitType::from_u8(v).to_u8(), v, "value {v}");
        }
    }

    #[test]
    fn the_vcl_set_is_one_to_five() {
        for v in 0..32u8 {
            assert_eq!(
                NalUnitType::from_u8(v).is_vcl(),
                (1..=5).contains(&v),
                "value {v}"
            );
        }
    }

    #[test]
    fn partitions_b_and_c_have_no_slice_header() {
        assert!(!NalUnitType::SlicePartitionB.has_slice_header());
        assert!(!NalUnitType::SlicePartitionC.has_slice_header());
        assert!(NalUnitType::SlicePartitionA.has_slice_header());
    }

    #[test]
    fn reserved_and_unspecified_are_distinguished() {
        assert_eq!(NalUnitType::from_u8(17), NalUnitType::Reserved(17));
        assert_eq!(NalUnitType::from_u8(24), NalUnitType::UnspecifiedHigh(24));
        assert_eq!(NalUnitType::from_u8(0), NalUnitType::Unspecified);
    }

    #[test]
    fn the_sps_header_byte_from_a_real_stream() {
        // 0x67, from `ffmpeg -f lavfi -i testsrc2 -c:v libx264 -f h264 out.264`.
        let h = H264NalHeader::parse(&[0x67]).expect("one byte");
        assert_eq!(h.nal_unit_type, NalUnitType::Sps);
        assert_eq!(h.nal_ref_idc, 3);
        assert!(!h.forbidden_zero_bit);
    }

    #[test]
    fn an_empty_unit_has_no_header() {
        assert!(H264NalHeader::parse(&[]).is_none());
    }
}
