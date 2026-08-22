//! `AV1CodecConfigurationRecord`, AV1 Codec ISO Media File Format Binding,
//! §2.3.3. MP4 and Matroska both carry it: MP4 as the `av1C` sample entry box,
//! Matroska's `WebM` mapping stores the same byte layout as the track's
//! `CodecPrivate`.
//!
//! Unlike H.264's `avcC` and HEVC's `hvcC`, this record does not wrap NAL
//! units with their own length prefixes — `configOBUs` is a bare, directly
//! concatenated sequence of OBUs (§5.3's "OBU stream" framing;
//! [`crate::obu::Av1Framing::ObuStream`]), because every OBU already sizes itself
//! when `obu_has_size_field` is set, which the specification requires of every
//! OBU inside this record.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::obu::{Av1Framing, ObuType, units};
use crate::profile::Tier;
use crate::seq::SequenceHeader;

/// A parsed `AV1CodecConfigurationRecord`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is one independent bit of the record's fixed layout, §2.3.3 Table 1; \
              grouping them into an enum would invent structure the specification does not have"
)]
pub struct Av1CodecConfigurationRecord {
    /// Always 1; a record with any other value is not this format at all, but
    /// callers rarely reject on it, so it is reported rather than enforced.
    pub version: u8,
    pub seq_profile: u8,
    pub seq_level_idx_0: u8,
    pub seq_tier_0: Tier,
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub monochrome: bool,
    pub chroma_subsampling_x: bool,
    pub chroma_subsampling_y: bool,
    pub chroma_sample_position: u8,
    pub initial_presentation_delay: Option<u8>,
    /// The bytes after the fixed header: a concatenation of self-sized OBUs,
    /// almost always exactly one `OBU_SEQUENCE_HEADER` and sometimes trailing
    /// metadata OBUs.
    pub config_obus: Vec<u8>,
}

impl Av1CodecConfigurationRecord {
    const MIN_LEN: usize = 4;

    /// Parse a record from a container's extradata (MP4 `av1C` payload or a
    /// `WebM` track's `CodecPrivate`).
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] if the record is shorter than its fixed
    /// header, or [`Error::LimitExceeded`] from `budget`.
    pub fn parse(data: &[u8], budget: &mut Budget) -> Result<Self> {
        if data.len() < Self::MIN_LEN {
            return Err(Error::UnexpectedEof);
        }
        budget.check_metadata_bytes(data.len() as u64)?;
        let mut r = ByteReader::new(data);
        let b0 = r.u8();
        // marker (bit 7) is required to be 1; not enforced here for the same
        // reason `vaco-parse-h264`'s `configurationVersion` is reported rather
        // than gated: a caller that wants strictness can check it itself.
        let version = b0 & 0x7F;
        let b1 = r.u8();
        let seq_profile = (b1 >> 5) & 0x07;
        let seq_level_idx_0 = b1 & 0x1F;
        let b2 = r.u8();
        let seq_tier_0 = Tier::from_flag(b2 & 0x80 != 0);
        let high_bitdepth = b2 & 0x40 != 0;
        let twelve_bit = b2 & 0x20 != 0;
        let monochrome = b2 & 0x10 != 0;
        let chroma_subsampling_x = b2 & 0x08 != 0;
        let chroma_subsampling_y = b2 & 0x04 != 0;
        let chroma_sample_position = b2 & 0x03;
        let b3 = r.u8();
        let initial_presentation_delay = if b3 & 0x10 != 0 {
            Some(b3 & 0x0F)
        } else {
            None
        };
        let mut config_obus = budget.alloc::<u8>(r.remaining())?;
        config_obus.copy_from_slice(r.rest());
        r.check()?;

        Ok(Self {
            version,
            seq_profile,
            seq_level_idx_0,
            seq_tier_0,
            high_bitdepth,
            twelve_bit,
            monochrome,
            chroma_subsampling_x,
            chroma_subsampling_y,
            chroma_sample_position,
            initial_presentation_delay,
            config_obus,
        })
    }

    /// The `OBU_SEQUENCE_HEADER` inside [`Av1CodecConfigurationRecord::config_obus`],
    /// parsed. `None` if the record carries none — legal but unusual; a
    /// caller then falls back to this record's own fixed-header fields, which
    /// duplicate the sequence header's profile/level/tier/bit-depth/chroma
    /// fields (deliberately, so a demuxer can report them without decoding
    /// `configOBUs` at all).
    ///
    /// # Errors
    ///
    /// Whatever [`SequenceHeader::parse`] returns for a sequence header OBU
    /// that is present but malformed.
    pub fn sequence_header(&self, budget: &mut Budget) -> Result<Option<SequenceHeader>> {
        for obu in units(&self.config_obus, Av1Framing::ObuStream) {
            if obu.header.obu_type == ObuType::SEQUENCE_HEADER {
                let payload = obu.payload(&self.config_obus);
                return SequenceHeader::parse(payload, budget).map(Some);
            }
        }
        Ok(None)
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
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// The exact `av1C` payload from `sample.mp4` (`ffmpeg -c:v libsvtav1`,
    /// 642x358, yuv420p, level 2.1).
    fn real_av1c() -> [u8; 17] {
        [
            0x81, 0x01, 0x0c, 0x00, 0x0a, 0x0b, 0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00,
            0xbe, 0x00, 0x10,
        ]
    }

    #[test]
    fn a_real_av1c_decodes_to_the_measured_stream_properties() {
        let data = real_av1c();
        let rec = Av1CodecConfigurationRecord::parse(&data, &mut budget()).expect("parses");
        assert_eq!(rec.version, 1);
        assert_eq!(rec.seq_profile, 0);
        assert_eq!(rec.seq_level_idx_0, 1);
        assert_eq!(rec.seq_tier_0, Tier::Main);
        assert!(!rec.high_bitdepth);
        assert!(!rec.monochrome);
        assert!(rec.chroma_subsampling_x && rec.chroma_subsampling_y);
        assert_eq!(rec.initial_presentation_delay, None);
        assert_eq!(rec.config_obus.len(), 13);
    }

    #[test]
    fn the_embedded_sequence_header_matches_the_fixed_header_fields() {
        let data = real_av1c();
        let mut b = budget();
        let rec = Av1CodecConfigurationRecord::parse(&data, &mut b).expect("parses");
        let sh = rec
            .sequence_header(&mut b)
            .expect("parses")
            .expect("a sequence header is present");
        assert_eq!(sh.seq_profile, rec.seq_profile);
        assert_eq!(sh.max_frame_width, 642);
        assert_eq!(sh.max_frame_height, 358);
        assert_eq!(
            sh.primary_operating_point().unwrap().seq_level_idx,
            rec.seq_level_idx_0
        );
    }

    #[test]
    fn truncation_never_panics() {
        let data = real_av1c();
        for n in 0..=data.len() {
            let _ = Av1CodecConfigurationRecord::parse(&data[..n], &mut budget());
        }
    }
}
