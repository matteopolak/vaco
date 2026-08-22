//! NAL unit types, ITU-T H.265 Table 7-1, and the two-byte header of §7.3.1.2.

use vaco_format_nalu::{HeaderKind, NalHeader};

/// `nal_unit_type`, ITU-T H.265 Table 7-1.
///
/// Six bits, so 64 values. Kept as a newtype over the raw number rather than a
/// 64-variant enum: HEVC's numbering is *structured* — VCL below 32, IRAP in
/// 16..=23, sub-layer non-reference on the even values below 16 — and every
/// question a parser asks is a range test on the number. A 64-arm enum would
/// turn each of those into a 64-arm match and would still have to keep the
/// number for the reserved and unspecified ranges.
///
/// The name each value gets in Table 7-1 is available from
/// [`NalUnitType::name`], for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct NalUnitType(u8);

impl NalUnitType {
    /// 0 — coded slice segment of a non-TSA, non-STSA trailing picture,
    /// sub-layer non-reference.
    pub const TRAIL_N: Self = Self(0);
    /// 1 — the sub-layer reference form of [`TRAIL_N`](Self::TRAIL_N).
    pub const TRAIL_R: Self = Self(1);
    /// 2 — temporal sub-layer access, sub-layer non-reference.
    pub const TSA_N: Self = Self(2);
    /// 3 — temporal sub-layer access, sub-layer reference.
    pub const TSA_R: Self = Self(3);
    /// 4 — step-wise temporal sub-layer access, sub-layer non-reference.
    pub const STSA_N: Self = Self(4);
    /// 5 — step-wise temporal sub-layer access, sub-layer reference.
    pub const STSA_R: Self = Self(5);
    /// 6 — random-access decodable leading picture, sub-layer non-reference.
    pub const RADL_N: Self = Self(6);
    /// 7 — random-access decodable leading picture, sub-layer reference.
    pub const RADL_R: Self = Self(7);
    /// 8 — random-access skipped leading picture, sub-layer non-reference.
    pub const RASL_N: Self = Self(8);
    /// 9 — random-access skipped leading picture, sub-layer reference.
    pub const RASL_R: Self = Self(9);
    /// 16 — broken link access with leading pictures.
    pub const BLA_W_LP: Self = Self(16);
    /// 17 — broken link access with RADL pictures.
    pub const BLA_W_RADL: Self = Self(17);
    /// 18 — broken link access with no leading pictures.
    pub const BLA_N_LP: Self = Self(18);
    /// 19 — instantaneous decoding refresh with RADL pictures.
    pub const IDR_W_RADL: Self = Self(19);
    /// 20 — instantaneous decoding refresh with no leading pictures.
    pub const IDR_N_LP: Self = Self(20);
    /// 21 — clean random access.
    pub const CRA_NUT: Self = Self(21);
    /// 32 — video parameter set.
    pub const VPS_NUT: Self = Self(32);
    /// 33 — sequence parameter set.
    pub const SPS_NUT: Self = Self(33);
    /// 34 — picture parameter set.
    pub const PPS_NUT: Self = Self(34);
    /// 35 — access unit delimiter.
    pub const AUD_NUT: Self = Self(35);
    /// 36 — end of sequence.
    pub const EOS_NUT: Self = Self(36);
    /// 37 — end of bitstream.
    pub const EOB_NUT: Self = Self(37);
    /// 38 — filler data.
    pub const FD_NUT: Self = Self(38);
    /// 39 — supplemental enhancement information, before the slice data.
    pub const PREFIX_SEI_NUT: Self = Self(39);
    /// 40 — supplemental enhancement information, after the slice data.
    pub const SUFFIX_SEI_NUT: Self = Self(40);

    /// Wrap a raw six-bit value.
    ///
    /// Values above 63 cannot occur — the field is six bits — and are masked
    /// rather than rejected, so a caller that shifts incorrectly gets a wrong
    /// answer rather than a panic.
    #[must_use]
    pub const fn from_u8(v: u8) -> Self {
        Self(v & 0x3F)
    }

    /// The raw six-bit value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Whether this is a VCL NAL unit — §7.4.2.2 defines that as
    /// `nal_unit_type` below 32.
    ///
    /// Note this includes the reserved VCL ranges 10..=15 and 22..=31, which is
    /// deliberate: §7.4.2.2 says a decoder must ignore reserved values, and the
    /// *access unit* structure still treats them as picture data.
    #[must_use]
    pub const fn is_vcl(self) -> bool {
        self.0 < 32
    }

    /// `IRAP` — intra random access point, §3.73: 16..=23.
    #[must_use]
    pub const fn is_irap(self) -> bool {
        self.0 >= 16 && self.0 <= 23
    }

    /// `IdrPicFlag`, §7.4.2.2: [`IDR_W_RADL`](Self::IDR_W_RADL) or
    /// [`IDR_N_LP`](Self::IDR_N_LP).
    ///
    /// This is the flag the slice segment header consults twice, so it is worth
    /// its own name: an IDR slice header carries no `slice_pic_order_cnt_lsb`
    /// and no reference picture set at all.
    #[must_use]
    pub const fn is_idr(self) -> bool {
        self.0 == 19 || self.0 == 20
    }

    /// `BlaPicFlag`, §7.4.2.2: 16..=18.
    #[must_use]
    pub const fn is_bla(self) -> bool {
        self.0 >= 16 && self.0 <= 18
    }

    /// Clean random access, §7.4.2.2.
    #[must_use]
    pub const fn is_cra(self) -> bool {
        self.0 == 21
    }

    /// A random-access skipped leading picture, §7.4.2.2 — 8 or 9.
    #[must_use]
    pub const fn is_rasl(self) -> bool {
        self.0 == 8 || self.0 == 9
    }

    /// A random-access decodable leading picture, §7.4.2.2 — 6 or 7.
    #[must_use]
    pub const fn is_radl(self) -> bool {
        self.0 == 6 || self.0 == 7
    }

    /// A sub-layer non-reference picture, §7.4.2.2.
    ///
    /// The even values below 16 — `TRAIL_N`, `TSA_N`, `STSA_N`, `RADL_N`,
    /// `RASL_N` and the three reserved `RSV_VCL_N` — are the pictures no other
    /// picture of the same sub-layer refers to. That parity is not an accident;
    /// it is how Table 7-1 is laid out, and it is why the reference-picture
    /// rules can be a bit test.
    #[must_use]
    pub const fn is_sub_layer_non_reference(self) -> bool {
        self.0 < 16 && self.0.is_multiple_of(2)
    }

    /// Whether this unit begins with a slice segment header — that is, whether
    /// it is a VCL unit.
    #[must_use]
    pub const fn has_slice_header(self) -> bool {
        self.is_vcl()
    }

    /// Whether this unit is a parameter set the parser must retain.
    #[must_use]
    pub const fn is_parameter_set(self) -> bool {
        self.0 >= 32 && self.0 <= 34
    }

    /// Whether this unit carries SEI, prefix or suffix.
    #[must_use]
    pub const fn is_sei(self) -> bool {
        self.0 == 39 || self.0 == 40
    }

    /// Whether §7.4.2.4.4 requires this unit to *precede* the first VCL unit of
    /// the access unit it belongs to.
    ///
    /// The access unit delimiter, the three parameter sets and prefix SEI all
    /// do, which is what makes one of them appearing *after* a slice the start
    /// of the next access unit. Suffix SEI and filler do not.
    #[must_use]
    pub const fn precedes_slice_data(self) -> bool {
        matches!(self.0, 32..=35 | 39 | 41..=44)
    }

    /// The name Table 7-1 gives this value.
    ///
    /// Diagnostics only; nothing in the output contract depends on it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self.0 {
            0 => "TRAIL_N",
            1 => "TRAIL_R",
            2 => "TSA_N",
            3 => "TSA_R",
            4 => "STSA_N",
            5 => "STSA_R",
            6 => "RADL_N",
            7 => "RADL_R",
            8 => "RASL_N",
            9 => "RASL_R",
            10 => "RSV_VCL_N10",
            11 => "RSV_VCL_R11",
            12 => "RSV_VCL_N12",
            13 => "RSV_VCL_R13",
            14 => "RSV_VCL_N14",
            15 => "RSV_VCL_R15",
            16 => "BLA_W_LP",
            17 => "BLA_W_RADL",
            18 => "BLA_N_LP",
            19 => "IDR_W_RADL",
            20 => "IDR_N_LP",
            21 => "CRA_NUT",
            22 => "RSV_IRAP_VCL22",
            23 => "RSV_IRAP_VCL23",
            24..=31 => "RSV_VCL",
            32 => "VPS_NUT",
            33 => "SPS_NUT",
            34 => "PPS_NUT",
            35 => "AUD_NUT",
            36 => "EOS_NUT",
            37 => "EOB_NUT",
            38 => "FD_NUT",
            39 => "PREFIX_SEI_NUT",
            40 => "SUFFIX_SEI_NUT",
            41..=47 => "RSV_NVCL",
            _ => "UNSPEC",
        }
    }
}

/// An HEVC NAL unit header: §7.3.1.2's four fields, decoded.
///
/// A thin projection of [`vaco_format_nalu::NalHeader`] onto HEVC's own
/// vocabulary, so HEVC code says `header.nal_unit_type.is_irap()` rather than
/// comparing a `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcNalHeader {
    /// Must be 0. A 1 means these bytes are not a conforming NAL unit.
    pub forbidden_zero_bit: bool,
    /// The unit's type.
    pub nal_unit_type: NalUnitType,
    /// `nuh_layer_id`, 0..=63. 0 is the base layer; this crate parses the base
    /// layer's syntax only and reports the id so a caller can drop the rest.
    pub nuh_layer_id: u8,
    /// `TemporalId`, that is `nuh_temporal_id_plus1 - 1`.
    pub temporal_id: u8,
    /// The raw `nuh_temporal_id_plus1`. Required to be non-zero, so a caller
    /// can detect the forbidden encoding rather than seeing it wrap.
    pub temporal_id_plus1: u8,
}

impl HevcNalHeader {
    /// Decode the two header bytes of a NAL unit.
    #[must_use]
    pub fn parse(nal: &[u8]) -> Option<Self> {
        let h = NalHeader::parse(HeaderKind::H265, nal)?;
        Some(Self {
            forbidden_zero_bit: h.forbidden_zero_bit,
            nal_unit_type: NalUnitType::from_u8(h.nal_unit_type),
            nuh_layer_id: h.nuh_layer_id,
            temporal_id: h.temporal_id,
            temporal_id_plus1: h.temporal_id_plus1,
        })
    }

    /// Whether the header satisfies the constraints every conforming stream
    /// obeys: `forbidden_zero_bit == 0` and `nuh_temporal_id_plus1 != 0`.
    #[must_use]
    pub const fn is_conforming(self) -> bool {
        !self.forbidden_zero_bit && self.temporal_id_plus1 != 0
    }

    /// Whether this unit belongs to the base layer, which is the only layer
    /// whose syntax this crate reads.
    #[must_use]
    pub const fn is_base_layer(self) -> bool {
        self.nuh_layer_id == 0
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
    fn the_vcl_set_is_everything_below_thirty_two() {
        for v in 0..64u8 {
            assert_eq!(NalUnitType::from_u8(v).is_vcl(), v < 32, "value {v}");
        }
    }

    #[test]
    fn the_irap_range_is_sixteen_to_twenty_three() {
        for v in 0..64u8 {
            assert_eq!(
                NalUnitType::from_u8(v).is_irap(),
                (16..=23).contains(&v),
                "value {v}"
            );
        }
    }

    #[test]
    fn sub_layer_non_reference_is_the_even_values_below_sixteen() {
        for v in 0..64u8 {
            assert_eq!(
                NalUnitType::from_u8(v).is_sub_layer_non_reference(),
                v < 16 && v % 2 == 0,
                "value {v}"
            );
        }
    }

    #[test]
    fn the_six_bit_field_round_trips() {
        for v in 0..64u8 {
            assert_eq!(NalUnitType::from_u8(v).get(), v);
        }
        // Anything above 63 is masked, not rejected.
        assert_eq!(NalUnitType::from_u8(0xFF).get(), 63);
    }

    #[test]
    fn a_real_vps_header() {
        // 0x40 0x01, from `x265`: type 32, layer 0, temporal id 0.
        let h = HevcNalHeader::parse(&[0x40, 0x01]).expect("two bytes");
        assert_eq!(h.nal_unit_type, NalUnitType::VPS_NUT);
        assert_eq!(h.nuh_layer_id, 0);
        assert_eq!(h.temporal_id, 0);
        assert!(h.is_conforming());
        assert!(h.is_base_layer());
    }

    #[test]
    fn a_real_idr_header() {
        // 0x26 0x01: type 19, IDR_W_RADL.
        let h = HevcNalHeader::parse(&[0x26, 0x01]).expect("two bytes");
        assert_eq!(h.nal_unit_type, NalUnitType::IDR_W_RADL);
        assert!(h.nal_unit_type.is_idr());
        assert!(h.nal_unit_type.is_irap());
        assert!(h.nal_unit_type.is_vcl());
    }

    #[test]
    fn a_one_byte_unit_has_no_header() {
        assert!(HevcNalHeader::parse(&[0x40]).is_none());
        assert!(HevcNalHeader::parse(&[]).is_none());
    }

    #[test]
    fn the_units_that_must_precede_slice_data() {
        for v in 0..64u8 {
            let expect = matches!(v, 32..=35 | 39 | 41..=44);
            assert_eq!(
                NalUnitType::from_u8(v).precedes_slice_data(),
                expect,
                "value {v}"
            );
        }
        // Suffix SEI explicitly does not.
        assert!(!NalUnitType::SUFFIX_SEI_NUT.precedes_slice_data());
    }
}
