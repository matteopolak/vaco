//! `HEVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §8.3.3.1.
//!
//! MP4 and Matroska carry HEVC parameter sets *out of band*, in an `hvcC` box
//! the container reads before any sample. This is that box, and it is why an
//! MP4's first sample can be a slice with no VPS, SPS or PPS in front of it.
//!
//! # How it differs from `avcC`
//!
//! `avcC` has three fixed parameter-set lists (SPS, PPS, SPS extension). `hvcC`
//! has an **array of arrays**: `numOfArrays` entries, each tagged with the NAL
//! unit type it holds, so a record can carry VPS, SPS, PPS *and* prefix SEI, in
//! whatever order the muxer chose, and a future NAL type needs no format change.
//! That is why [`HevcDecoderConfigurationRecord::arrays`] is a list rather than
//! three named fields — and why [`HevcDecoderConfigurationRecord::sps`] exists,
//! for the common case.
//!
//! It also repeats the whole `profile_tier_level()` general layer in a
//! byte-aligned form: profile space, tier, profile idc, the 32 compatibility
//! flags, the 48 constraint bits and the level. All of it is redundant with the
//! SPS the record carries, and all of it is what a player reads to decide
//! whether it can play the file without parsing a bitstream at all.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_format_nalu::LengthSize;
use vaco_limits::Budget;

use crate::nal::NalUnitType;
use crate::ptl::ProfileTier;

/// One `array` of the record: a NAL unit type and the units of that type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NalArray {
    /// `array_completeness`: the muxer promises no unit of this type appears in
    /// the samples.
    pub array_completeness: bool,
    /// `NAL_unit_type`, six bits.
    pub nal_unit_type: NalUnitType,
    /// The units, as raw NAL units (EBSP), header bytes included.
    pub units: Vec<Vec<u8>>,
}

/// A parsed `HEVCDecoderConfigurationRecord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcDecoderConfigurationRecord {
    /// `configurationVersion`. 1 in every record ever written.
    pub configuration_version: u8,
    /// The `profile_tier_level()` general layer, byte-aligned.
    ///
    /// Reconstructed into the same type the SPS parser produces, so a caller can
    /// compare the two without a second code path — and so
    /// [`ProfileTier::effective_profile_idc`] gives the same answer from either.
    pub profile_tier: ProfileTier,
    /// `general_level_idc`.
    pub level_idc: u8,
    /// `min_spatial_segmentation_idc`, twelve bits.
    pub min_spatial_segmentation_idc: u16,
    /// `parallelismType`: 0 unknown, 1 slices, 2 tiles, 3 entropy sync.
    pub parallelism_type: u8,
    /// `chromaFormat`, `chroma_format_idc`'s two bits.
    pub chroma_format_idc: u8,
    /// `bitDepthLumaMinus8 + 8`.
    pub bit_depth_luma: u8,
    /// `bitDepthChromaMinus8 + 8`.
    pub bit_depth_chroma: u8,
    /// `avgFrameRate`, in frames per 256 seconds. Zero means unstated.
    pub avg_frame_rate: u16,
    /// `constantFrameRate`: 0 unknown, 1 constant, 2 constant per temporal
    /// layer.
    pub constant_frame_rate: u8,
    /// `numTemporalLayers`. Zero means unstated.
    pub num_temporal_layers: u8,
    /// `temporalIdNested`.
    pub temporal_id_nested: bool,
    /// `lengthSizeMinusOne + 1`, the in-band NAL length prefix width.
    pub length_size: LengthSize,
    /// The arrays, in the order the record lists them.
    pub arrays: Vec<NalArray>,
}

impl HevcDecoderConfigurationRecord {
    /// The fixed part: 22 bytes before `numOfArrays`.
    const MIN_LEN: usize = 23;

    /// Every unit of `nal_unit_type` the record carries, across all arrays.
    ///
    /// A record *may* list the same type twice; nothing forbids it, and
    /// concatenating is the only reading that loses nothing.
    pub fn units_of(&self, nal_unit_type: NalUnitType) -> impl Iterator<Item = &[u8]> {
        self.arrays
            .iter()
            .filter(move |a| a.nal_unit_type == nal_unit_type)
            .flat_map(|a| a.units.iter().map(Vec::as_slice))
    }

    /// The video parameter sets.
    pub fn vps(&self) -> impl Iterator<Item = &[u8]> {
        self.units_of(NalUnitType::VPS_NUT)
    }

    /// The sequence parameter sets.
    pub fn sps(&self) -> impl Iterator<Item = &[u8]> {
        self.units_of(NalUnitType::SPS_NUT)
    }

    /// The picture parameter sets.
    pub fn pps(&self) -> impl Iterator<Item = &[u8]> {
        self.units_of(NalUnitType::PPS_NUT)
    }

    /// Parse a record from a container's extradata.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] if the record is shorter than its own fields,
    /// [`Error::InvalidData`] for a reserved `lengthSizeMinusOne` of 2 (a
    /// three-byte length, which no decoder implements) and for a unit whose
    /// declared length runs past the end, and [`Error::LimitExceeded`] when the
    /// declared sizes exceed the budget.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        if data.len() < Self::MIN_LEN {
            return Err(Error::UnexpectedEof);
        }
        budget.check_metadata_bytes(data.len() as u64)?;
        let mut r = ByteReader::new(data);
        let configuration_version = r.u8();
        let b = r.u8();
        let profile_space = (b >> 6) & 0x03;
        let tier_flag = b & 0x20 != 0;
        let profile_idc = b & 0x1F;
        let compatibility_flags = r.be32();
        // The 48 constraint bits, big-endian.
        let constraint = (u64::from(r.be16()) << 32) | u64::from(r.be32());
        let level_idc = r.u8();
        let profile_tier = ProfileTier {
            profile_space,
            tier_flag,
            profile_idc,
            compatibility_flags,
            ..ProfileTier::default()
        }
        .with_constraint_indicator_flags(constraint);

        // The reserved bits above each of the next five fields are `1` in every
        // real record, but nothing depends on them, so they are masked rather
        // than checked — a record with a stray zero there is still readable.
        let min_spatial_segmentation_idc = r.be16() & 0x0FFF;
        let parallelism_type = r.u8() & 0x03;
        let chroma_format_idc = r.u8() & 0x03;
        let bit_depth_luma = (r.u8() & 0x07) + 8;
        let bit_depth_chroma = (r.u8() & 0x07) + 8;
        let avg_frame_rate = r.be16();
        let flags = r.u8();
        let constant_frame_rate = (flags >> 6) & 0x03;
        let num_temporal_layers = (flags >> 3) & 0x07;
        let temporal_id_nested = flags & 0x04 != 0;
        let length_size = LengthSize::from_minus_one(flags & 0x03).ok_or(Error::InvalidData(
            "hvcC declares a three-byte NAL length prefix",
        ))?;

        let num_arrays = u32::from(r.u8());
        budget.consume_fuel(u64::from(num_arrays))?;
        let mut arrays = Vec::new();
        for _ in 0..num_arrays {
            if r.remaining() < 3 {
                return Err(Error::UnexpectedEof);
            }
            let tag = r.u8();
            let count = u32::from(r.be16());
            budget.consume_fuel(u64::from(count))?;
            let mut units = Vec::new();
            for _ in 0..count {
                if r.remaining() < 2 {
                    return Err(Error::UnexpectedEof);
                }
                let len = usize::from(r.be16());
                if len > r.remaining() {
                    return Err(Error::InvalidData(
                        "hvcC parameter set runs past the end of the record",
                    ));
                }
                // Two-phase: the length is checked against bytes that actually
                // exist before anything is charged or copied, so a declared
                // 65535 in a ten-byte record cannot allocate.
                let mut buf = budget.alloc::<u8>(len)?;
                buf.clear();
                buf.extend_from_slice(r.bytes(len));
                units.push(buf);
            }
            arrays.push(NalArray {
                array_completeness: tag & 0x80 != 0,
                nal_unit_type: NalUnitType::from_u8(tag),
                units,
            });
        }

        r.check()?;
        Ok(Self {
            configuration_version,
            profile_tier,
            level_idc,
            min_spatial_segmentation_idc,
            parallelism_type,
            chroma_format_idc,
            bit_depth_luma,
            bit_depth_chroma,
            avg_frame_rate,
            constant_frame_rate,
            num_temporal_layers,
            temporal_id_nested,
            length_size,
            arrays,
        })
    }

    /// The `hvc1.` codec parameter of RFC 6381 §3.3.
    ///
    /// Six dot-separated fields:
    ///
    /// ```text
    ///   hvc1 . [profile_space letter] profile_idc
    ///        . profile_compatibility_flags, REVERSED, hex, leading zeros trimmed
    ///        . [tier letter L or H] level_idc
    ///        . up to six constraint bytes, hex, trailing zero bytes trimmed
    /// ```
    ///
    /// The reversed compatibility field is the part everyone gets wrong: RFC
    /// 6381 specifies the 32 bits *in reverse bit order* from the bitstream, so
    /// a stream with flags 1 and 2 set — `0x60000000` in the bitstream — is
    /// written `6`.
    ///
    /// `// D17:` **not pinned against the reference.** `ffprobe 8.1` accepts
    /// `-show_entries stream=mime_codec_string` and prints nothing for an HEVC
    /// track, so there is no observed output to match; this follows RFC 6381
    /// alone. If a future reference version starts printing one, this is the
    /// first thing to re-derive.
    #[must_use]
    pub fn mime_codec_string(&self) -> String {
        let pt = self.profile_tier;
        let space = match pt.profile_space {
            0 => String::new(),
            n => char::from(b'A' + n - 1).to_string(),
        };
        let compat = pt.compatibility_flags.reverse_bits();
        let tier = if pt.tier_flag { 'H' } else { 'L' };
        let mut s = format!(
            "hvc1.{space}{}.{compat:X}.{tier}{}",
            pt.profile_idc, self.level_idc
        );
        let bits = pt.constraint_indicator_flags();
        // Six bytes, most significant first, with trailing zero bytes dropped.
        let bytes: [u8; 6] = [
            (bits >> 40) as u8,
            (bits >> 32) as u8,
            (bits >> 24) as u8,
            (bits >> 16) as u8,
            (bits >> 8) as u8,
            bits as u8,
        ];
        if let Some(last) = bytes.iter().rposition(|&b| b != 0) {
            use core::fmt::Write as _;
            for b in bytes.iter().take(last + 1) {
                // Infallible: writing to a `String` cannot fail.
                let _ = write!(s, ".{b:02X}");
            }
        }
        s
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// The `hvcC` from an MP4 written by `ffmpeg 8.1`:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=s=640x360:r=24:d=0.4 -c:v libx265 -tag:v hvc1 out.mp4
    /// ```
    ///
    /// then the box payload lifted verbatim. The real record continues with a
    /// 2.3 KiB prefix-SEI array holding `x265`'s version string; it is truncated
    /// to the first four bytes of that array here, with the count corrected, so
    /// the fixture stays readable.
    const REAL_HVCC: &[u8] = &[
        0x01, 0x01, 0x60, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3f, 0xf0, 0x00,
        0xfc, 0xfd, 0xf8, 0xf8, 0x00, 0x00, 0x0f, 0x03, //
        // VPS array: completeness 1, type 32, one unit of 24 bytes.
        0xa0, 0x00, 0x01, 0x00, 0x18, //
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09, //
        // SPS array: type 33, one unit of 42 bytes.
        0xa1, 0x00, 0x01, 0x00, 0x2a, //
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32, 0xbc, 0x05,
        0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
        //
        // PPS array: type 34, one unit of 7 bytes.
        0xa2, 0x00, 0x01, 0x00, 0x07, //
        0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40,
    ];

    #[test]
    fn a_real_record() {
        let rec = HevcDecoderConfigurationRecord::parse(REAL_HVCC, &mut budget())
            .expect("a real hvcC parses");
        assert_eq!(rec.configuration_version, 1);
        assert_eq!(rec.profile_tier.profile_idc, 1);
        assert!(!rec.profile_tier.tier_flag);
        assert_eq!(rec.profile_tier.profile_space, 0);
        assert!(rec.profile_tier.compatible_with(1));
        assert!(rec.profile_tier.compatible_with(2));
        assert_eq!(rec.level_idc, 63);
        assert_eq!(rec.min_spatial_segmentation_idc, 0);
        assert_eq!(rec.parallelism_type, 0);
        assert_eq!(rec.chroma_format_idc, 1);
        assert_eq!(rec.bit_depth_luma, 8);
        assert_eq!(rec.bit_depth_chroma, 8);
        assert_eq!(rec.avg_frame_rate, 0);
        assert_eq!(rec.num_temporal_layers, 1);
        assert!(rec.temporal_id_nested);
        assert_eq!(rec.length_size, LengthSize::FOUR);
        assert_eq!(rec.arrays.len(), 3);
        assert_eq!(rec.vps().count(), 1);
        assert_eq!(rec.sps().count(), 1);
        assert_eq!(rec.pps().count(), 1);
        assert_eq!(rec.sps().next().map(<[u8]>::len), Some(42));
        assert!(rec.arrays[0].array_completeness);
        assert_eq!(rec.arrays[0].nal_unit_type, NalUnitType::VPS_NUT);
    }

    /// The record's own profile-tier block must say the same thing as the SPS
    /// it carries — that is the whole point of it being there.
    #[test]
    fn the_record_and_its_sps_agree_about_the_profile() {
        let rec = HevcDecoderConfigurationRecord::parse(REAL_HVCC, &mut budget()).expect("parses");
        let sps_bytes = rec.sps().next().expect("one SPS").to_vec();
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(&sps_bytes, &mut scratch);
        let sps = crate::sps::Sps::parse(rbsp, &mut budget()).expect("the SPS parses");
        let from_sps = sps.ptl.general.expect("profile present");
        assert_eq!(from_sps.profile_idc, rec.profile_tier.profile_idc);
        assert_eq!(from_sps.tier_flag, rec.profile_tier.tier_flag);
        assert_eq!(
            from_sps.compatibility_flags,
            rec.profile_tier.compatibility_flags
        );
        assert_eq!(
            from_sps.constraint_indicator_flags(),
            rec.profile_tier.constraint_indicator_flags(),
            "the 48 constraint bits round-trip through the record"
        );
        assert_eq!(sps.ptl.general_level_idc, rec.level_idc);
        assert_eq!(u32::from(rec.chroma_format_idc), sps.chroma_format.idc());
        assert_eq!(rec.bit_depth_luma, sps.bit_depth_luma);
    }

    /// RFC 6381's reversed compatibility field, on the real record.
    #[test]
    fn the_mime_string_follows_rfc_6381() {
        let rec = HevcDecoderConfigurationRecord::parse(REAL_HVCC, &mut budget()).expect("parses");
        // profile 1, compat 0x60000000 reversed = 6, Main tier, level 63,
        // constraints 90 00 00 00 00 00 -> trailing zeros dropped.
        assert_eq!(rec.mime_codec_string(), "hvc1.1.6.L63.90");
    }

    #[test]
    fn the_reserved_length_size_is_refused() {
        let mut data = REAL_HVCC.to_vec();
        data[21] = 0x0E; // lengthSizeMinusOne = 2
        assert!(matches!(
            HevcDecoderConfigurationRecord::parse(&data, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn a_declared_length_past_the_end_cannot_allocate() {
        let mut data = REAL_HVCC[..28].to_vec();
        data[26] = 0xFF; // the VPS's declared length
        data[27] = 0xFF;
        assert!(matches!(
            HevcDecoderConfigurationRecord::parse(&data, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn a_declared_array_count_of_255_in_an_empty_record_fails_cleanly() {
        let mut data = REAL_HVCC[..23].to_vec();
        data[22] = 0xFF;
        assert!(HevcDecoderConfigurationRecord::parse(&data, &mut budget()).is_err());
    }

    #[test]
    fn every_truncation_of_a_real_record_is_handled() {
        for n in 0..REAL_HVCC.len() {
            let _ = HevcDecoderConfigurationRecord::parse(&REAL_HVCC[..n], &mut budget());
        }
    }
}
