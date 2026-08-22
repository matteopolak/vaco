//! The sequence parameter set, ITU-T H.264 §7.3.2.1.1, and everything the
//! displayed picture geometry is derived from (§7.4.2.1.1).
//!
//! Also Annex E: `vui_parameters()` (§E.1.1) and `hrd_parameters()` (§E.1.2).

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_color::{
    ChromaLocation, ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients,
    TransferCharacteristic,
};
use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;

use crate::nal::{H264NalHeader, NalUnitType};
use crate::profile::{ConstraintFlags, profile_name};

/// `Extended_SAR`, Table E-1: the aspect ratio is given explicitly.
pub const EXTENDED_SAR: u8 = 255;

/// Table E-1, the sixteen predefined sample aspect ratios.
///
/// Index is `aspect_ratio_idc`; entry 0 is "unspecified" and is stored as
/// `(0, 0)` so it is distinguishable from a real ratio. Indices 17..=254 are
/// reserved and are also unspecified.
///
/// Format-dictated: a conforming parser has no freedom here (D7/D15 merger).
const ASPECT_RATIO_TABLE: [(u16, u16); 17] = [
    (0, 0),    // 0: unspecified
    (1, 1),    // 1
    (12, 11),  // 2
    (10, 11),  // 3
    (16, 11),  // 4
    (40, 33),  // 5
    (24, 11),  // 6
    (20, 11),  // 7
    (32, 11),  // 8
    (80, 33),  // 9
    (18, 11),  // 10
    (15, 11),  // 11
    (64, 33),  // 12
    (160, 99), // 13
    (4, 3),    // 14
    (3, 2),    // 15
    (2, 1),    // 16
];

/// The chroma sampling structure, Table 6-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromaFormat {
    /// 0 — monochrome. There are no chroma arrays at all.
    Monochrome,
    /// 1 — 4:2:0.
    #[default]
    Yuv420,
    /// 2 — 4:2:2.
    Yuv422,
    /// 3 — 4:4:4. With `separate_colour_plane_flag` the three planes are coded
    /// as separate monochrome pictures and `ChromaArrayType` becomes 0.
    Yuv444,
}

impl ChromaFormat {
    /// From `chroma_format_idc`.
    #[must_use]
    pub const fn from_idc(idc: u32) -> Option<Self> {
        Some(match idc {
            0 => Self::Monochrome,
            1 => Self::Yuv420,
            2 => Self::Yuv422,
            3 => Self::Yuv444,
            _ => return None,
        })
    }

    /// `chroma_format_idc`.
    #[must_use]
    pub const fn idc(self) -> u32 {
        match self {
            Self::Monochrome => 0,
            Self::Yuv420 => 1,
            Self::Yuv422 => 2,
            Self::Yuv444 => 3,
        }
    }

    /// `SubWidthC`, Table 6-1. Monochrome has none; 1 is returned so the crop
    /// arithmetic has a neutral factor, and the caller uses
    /// [`Sps::crop_unit`] rather than this directly.
    #[must_use]
    pub const fn sub_width_c(self) -> u32 {
        match self {
            Self::Monochrome | Self::Yuv444 => 1,
            Self::Yuv420 | Self::Yuv422 => 2,
        }
    }

    /// `SubHeightC`, Table 6-1.
    #[must_use]
    pub const fn sub_height_c(self) -> u32 {
        match self {
            Self::Yuv420 => 2,
            _ => 1,
        }
    }
}

/// `hrd_parameters()`, §E.1.2.
///
/// Kept whole rather than reduced to a bit rate, because the field widths it
/// declares — `cpb_removal_delay_length_minus1` and friends — are what the
/// `pic_timing` SEI needs in order to be parsable at all. Dropping them would
/// make that SEI undecodable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HrdParameters {
    /// One entry per CPB; at least one and at most 32.
    pub cpb: Vec<CpbEntry>,
    /// `bit_rate_scale`.
    pub bit_rate_scale: u8,
    /// `cpb_size_scale`.
    pub cpb_size_scale: u8,
    /// `initial_cpb_removal_delay_length_minus1`.
    pub initial_cpb_removal_delay_length_minus1: u8,
    /// `cpb_removal_delay_length_minus1`.
    pub cpb_removal_delay_length_minus1: u8,
    /// `dpb_output_delay_length_minus1`.
    pub dpb_output_delay_length_minus1: u8,
    /// `time_offset_length`.
    pub time_offset_length: u8,
}

/// One coded picture buffer's declaration inside [`HrdParameters`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpbEntry {
    /// `bit_rate_value_minus1`.
    pub bit_rate_value_minus1: u32,
    /// `cpb_size_value_minus1`.
    pub cpb_size_value_minus1: u32,
    /// `cbr_flag`.
    pub cbr: bool,
}

impl HrdParameters {
    /// `BitRate[i]` in bits per second, §E.2.2:
    /// `(bit_rate_value_minus1 + 1) * 2^(6 + bit_rate_scale)`.
    ///
    /// Returns `None` on overflow rather than saturating, because a bit rate
    /// that does not fit 64 bits is a malformed declaration and reporting a
    /// clamped one would be worse than reporting none.
    #[must_use]
    pub fn bit_rate(&self, i: usize) -> Option<u64> {
        let e = self.cpb.get(i)?;
        u64::from(e.bit_rate_value_minus1)
            .checked_add(1)?
            .checked_shl(6 + u32::from(self.bit_rate_scale))
    }

    /// `CpbSize[i]` in bits, §E.2.2:
    /// `(cpb_size_value_minus1 + 1) * 2^(4 + cpb_size_scale)`.
    #[must_use]
    pub fn cpb_size(&self, i: usize) -> Option<u64> {
        let e = self.cpb.get(i)?;
        u64::from(e.cpb_size_value_minus1)
            .checked_add(1)?
            .checked_shl(4 + u32::from(self.cpb_size_scale))
    }
}

/// `bitstream_restriction` fields of the VUI, §E.1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitstreamRestriction {
    /// `motion_vectors_over_pic_boundaries_flag`.
    pub motion_vectors_over_pic_boundaries: bool,
    /// `max_bytes_per_pic_denom`.
    pub max_bytes_per_pic_denom: u32,
    /// `max_bits_per_mb_denom`.
    pub max_bits_per_mb_denom: u32,
    /// `log2_max_mv_length_horizontal`.
    pub log2_max_mv_length_horizontal: u32,
    /// `log2_max_mv_length_vertical`.
    pub log2_max_mv_length_vertical: u32,
    /// `max_num_reorder_frames` — the reorder depth, and what `ffprobe` prints
    /// as `has_b_frames`.
    pub max_num_reorder_frames: u32,
    /// `max_dec_frame_buffering`.
    pub max_dec_frame_buffering: u32,
}

/// `vui_parameters()`, §E.1.1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VuiParameters {
    /// `aspect_ratio_idc`, present only if `aspect_ratio_info_present_flag`.
    pub aspect_ratio_idc: Option<u8>,
    /// `sar_width` / `sar_height`, present only for `Extended_SAR`.
    pub sar: Option<(u16, u16)>,
    /// `overscan_appropriate_flag`, present only if
    /// `overscan_info_present_flag`.
    pub overscan_appropriate: Option<bool>,
    /// `video_format`, present only if `video_signal_type_present_flag`.
    pub video_format: Option<u8>,
    /// `video_full_range_flag`. `None` means `video_signal_type_present_flag`
    /// was 0, which is what distinguishes "limited range" from "not stated".
    pub video_full_range: Option<bool>,
    /// `colour_primaries`, `transfer_characteristics`, `matrix_coefficients` —
    /// raw code points, present only if `colour_description_present_flag`.
    pub colour_description: Option<(u8, u8, u8)>,
    /// `chroma_sample_loc_type_top_field` / `_bottom_field`.
    pub chroma_sample_loc: Option<(u32, u32)>,
    /// `num_units_in_tick` and `time_scale`, present only if
    /// `timing_info_present_flag`.
    pub timing: Option<Timing>,
    /// `hrd_parameters()` for the NAL HRD.
    pub nal_hrd: Option<HrdParameters>,
    /// `hrd_parameters()` for the VCL HRD.
    pub vcl_hrd: Option<HrdParameters>,
    /// `low_delay_hrd_flag`, present only if either HRD is.
    pub low_delay_hrd: Option<bool>,
    /// `pic_struct_present_flag` — decides whether the `pic_timing` SEI carries
    /// a `pic_struct`, so a parser cannot read that SEI without it.
    pub pic_struct_present: bool,
    /// The `bitstream_restriction` block.
    pub bitstream_restriction: Option<BitstreamRestriction>,
}

/// `timing_info`, §E.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// `num_units_in_tick`. Required to be greater than 0.
    pub num_units_in_tick: u32,
    /// `time_scale`. Required to be greater than 0.
    pub time_scale: u32,
    /// `fixed_frame_rate_flag`.
    pub fixed_frame_rate: bool,
}

impl VuiParameters {
    /// The sample aspect ratio Table E-1 implies, or [`Rational::UNDEFINED`]
    /// when it is unspecified or reserved.
    #[must_use]
    pub fn sample_aspect_ratio(&self) -> Rational {
        match self.aspect_ratio_idc {
            None => Rational::UNDEFINED,
            Some(EXTENDED_SAR) => match self.sar {
                Some((w, h)) => Rational::new(i32::from(w), i32::from(h)),
                None => Rational::UNDEFINED,
            },
            Some(idc) => match ASPECT_RATIO_TABLE.get(idc as usize) {
                Some(&(0, 0)) | None => Rational::UNDEFINED,
                Some(&(w, h)) => Rational::new(i32::from(w), i32::from(h)),
            },
        }
    }

    /// The colour description, mapped onto `vaco-color`'s enums.
    ///
    /// Absent fields become `Unspecified`, which is exactly what the
    /// specification's inference rules say (§E.2.1: an absent
    /// `colour_description` infers code point 2 for all three).
    ///
    /// `chroma_location` is the one place the reference and the specification
    /// part company; see [`Sps::color_info`], which is where the difference is
    /// resolved, because it depends on whether the VUI exists at all.
    #[must_use]
    pub fn color_info(&self) -> ColorInfo {
        let (p, t, m) = self.colour_description.unwrap_or((2, 2, 2));
        ColorInfo {
            primaries: ColorPrimaries::from_u8(p).unwrap_or_default(),
            transfer: TransferCharacteristic::from_u8(t).unwrap_or_default(),
            matrix: MatrixCoefficients::from_u8(m).unwrap_or_default(),
            range: self
                .video_full_range
                .map_or(ColorRange::Unspecified, ColorRange::from_full_range_flag),
            // §7.4.2.1.1: when `chroma_loc_info_present_flag` is 0, both
            // `chroma_sample_loc_type` values are inferred to be 0, which
            // Table E-1 (figure E-1) places at the left edge.
            chroma_location: self
                .chroma_sample_loc
                .map_or(ChromaLocation::Left, |(t, _)| {
                    ChromaLocation::from_h264_loc_type(t as u8)
                        .unwrap_or(ChromaLocation::Unspecified)
                }),
        }
    }

    /// The rate `time_scale / num_units_in_tick` — **twice** the picture rate.
    ///
    /// §E.2.1 defines a clock tick as `num_units_in_tick / time_scale` seconds
    /// and says that when `fixed_frame_rate_flag` is 1 the temporal distance
    /// between two consecutive *fields* is one tick. A frame is two fields, so
    /// the frame rate is `time_scale / (2 * num_units_in_tick)` and this is the
    /// field rate.
    ///
    /// Both are exposed because both are wanted. See [`Sps::frame_rate`] for
    /// the halved one and for which of them the reference tool prints.
    #[must_use]
    pub fn tick_rate(&self) -> Rational {
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
}

/// A sequence parameter set: ITU-T H.264 §7.3.2.1.1, in field order.
///
/// The specification's own field order and its own names, flags included. A
/// syntax table transcribed into a struct is easier to check against the
/// standard than one reorganised for taste.
///
/// Every field is the syntax element of the same name, undecorated. Derived
/// quantities — the ones §7.4.2.1.1 defines in terms of these — are methods,
/// so there is exactly one place each derivation is written and no way for a
/// stored copy to go stale.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct Sps {
    /// `profile_idc`.
    pub profile_idc: u8,
    /// `constraint_set0_flag` .. `constraint_set5_flag`, plus the two reserved
    /// bits, as they appear in the byte.
    pub constraint_flags: ConstraintFlags,
    /// `level_idc`.
    pub level_idc: u8,
    /// `seq_parameter_set_id`, 0..=31.
    pub id: u8,
    /// `chroma_format_idc`. Inferred as 4:2:0 for the profiles that do not code
    /// it (§7.4.2.1.1).
    pub chroma_format: ChromaFormat,
    /// `separate_colour_plane_flag`.
    pub separate_colour_plane: bool,
    /// `bit_depth_luma_minus8 + 8`, 8..=14.
    pub bit_depth_luma: u8,
    /// `bit_depth_chroma_minus8 + 8`, 8..=14.
    pub bit_depth_chroma: u8,
    /// `qpprime_y_zero_transform_bypass_flag`.
    pub qpprime_y_zero_transform_bypass: bool,
    /// The `seq_scaling_list_present_flag` bits and the lists they introduce.
    ///
    /// Stored raw and boxed. Deriving the *effective* matrices needs the
    /// fall-back rules of §7.4.2.1.1.1 and the default lists of Tables 7-3 and
    /// 7-4, which only a decoder needs — and this crate deliberately implements
    /// no decoder (D5, plan 15 §6.2).
    pub scaling_lists: Option<Box<ScalingLists>>,
    /// `log2_max_frame_num_minus4 + 4`, 4..=16.
    pub log2_max_frame_num: u8,
    /// `pic_order_cnt_type`, 0..=2.
    pub pic_order_cnt_type: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4`, for POC type 0 only.
    pub log2_max_pic_order_cnt_lsb: u8,
    /// POC type 1's parameters.
    pub poc_type1: Option<PocType1>,
    /// `max_num_ref_frames`.
    pub max_num_ref_frames: u32,
    /// `gaps_in_frame_num_value_allowed_flag`.
    pub gaps_in_frame_num_value_allowed: bool,
    /// `pic_width_in_mbs_minus1 + 1`.
    pub pic_width_in_mbs: u32,
    /// `pic_height_in_map_units_minus1 + 1`.
    pub pic_height_in_map_units: u32,
    /// `frame_mbs_only_flag`.
    pub frame_mbs_only: bool,
    /// `mb_adaptive_frame_field_flag`; always false when `frame_mbs_only`.
    pub mb_adaptive_frame_field: bool,
    /// `direct_8x8_inference_flag`.
    pub direct_8x8_inference: bool,
    /// The four `frame_crop_*_offset` values, in crop units, or `None` when
    /// `frame_cropping_flag` was 0.
    pub crop: Option<Crop>,
    /// `vui_parameters()`.
    pub vui: Option<VuiParameters>,
}

/// POC type 1's parameters, §7.3.2.1.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PocType1 {
    /// `delta_pic_order_always_zero_flag`.
    pub delta_pic_order_always_zero: bool,
    /// `offset_for_non_ref_pic`.
    pub offset_for_non_ref_pic: i32,
    /// `offset_for_top_to_bottom_field`.
    pub offset_for_top_to_bottom_field: i32,
    /// `offset_for_ref_frame[]`, at most 255 entries.
    pub offset_for_ref_frame: Vec<i32>,
}

impl PocType1 {
    /// `ExpectedDeltaPerPicOrderCntCycle`, §7.4.2.1.1: the sum of the cycle.
    ///
    /// Wrapping rather than saturating, because §8.2.1.2 computes POC in
    /// modular arithmetic and a saturating sum would silently change the
    /// picture order rather than merely overflowing.
    #[must_use]
    pub fn expected_delta_per_cycle(&self) -> i32 {
        self.offset_for_ref_frame
            .iter()
            .fold(0i32, |a, &b| a.wrapping_add(b))
    }
}

/// `frame_crop_*_offset`, §7.4.2.1.1. In **crop units**, not luma samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Crop {
    /// `frame_crop_left_offset`.
    pub left: u32,
    /// `frame_crop_right_offset`.
    pub right: u32,
    /// `frame_crop_top_offset`.
    pub top: u32,
    /// `frame_crop_bottom_offset`.
    pub bottom: u32,
}

/// The raw scaling lists, §7.3.2.1.1.1.
///
/// `present[i]` is `seq_scaling_list_present_flag[i]`; `use_default[i]` is the
/// `UseDefaultScalingMatrix` flag the list's first delta sets. A list whose
/// `present` flag is 0 is left at zeros — its effective value comes from the
/// fall-back rules, which this crate does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingLists {
    /// The six 4x4 lists.
    pub list_4x4: [[u8; 16]; 6],
    /// The six 8x8 lists. Only the first two are coded unless
    /// `chroma_format_idc == 3`.
    pub list_8x8: [[u8; 64]; 6],
    /// `seq_scaling_list_present_flag[i]` for i in 0..12.
    pub present: [bool; 12],
    /// `UseDefaultScalingMatrix4x4Flag` / `8x8Flag` for i in 0..12.
    pub use_default: [bool; 12],
}

impl Default for ScalingLists {
    fn default() -> Self {
        Self {
            list_4x4: [[0; 16]; 6],
            list_8x8: [[0; 64]; 6],
            present: [false; 12],
            use_default: [false; 12],
        }
    }
}

// ------------------------------------------------------------------- derived

impl Sps {
    /// `ChromaArrayType`, §7.4.2.1.1: the chroma format *as decoded*, which is
    /// 0 when the three 4:4:4 planes are coded separately.
    ///
    /// Everywhere except the SPS syntax itself this — not `chroma_format_idc` —
    /// is the value that matters, and confusing the two is how a 4:4:4 stream
    /// with separate planes gets the wrong crop.
    #[must_use]
    pub const fn chroma_array_type(&self) -> ChromaFormat {
        if self.separate_colour_plane {
            ChromaFormat::Monochrome
        } else {
            self.chroma_format
        }
    }

    /// `FrameHeightInMbs`, §7.4.2.1.1:
    /// `(2 - frame_mbs_only_flag) * PicHeightInMapUnits`.
    #[must_use]
    pub const fn frame_height_in_mbs(&self) -> u32 {
        let factor = if self.frame_mbs_only { 1 } else { 2 };
        self.pic_height_in_map_units.saturating_mul(factor)
    }

    /// Macroblock-aligned luma width, `PicWidthInSamplesL`.
    #[must_use]
    pub const fn coded_width(&self) -> u32 {
        self.pic_width_in_mbs.saturating_mul(16)
    }

    /// Macroblock-aligned luma height, before cropping.
    ///
    /// This is the number every 1080-line stream makes famous: 1080 is not a
    /// multiple of 16, so the coded height is **1088** and the last eight rows
    /// are cropped away.
    #[must_use]
    pub const fn coded_height(&self) -> u32 {
        self.frame_height_in_mbs().saturating_mul(16)
    }

    /// `CropUnitX`, `CropUnitY`, §7.4.2.1.1.
    ///
    /// The crop offsets are counted in these units, not in luma samples, and
    /// the unit depends on both the chroma format and on frame/field coding:
    ///
    /// * `ChromaArrayType == 0`: `CropUnitX = 1`,
    ///   `CropUnitY = 2 - frame_mbs_only_flag`
    /// * otherwise: `CropUnitX = SubWidthC`,
    ///   `CropUnitY = SubHeightC * (2 - frame_mbs_only_flag)`
    ///
    /// So the same `frame_crop_bottom_offset = 4` removes 8 luma rows from a
    /// progressive 4:2:0 stream and 16 from an interlaced one.
    #[must_use]
    pub const fn crop_unit(&self) -> (u32, u32) {
        let field_factor = if self.frame_mbs_only { 1 } else { 2 };
        match self.chroma_array_type() {
            ChromaFormat::Monochrome => (1, field_factor),
            c => (c.sub_width_c(), c.sub_height_c() * field_factor),
        }
    }

    /// The displayed luma width and height, §7.4.2.1.1 — the two numbers
    /// `ffprobe` prints as `width` and `height`.
    ///
    /// Returns `None` if the crop would leave nothing, which is what the
    /// specification's range constraint on the crop offsets forbids and what
    /// the reference tool rejects the whole SPS for. Verified against
    /// `ffmpeg 8.1`: an SPS cropped to zero width prints
    /// `crop values invalid 0 320 0 4 / 640 368` and the stream is dropped.
    #[must_use]
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let (unit_x, unit_y) = self.crop_unit();
        let (cw, ch) = (self.coded_width(), self.coded_height());
        let Some(crop) = self.crop else {
            return (cw > 0 && ch > 0).then_some((cw, ch));
        };
        let dx = crop.left.checked_add(crop.right)?.checked_mul(unit_x)?;
        let dy = crop.top.checked_add(crop.bottom)?.checked_mul(unit_y)?;
        let w = cw.checked_sub(dx)?;
        let h = ch.checked_sub(dy)?;
        (w > 0 && h > 0).then_some((w, h))
    }

    /// The sample aspect ratio, or [`Rational::UNDEFINED`] with no VUI.
    #[must_use]
    pub fn sample_aspect_ratio(&self) -> Rational {
        self.vui
            .as_ref()
            .map_or(Rational::UNDEFINED, VuiParameters::sample_aspect_ratio)
    }

    /// Colour signalling.
    ///
    /// # `chroma_location` and the reference
    ///
    /// The specification infers `chroma_sample_loc_type_top_field` to be 0 —
    /// which is "left" — whenever it is not present, and that inference applies
    /// whether the VUI is absent or merely silent (§7.4.2.1.1).
    ///
    /// `// D17:` the reference tool applies it only when a VUI **is** present.
    /// Probed against `ffmpeg 8.1` on a stream whose SPS was patched to clear
    /// `vui_parameters_present_flag`:
    ///
    /// ```text
    /// VUI present, chroma_loc_info_present_flag = 0  ->  chroma_location=left
    /// no VUI at all                                 ->  chroma_location=unspecified
    /// ```
    ///
    /// `chroma_location` is printed by `-show_streams`, so this is an
    /// observable divergence and D17 says to reproduce it. Standard's answer:
    /// `left` in both rows.
    #[must_use]
    pub fn color_info(&self) -> ColorInfo {
        self.vui
            .as_ref()
            .map_or_else(ColorInfo::default, VuiParameters::color_info)
    }

    /// The picture rate the VUI implies: `time_scale / (2 * num_units_in_tick)`
    /// (§E.2.1), or [`Rational::UNDEFINED`] when there is no timing info.
    ///
    /// # The factor of two
    ///
    /// `num_units_in_tick` counts clock ticks per **field**, so the frame rate
    /// is half the tick rate. A 24 fps stream from `libx264` carries
    /// `num_units_in_tick = 1, time_scale = 48`; probed with `trace_headers` on
    /// `ffmpeg 8.1`.
    ///
    /// `// D17:` the reference reports the **unhalved** rate. For a raw Annex B
    /// stream, `ffprobe -f h264 -show_streams` prints `r_frame_rate=48/1` for
    /// that same file, and `50/1` and `60000/1001` for 25 fps and 30000/1001
    /// streams. That is defensible rather than wrong — `r_frame_rate` is
    /// documented as the lowest rate that can represent every timestamp, and a
    /// field-coded picture arrives at the tick rate — but it is not the frame
    /// rate, and a caller that wants one must not use the other.
    /// [`VuiParameters::tick_rate`] is what the reference prints;
    /// this is what §E.2.1 defines.
    #[must_use]
    pub fn frame_rate(&self) -> Rational {
        let Some(vui) = self.vui.as_ref() else {
            return Rational::UNDEFINED;
        };
        match vui.timing {
            Some(t) if t.num_units_in_tick != 0 && t.time_scale != 0 => {
                let (r, _) = Rational::reduce(
                    i64::from(t.time_scale),
                    i64::from(t.num_units_in_tick) * 2,
                    i64::from(i32::MAX),
                );
                r
            }
            _ => Rational::UNDEFINED,
        }
    }

    /// `max_num_reorder_frames` from the VUI, when it states one.
    ///
    /// This is what `ffprobe` prints as `has_b_frames`. The reference will
    /// *raise* it if it observes deeper reordering while decoding — probed by
    /// patching the field to 0 in a stream that does reorder, which reports 1 —
    /// but raising it requires decoding and is therefore outside a parser's
    /// reach. When the VUI does not state one, `None`.
    #[must_use]
    pub fn max_num_reorder_frames(&self) -> Option<u32> {
        self.vui
            .as_ref()?
            .bitstream_restriction
            .as_ref()
            .map(|b| b.max_num_reorder_frames)
    }

    /// `MaxFrameNum`, §7.4.3: `2^log2_max_frame_num`.
    #[must_use]
    pub const fn max_frame_num(&self) -> u32 {
        1u32.wrapping_shl(self.log2_max_frame_num as u32)
    }

    /// `MaxPicOrderCntLsb`, §7.4.3: `2^log2_max_pic_order_cnt_lsb`.
    #[must_use]
    pub const fn max_pic_order_cnt_lsb(&self) -> u32 {
        1u32.wrapping_shl(self.log2_max_pic_order_cnt_lsb as u32)
    }

    /// The profile's display name, as the reference prints it, or `None` for a
    /// `profile_idc` nothing names.
    #[must_use]
    pub fn profile_name(&self) -> Option<&'static str> {
        profile_name(self.profile_idc, self.constraint_flags)
    }

    /// Total macroblocks per frame, `PicSizeInMbs` for a frame picture.
    #[must_use]
    pub const fn frame_size_in_mbs(&self) -> u64 {
        (self.pic_width_in_mbs as u64) * (self.frame_height_in_mbs() as u64)
    }
}

// ------------------------------------------------------------------- parsing

/// Profiles whose SPS carries the `chroma_format_idc` block, §7.3.2.1.1.
///
/// Format-dictated: this exact set decides how many bits follow, so a parser
/// with a different set produces garbage rather than a different opinion.
const HAS_CHROMA_BLOCK: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

/// Whether `profile_idc` selects the extended SPS syntax.
#[must_use]
pub fn profile_has_chroma_block(profile_idc: u8) -> bool {
    HAS_CHROMA_BLOCK.contains(&profile_idc)
}

impl Sps {
    /// Parse a sequence parameter set from a NAL unit's RBSP.
    ///
    /// `rbsp` is the whole NAL unit with emulation prevention already removed —
    /// header byte included, because that is where the specification's bit
    /// numbering starts.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a syntax element outside its permitted range
    /// or a structurally impossible codeword, [`Error::UnexpectedEof`] for a
    /// truncated unit, and [`Error::LimitExceeded`] when a declared count would
    /// exceed the budget.
    pub fn parse(rbsp: &[u8], budget: &mut Budget) -> Result<Self> {
        let header = H264NalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if header.nal_unit_type != NalUnitType::Sps
            && header.nal_unit_type != NalUnitType::SubsetSps
        {
            return Err(Error::InvalidData("not a sequence parameter set"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(8); // the NAL header byte
        let sps = Self::parse_data(&mut reader, budget)?;
        reader.check()?;
        Ok(sps)
    }

    /// `seq_parameter_set_data()`, §7.3.2.1.1, from a reader positioned just
    /// after the NAL header.
    ///
    /// Exposed because `avcC` and `subset_seq_parameter_set` both reach it by a
    /// different route.
    ///
    /// # Errors
    ///
    /// As [`Sps::parse`].
    pub fn parse_data(reader: &mut BitReader<'_>, budget: &mut Budget) -> Result<Self> {
        let max_mbs_x = (budget.limits().max_dimension >> 4).max(1);
        let mut g = BoundedGolomb::new(reader, budget);

        let profile_idc = g.u(8)? as u8;
        let constraint_flags = ConstraintFlags::from_bits(g.u(8)? as u8);
        let level_idc = g.u(8)? as u8;
        let id = g.ue_v(31)? as u8;

        let mut chroma_format = ChromaFormat::Yuv420;
        let mut separate_colour_plane = false;
        let mut bit_depth_luma = 8u8;
        let mut bit_depth_chroma = 8u8;
        let mut qpprime_y_zero_transform_bypass = false;
        let mut scaling_lists = None;

        if profile_has_chroma_block(profile_idc) {
            chroma_format = ChromaFormat::from_idc(g.ue_v(3)?)
                .ok_or(Error::InvalidData("chroma_format_idc out of range"))?;
            if chroma_format == ChromaFormat::Yuv444 {
                separate_colour_plane = g.u(1)? != 0;
            }
            // §7.4.2.1.1 caps both depths at 6, i.e. 14 bits.
            bit_depth_luma = g.ue_v(6)? as u8 + 8;
            bit_depth_chroma = g.ue_v(6)? as u8 + 8;
            qpprime_y_zero_transform_bypass = g.u(1)? != 0;
            if g.u(1)? != 0 {
                let count = if chroma_format == ChromaFormat::Yuv444 {
                    12
                } else {
                    8
                };
                scaling_lists = Some(Box::new(read_scaling_lists(&mut g, count)?));
            }
        }

        // §7.4.2.1.1 bounds both log2 fields at 12.
        let log2_max_frame_num = g.ue_v(12)? as u8 + 4;
        let pic_order_cnt_type = g.ue_v(2)? as u8;
        let mut log2_max_pic_order_cnt_lsb = 4u8;
        let mut poc_type1 = None;
        match pic_order_cnt_type {
            0 => log2_max_pic_order_cnt_lsb = g.ue_v(12)? as u8 + 4,
            1 => {
                let delta_pic_order_always_zero = g.u(1)? != 0;
                let offset_for_non_ref_pic = g.se_v(i32::MIN + 1, i32::MAX)?;
                let offset_for_top_to_bottom_field = g.se_v(i32::MIN + 1, i32::MAX)?;
                let n = g.ue_v(255)?;
                // Charge the loop before running it, so a declared 255 costs
                // 255 units of fuel up front rather than 255 reads.
                g.budget().consume_fuel(u64::from(n))?;
                let mut offset_for_ref_frame = g.budget().alloc::<i32>(n as usize)?;
                offset_for_ref_frame.clear();
                for _ in 0..n {
                    offset_for_ref_frame.push(g.se_v(i32::MIN + 1, i32::MAX)?);
                }
                poc_type1 = Some(PocType1 {
                    delta_pic_order_always_zero,
                    offset_for_non_ref_pic,
                    offset_for_top_to_bottom_field,
                    offset_for_ref_frame,
                });
            }
            _ => {}
        }

        // §7.4.2.1.1: at most MaxDpbFrames, which Annex A caps at 16.
        let max_num_ref_frames = g.ue_v(16)?;
        let gaps_in_frame_num_value_allowed = g.u(1)? != 0;
        let pic_width_in_mbs = g.ue_v(max_mbs_x.saturating_sub(1))? + 1;
        let pic_height_in_map_units = g.ue_v(max_mbs_x.saturating_sub(1))? + 1;
        let frame_mbs_only = g.u(1)? != 0;
        let mb_adaptive_frame_field = if frame_mbs_only { false } else { g.u(1)? != 0 };
        let direct_8x8_inference = g.u(1)? != 0;
        let crop = if g.u(1)? != 0 {
            // The offsets are in crop units, so the widest legal value is the
            // picture size divided by the unit; bounding by the picture size in
            // luma samples is looser but cannot overflow, and
            // `Sps::dimensions` rejects what is actually out of range.
            let max = pic_width_in_mbs.saturating_mul(16);
            let max_y = pic_height_in_map_units.saturating_mul(32);
            Some(Crop {
                left: g.ue_v(max)?,
                right: g.ue_v(max)?,
                top: g.ue_v(max_y)?,
                bottom: g.ue_v(max_y)?,
            })
        } else {
            None
        };
        let vui = if g.u(1)? != 0 {
            Some(parse_vui(&mut g)?)
        } else {
            None
        };

        let sps = Self {
            profile_idc,
            constraint_flags,
            level_idc,
            id,
            chroma_format,
            separate_colour_plane,
            bit_depth_luma,
            bit_depth_chroma,
            qpprime_y_zero_transform_bypass,
            scaling_lists,
            log2_max_frame_num,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb,
            poc_type1,
            max_num_ref_frames,
            gaps_in_frame_num_value_allowed,
            pic_width_in_mbs,
            pic_height_in_map_units,
            frame_mbs_only,
            mb_adaptive_frame_field,
            direct_8x8_inference,
            crop,
            vui,
        };

        // The crop constraint of §7.4.2.1.1, checked here rather than left to
        // the caller: an SPS whose crop leaves nothing has no usable geometry
        // at all, and the reference rejects the whole parameter set for it.
        let (w, h) = sps
            .dimensions()
            .ok_or(Error::InvalidData("frame cropping leaves no picture"))?;
        // Four bytes per pixel is the widest packed 8-bit layout; the real
        // pixel format tightens this once it is known.
        budget.check_frame(w.max(sps.coded_width()), h.max(sps.coded_height()), 4)?;
        Ok(sps)
    }
}

/// `scaling_list()`, §7.3.2.1.1.1, `count` times.
pub(crate) fn read_scaling_lists(
    g: &mut BoundedGolomb<'_, '_, '_>,
    count: usize,
) -> Result<ScalingLists> {
    let mut out = ScalingLists::default();
    for i in 0..count {
        let present = g.u(1)? != 0;
        if let Some(slot) = out.present.get_mut(i) {
            *slot = present;
        }
        if !present {
            continue;
        }
        let use_default = if i < 6 {
            let mut list = [0u8; 16];
            let d = read_scaling_list(g, &mut list)?;
            if let Some(slot) = out.list_4x4.get_mut(i) {
                *slot = list;
            }
            d
        } else {
            let mut list = [0u8; 64];
            let d = read_scaling_list(g, &mut list)?;
            if let Some(slot) = out.list_8x8.get_mut(i - 6) {
                *slot = list;
            }
            d
        };
        if let Some(slot) = out.use_default.get_mut(i) {
            *slot = use_default;
        }
    }
    Ok(out)
}

/// One `scaling_list()`, §7.3.2.1.1.1. Returns `UseDefaultScalingMatrixFlag`.
///
/// The loop is bounded by `list.len()`, which is a compile-time 16 or 64, so it
/// cannot be driven by input.
fn read_scaling_list(g: &mut BoundedGolomb<'_, '_, '_>, list: &mut [u8]) -> Result<bool> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    let mut use_default = false;
    for (j, slot) in list.iter_mut().enumerate() {
        if next_scale != 0 {
            let delta = g.se_v(-128, 127)?;
            next_scale = (last_scale + delta + 256).rem_euclid(256);
            if j == 0 && next_scale == 0 {
                use_default = true;
            }
        }
        let value = if next_scale == 0 {
            last_scale
        } else {
            next_scale
        };
        *slot = value as u8;
        last_scale = value;
    }
    Ok(use_default)
}

/// `vui_parameters()`, §E.1.1.
fn parse_vui(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<VuiParameters> {
    let mut vui = VuiParameters::default();

    if g.u(1)? != 0 {
        let idc = g.u(8)? as u8;
        vui.aspect_ratio_idc = Some(idc);
        if idc == EXTENDED_SAR {
            vui.sar = Some((g.u(16)? as u16, g.u(16)? as u16));
        }
    }
    if g.u(1)? != 0 {
        vui.overscan_appropriate = Some(g.u(1)? != 0);
    }
    if g.u(1)? != 0 {
        vui.video_format = Some(g.u(3)? as u8);
        vui.video_full_range = Some(g.u(1)? != 0);
        if g.u(1)? != 0 {
            vui.colour_description = Some((g.u(8)? as u8, g.u(8)? as u8, g.u(8)? as u8));
        }
    }
    if g.u(1)? != 0 {
        // §E.2.1 bounds both at 5.
        vui.chroma_sample_loc = Some((g.ue_v(5)?, g.ue_v(5)?));
    }
    if g.u(1)? != 0 {
        vui.timing = Some(Timing {
            num_units_in_tick: g.u(32)?,
            time_scale: g.u(32)?,
            fixed_frame_rate: g.u(1)? != 0,
        });
    }
    if g.u(1)? != 0 {
        vui.nal_hrd = Some(parse_hrd(g)?);
    }
    if g.u(1)? != 0 {
        vui.vcl_hrd = Some(parse_hrd(g)?);
    }
    if vui.nal_hrd.is_some() || vui.vcl_hrd.is_some() {
        vui.low_delay_hrd = Some(g.u(1)? != 0);
    }
    vui.pic_struct_present = g.u(1)? != 0;
    if g.u(1)? != 0 {
        vui.bitstream_restriction = Some(BitstreamRestriction {
            motion_vectors_over_pic_boundaries: g.u(1)? != 0,
            max_bytes_per_pic_denom: g.ue_v(16)?,
            max_bits_per_mb_denom: g.ue_v(16)?,
            log2_max_mv_length_horizontal: g.ue_v(16)?,
            log2_max_mv_length_vertical: g.ue_v(16)?,
            max_num_reorder_frames: g.ue_v(16)?,
            max_dec_frame_buffering: g.ue_v(16)?,
        });
    }
    Ok(vui)
}

/// `hrd_parameters()`, §E.1.2.
fn parse_hrd(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<HrdParameters> {
    // §E.2.2 bounds `cpb_cnt_minus1` at 31.
    let cpb_cnt = g.ue_v(31)? + 1;
    let bit_rate_scale = g.u(4)? as u8;
    let cpb_size_scale = g.u(4)? as u8;
    g.budget().consume_fuel(u64::from(cpb_cnt))?;
    let mut cpb = g.budget().alloc::<CpbEntry>(cpb_cnt as usize)?;
    cpb.clear();
    for _ in 0..cpb_cnt {
        // The values run to 2^32 - 2, which a `ue(v)` prefix of 31 zeros can
        // still express, so the bound is the type's rather than a policy.
        let bit_rate_value_minus1 = g.ue_v(u32::MAX - 1)?;
        let cpb_size_value_minus1 = g.ue_v(u32::MAX - 1)?;
        let cbr = g.u(1)? != 0;
        cpb.push(CpbEntry {
            bit_rate_value_minus1,
            cpb_size_value_minus1,
            cbr,
        });
    }
    Ok(HrdParameters {
        cpb,
        bit_rate_scale,
        cpb_size_scale,
        initial_cpb_removal_delay_length_minus1: g.u(5)? as u8,
        cpb_removal_delay_length_minus1: g.u(5)? as u8,
        dpb_output_delay_length_minus1: g.u(5)? as u8,
        time_offset_length: g.u(5)? as u8,
    })
}
