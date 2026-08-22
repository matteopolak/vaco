//! The slice header, ITU-T H.264 §7.3.3, and the three sub-structures it
//! contains: §7.3.3.1 `ref_pic_list_modification()`, §7.3.3.2
//! `pred_weight_table()` and §7.3.3.3 `dec_ref_pic_marking()`.
//!
//! **The header only.** `slice_data()` — §7.3.4 and everything below it — is
//! the decoding process and is deliberately absent: this crate parses, and
//! parsing an H.264 slice header implements no decoder (D5, plan 15 §6.2).

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::{H264NalHeader, NalUnitType};
use crate::pps::Pps;
use crate::sps::{ChromaFormat, Sps};
use crate::util::MAX_SYNTAX_COMMANDS;

/// `slice_type`, Table 7-6.
///
/// The values 5..=9 mean the same five types with the additional guarantee that
/// **all** slices of the picture have that type. That guarantee is worth
/// keeping, so the raw value is retained alongside the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    /// P — predictive.
    P,
    /// B — bi-predictive.
    B,
    /// I — intra.
    I,
    /// SP — switching predictive.
    Sp,
    /// SI — switching intra.
    Si,
}

impl SliceKind {
    /// From a raw `slice_type`, 0..=9.
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        Some(match v % 5 {
            0 if v < 10 => Self::P,
            1 if v < 10 => Self::B,
            2 if v < 10 => Self::I,
            3 if v < 10 => Self::Sp,
            4 if v < 10 => Self::Si,
            _ => return None,
        })
    }

    /// Whether the type predicts from list 0 — P, SP and B.
    #[must_use]
    pub const fn uses_list0(self) -> bool {
        matches!(self, Self::P | Self::Sp | Self::B)
    }

    /// Whether the type predicts from list 1 — B only.
    #[must_use]
    pub const fn uses_list1(self) -> bool {
        matches!(self, Self::B)
    }

    /// The single-letter name `ffprobe` prints for a picture type.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::P => 'P',
            Self::B => 'B',
            Self::I => 'I',
            // The reference prints one letter for both switching types.
            Self::Sp | Self::Si => 'S',
        }
    }
}

/// One `ref_pic_list_modification` command, §7.3.3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefPicListModification {
    /// `modification_of_pic_nums_idc`, 0..=2 (3 terminates and is not stored).
    pub idc: u8,
    /// `abs_diff_pic_num_minus1` for idc 0 and 1, `long_term_pic_num` for
    /// idc 2.
    pub value: u32,
}

/// One `memory_management_control_operation` command, §7.3.3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmcoCommand {
    /// `memory_management_control_operation`, 1..=6 (0 terminates).
    pub op: u8,
    /// `difference_of_pic_nums_minus1` (ops 1, 3) or `long_term_pic_num`
    /// (op 2) or `max_long_term_frame_idx_plus1` (op 4).
    pub arg0: u32,
    /// `long_term_frame_idx` (ops 3, 6).
    pub arg1: u32,
}

/// `dec_ref_pic_marking()`, §7.3.3.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefPicMarking {
    /// The IDR form.
    Idr {
        /// `no_output_of_prior_pics_flag`.
        no_output_of_prior_pics: bool,
        /// `long_term_reference_flag`.
        long_term_reference: bool,
    },
    /// `adaptive_ref_pic_marking_mode_flag == 0`: sliding window.
    SlidingWindow,
    /// `adaptive_ref_pic_marking_mode_flag == 1`: explicit commands.
    Adaptive(Vec<MmcoCommand>),
}

impl RefPicMarking {
    /// Whether any command is `memory_management_control_operation == 5`,
    /// which §8.2.1 makes reset the picture order count.
    #[must_use]
    pub fn has_mmco5(&self) -> bool {
        match self {
            Self::Adaptive(cmds) => cmds.iter().any(|c| c.op == 5),
            _ => false,
        }
    }
}

/// `pred_weight_table()`, §7.3.3.2.
///
/// The weights themselves are prediction parameters and only a decoder uses
/// them; they are parsed because the bits must be consumed, and kept because
/// throwing away syntax a caller might want is not this layer's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom`, 0..=7.
    pub luma_log2_weight_denom: u8,
    /// `chroma_log2_weight_denom`, 0..=7. Absent when `ChromaArrayType == 0`.
    pub chroma_log2_weight_denom: Option<u8>,
    /// List 0's per-reference weights.
    pub l0: Vec<RefWeight>,
    /// List 1's, for B slices.
    pub l1: Vec<RefWeight>,
}

/// One reference picture's weights inside a [`PredWeightTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RefWeight {
    /// `luma_weight_lX` and `luma_offset_lX`, when the flag was set.
    pub luma: Option<(i32, i32)>,
    /// `chroma_weight_lX[j]` and `chroma_offset_lX[j]` for Cb and Cr.
    pub chroma: Option<[(i32, i32); 2]>,
}

/// A slice header: ITU-T H.264 §7.3.3, in field order.
///
/// Every field the syntax codes conditionally is an `Option`, so "absent" and
/// "present and zero" stay distinguishable — which §7.4.1.2.4's access-unit
/// boundary rule depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceHeader {
    /// The NAL header the slice arrived in.
    pub nal: H264NalHeader,
    /// `first_mb_in_slice`.
    pub first_mb_in_slice: u32,
    /// `slice_type`, raw, 0..=9.
    pub slice_type: u32,
    /// `slice_type` reduced to one of the five kinds.
    pub kind: SliceKind,
    /// `pic_parameter_set_id`.
    pub pps_id: u8,
    /// `colour_plane_id`, present only with `separate_colour_plane_flag`.
    pub colour_plane_id: Option<u8>,
    /// `frame_num`.
    pub frame_num: u32,
    /// `field_pic_flag`; false when `frame_mbs_only_flag` made it absent.
    pub field_pic: bool,
    /// `bottom_field_flag`; `None` unless `field_pic_flag` is set.
    pub bottom_field: Option<bool>,
    /// `idr_pic_id`, IDR pictures only.
    pub idr_pic_id: Option<u32>,
    /// `pic_order_cnt_lsb`, POC type 0 only.
    pub pic_order_cnt_lsb: Option<u32>,
    /// `delta_pic_order_cnt_bottom`, POC type 0 with the PPS flag set.
    pub delta_pic_order_cnt_bottom: Option<i32>,
    /// `delta_pic_order_cnt[0]` and `[1]`, POC type 1 only.
    pub delta_pic_order_cnt: [Option<i32>; 2],
    /// `redundant_pic_cnt`.
    pub redundant_pic_cnt: Option<u32>,
    /// `direct_spatial_mv_pred_flag`, B slices only.
    pub direct_spatial_mv_pred: Option<bool>,
    /// `num_ref_idx_l0_active_minus1`, after the override.
    pub num_ref_idx_l0_active_minus1: u32,
    /// `num_ref_idx_l1_active_minus1`, after the override.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `ref_pic_list_modification()` for list 0.
    pub ref_pic_list_modification_l0: Vec<RefPicListModification>,
    /// …and for list 1.
    pub ref_pic_list_modification_l1: Vec<RefPicListModification>,
    /// `pred_weight_table()`.
    pub pred_weight_table: Option<PredWeightTable>,
    /// `dec_ref_pic_marking()`, present when `nal_ref_idc != 0`.
    pub ref_pic_marking: Option<RefPicMarking>,
    /// `cabac_init_idc`.
    pub cabac_init_idc: Option<u32>,
    /// `slice_qp_delta`.
    pub slice_qp_delta: i32,
    /// `sp_for_switch_flag`, SP slices only.
    pub sp_for_switch: Option<bool>,
    /// `slice_qs_delta`, SP and SI slices only.
    pub slice_qs_delta: Option<i32>,
    /// `disable_deblocking_filter_idc`.
    pub disable_deblocking_filter_idc: u32,
    /// `slice_alpha_c0_offset_div2`.
    pub slice_alpha_c0_offset_div2: i32,
    /// `slice_beta_offset_div2`.
    pub slice_beta_offset_div2: i32,
    /// `slice_group_change_cycle`.
    pub slice_group_change_cycle: Option<u32>,
}

impl SliceHeader {
    /// `MbaffFrameFlag`, §7.4.3.
    #[must_use]
    pub const fn mbaff(&self, sps: &Sps) -> bool {
        sps.mb_adaptive_frame_field && !self.field_pic
    }

    /// `IdrPicFlag`, §7.4.1.
    #[must_use]
    pub const fn is_idr(&self) -> bool {
        self.nal.is_idr()
    }

    /// Whether this slice belongs to a picture used for reference.
    #[must_use]
    pub const fn is_reference(&self) -> bool {
        self.nal.is_reference()
    }

    /// Whether this header begins a new primary coded picture, given the
    /// previous one — ITU-T H.264 §7.4.1.2.4.
    ///
    /// The clause lists seven conditions, any one of which makes this the first
    /// VCL NAL unit of a new picture. It is the whole basis of access-unit
    /// splitting in an Annex B stream, where nothing else marks a picture
    /// boundary, and every one of the seven matters: the `nal_ref_idc`
    /// condition, for instance, is what separates a reference picture from a
    /// non-reference one that happens to share a `frame_num`.
    ///
    /// The eighth condition — the one about `MMCO 5` — is deliberately absent
    /// here because it is stated in terms of derived POC values rather than
    /// syntax elements; [`PocState`](crate::PocState) applies it.
    #[must_use]
    pub fn starts_new_picture(&self, prev: &Self, sps: &Sps) -> bool {
        // 1. frame_num differs.
        self.frame_num != prev.frame_num
            // 2. pic_parameter_set_id differs.
            || self.pps_id != prev.pps_id
            // 3. field_pic_flag differs.
            || self.field_pic != prev.field_pic
            // 4. both have field_pic_flag set and bottom_field_flag differs.
            || (self.field_pic && prev.field_pic && self.bottom_field != prev.bottom_field)
            // 5. exactly one of them has nal_ref_idc == 0.
            || (self.nal.nal_ref_idc == 0) != (prev.nal.nal_ref_idc == 0)
            // 6. POC type 0 and either pic_order_cnt_lsb or
            //    delta_pic_order_cnt_bottom differs.
            || (sps.pic_order_cnt_type == 0
                && (self.pic_order_cnt_lsb != prev.pic_order_cnt_lsb
                    || self.delta_pic_order_cnt_bottom != prev.delta_pic_order_cnt_bottom))
            // 7. POC type 1 and either delta_pic_order_cnt entry differs.
            || (sps.pic_order_cnt_type == 1
                && self.delta_pic_order_cnt != prev.delta_pic_order_cnt)
            // 8. IdrPicFlag differs, or both are IDR and idr_pic_id differs.
            || self.is_idr() != prev.is_idr()
            || (self.is_idr() && self.idr_pic_id != prev.idr_pic_id)
    }

    /// Parse a slice header from a NAL unit's RBSP.
    ///
    /// `sps` and `pps` must be the parameter sets the slice refers to. There is
    /// no way around that: `frame_num`'s *width in bits* comes from the SPS,
    /// and whether `delta_pic_order_cnt_bottom` is present at all comes from
    /// the PPS. A slice whose parameter sets have not been seen cannot be
    /// parsed, only skipped.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a syntax element outside its permitted range
    /// or a NAL type that carries no slice header, [`Error::UnexpectedEof`] for
    /// a truncated unit, [`Error::LimitExceeded`] on a budget cap.
    pub fn parse(rbsp: &[u8], sps: &Sps, pps: &Pps, budget: &mut Budget) -> Result<Self> {
        let nal = H264NalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if !nal.nal_unit_type.has_slice_header() {
            return Err(Error::InvalidData("NAL unit carries no slice header"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(8);
        if matches!(
            nal.nal_unit_type,
            NalUnitType::SliceExtension | NalUnitType::SliceExtensionDepth
        ) {
            // Annex G/H put a three-byte extension header before the slice
            // header, and its contents change how the rest is read. Not
            // implemented; saying so beats mis-parsing.
            return Err(Error::Unsupported(
                "MVC and 3D-AVC slice extensions are not parsed",
            ));
        }
        let header = Self::parse_data(&mut reader, nal, sps, pps, budget)?;
        reader.check()?;
        Ok(header)
    }

    /// `slice_header()`, §7.3.3, from a reader positioned after the NAL header.
    ///
    /// # Errors
    ///
    /// As [`SliceHeader::parse`].
    #[allow(clippy::too_many_lines, reason = "one specification syntax table")]
    pub fn parse_data(
        reader: &mut BitReader<'_>,
        nal: H264NalHeader,
        sps: &Sps,
        pps: &Pps,
        budget: &mut Budget,
    ) -> Result<Self> {
        let mut g = BoundedGolomb::new(reader, budget);

        let pic_size_in_mbs = sps.frame_size_in_mbs().min(u64::from(u32::MAX)) as u32;
        let first_mb_in_slice = g.ue_v(pic_size_in_mbs.saturating_sub(1).max(1))?;
        let slice_type = g.ue_v(9)?;
        let kind =
            SliceKind::from_u32(slice_type).ok_or(Error::InvalidData("slice_type out of range"))?;
        let pps_id = g.ue_v(255)? as u8;
        let colour_plane_id = if sps.separate_colour_plane {
            Some(g.u(2)? as u8)
        } else {
            None
        };
        let frame_num = g.u(u32::from(sps.log2_max_frame_num))?;

        let mut field_pic = false;
        let mut bottom_field = None;
        if !sps.frame_mbs_only {
            field_pic = g.u(1)? != 0;
            if field_pic {
                bottom_field = Some(g.u(1)? != 0);
            }
        }

        let idr_pic_id = if nal.is_idr() {
            // §7.4.3 bounds it at 65535.
            Some(g.ue_v(65_535)?)
        } else {
            None
        };

        let mut pic_order_cnt_lsb = None;
        let mut delta_pic_order_cnt_bottom = None;
        let mut delta_pic_order_cnt = [None, None];
        match sps.pic_order_cnt_type {
            0 => {
                pic_order_cnt_lsb = Some(g.u(u32::from(sps.log2_max_pic_order_cnt_lsb))?);
                if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                    delta_pic_order_cnt_bottom = Some(g.se_v(i32::MIN + 1, i32::MAX)?);
                }
            }
            1 => {
                let always_zero = sps
                    .poc_type1
                    .as_ref()
                    .is_some_and(|p| p.delta_pic_order_always_zero);
                if !always_zero {
                    delta_pic_order_cnt[0] = Some(g.se_v(i32::MIN + 1, i32::MAX)?);
                    if pps.bottom_field_pic_order_in_frame_present && !field_pic {
                        delta_pic_order_cnt[1] = Some(g.se_v(i32::MIN + 1, i32::MAX)?);
                    }
                }
            }
            _ => {}
        }

        let redundant_pic_cnt = if pps.redundant_pic_cnt_present {
            // §7.4.3 bounds it at 127.
            Some(g.ue_v(127)?)
        } else {
            None
        };

        let direct_spatial_mv_pred = if kind == SliceKind::B {
            Some(g.u(1)? != 0)
        } else {
            None
        };

        let mut num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
        let mut num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
        if kind.uses_list0() && g.u(1)? != 0 {
            // §7.4.3: at most 31 for a frame, 31 for a field too — the doubling
            // applies to the derived list length, not to this field.
            num_ref_idx_l0_active_minus1 = g.ue_v(31)?;
            if kind == SliceKind::B {
                num_ref_idx_l1_active_minus1 = g.ue_v(31)?;
            }
        }

        let ref_pic_list_modification_l0 = if kind != SliceKind::I && kind != SliceKind::Si {
            read_ref_pic_list_modification(&mut g)?
        } else {
            Vec::new()
        };
        let ref_pic_list_modification_l1 = if kind == SliceKind::B {
            read_ref_pic_list_modification(&mut g)?
        } else {
            Vec::new()
        };

        let weighted = (pps.weighted_pred && matches!(kind, SliceKind::P | SliceKind::Sp))
            || (pps.weighted_bipred_idc == 1 && kind == SliceKind::B);
        let pred_weight_table = if weighted {
            Some(read_pred_weight_table(
                &mut g,
                sps,
                kind,
                num_ref_idx_l0_active_minus1,
                num_ref_idx_l1_active_minus1,
            )?)
        } else {
            None
        };

        let ref_pic_marking = if nal.is_reference() {
            Some(read_dec_ref_pic_marking(&mut g, nal.is_idr())?)
        } else {
            None
        };

        let cabac_init_idc =
            if pps.entropy_coding_mode && kind != SliceKind::I && kind != SliceKind::Si {
                // §7.4.3 bounds it at 2.
                Some(g.ue_v(2)?)
            } else {
                None
            };

        // §7.4.3: SliceQPY must land in -QpBdOffsetY..=51, and QpBdOffsetY is
        // at most 48, so this covers every legal value at every bit depth.
        let slice_qp_delta = g.se_v(-128, 128)?;

        let mut sp_for_switch = None;
        let mut slice_qs_delta = None;
        if matches!(kind, SliceKind::Sp | SliceKind::Si) {
            if kind == SliceKind::Sp {
                sp_for_switch = Some(g.u(1)? != 0);
            }
            slice_qs_delta = Some(g.se_v(-128, 128)?);
        }

        let mut disable_deblocking_filter_idc = 0;
        let mut slice_alpha_c0_offset_div2 = 0;
        let mut slice_beta_offset_div2 = 0;
        if pps.deblocking_filter_control_present {
            // §7.4.3 bounds it at 2.
            disable_deblocking_filter_idc = g.ue_v(2)?;
            if disable_deblocking_filter_idc != 1 {
                slice_alpha_c0_offset_div2 = g.se_v(-6, 6)?;
                slice_beta_offset_div2 = g.se_v(-6, 6)?;
            }
        }

        let slice_group_change_cycle = match &pps.slice_group_map {
            Some(m) if (3..=5).contains(&m.map_type()) => {
                // §7.4.3: the field is
                // `Ceil(Log2(PicSizeInMapUnits / SliceGroupChangeRate + 1))`
                // bits wide. `SliceGroupChangeRate` is at least 1, so the map
                // unit count alone bounds the width.
                let units = sps
                    .pic_width_in_mbs
                    .saturating_mul(sps.pic_height_in_map_units)
                    .saturating_add(1);
                let bits = (32 - units.leading_zeros()).clamp(1, 32);
                Some(g.u(bits)?)
            }
            _ => None,
        };

        Ok(Self {
            nal,
            first_mb_in_slice,
            slice_type,
            kind,
            pps_id,
            colour_plane_id,
            frame_num,
            field_pic,
            bottom_field,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            delta_pic_order_cnt,
            redundant_pic_cnt,
            direct_spatial_mv_pred,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            ref_pic_list_modification_l0,
            ref_pic_list_modification_l1,
            pred_weight_table,
            ref_pic_marking,
            cabac_init_idc,
            slice_qp_delta,
            sp_for_switch,
            slice_qs_delta,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            slice_group_change_cycle,
        })
    }
}

/// `ref_pic_list_modification()` for one list, §7.3.3.1.
///
/// The `do … while (idc != 3)` is the first of two input-driven loops in the
/// slice header. Bounded twice: by [`MAX_SYNTAX_COMMANDS`] and by the fuel
/// every read charges. A stream that never sends the terminator is refused,
/// not looped on.
fn read_ref_pic_list_modification(
    g: &mut BoundedGolomb<'_, '_, '_>,
) -> Result<Vec<RefPicListModification>> {
    let mut out = Vec::new();
    if g.u(1)? == 0 {
        return Ok(out);
    }
    for _ in 0..MAX_SYNTAX_COMMANDS {
        let idc = g.ue_v(3)? as u8;
        if idc == 3 {
            return Ok(out);
        }
        let value = g.ue_v(u32::MAX - 1)?;
        out.push(RefPicListModification { idc, value });
    }
    Err(Error::InvalidData(
        "ref_pic_list_modification did not terminate",
    ))
}

/// `pred_weight_table()`, §7.3.3.2.
fn read_pred_weight_table(
    g: &mut BoundedGolomb<'_, '_, '_>,
    sps: &Sps,
    kind: SliceKind,
    l0_minus1: u32,
    l1_minus1: u32,
) -> Result<PredWeightTable> {
    let has_chroma = sps.chroma_array_type() != ChromaFormat::Monochrome;
    // §7.4.3 bounds both denominators at 7.
    let luma_log2_weight_denom = g.ue_v(7)? as u8;
    let chroma_log2_weight_denom = if has_chroma {
        Some(g.ue_v(7)? as u8)
    } else {
        None
    };
    let l0 = read_weights(g, has_chroma, l0_minus1)?;
    let l1 = if kind == SliceKind::B {
        read_weights(g, has_chroma, l1_minus1)?
    } else {
        Vec::new()
    };
    Ok(PredWeightTable {
        luma_log2_weight_denom,
        chroma_log2_weight_denom,
        l0,
        l1,
    })
}

/// One list's worth of weights. The trip count comes from
/// `num_ref_idx_lX_active_minus1`, which the caller has already bounded at 31.
fn read_weights(
    g: &mut BoundedGolomb<'_, '_, '_>,
    has_chroma: bool,
    active_minus1: u32,
) -> Result<Vec<RefWeight>> {
    let n = active_minus1.saturating_add(1).min(32);
    g.budget().consume_fuel(u64::from(n))?;
    let mut out = g.budget().alloc::<RefWeight>(n as usize)?;
    out.clear();
    for _ in 0..n {
        let luma = if g.u(1)? != 0 {
            // §7.4.3 bounds the weights and offsets at -128..=127.
            Some((g.se_v(-128, 127)?, g.se_v(-128, 127)?))
        } else {
            None
        };
        let chroma = if has_chroma && g.u(1)? != 0 {
            Some([
                (g.se_v(-128, 127)?, g.se_v(-128, 127)?),
                (g.se_v(-128, 127)?, g.se_v(-128, 127)?),
            ])
        } else {
            None
        };
        out.push(RefWeight { luma, chroma });
    }
    Ok(out)
}

/// `dec_ref_pic_marking()`, §7.3.3.3.
///
/// The second input-driven loop, bounded the same way as the first.
fn read_dec_ref_pic_marking(
    g: &mut BoundedGolomb<'_, '_, '_>,
    is_idr: bool,
) -> Result<RefPicMarking> {
    if is_idr {
        return Ok(RefPicMarking::Idr {
            no_output_of_prior_pics: g.u(1)? != 0,
            long_term_reference: g.u(1)? != 0,
        });
    }
    if g.u(1)? == 0 {
        return Ok(RefPicMarking::SlidingWindow);
    }
    let mut cmds = Vec::new();
    for _ in 0..MAX_SYNTAX_COMMANDS {
        // §7.4.3.3 bounds the operation at 6.
        let op = g.ue_v(6)? as u8;
        if op == 0 {
            return Ok(RefPicMarking::Adaptive(cmds));
        }
        let mut cmd = MmcoCommand {
            op,
            arg0: 0,
            arg1: 0,
        };
        if matches!(op, 1 | 3) {
            cmd.arg0 = g.ue_v(u32::MAX - 1)?;
        }
        if op == 2 {
            cmd.arg0 = g.ue_v(u32::MAX - 1)?;
        }
        if matches!(op, 3 | 6) {
            // §7.4.3.3 bounds `long_term_frame_idx` by MaxLongTermFrameIdx,
            // which cannot exceed the DPB's 16 frames.
            cmd.arg1 = g.ue_v(16)?;
        }
        if op == 4 {
            cmd.arg0 = g.ue_v(17)?;
        }
        cmds.push(cmd);
    }
    Err(Error::InvalidData("dec_ref_pic_marking did not terminate"))
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
    fn slice_type_table_7_6() {
        for v in 0..5u32 {
            assert_eq!(SliceKind::from_u32(v), SliceKind::from_u32(v + 5));
        }
        assert_eq!(SliceKind::from_u32(0), Some(SliceKind::P));
        assert_eq!(SliceKind::from_u32(1), Some(SliceKind::B));
        assert_eq!(SliceKind::from_u32(2), Some(SliceKind::I));
        assert_eq!(SliceKind::from_u32(3), Some(SliceKind::Sp));
        assert_eq!(SliceKind::from_u32(4), Some(SliceKind::Si));
        assert_eq!(SliceKind::from_u32(7), Some(SliceKind::I));
        assert_eq!(SliceKind::from_u32(10), None);
        assert_eq!(SliceKind::from_u32(u32::MAX), None);
    }

    #[test]
    fn only_b_uses_list_one() {
        assert!(SliceKind::B.uses_list1());
        for k in [SliceKind::P, SliceKind::I, SliceKind::Sp, SliceKind::Si] {
            assert!(!k.uses_list1(), "{k:?}");
        }
    }
}
