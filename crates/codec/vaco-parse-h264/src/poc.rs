//! Picture order count, ITU-T H.264 §8.2.1.
//!
//! # Why a parser computes this
//!
//! POC is the picture's position in *output* order, and a coded stream is in
//! *decode* order. Without it nothing downstream can say which picture comes
//! first on screen, which is a container-level and probe-level fact rather than
//! a pixel one — it is what a demuxer needs to synthesise presentation
//! timestamps for an elementary stream that has none.
//!
//! §8.2.1 sits in clause 8, the decoding process, and this is the only part of
//! clause 8 the crate implements. It reconstructs no samples, needs no
//! reference pictures and touches no macroblock: it is integer arithmetic over
//! slice-header fields. The line this crate does not cross is reconstruction
//! (D5, plan 15 §6.2), and this is well on the near side of it.
//!
//! # The three types
//!
//! | `pic_order_cnt_type` | mechanism | clause |
//! |---|---|---|
//! | 0 | an explicit LSB per slice, with the MSB tracked across pictures | §8.2.1.1 |
//! | 1 | a repeating cycle of offsets, indexed by `frame_num` | §8.2.1.2 |
//! | 2 | POC derived from `frame_num` alone; output order equals decode order | §8.2.1.3 |
//!
//! All three are stateful across pictures, which is why this is a struct and
//! not a function.

use crate::slice::SliceHeader;
use crate::sps::Sps;

/// The picture order counts of one picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PictureOrderCount {
    /// `TopFieldOrderCnt`, absent for a bottom field picture.
    pub top: Option<i32>,
    /// `BottomFieldOrderCnt`, absent for a top field picture.
    pub bottom: Option<i32>,
}

impl PictureOrderCount {
    /// `PicOrderCnt(picX)`, §8.2.1: the minimum of the two for a frame, and the
    /// single one that exists for a field.
    #[must_use]
    pub fn value(&self) -> i32 {
        match (self.top, self.bottom) {
            (Some(t), Some(b)) => t.min(b),
            (Some(t), None) => t,
            (None, Some(b)) => b,
            (None, None) => 0,
        }
    }
}

/// The state §8.2.1 carries from one picture to the next.
///
/// Reset on an IDR, on a seek ([`PocState::reset`]) and by
/// `memory_management_control_operation` 5.
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    clippy::struct_field_names,
    reason = "the specification names every one of these prevSomething; renaming them would break the correspondence with §8.2.1"
)]
pub struct PocState {
    /// `prevPicOrderCntMsb`, POC type 0.
    prev_poc_msb: i32,
    /// `prevPicOrderCntLsb`, POC type 0.
    prev_poc_lsb: i32,
    /// `prevFrameNum`, POC types 1 and 2.
    prev_frame_num: u32,
    /// `prevFrameNumOffset`, POC types 1 and 2.
    prev_frame_num_offset: i64,
    /// Whether the previous picture carried an `MMCO 5`.
    prev_had_mmco5: bool,
    /// Whether the previous picture was a bottom field.
    prev_was_bottom_field: bool,
    /// `TopFieldOrderCnt` of the previous picture, needed by the `MMCO 5` rule.
    prev_top_field_order_cnt: i32,
}

impl PocState {
    /// Fresh state, as at the start of a stream.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_poc_msb: 0,
            prev_poc_lsb: 0,
            prev_frame_num: 0,
            prev_frame_num_offset: 0,
            prev_had_mmco5: false,
            prev_was_bottom_field: false,
            prev_top_field_order_cnt: 0,
        }
    }

    /// Forget everything, after a seek or a discontinuity.
    ///
    /// The first picture after this is treated as if it followed an IDR, which
    /// is what a decoder starting at a random access point does anyway.
    pub const fn reset(&mut self) {
        *self = Self::new();
    }

    /// Compute the POC of the picture this slice header begins, and advance the
    /// state.
    ///
    /// Call once per *picture*, on its first slice — every slice of a picture
    /// carries the same POC fields, so calling per slice would advance the
    /// state too often and is what makes POC drift on multi-slice streams.
    ///
    /// # Arithmetic
    ///
    /// Every addition is wrapping. §8.2.1 is written over unbounded integers,
    /// and a conforming stream keeps the values far inside `i32` — but a
    /// malformed one need not, and wrapping is both panic-free and the only
    /// choice that leaves the modular comparisons in §8.2.1.1 meaning what they
    /// mean. Saturating would silently reorder pictures instead.
    pub fn advance(&mut self, sps: &Sps, header: &SliceHeader) -> PictureOrderCount {
        let poc = match sps.pic_order_cnt_type {
            0 => self.advance_type0(sps, header),
            1 => self.advance_type1(sps, header),
            _ => self.advance_type2(sps, header),
        };

        let has_mmco5 = header
            .ref_pic_marking
            .as_ref()
            .is_some_and(crate::slice::RefPicMarking::has_mmco5);
        self.prev_had_mmco5 = has_mmco5;
        self.prev_was_bottom_field = header.bottom_field.unwrap_or(false);
        self.prev_top_field_order_cnt = poc.top.unwrap_or(0);
        if !header.field_pic || !self.prev_was_bottom_field {
            // `prevFrameNum` tracks frames, so a field pair updates it once.
            self.prev_frame_num = header.frame_num;
        }
        poc
    }

    /// §8.2.1.1.
    fn advance_type0(&mut self, sps: &Sps, header: &SliceHeader) -> PictureOrderCount {
        let max_lsb = sps.max_pic_order_cnt_lsb().cast_signed();
        let lsb = header.pic_order_cnt_lsb.unwrap_or(0).cast_signed();

        let (prev_msb, prev_lsb) = if header.is_idr() {
            (0, 0)
        } else if self.prev_had_mmco5 {
            // §8.2.1.1: after an MMCO 5 the previous picture's POC is taken as
            // 0 for a bottom field and as its own TopFieldOrderCnt otherwise.
            if self.prev_was_bottom_field {
                (0, 0)
            } else {
                (0, self.prev_top_field_order_cnt)
            }
        } else {
            (self.prev_poc_msb, self.prev_poc_lsb)
        };

        let half = max_lsb >> 1;
        let msb = if lsb < prev_lsb && prev_lsb.wrapping_sub(lsb) >= half {
            prev_msb.wrapping_add(max_lsb)
        } else if lsb > prev_lsb && lsb.wrapping_sub(prev_lsb) > half {
            prev_msb.wrapping_sub(max_lsb)
        } else {
            prev_msb
        };

        let bottom_field = header.bottom_field.unwrap_or(false);
        let mut poc = PictureOrderCount::default();
        if !bottom_field {
            poc.top = Some(msb.wrapping_add(lsb));
        }
        if !header.field_pic {
            poc.bottom = Some(
                poc.top
                    .unwrap_or(0)
                    .wrapping_add(header.delta_pic_order_cnt_bottom.unwrap_or(0)),
            );
        } else if bottom_field {
            poc.bottom = Some(msb.wrapping_add(lsb));
        }

        // §8.2.1.1: only a *reference* picture updates the previous values.
        if header.is_reference() {
            self.prev_poc_msb = msb;
            self.prev_poc_lsb = lsb;
        }
        poc
    }

    /// §8.2.1.2.
    fn advance_type1(&mut self, sps: &Sps, header: &SliceHeader) -> PictureOrderCount {
        let max_frame_num = i64::from(sps.max_frame_num());
        let frame_num_offset = self.frame_num_offset(header, max_frame_num);
        self.prev_frame_num_offset = frame_num_offset;

        let Some(p1) = sps.poc_type1.as_ref() else {
            // POC type 1 without its parameters is malformed; every picture
            // gets POC 0 rather than an arbitrary number.
            return PictureOrderCount {
                top: Some(0),
                bottom: Some(0),
            };
        };
        // At most 255 entries: `num_ref_frames_in_pic_order_cnt_cycle` is
        // bounded there at the read site, so this cannot wrap.
        let cycle_len = i64::try_from(p1.offset_for_ref_frame.len()).unwrap_or(0);

        let mut abs_frame_num = if cycle_len != 0 {
            frame_num_offset.wrapping_add(i64::from(header.frame_num))
        } else {
            0
        };
        if !header.is_reference() && abs_frame_num > 0 {
            abs_frame_num -= 1;
        }

        let mut expected = 0i32;
        if abs_frame_num > 0 && cycle_len > 0 {
            let cycle_cnt = (abs_frame_num - 1).checked_div(cycle_len).unwrap_or(0);
            let index = (abs_frame_num - 1).checked_rem(cycle_len).unwrap_or(0);
            expected = (cycle_cnt as i32).wrapping_mul(p1.expected_delta_per_cycle());
            for offset in p1
                .offset_for_ref_frame
                .iter()
                .take((index as usize).saturating_add(1))
            {
                expected = expected.wrapping_add(*offset);
            }
        }
        if !header.is_reference() {
            expected = expected.wrapping_add(p1.offset_for_non_ref_pic);
        }

        let d0 = header.delta_pic_order_cnt[0].unwrap_or(0);
        let d1 = header.delta_pic_order_cnt[1].unwrap_or(0);
        let bottom_field = header.bottom_field.unwrap_or(false);
        let mut poc = PictureOrderCount::default();
        if !header.field_pic {
            let top = expected.wrapping_add(d0);
            poc.top = Some(top);
            poc.bottom = Some(
                top.wrapping_add(p1.offset_for_top_to_bottom_field)
                    .wrapping_add(d1),
            );
        } else if bottom_field {
            poc.bottom = Some(
                expected
                    .wrapping_add(p1.offset_for_top_to_bottom_field)
                    .wrapping_add(d0),
            );
        } else {
            poc.top = Some(expected.wrapping_add(d0));
        }
        poc
    }

    /// §8.2.1.3.
    fn advance_type2(&mut self, sps: &Sps, header: &SliceHeader) -> PictureOrderCount {
        let max_frame_num = i64::from(sps.max_frame_num());
        let frame_num_offset = self.frame_num_offset(header, max_frame_num);
        self.prev_frame_num_offset = frame_num_offset;

        let temp = if header.is_idr() {
            0i64
        } else {
            let base = frame_num_offset
                .wrapping_add(i64::from(header.frame_num))
                .wrapping_mul(2);
            if header.is_reference() {
                base
            } else {
                base - 1
            }
        };
        let temp = temp as i32;

        let bottom_field = header.bottom_field.unwrap_or(false);
        let mut poc = PictureOrderCount::default();
        if !header.field_pic {
            poc.top = Some(temp);
            poc.bottom = Some(temp);
        } else if bottom_field {
            poc.bottom = Some(temp);
        } else {
            poc.top = Some(temp);
        }
        poc
    }

    /// `FrameNumOffset`, shared by §8.2.1.2 and §8.2.1.3.
    fn frame_num_offset(&self, header: &SliceHeader, max_frame_num: i64) -> i64 {
        if header.is_idr() {
            return 0;
        }
        // §8.2.1.2: after an MMCO 5 the previous offset is taken as 0.
        let (prev_offset, prev_frame_num) = if self.prev_had_mmco5 {
            (0, 0)
        } else {
            (self.prev_frame_num_offset, self.prev_frame_num)
        };
        if prev_frame_num > header.frame_num {
            prev_offset.wrapping_add(max_frame_num)
        } else {
            prev_offset
        }
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
    use crate::nal::{H264NalHeader, NalUnitType};
    use crate::slice::SliceKind;

    fn sps(poc_type: u8, log2_lsb: u8) -> Sps {
        Sps {
            profile_idc: 100,
            constraint_flags: crate::profile::ConstraintFlags::from_bits(0),
            level_idc: 30,
            id: 0,
            chroma_format: crate::sps::ChromaFormat::Yuv420,
            separate_colour_plane: false,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            qpprime_y_zero_transform_bypass: false,
            scaling_lists: None,
            log2_max_frame_num: 8,
            pic_order_cnt_type: poc_type,
            log2_max_pic_order_cnt_lsb: log2_lsb,
            poc_type1: None,
            max_num_ref_frames: 4,
            gaps_in_frame_num_value_allowed: false,
            pic_width_in_mbs: 40,
            pic_height_in_map_units: 23,
            frame_mbs_only: true,
            mb_adaptive_frame_field: false,
            direct_8x8_inference: true,
            crop: None,
            vui: None,
        }
    }

    fn header(frame_num: u32, lsb: u32, idr: bool, reference: bool) -> SliceHeader {
        SliceHeader {
            nal: H264NalHeader {
                forbidden_zero_bit: false,
                nal_ref_idc: u8::from(reference),
                nal_unit_type: if idr {
                    NalUnitType::IdrSlice
                } else {
                    NalUnitType::NonIdrSlice
                },
            },
            first_mb_in_slice: 0,
            slice_type: 2,
            kind: SliceKind::I,
            pps_id: 0,
            colour_plane_id: None,
            frame_num,
            field_pic: false,
            bottom_field: None,
            idr_pic_id: idr.then_some(0),
            pic_order_cnt_lsb: Some(lsb),
            delta_pic_order_cnt_bottom: None,
            delta_pic_order_cnt: [None, None],
            redundant_pic_cnt: None,
            direct_spatial_mv_pred: None,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list_modification_l0: Vec::new(),
            ref_pic_list_modification_l1: Vec::new(),
            pred_weight_table: None,
            ref_pic_marking: None,
            cabac_init_idc: None,
            slice_qp_delta: 0,
            sp_for_switch: None,
            slice_qs_delta: None,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            slice_group_change_cycle: None,
        }
    }

    #[test]
    fn type0_counts_up_and_wraps_the_msb() {
        // MaxPicOrderCntLsb = 16, so the lsb wraps every eight frames.
        let s = sps(0, 4);
        let mut state = PocState::new();
        let mut seen = Vec::new();
        // An IDR then a run of pictures whose lsb wraps twice.
        seen.push(state.advance(&s, &header(0, 0, true, true)).value());
        for i in 1..24u32 {
            let lsb = (i * 2) % 16;
            seen.push(state.advance(&s, &header(i, lsb, false, true)).value());
        }
        // POC must be monotonically increasing across the wrap; if the MSB
        // logic were wrong it would sawtooth back to 0 every eight pictures.
        for w in seen.windows(2) {
            assert!(w[1] > w[0], "POC went backwards: {seen:?}");
        }
        assert_eq!(seen[0], 0);
        assert_eq!(seen[1], 2);
        assert_eq!(seen[8], 16, "the first wrap must land on the next msb");
    }

    #[test]
    fn type0_resets_on_every_idr() {
        let s = sps(0, 4);
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(0, 0, true, true));
        let _ = state.advance(&s, &header(1, 4, false, true));
        let after = state.advance(&s, &header(0, 0, true, true));
        assert_eq!(after.value(), 0, "an IDR restarts the count");
    }

    #[test]
    fn type2_follows_frame_num() {
        let s = sps(2, 4);
        let mut state = PocState::new();
        assert_eq!(state.advance(&s, &header(0, 0, true, true)).value(), 0);
        assert_eq!(state.advance(&s, &header(1, 0, false, true)).value(), 2);
        assert_eq!(state.advance(&s, &header(2, 0, false, true)).value(), 4);
        // A non-reference picture sits one below its reference neighbour.
        assert_eq!(state.advance(&s, &header(3, 0, false, false)).value(), 5);
    }

    #[test]
    fn type2_handles_a_frame_num_wrap() {
        let mut s = sps(2, 4);
        s.log2_max_frame_num = 4; // MaxFrameNum = 16
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(0, 0, true, true));
        let mut last = 0;
        for i in 1..40u32 {
            let v = state.advance(&s, &header(i % 16, 0, false, true)).value();
            assert!(v > last, "POC went backwards at frame_num {}", i % 16);
            last = v;
        }
    }

    #[test]
    fn type1_with_no_parameters_is_zero_rather_than_arbitrary() {
        let s = sps(1, 4);
        let mut state = PocState::new();
        assert_eq!(state.advance(&s, &header(0, 0, true, true)).value(), 0);
        assert_eq!(state.advance(&s, &header(1, 0, false, true)).value(), 0);
    }

    #[test]
    fn type1_walks_the_offset_cycle() {
        let mut s = sps(1, 4);
        s.poc_type1 = Some(crate::sps::PocType1 {
            delta_pic_order_always_zero: true,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offset_for_ref_frame: vec![2],
        });
        let mut state = PocState::new();
        // A cycle of one offset of 2 gives the same 0, 2, 4, ... as type 2.
        assert_eq!(state.advance(&s, &header(0, 0, true, true)).value(), 0);
        assert_eq!(state.advance(&s, &header(1, 0, false, true)).value(), 2);
        assert_eq!(state.advance(&s, &header(2, 0, false, true)).value(), 4);
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let mut s = sps(1, 16);
        s.log2_max_frame_num = 16;
        s.poc_type1 = Some(crate::sps::PocType1 {
            delta_pic_order_always_zero: false,
            offset_for_non_ref_pic: i32::MIN + 1,
            offset_for_top_to_bottom_field: i32::MAX,
            offset_for_ref_frame: vec![i32::MAX; 255],
        });
        let mut state = PocState::new();
        for i in 0..64u32 {
            let mut h = header(
                i.wrapping_mul(1031) % 65_536,
                u32::MAX % 65_536,
                false,
                i % 3 == 0,
            );
            h.delta_pic_order_cnt = [Some(i32::MIN + 1), Some(i32::MAX)];
            let _ = state.advance(&s, &h);
        }
        let mut s2 = sps(0, 16);
        s2.log2_max_frame_num = 16;
        let mut state = PocState::new();
        for i in 0..64u32 {
            let _ = state.advance(&s2, &header(i, u32::MAX % 65_536, false, true));
        }
    }
}
