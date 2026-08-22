//! `AVCDecoderConfigurationRecord`, ISO/IEC 14496-15 §5.3.3.1.
//!
//! MP4 and Matroska carry H.264 parameter sets *out of band*, in a box the
//! container reads before any sample. This is that box, and it is why an MP4's
//! first sample can be a slice with no SPS in front of it.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_format_nalu::LengthSize;
use vaco_limits::Budget;

/// The profiles whose configuration record carries the trailing
/// `chroma_format` / `bit_depth` block, ISO/IEC 14496-15 §5.3.3.1.
///
/// # A discrepancy worth knowing about
///
/// The published text conditions the block on
/// `profile_idc in {100, 110, 122, 144}`. Observed: `ffmpeg 8.1` writes it for
/// `profile_idc == 244` as well — a High 4:4:4 Predictive stream muxed to MP4
/// has `... 01 00 06 68 eb e3 c4 48 44 | ff f8 f8 00`, four extra bytes whose
/// first is `0xff`, i.e. `chroma_format == 3`.
///
/// So the presence test used here is **"the profile is one of the extended set
/// *and* at least four bytes remain"**, which reads both spellings and cannot
/// misread a record that simply ends. The block is redundant in any case: the
/// same three values are in the SPS this record carries.
const EXTENDED_PROFILES: [u8; 13] = [100, 110, 122, 144, 244, 44, 83, 86, 118, 128, 134, 135, 138];

/// A parsed `AVCDecoderConfigurationRecord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvcDecoderConfigurationRecord {
    /// `configurationVersion`. 1 in every record ever written.
    pub configuration_version: u8,
    /// `AVCProfileIndication` — the SPS's `profile_idc`.
    pub profile_indication: u8,
    /// `profile_compatibility` — the SPS's constraint-flag byte.
    pub profile_compatibility: u8,
    /// `AVCLevelIndication` — the SPS's `level_idc`.
    pub level_indication: u8,
    /// `lengthSizeMinusOne + 1`, the in-band NAL length prefix width.
    pub length_size: LengthSize,
    /// The sequence parameter sets, as raw NAL units (EBSP).
    pub sps: Vec<Vec<u8>>,
    /// The picture parameter sets, as raw NAL units (EBSP).
    pub pps: Vec<Vec<u8>>,
    /// `chroma_format`, from the extended block when present.
    pub chroma_format: Option<u8>,
    /// `bit_depth_luma_minus8 + 8`, from the extended block.
    pub bit_depth_luma: Option<u8>,
    /// `bit_depth_chroma_minus8 + 8`, from the extended block.
    pub bit_depth_chroma: Option<u8>,
    /// The sequence parameter set extensions, as raw NAL units.
    pub sps_ext: Vec<Vec<u8>>,
}

impl AvcDecoderConfigurationRecord {
    /// The smallest record that can exist: the seven fixed bytes.
    const MIN_LEN: usize = 7;

    /// Parse a record from a container's extradata.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] if the record is shorter than its own fields,
    /// [`Error::InvalidData`] for a reserved `lengthSizeMinusOne` of 2 (a
    /// three-byte length, which no decoder implements) and for a parameter set
    /// whose declared length runs past the end, and [`Error::LimitExceeded`]
    /// when the declared sizes exceed the budget.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        if data.len() < Self::MIN_LEN {
            return Err(Error::UnexpectedEof);
        }
        budget.check_metadata_bytes(data.len() as u64)?;
        let mut r = ByteReader::new(data);
        let configuration_version = r.u8();
        let profile_indication = r.u8();
        let profile_compatibility = r.u8();
        let level_indication = r.u8();
        let length_size = LengthSize::from_minus_one(r.u8() & 0x03).ok_or(Error::InvalidData(
            "avcC declares a three-byte NAL length prefix",
        ))?;

        // The upper three bits are `reserved` and are `111` in every real
        // record, but nothing depends on them, so they are masked rather than
        // checked — a record with a stray zero there is still readable.
        let sps_count = u32::from(r.u8() & 0x1F);
        let sps = read_parameter_sets(&mut r, sps_count, budget)?;
        let pps_count = u32::from(r.u8());
        let pps = read_parameter_sets(&mut r, pps_count, budget)?;

        let mut chroma_format = None;
        let mut bit_depth_luma = None;
        let mut bit_depth_chroma = None;
        let mut sps_ext = Vec::new();
        if EXTENDED_PROFILES.contains(&profile_indication) && r.remaining() >= 4 {
            chroma_format = Some(r.u8() & 0x03);
            bit_depth_luma = Some((r.u8() & 0x07) + 8);
            bit_depth_chroma = Some((r.u8() & 0x07) + 8);
            let ext_count = u32::from(r.u8());
            sps_ext = read_parameter_sets(&mut r, ext_count, budget)?;
        }

        r.check()?;
        Ok(Self {
            configuration_version,
            profile_indication,
            profile_compatibility,
            level_indication,
            length_size,
            sps,
            pps,
            chroma_format,
            bit_depth_luma,
            bit_depth_chroma,
            sps_ext,
        })
    }

    /// The `avc1.PPCCLL` codec parameter of RFC 6381, which `ffprobe` prints as
    /// `mime_codec_string`.
    ///
    /// Six lower-case hex digits: profile, compatibility byte, level. Verified
    /// against `ffprobe 8.1`, which prints `avc1.640028` for a High profile,
    /// zero constraints, level 4.0 stream.
    #[must_use]
    pub fn mime_codec_string(&self) -> String {
        format!(
            "avc1.{:02x}{:02x}{:02x}",
            self.profile_indication, self.profile_compatibility, self.level_indication
        )
    }
}

/// `numOf…ParameterSets` entries of `(u16 length, bytes)`.
fn read_parameter_sets(
    r: &mut ByteReader<'_>,
    count: u32,
    budget: &mut Budget,
) -> Result<Vec<Vec<u8>>> {
    budget.consume_fuel(u64::from(count))?;
    let mut out = Vec::new();
    for _ in 0..count {
        if r.remaining() < 2 {
            return Err(Error::UnexpectedEof);
        }
        let len = usize::from(r.be16());
        if len > r.remaining() {
            return Err(Error::InvalidData(
                "avcC parameter set runs past the end of the record",
            ));
        }
        // Two-phase: the length is checked against bytes that actually exist
        // before anything is charged or copied, so a declared 65535 in a
        // ten-byte record cannot allocate.
        let mut buf = budget.alloc::<u8>(len)?;
        buf.clear();
        buf.extend_from_slice(r.bytes(len));
        out.push(buf);
    }
    Ok(out)
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

    /// The `avcC` from an MP4 written by `ffmpeg 8.1`:
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=s=640x360:r=24:d=1 -c:v libx264 out.mp4
    /// ```
    ///
    /// then the box payload lifted verbatim.
    const REAL_AVCC: &[u8] = &[
        0x01, 0x64, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x1A, 0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40,
        0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30,
        0x0F, 0x16, 0x2D, 0x96, 0x01, 0x00, 0x06, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0, 0xFD, 0xF8,
        0xF8, 0x00,
    ];

    #[test]
    fn a_real_record() {
        let rec = AvcDecoderConfigurationRecord::parse(REAL_AVCC, &mut budget()).expect("parses");
        assert_eq!(rec.configuration_version, 1);
        assert_eq!(rec.profile_indication, 100);
        assert_eq!(rec.profile_compatibility, 0);
        assert_eq!(rec.level_indication, 30);
        assert_eq!(rec.length_size, LengthSize::FOUR);
        assert_eq!(rec.sps.len(), 1);
        assert_eq!(rec.pps.len(), 1);
        assert_eq!(rec.sps[0].len(), 0x1A);
        assert_eq!(rec.sps[0][0], 0x67);
        assert_eq!(rec.pps[0], vec![0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0]);
        assert_eq!(rec.chroma_format, Some(1));
        assert_eq!(rec.bit_depth_luma, Some(8));
        assert_eq!(rec.bit_depth_chroma, Some(8));
        assert!(rec.sps_ext.is_empty());
    }

    #[test]
    fn the_mime_string_matches_the_reference() {
        // ffprobe 8.1 prints `avc1.640028` for a High/level-4.0 stream.
        let rec = AvcDecoderConfigurationRecord {
            configuration_version: 1,
            profile_indication: 100,
            profile_compatibility: 0,
            level_indication: 40,
            length_size: LengthSize::FOUR,
            sps: Vec::new(),
            pps: Vec::new(),
            chroma_format: None,
            bit_depth_luma: None,
            bit_depth_chroma: None,
            sps_ext: Vec::new(),
        };
        assert_eq!(rec.mime_codec_string(), "avc1.640028");
    }

    /// 4:4:4 is not in the published presence list, and `ffmpeg` writes the
    /// block for it anyway.
    #[test]
    fn a_high_444_record_still_reads_its_extended_block() {
        let data = &[
            0x01, 0xF4, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x02, 0x67, 0xF4, 0x01, 0x00, 0x02, 0x68,
            0xEB, 0xFF, 0xF8, 0xF8, 0x00,
        ];
        let rec = AvcDecoderConfigurationRecord::parse(data, &mut budget()).expect("parses");
        assert_eq!(rec.profile_indication, 244);
        assert_eq!(rec.chroma_format, Some(3));
    }

    #[test]
    fn a_record_that_simply_ends_is_not_a_truncated_extended_block() {
        let data = &[
            0x01, 0x64, 0x00, 0x1E, 0xFF, 0xE1, 0x00, 0x02, 0x67, 0x64, 0x00,
        ];
        let rec = AvcDecoderConfigurationRecord::parse(data, &mut budget()).expect("parses");
        assert_eq!(rec.chroma_format, None);
        assert!(rec.pps.is_empty());
    }

    #[test]
    fn the_reserved_length_size_is_refused() {
        let mut data = REAL_AVCC.to_vec();
        data[4] = 0xFE; // lengthSizeMinusOne = 2 -> a three-byte length
        assert!(matches!(
            AvcDecoderConfigurationRecord::parse(&data, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn a_declared_length_past_the_end_cannot_allocate() {
        // One SPS declaring 65535 bytes in a record that holds two.
        let data = &[0x01, 0x64, 0x00, 0x1E, 0xFF, 0xE1, 0xFF, 0xFF, 0x67, 0x64];
        assert!(matches!(
            AvcDecoderConfigurationRecord::parse(data, &mut budget()),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncation_of_a_real_record_is_handled() {
        for n in 0..REAL_AVCC.len() {
            let _ = AvcDecoderConfigurationRecord::parse(&REAL_AVCC[..n], &mut budget());
        }
    }

    #[test]
    fn a_declared_count_of_thirty_one_in_an_empty_record_fails_cleanly() {
        let data = &[0x01, 0x64, 0x00, 0x1E, 0xFF, 0xFF];
        assert!(AvcDecoderConfigurationRecord::parse(data, &mut budget()).is_err());
    }
}
