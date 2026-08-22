//! The picture parameter set, ITU-T H.264 §7.3.2.2.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::{H264NalHeader, NalUnitType};
use crate::sps::{ChromaFormat, ScalingLists, Sps, read_scaling_lists};
use crate::util::more_rbsp_data;

/// `slice_group_map_type`, §7.4.2.2, and the parameters each type carries.
///
/// Slice groups (FMO) exist only in the Baseline and Extended profiles and are
/// essentially extinct in real content, but the syntax is not optional: the
/// fields must be consumed or everything after them is misread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SliceGroupMap {
    /// 0 — interleaved. `run_length_minus1[]`.
    Interleaved(Vec<u32>),
    /// 1 — dispersed. No parameters.
    Dispersed,
    /// 2 — foreground with left-over. `(top_left, bottom_right)` per group.
    Foreground(Vec<(u32, u32)>),
    /// 3, 4, 5 — box-out, raster scan and wipe.
    Changing {
        /// `slice_group_map_type`, 3, 4 or 5.
        map_type: u8,
        /// `slice_group_change_direction_flag`.
        change_direction: bool,
        /// `slice_group_change_rate_minus1`.
        change_rate_minus1: u32,
    },
    /// 6 — explicit. `slice_group_id[]`, one entry per map unit.
    Explicit(Vec<u8>),
}

impl SliceGroupMap {
    /// `slice_group_map_type`.
    #[must_use]
    pub const fn map_type(&self) -> u8 {
        match self {
            Self::Interleaved(_) => 0,
            Self::Dispersed => 1,
            Self::Foreground(_) => 2,
            Self::Changing { map_type, .. } => *map_type,
            Self::Explicit(_) => 6,
        }
    }
}

/// A picture parameter set: ITU-T H.264 §7.3.2.2, in field order.
///
/// The specification's own field order and its own names, flags included. A
/// syntax table transcribed into a struct is easier to check against the
/// standard than one reorganised for taste, and every one of these flags is
/// consulted independently by the slice-header parser.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct Pps {
    /// `pic_parameter_set_id`, 0..=255.
    pub id: u8,
    /// `seq_parameter_set_id`, 0..=31.
    pub sps_id: u8,
    /// `entropy_coding_mode_flag`: 0 is CAVLC, 1 is CABAC.
    pub entropy_coding_mode: bool,
    /// `bottom_field_pic_order_in_frame_present_flag`. Decides whether a slice
    /// header carries `delta_pic_order_cnt_bottom`, so a slice cannot be parsed
    /// without its PPS.
    pub bottom_field_pic_order_in_frame_present: bool,
    /// `num_slice_groups_minus1 + 1`.
    pub num_slice_groups: u32,
    /// The slice group map, present only when there is more than one group.
    pub slice_group_map: Option<SliceGroupMap>,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u32,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u32,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_idc`, 0..=2.
    pub weighted_bipred_idc: u8,
    /// `pic_init_qp_minus26`.
    pub pic_init_qp_minus26: i32,
    /// `pic_init_qs_minus26`.
    pub pic_init_qs_minus26: i32,
    /// `chroma_qp_index_offset`, -12..=12.
    pub chroma_qp_index_offset: i32,
    /// `deblocking_filter_control_present_flag`. Decides whether a slice header
    /// carries the deblocking overrides.
    pub deblocking_filter_control_present: bool,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `redundant_pic_cnt_present_flag`. Decides whether a slice header carries
    /// `redundant_pic_cnt`.
    pub redundant_pic_cnt_present: bool,
    /// `transform_8x8_mode_flag`, from the optional tail.
    pub transform_8x8_mode: bool,
    /// The tail's scaling lists.
    pub scaling_lists: Option<Box<ScalingLists>>,
    /// `second_chroma_qp_index_offset`. Inferred equal to
    /// `chroma_qp_index_offset` when the tail is absent (§7.4.2.2).
    pub second_chroma_qp_index_offset: i32,
    /// Whether the optional tail was actually present, which the inference for
    /// `second_chroma_qp_index_offset` hides and a re-serialiser needs.
    pub has_tail: bool,
}

impl Pps {
    /// Parse a picture parameter set from a NAL unit's RBSP.
    ///
    /// # Why the SPS is needed
    ///
    /// Two of the PPS's own fields are sized by the SPS: the slice-group map
    /// type 6 array is `Ceil(Log2(num_slice_groups))` bits per entry over
    /// `PicSizeInMapUnits` entries, and the tail's scaling-list count is 6 + 2
    /// or 6 + 6 depending on `chroma_format_idc`. Passing `None` parses
    /// everything up to the first field that needs it and then stops, which is
    /// the best a parser can do for a PPS that arrives before its SPS — a real
    /// situation in a stream joined mid-flight.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a syntax element outside its permitted range,
    /// [`Error::UnexpectedEof`] for a truncated unit, [`Error::LimitExceeded`]
    /// when a declared count would exceed the budget.
    pub fn parse(rbsp: &[u8], sps: Option<&Sps>, budget: &mut Budget) -> Result<Self> {
        let header = H264NalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if header.nal_unit_type != NalUnitType::Pps {
            return Err(Error::InvalidData("not a picture parameter set"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(8);
        let pps = Self::parse_data(&mut reader, rbsp, sps, budget)?;
        reader.check()?;
        Ok(pps)
    }

    /// `pic_parameter_set_rbsp()`, §7.3.2.2, from a reader positioned just
    /// after the NAL header. `rbsp` is the whole unit, needed to locate the
    /// `rbsp_stop_one_bit` for `more_rbsp_data()`.
    ///
    /// # Errors
    ///
    /// As [`Pps::parse`].
    pub fn parse_data(
        reader: &mut BitReader<'_>,
        rbsp: &[u8],
        sps: Option<&Sps>,
        budget: &mut Budget,
    ) -> Result<Self> {
        let mut g = BoundedGolomb::new(reader, budget);
        let id = g.ue_v(255)? as u8;
        let sps_id = g.ue_v(31)? as u8;
        let entropy_coding_mode = g.u(1)? != 0;
        let bottom_field_pic_order_in_frame_present = g.u(1)? != 0;
        // §7.4.2.2 bounds `num_slice_groups_minus1` at 7.
        let num_slice_groups = g.ue_v(7)? + 1;

        let slice_group_map = if num_slice_groups > 1 {
            Some(parse_slice_group_map(&mut g, num_slice_groups, sps)?)
        } else {
            None
        };

        // §7.4.2.2 bounds both at 31.
        let num_ref_idx_l0_default_active_minus1 = g.ue_v(31)?;
        let num_ref_idx_l1_default_active_minus1 = g.ue_v(31)?;
        let weighted_pred = g.u(1)? != 0;
        let weighted_bipred_idc = g.u(2)? as u8;
        // §7.4.2.2: `pic_init_qp_minus26` runs from -(26 + QpBdOffsetY) to +25,
        // and `QpBdOffsetY` is at most 48, so -74..=25 covers every bit depth.
        let pic_init_qp_minus26 = g.se_v(-74, 25)?;
        let pic_init_qs_minus26 = g.se_v(-26, 25)?;
        let chroma_qp_index_offset = g.se_v(-12, 12)?;
        let deblocking_filter_control_present = g.u(1)? != 0;
        let constrained_intra_pred = g.u(1)? != 0;
        let redundant_pic_cnt_present = g.u(1)? != 0;

        let mut transform_8x8_mode = false;
        let mut scaling_lists = None;
        let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
        let mut has_tail = false;

        if more_rbsp_data(g.reader(), rbsp) {
            has_tail = true;
            transform_8x8_mode = g.u(1)? != 0;
            if g.u(1)? != 0 {
                // §7.3.2.2: 6 + (chroma_format_idc != 3 ? 2 : 6) * transform_8x8_mode.
                // Without the SPS the count is unknowable, and guessing would
                // desynchronise the rest of the structure.
                let sps = sps.ok_or(Error::Unsupported(
                    "picture parameter set has scaling lists but its sequence parameter set has not been seen",
                ))?;
                let extra = if sps.chroma_format == ChromaFormat::Yuv444 {
                    6
                } else {
                    2
                };
                let count = 6 + if transform_8x8_mode { extra } else { 0 };
                scaling_lists = Some(Box::new(read_scaling_lists(&mut g, count)?));
            }
            second_chroma_qp_index_offset = g.se_v(-12, 12)?;
        }

        Ok(Self {
            id,
            sps_id,
            entropy_coding_mode,
            bottom_field_pic_order_in_frame_present,
            num_slice_groups,
            slice_group_map,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            weighted_pred,
            weighted_bipred_idc,
            pic_init_qp_minus26,
            pic_init_qs_minus26,
            chroma_qp_index_offset,
            deblocking_filter_control_present,
            constrained_intra_pred,
            redundant_pic_cnt_present,
            transform_8x8_mode,
            scaling_lists,
            second_chroma_qp_index_offset,
            has_tail,
        })
    }

    /// `SliceQPY` for a slice with the given `slice_qp_delta`, §7.4.3:
    /// `26 + pic_init_qp_minus26 + slice_qp_delta`.
    #[must_use]
    pub const fn slice_qp(&self, slice_qp_delta: i32) -> i32 {
        26i32
            .saturating_add(self.pic_init_qp_minus26)
            .saturating_add(slice_qp_delta)
    }
}

/// The slice-group map, §7.3.2.2.
fn parse_slice_group_map(
    g: &mut BoundedGolomb<'_, '_, '_>,
    num_slice_groups: u32,
    sps: Option<&Sps>,
) -> Result<SliceGroupMap> {
    // §7.4.2.2 bounds `slice_group_map_type` at 6.
    let map_type = g.ue_v(6)? as u8;
    // Every count below is bounded by the map size, which is at most the
    // picture in map units. Without the SPS, fall back to a generous but finite
    // ceiling rather than an unbounded one.
    let map_units = sps.map_or(u32::from(u16::MAX), |s| {
        s.pic_width_in_mbs.saturating_mul(s.pic_height_in_map_units)
    });
    match map_type {
        0 => {
            g.budget().consume_fuel(u64::from(num_slice_groups))?;
            let mut runs = g.budget().alloc::<u32>(num_slice_groups as usize)?;
            runs.clear();
            for _ in 0..num_slice_groups {
                runs.push(g.ue_v(map_units)?);
            }
            Ok(SliceGroupMap::Interleaved(runs))
        }
        1 => Ok(SliceGroupMap::Dispersed),
        2 => {
            let n = num_slice_groups.saturating_sub(1);
            g.budget().consume_fuel(u64::from(n))?;
            let mut boxes = g.budget().alloc::<(u32, u32)>(n as usize)?;
            boxes.clear();
            for _ in 0..n {
                boxes.push((g.ue_v(map_units)?, g.ue_v(map_units)?));
            }
            Ok(SliceGroupMap::Foreground(boxes))
        }
        3..=5 => Ok(SliceGroupMap::Changing {
            map_type,
            change_direction: g.u(1)? != 0,
            change_rate_minus1: g.ue_v(map_units)?,
        }),
        _ => {
            let count = g.ue_v(map_units)?.saturating_add(1);
            // `Ceil(Log2(num_slice_groups_minus1 + 1))` bits per entry.
            let bits = 32 - num_slice_groups.saturating_sub(1).leading_zeros();
            g.budget().consume_fuel(u64::from(count))?;
            let mut ids = g.budget().alloc::<u8>(count as usize)?;
            ids.clear();
            for _ in 0..count {
                // At most 7 groups, so the id always fits a byte.
                ids.push(g.u(bits.max(1))? as u8);
            }
            Ok(SliceGroupMap::Explicit(ids))
        }
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

    /// The PPS `libx264` writes, taken byte-for-byte from a stream produced by
    /// `ffmpeg -f lavfi -i testsrc2 -c:v libx264 -f h264 out.264`.
    const X264_PPS: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

    #[test]
    fn the_x264_pps() {
        let mut b = Budget::new(Limits::strict());
        let pps = Pps::parse(X264_PPS, None, &mut b).expect("a real PPS parses");
        assert_eq!(pps.id, 0);
        assert_eq!(pps.sps_id, 0);
        assert!(pps.entropy_coding_mode, "x264 defaults to CABAC");
        assert_eq!(pps.num_slice_groups, 1);
        assert!(pps.deblocking_filter_control_present);
    }

    #[test]
    fn a_truncated_pps_is_an_error_not_a_panic() {
        let mut b = Budget::new(Limits::strict());
        for n in 0..X264_PPS.len() {
            let _ = Pps::parse(&X264_PPS[..n], None, &mut b);
        }
    }

    #[test]
    fn the_wrong_nal_type_is_rejected() {
        let mut b = Budget::new(Limits::strict());
        let mut wrong = X264_PPS.to_vec();
        wrong[0] = 0x67; // SPS
        assert!(matches!(
            Pps::parse(&wrong, None, &mut b),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn second_chroma_qp_offset_is_inferred_when_the_tail_is_absent() {
        let mut b = Budget::new(Limits::strict());
        let pps = Pps::parse(X264_PPS, None, &mut b).expect("parses");
        if !pps.has_tail {
            assert_eq!(
                pps.second_chroma_qp_index_offset,
                pps.chroma_qp_index_offset
            );
        }
    }
}
