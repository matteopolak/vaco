//! The video parameter set, ITU-T H.265 §7.3.2.1.
//!
//! # What a VPS is for, and why it is parsed at all
//!
//! H.264 has no equivalent. The VPS sits above the SPS and describes the whole
//! *bitstream*: how many temporal sub-layers and layers it has, what
//! profile-tier-level each operating point conforms to, and — optionally — the
//! bitstream-wide timing. An SPS names one with `sps_video_parameter_set_id`.
//!
//! For a single-layer stream almost nothing here is load-bearing: the SPS
//! repeats the profile, the level and the DPB sizes, and every consumer reads
//! them from there. It is parsed anyway for two reasons. It is the first NAL
//! unit of every HEVC stream and of every `hvcC`, so a parser that skipped it
//! would still have to frame it correctly; and `vps_timing_info` is the only
//! place a frame rate appears in a stream whose SPS has no VUI.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};
use crate::ptl::ProfileTierLevel;
use crate::sps::HrdParameters;
use crate::util::MAX_SUB_LAYERS;

/// The largest `vps_num_layer_sets_minus1` accepted, §7.4.3.1, which the
/// specification caps at 1023.
const MAX_LAYER_SETS: u32 = 1023;

/// The largest `vps_num_hrd_parameters` accepted, §7.4.3.1: bounded by
/// `vps_num_layer_sets_minus1 + 1`, so at most 1024.
const MAX_HRD_PARAMETERS: u32 = 1024;

/// `vps_timing_info`, §7.3.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VpsTiming {
    /// `vps_num_units_in_tick`.
    pub num_units_in_tick: u32,
    /// `vps_time_scale`.
    pub time_scale: u32,
    /// `vps_num_ticks_poc_diff_one_minus1`, present only when
    /// `vps_poc_proportional_to_timing_flag` was set.
    pub num_ticks_poc_diff_one_minus1: Option<u32>,
}

/// A video parameter set: §7.3.2.1, in field order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct Vps {
    /// `vps_video_parameter_set_id`, 0..=15.
    pub id: u8,
    /// `vps_base_layer_internal_flag`.
    pub base_layer_internal: bool,
    /// `vps_base_layer_available_flag`.
    pub base_layer_available: bool,
    /// `vps_max_layers_minus1 + 1`.
    pub max_layers: u8,
    /// `vps_max_sub_layers_minus1 + 1`, 1..=7.
    pub max_sub_layers: u8,
    /// `vps_temporal_id_nesting_flag`.
    pub temporal_id_nesting: bool,
    /// `profile_tier_level()`.
    pub ptl: ProfileTierLevel,
    /// `vps_max_dec_pic_buffering_minus1[i]`.
    pub max_dec_pic_buffering_minus1: Vec<u32>,
    /// `vps_max_num_reorder_pics[i]`.
    pub max_num_reorder_pics: Vec<u32>,
    /// `vps_max_latency_increase_plus1[i]`.
    pub max_latency_increase_plus1: Vec<u32>,
    /// `vps_max_layer_id`.
    pub max_layer_id: u8,
    /// `vps_num_layer_sets_minus1 + 1`.
    pub num_layer_sets: u32,
    /// `vps_timing_info`, present only if `vps_timing_info_present_flag`.
    pub timing: Option<VpsTiming>,
    /// The `hrd_parameters()` the timing block declares, with the layer-set
    /// index each applies to.
    pub hrd: Vec<(u32, HrdParameters)>,
    /// `vps_extension_flag`. The extension itself is not parsed; §7.3.2.1 says
    /// a decoder ignores `vps_extension_data_flag`, and everything after it is
    /// multi-layer syntax this crate does not describe.
    pub extension_present: bool,
}

impl Vps {
    /// The frame rate `vps_timing_info` implies, or [`Rational::UNDEFINED`].
    ///
    /// Same units as the VUI's: `vps_time_scale / vps_num_units_in_tick`, not
    /// halved. See [`VuiParameters::frame_rate`](crate::sps::VuiParameters::frame_rate).
    #[must_use]
    pub fn frame_rate(&self) -> Rational {
        match self.timing {
            Some(t) if t.num_units_in_tick != 0 && t.time_scale != 0 => {
                let (r, _) = Rational::reduce(
                    i64::from(t.time_scale),
                    i64::from(t.num_units_in_tick),
                    i64::from(i32::MAX),
                );
                r
            }
            _ => Rational::UNDEFINED,
        }
    }

    /// Parse a video parameter set from a NAL unit's RBSP.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the unit is not a VPS or a syntax element is
    /// out of range, [`Error::UnexpectedEof`] on truncation, or a budget error.
    pub fn parse(rbsp: &[u8], budget: &mut Budget) -> Result<Self> {
        let header = HevcNalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if header.nal_unit_type != NalUnitType::VPS_NUT {
            return Err(Error::InvalidData("not a video parameter set"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(16);
        let vps = Self::parse_data(&mut reader, budget)?;
        reader.check()?;
        Ok(vps)
    }

    /// `video_parameter_set_rbsp()`, §7.3.2.1, from a reader positioned just
    /// after the NAL header.
    ///
    /// # Errors
    ///
    /// As [`Vps::parse`].
    pub fn parse_data(reader: &mut BitReader<'_>, budget: &mut Budget) -> Result<Self> {
        let mut g = BoundedGolomb::new(reader, budget);
        let id = g.u(4)? as u8;
        let base_layer_internal = g.u(1)? != 0;
        let base_layer_available = g.u(1)? != 0;
        let max_layers_minus1 = g.u(6)?;
        let max_sub_layers_minus1 = g.u(3)?;
        let temporal_id_nesting = g.u(1)? != 0;
        // `vps_reserved_0xffff_16bits`. Not checked: §7.4.3.1 says a decoder
        // ignores its value, and rejecting a stream over it would refuse
        // otherwise-readable content.
        g.u(16)?;
        let ptl = ProfileTierLevel::parse(&mut g, true, max_sub_layers_minus1)?;

        let sub_layer_ordering_info_present = g.u(1)? != 0;
        let first = if sub_layer_ordering_info_present {
            0
        } else {
            max_sub_layers_minus1
        };
        let mut max_dec_pic_buffering_minus1 = Vec::new();
        let mut max_num_reorder_pics = Vec::new();
        let mut max_latency_increase_plus1 = Vec::new();
        for _ in first..=max_sub_layers_minus1.min(MAX_SUB_LAYERS - 1) {
            max_dec_pic_buffering_minus1.push(g.ue_v(15)?);
            max_num_reorder_pics.push(g.ue_v(15)?);
            max_latency_increase_plus1.push(g.ue_v(u32::MAX - 1)?);
        }

        let max_layer_id = g.u(6)? as u8;
        let num_layer_sets_minus1 = g.ue_v(MAX_LAYER_SETS)?;
        // `layer_id_included_flag[i][j]`: one bit per (layer set, layer id), so
        // up to 1023 * 64 bits. Charged before it runs.
        let flags = u64::from(num_layer_sets_minus1)
            .saturating_mul(u64::from(max_layer_id).saturating_add(1));
        g.budget().consume_fuel(flags.div_ceil(8).max(1))?;
        for _ in 0..num_layer_sets_minus1 {
            for _ in 0..=u32::from(max_layer_id) {
                g.u(1)?;
            }
        }

        let mut timing = None;
        let mut hrd = Vec::new();
        if g.u(1)? != 0 {
            let num_units_in_tick = g.u(32)?;
            let time_scale = g.u(32)?;
            let num_ticks_poc_diff_one_minus1 = if g.u(1)? != 0 {
                Some(g.ue_v(u32::MAX - 1)?)
            } else {
                None
            };
            timing = Some(VpsTiming {
                num_units_in_tick,
                time_scale,
                num_ticks_poc_diff_one_minus1,
            });
            let num_hrd = g.ue_v(MAX_HRD_PARAMETERS)?;
            g.budget().consume_fuel(u64::from(num_hrd))?;
            for i in 0..num_hrd {
                let layer_set_idx = g.ue_v(MAX_LAYER_SETS)?;
                // `cprms_present_flag[0]` is inferred to be 1, §7.4.3.1.
                let cprms_present = if i > 0 { g.u(1)? != 0 } else { true };
                let params = crate::sps::parse_hrd(&mut g, cprms_present, max_sub_layers_minus1)?;
                hrd.push((layer_set_idx, params));
            }
        }
        let extension_present = g.u(1)? != 0;

        Ok(Self {
            id,
            base_layer_internal,
            base_layer_available,
            max_layers: max_layers_minus1 as u8 + 1,
            max_sub_layers: max_sub_layers_minus1 as u8 + 1,
            temporal_id_nesting,
            ptl,
            max_dec_pic_buffering_minus1,
            max_num_reorder_pics,
            max_latency_increase_plus1,
            max_layer_id,
            num_layer_sets: num_layer_sets_minus1.saturating_add(1),
            timing,
            hrd,
            extension_present,
        })
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

    /// The VPS `x265` writes for a Main-profile 640x360 stream, byte for byte
    /// from `sd.265`, emulation prevention still in place.
    const REAL_VPS_EBSP: &[u8] = &[
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09,
    ];

    fn parse(ebsp: &[u8]) -> Vps {
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(ebsp, &mut scratch);
        let mut budget = Budget::new(Limits::strict());
        Vps::parse(rbsp, &mut budget).expect("a real VPS parses")
    }

    #[test]
    fn a_real_vps() {
        let vps = parse(REAL_VPS_EBSP);
        assert_eq!(vps.id, 0);
        assert!(vps.base_layer_internal);
        assert!(vps.base_layer_available);
        assert_eq!(vps.max_layers, 1);
        assert_eq!(vps.max_sub_layers, 1);
        assert!(vps.temporal_id_nesting);
        let g = vps.ptl.general.expect("profile present");
        assert_eq!(g.profile_idc, 1);
        assert_eq!(vps.ptl.general_level_idc, 63);
        assert_eq!(vps.max_dec_pic_buffering_minus1, [4]);
        assert_eq!(vps.max_num_reorder_pics, [2]);
        assert_eq!(vps.max_latency_increase_plus1, [5]);
        assert_eq!(vps.max_layer_id, 0);
        assert_eq!(vps.num_layer_sets, 1);
        assert!(vps.timing.is_none());
        assert!(!vps.extension_present);
    }

    #[test]
    fn a_unit_of_the_wrong_type_is_refused() {
        let mut data = REAL_VPS_EBSP.to_vec();
        data[0] = 0x42; // SPS
        let mut budget = Budget::new(Limits::strict());
        assert!(matches!(
            Vps::parse(&data, &mut budget),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncation_is_handled() {
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(REAL_VPS_EBSP, &mut scratch).to_vec();
        for n in 0..rbsp.len() {
            let mut budget = Budget::new(Limits::strict());
            let _ = Vps::parse(&rbsp[..n], &mut budget);
        }
    }
}
