//! The sequence parameter set, ITU-T H.265 §7.3.2.2, and everything the
//! displayed picture geometry is derived from (§7.4.3.2).
//!
//! Also Annex E: `vui_parameters()` (§E.2.1) and `hrd_parameters()` (§E.2.2),
//! and §7.3.4's `scaling_list_data()`.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_color::{
    ChromaLocation, ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients,
    TransferCharacteristic,
};
use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};
use crate::ptl::ProfileTierLevel;
use crate::rps::{ShortTermRps, parse_st_ref_pic_set};
use crate::util::{MAX_LONG_TERM_RPS, MAX_SHORT_TERM_RPS, MAX_SUB_LAYERS, ceil_log2};

/// `EXTENDED_SAR`, Table E-1: the aspect ratio is given explicitly.
pub const EXTENDED_SAR: u8 = 255;

/// Table E-1, the sixteen predefined sample aspect ratios.
///
/// Index is `aspect_ratio_idc`; entry 0 is "unspecified" and is stored as
/// `(0, 0)` so it is distinguishable from a real ratio. Indices 17..=254 are
/// reserved and are also unspecified.
///
/// The same table as H.264's Table E-1, value for value — verified against
/// `ffprobe 8.1` by patching `aspect_ratio_idc` through 0..=17, 254 and 255 in
/// an HEVC SPS, which returned exactly these ratios. Format-dictated: a
/// conforming parser has no freedom here (D7/D15, merger).
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

    /// `SubWidthC`, Table 6-1. Monochrome has none; 1 is returned so the
    /// conformance-window arithmetic has a neutral factor.
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

/// `conf_win_*_offset` (§7.4.3.2) or `def_disp_win_*_offset` (§E.2.1), in
/// **chroma units** — `SubWidthC` and `SubHeightC` luma samples each, not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Window {
    /// The left offset.
    pub left: u32,
    /// The right offset.
    pub right: u32,
    /// The top offset.
    pub top: u32,
    /// The bottom offset.
    pub bottom: u32,
}

impl Window {
    /// Whether the window removes nothing.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.left == 0 && self.right == 0 && self.top == 0 && self.bottom == 0
    }
}

/// `hrd_parameters()`, §E.2.2.
///
/// Kept whole rather than reduced to a bit rate, because the field widths it
/// declares — `au_cpb_removal_delay_length_minus1` and friends — are what the
/// `pic_timing` SEI needs in order to be parsable at all. Dropping them would
/// make that SEI undecodable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HrdParameters {
    /// `nal_hrd_parameters_present_flag`.
    pub nal_hrd_present: bool,
    /// `vcl_hrd_parameters_present_flag`.
    pub vcl_hrd_present: bool,
    /// `sub_pic_hrd_params_present_flag`, and the four fields it introduces.
    pub sub_pic: Option<SubPicHrd>,
    /// `bit_rate_scale`.
    pub bit_rate_scale: u8,
    /// `cpb_size_scale`.
    pub cpb_size_scale: u8,
    /// `cpb_size_du_scale`, present only with sub-picture parameters.
    pub cpb_size_du_scale: u8,
    /// `initial_cpb_removal_delay_length_minus1`.
    pub initial_cpb_removal_delay_length_minus1: u8,
    /// `au_cpb_removal_delay_length_minus1`. **This** is the width of
    /// `au_cpb_removal_delay_minus1` in a `pic_timing` SEI.
    pub au_cpb_removal_delay_length_minus1: u8,
    /// `dpb_output_delay_length_minus1`.
    pub dpb_output_delay_length_minus1: u8,
    /// One entry per sub-layer.
    pub sub_layers: Vec<SubLayerHrd>,
}

/// The `sub_pic_hrd_params` block of §E.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubPicHrd {
    /// `tick_divisor_minus2`.
    pub tick_divisor_minus2: u8,
    /// `du_cpb_removal_delay_increment_length_minus1`.
    pub du_cpb_removal_delay_increment_length_minus1: u8,
    /// `sub_pic_cpb_params_in_pic_timing_sei_flag`.
    pub sub_pic_cpb_params_in_pic_timing_sei: bool,
    /// `dpb_output_delay_du_length_minus1`.
    pub dpb_output_delay_du_length_minus1: u8,
}

/// One sub-layer's HRD entry, §E.2.2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubLayerHrd {
    /// `fixed_pic_rate_general_flag[i]`.
    pub fixed_pic_rate_general: bool,
    /// `fixed_pic_rate_within_cvs_flag[i]`. §E.3.2 infers it to be 1 whenever
    /// `fixed_pic_rate_general_flag[i]` is, which is applied here.
    pub fixed_pic_rate_within_cvs: bool,
    /// `elemental_duration_in_tc_minus1[i]`, present only for a fixed rate.
    pub elemental_duration_in_tc_minus1: Option<u32>,
    /// `low_delay_hrd_flag[i]`.
    pub low_delay_hrd: bool,
    /// `cpb_cnt_minus1[i]`.
    pub cpb_cnt_minus1: u32,
    /// `sub_layer_hrd_parameters()` for the NAL HRD.
    pub nal_cpb: Vec<CpbEntry>,
    /// `sub_layer_hrd_parameters()` for the VCL HRD.
    pub vcl_cpb: Vec<CpbEntry>,
}

/// One coded picture buffer's declaration, §E.2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpbEntry {
    /// `bit_rate_value_minus1`.
    pub bit_rate_value_minus1: u32,
    /// `cpb_size_value_minus1`.
    pub cpb_size_value_minus1: u32,
    /// `cpb_size_du_value_minus1`, with sub-picture parameters only.
    pub cpb_size_du_value_minus1: u32,
    /// `bit_rate_du_value_minus1`, with sub-picture parameters only.
    pub bit_rate_du_value_minus1: u32,
    /// `cbr_flag`.
    pub cbr: bool,
}

impl HrdParameters {
    /// `BitRate[i]` in bits per second, §E.3.3:
    /// `(bit_rate_value_minus1 + 1) * 2^(6 + bit_rate_scale)`.
    ///
    /// Returns `None` on overflow rather than saturating, because a bit rate
    /// that does not fit 64 bits is a malformed declaration and reporting a
    /// clamped one would be worse than reporting none.
    #[must_use]
    pub fn bit_rate(&self, sub_layer: usize, i: usize) -> Option<u64> {
        let e = self.sub_layers.get(sub_layer)?.nal_cpb.get(i)?;
        u64::from(e.bit_rate_value_minus1)
            .checked_add(1)?
            .checked_shl(6 + u32::from(self.bit_rate_scale))
    }

    /// `CpbSize[i]` in bits, §E.3.3:
    /// `(cpb_size_value_minus1 + 1) * 2^(4 + cpb_size_scale)`.
    #[must_use]
    pub fn cpb_size(&self, sub_layer: usize, i: usize) -> Option<u64> {
        let e = self.sub_layers.get(sub_layer)?.nal_cpb.get(i)?;
        u64::from(e.cpb_size_value_minus1)
            .checked_add(1)?
            .checked_shl(4 + u32::from(self.cpb_size_scale))
    }
}

/// The `bitstream_restriction` block of the VUI, §E.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BitstreamRestriction {
    /// `tiles_fixed_structure_flag`.
    pub tiles_fixed_structure: bool,
    /// `motion_vectors_over_pic_boundaries_flag`.
    pub motion_vectors_over_pic_boundaries: bool,
    /// `restricted_ref_pic_lists_flag`.
    pub restricted_ref_pic_lists: bool,
    /// `min_spatial_segmentation_idc` — the field `hvcC` copies verbatim.
    pub min_spatial_segmentation_idc: u32,
    /// `max_bytes_per_pic_denom`.
    pub max_bytes_per_pic_denom: u32,
    /// `max_bits_per_min_cu_denom`.
    pub max_bits_per_min_cu_denom: u32,
    /// `log2_max_mv_length_horizontal`.
    pub log2_max_mv_length_horizontal: u32,
    /// `log2_max_mv_length_vertical`.
    pub log2_max_mv_length_vertical: u32,
}

/// `vui_timing_info`, §E.2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// `vui_num_units_in_tick`. Required to be greater than 0.
    pub num_units_in_tick: u32,
    /// `vui_time_scale`. Required to be greater than 0.
    pub time_scale: u32,
    /// `vui_num_ticks_poc_diff_one_minus1`, present only when
    /// `vui_poc_proportional_to_timing_flag` was set.
    pub num_ticks_poc_diff_one_minus1: Option<u32>,
}

/// `vui_parameters()`, §E.2.1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VuiParameters {
    /// `aspect_ratio_idc`, present only if `aspect_ratio_info_present_flag`.
    pub aspect_ratio_idc: Option<u8>,
    /// `sar_width` / `sar_height`, present only for `EXTENDED_SAR`.
    pub sar: Option<(u16, u16)>,
    /// `overscan_appropriate_flag`, present only if `overscan_info_present_flag`.
    pub overscan_appropriate: Option<bool>,
    /// `video_format`, present only if `video_signal_type_present_flag`.
    pub video_format: Option<u8>,
    /// `video_full_range_flag`. `None` means `video_signal_type_present_flag`
    /// was 0, which is what distinguishes "limited range" from "not stated".
    pub video_full_range: Option<bool>,
    /// `colour_primaries`, `transfer_characteristics`, `matrix_coeffs` — raw
    /// code points, present only if `colour_description_present_flag`.
    pub colour_description: Option<(u8, u8, u8)>,
    /// `chroma_sample_loc_type_top_field` / `_bottom_field`.
    pub chroma_sample_loc: Option<(u32, u32)>,
    /// `neutral_chroma_indication_flag`.
    pub neutral_chroma_indication: bool,
    /// `field_seq_flag` — the stream codes fields rather than frames.
    pub field_seq: bool,
    /// `frame_field_info_present_flag`, which decides whether a `pic_timing`
    /// SEI carries `pic_struct`. A parser cannot read that SEI without it.
    pub frame_field_info_present: bool,
    /// `def_disp_win_*_offset`, present only if `default_display_window_flag`.
    ///
    /// **Not** applied to the reported dimensions; see [`Sps::dimensions`].
    pub default_display_window: Option<Window>,
    /// `vui_num_units_in_tick` and `vui_time_scale`.
    pub timing: Option<Timing>,
    /// `hrd_parameters()`, present only inside the timing block.
    pub hrd: Option<HrdParameters>,
    /// The `bitstream_restriction` block.
    pub bitstream_restriction: Option<BitstreamRestriction>,
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

    /// The picture rate the timing info implies: `vui_time_scale /
    /// vui_num_units_in_tick`.
    ///
    /// # Not halved, unlike H.264
    ///
    /// §E.3.1 defines a clock tick the same way in both standards, but HEVC's
    /// `elemental_duration_in_tc_minus1` counts ticks per *picture* where
    /// H.264's convention made `num_units_in_tick` a field duration. So the
    /// factor of two that `vaco-parse-h264` documents is **not** here.
    ///
    /// Confirmed rather than assumed, at four rates: `-bsf:v trace_headers` on
    /// `x265` output gives `vui_num_units_in_tick = 1, vui_time_scale = 24` for
    /// a 24 fps stream, and `ffprobe -f hevc` prints `r_frame_rate=24/1` for the
    /// same file. The 25, 30000/1001 and 60000/1001 streams agree. The H.264
    /// encode of the same source prints `48/1`.
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
}

/// A sequence parameter set: ITU-T H.265 §7.3.2.2, in field order.
///
/// The specification's own field order and its own names, flags included. A
/// syntax table transcribed into a struct is easier to check against the
/// standard than one reorganised for taste.
///
/// Every field is the syntax element of the same name, undecorated. Derived
/// quantities — the ones §7.4.3.2 defines in terms of these — are methods, so
/// there is exactly one place each derivation is written and no way for a
/// stored copy to go stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct Sps {
    /// `sps_video_parameter_set_id`, 0..=15.
    pub vps_id: u8,
    /// `sps_max_sub_layers_minus1 + 1`, 1..=7.
    pub max_sub_layers: u8,
    /// `sps_temporal_id_nesting_flag`.
    pub temporal_id_nesting: bool,
    /// `profile_tier_level()`.
    pub ptl: ProfileTierLevel,
    /// `sps_seq_parameter_set_id`, 0..=15.
    pub id: u8,
    /// `chroma_format_idc`.
    pub chroma_format: ChromaFormat,
    /// `separate_colour_plane_flag`.
    pub separate_colour_plane: bool,
    /// `pic_width_in_luma_samples` — the **coded** width, already a multiple of
    /// `MinCbSizeY`. This is what `ffprobe` prints as `coded_width`.
    pub pic_width_in_luma_samples: u32,
    /// `pic_height_in_luma_samples`.
    pub pic_height_in_luma_samples: u32,
    /// `conf_win_*_offset`, or `None` when `conformance_window_flag` was 0.
    pub conformance_window: Option<Window>,
    /// `bit_depth_luma_minus8 + 8`, 8..=16.
    pub bit_depth_luma: u8,
    /// `bit_depth_chroma_minus8 + 8`, 8..=16.
    pub bit_depth_chroma: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4 + 4`, 4..=16.
    pub log2_max_pic_order_cnt_lsb: u8,
    /// `sps_max_dec_pic_buffering_minus1[i]`, one per coded sub-layer.
    pub max_dec_pic_buffering_minus1: Vec<u32>,
    /// `sps_max_num_reorder_pics[i]`, one per coded sub-layer. The last entry
    /// is what `ffprobe` prints as `has_b_frames`.
    pub max_num_reorder_pics: Vec<u32>,
    /// `sps_max_latency_increase_plus1[i]`.
    pub max_latency_increase_plus1: Vec<u32>,
    /// `log2_min_luma_coding_block_size_minus3 + 3`.
    pub log2_min_cb_size: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_cb_size: u8,
    /// `log2_min_luma_transform_block_size_minus2 + 2`.
    pub log2_min_tb_size: u8,
    /// `log2_diff_max_min_luma_transform_block_size`.
    pub log2_diff_max_min_tb_size: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u32,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u32,
    /// `scaling_list_enabled_flag`.
    pub scaling_list_enabled: bool,
    /// The raw `scaling_list_data()`, when the SPS carried one.
    ///
    /// Boxed because it is 400-odd bytes that almost no stream has. Stored raw:
    /// deriving the *effective* matrices needs §7.4.5's fall-back rules and the
    /// default lists of Tables 7-5 and 7-6, which only a decoder needs — and
    /// this crate deliberately implements no decoder (D5, plan 15 §6.2).
    pub scaling_list: Option<Box<ScalingListData>>,
    /// `amp_enabled_flag`.
    pub amp_enabled: bool,
    /// `sample_adaptive_offset_enabled_flag`. The slice segment header's
    /// `slice_sao_luma_flag` depends on it, so a parser needs it.
    pub sample_adaptive_offset_enabled: bool,
    /// The `pcm_*` block, present only if `pcm_enabled_flag`.
    pub pcm: Option<PcmParameters>,
    /// The short-term reference picture sets the SPS declares.
    pub short_term_ref_pic_sets: Vec<ShortTermRps>,
    /// `long_term_ref_pics_present_flag`.
    pub long_term_ref_pics_present: bool,
    /// `lt_ref_pic_poc_lsb_sps[i]` and `used_by_curr_pic_lt_sps_flag[i]`.
    pub long_term_ref_pics: Vec<(u32, bool)>,
    /// `sps_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing_enabled: bool,
    /// `vui_parameters()`.
    pub vui: Option<VuiParameters>,
    /// `sps_range_extension()`, when the extension flag introduced one.
    pub range_extension: Option<SpsRangeExtension>,
    /// `sps_scc_extension()`'s one field a slice header depends on.
    pub scc_extension: Option<SpsSccExtension>,
}

/// The `pcm_*` block of §7.3.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PcmParameters {
    /// `pcm_sample_bit_depth_luma_minus1 + 1`.
    pub sample_bit_depth_luma: u8,
    /// `pcm_sample_bit_depth_chroma_minus1 + 1`.
    pub sample_bit_depth_chroma: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3 + 3`.
    pub log2_min_cb_size: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size`.
    pub log2_diff_max_min_cb_size: u8,
    /// `pcm_loop_filter_disabled_flag`.
    pub loop_filter_disabled: bool,
}

/// `sps_range_extension()`, §7.3.2.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct SpsRangeExtension {
    /// `transform_skip_rotation_enabled_flag`.
    pub transform_skip_rotation_enabled: bool,
    /// `transform_skip_context_enabled_flag`.
    pub transform_skip_context_enabled: bool,
    /// `implicit_rdpcm_enabled_flag`.
    pub implicit_rdpcm_enabled: bool,
    /// `explicit_rdpcm_enabled_flag`.
    pub explicit_rdpcm_enabled: bool,
    /// `extended_precision_processing_flag`.
    pub extended_precision_processing: bool,
    /// `intra_smoothing_disabled_flag`.
    pub intra_smoothing_disabled: bool,
    /// `high_precision_offsets_enabled_flag`. Widens `luma_offset_l0` in a
    /// slice's prediction weight table, so a parser needs it.
    pub high_precision_offsets_enabled: bool,
    /// `persistent_rice_adaptation_enabled_flag`.
    pub persistent_rice_adaptation_enabled: bool,
    /// `cabac_bypass_alignment_enabled_flag`.
    pub cabac_bypass_alignment_enabled: bool,
}

/// The two `sps_scc_extension()` fields a slice segment header's syntax depends
/// on, §7.3.2.2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpsSccExtension {
    /// `sps_curr_pic_ref_enabled_flag`. Adds one to `NumPicTotalCurr`, which
    /// decides whether `ref_pic_lists_modification()` is present at all.
    pub curr_pic_ref_enabled: bool,
    /// `motion_vector_resolution_control_idc`. A value of 2 adds
    /// `use_integer_mv_flag` to every P and B slice header.
    pub motion_vector_resolution_control_idc: u8,
    /// `intra_boundary_filtering_disabled_flag`.
    pub intra_boundary_filtering_disabled: bool,
    /// `palette_mode_enabled_flag`.
    pub palette_mode_enabled: bool,
}

/// The raw `scaling_list_data()`, §7.3.4.
///
/// Four sizes; six matrices each except at size 3, where only matrices 0 and 3
/// are coded. A list whose `pred_mode` flag is 0 refers to an earlier one and
/// is left at zeros here — its effective value comes from the fall-back rules,
/// which this crate does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingListData {
    /// `scaling_list_pred_mode_flag[sizeId][matrixId]`.
    pub pred_mode: [[bool; 6]; 4],
    /// `scaling_list_pred_matrix_id_delta[sizeId][matrixId]`.
    pub pred_matrix_id_delta: [[u32; 6]; 4],
    /// `scaling_list_dc_coef_minus8[sizeId - 2][matrixId] + 8`, for sizes 2
    /// and 3.
    pub dc_coef: [[i32; 6]; 2],
    /// The coefficients: up to 64 per matrix, and only 16 at size 0.
    pub coef: [[[u8; 64]; 6]; 4],
}

impl Default for ScalingListData {
    fn default() -> Self {
        Self {
            pred_mode: [[false; 6]; 4],
            pred_matrix_id_delta: [[0; 6]; 4],
            dc_coef: [[8; 6]; 2],
            coef: [[[0; 64]; 6]; 4],
        }
    }
}

// ------------------------------------------------------------------- derived

impl Sps {
    /// `ChromaArrayType`, §7.4.3.2: the chroma format *as decoded*, which is 0
    /// when the three 4:4:4 planes are coded separately.
    ///
    /// Everywhere except the SPS syntax itself this — not `chroma_format_idc` —
    /// is the value that matters, and confusing the two is how a 4:4:4 stream
    /// with separate planes gets the wrong conformance window.
    #[must_use]
    pub const fn chroma_array_type(&self) -> ChromaFormat {
        if self.separate_colour_plane {
            ChromaFormat::Monochrome
        } else {
            self.chroma_format
        }
    }

    /// The **coded** luma width, `pic_width_in_luma_samples`.
    ///
    /// Already a multiple of `MinCbSizeY`, so a 1918-wide source is coded as
    /// 1920 and the conformance window takes the two columns back. Unlike
    /// H.264's macroblock alignment this is a *variable* granularity: an
    /// encoder with an 8-sample minimum coding block pads to 8, one with 64 pads
    /// to 64.
    #[must_use]
    pub const fn coded_width(&self) -> u32 {
        self.pic_width_in_luma_samples
    }

    /// The coded luma height, `pic_height_in_luma_samples`.
    #[must_use]
    pub const fn coded_height(&self) -> u32 {
        self.pic_height_in_luma_samples
    }

    /// `MinCbSizeY`, §7.4.3.2: `1 << log2_min_luma_coding_block_size`.
    #[must_use]
    pub const fn min_cb_size(&self) -> u32 {
        1u32.wrapping_shl(self.log2_min_cb_size as u32)
    }

    /// `CtbSizeY`, §7.4.3.2 — the coding tree block edge, 16, 32 or 64.
    #[must_use]
    pub const fn ctb_size(&self) -> u32 {
        1u32.wrapping_shl(self.log2_min_cb_size as u32 + self.log2_diff_max_min_cb_size as u32)
    }

    /// `PicWidthInCtbsY`, §7.4.3.2: `Ceil( pic_width_in_luma_samples / CtbSizeY )`.
    #[must_use]
    pub const fn pic_width_in_ctbs(&self) -> u32 {
        let ctb = self.ctb_size();
        if ctb == 0 {
            return 0;
        }
        self.pic_width_in_luma_samples.div_ceil(ctb)
    }

    /// `PicHeightInCtbsY`, §7.4.3.2.
    #[must_use]
    pub const fn pic_height_in_ctbs(&self) -> u32 {
        let ctb = self.ctb_size();
        if ctb == 0 {
            return 0;
        }
        self.pic_height_in_luma_samples.div_ceil(ctb)
    }

    /// `PicSizeInCtbsY`, §7.4.3.2 — what sizes `slice_segment_address`.
    #[must_use]
    pub const fn pic_size_in_ctbs(&self) -> u64 {
        (self.pic_width_in_ctbs() as u64) * (self.pic_height_in_ctbs() as u64)
    }

    /// The number of bits `slice_segment_address` occupies, §7.3.6.1:
    /// `Ceil( Log2( PicSizeInCtbsY ) )`.
    #[must_use]
    pub const fn slice_address_bits(&self) -> u32 {
        ceil_log2(self.pic_size_in_ctbs())
    }

    /// The displayed luma width and height, §7.4.3.2 — the two numbers
    /// `ffprobe` prints as `width` and `height`.
    ///
    /// The conformance window's offsets are in **chroma units**: for 4:2:0 a
    /// `conf_win_right_offset` of 1 removes *two* luma columns. A 1918x1078
    /// stream from `x265` is coded 1920x1080 with right and bottom offsets of 1
    /// each; a 66x34 one is coded 72x40 with offsets of 3.
    ///
    /// Returns `None` if the window would leave nothing.
    ///
    /// # `default_display_window` is deliberately not applied
    ///
    /// §E.2.1's `def_disp_win_*_offset` is a *display* hint, not the picture
    /// size, and the reference does not apply it either unless its
    /// `apply_defdispwin` option is set — which is off by default. The offsets
    /// are still parsed and available at
    /// [`VuiParameters::default_display_window`] for a caller that wants them.
    #[must_use]
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let (cw, ch) = (self.coded_width(), self.coded_height());
        let Some(win) = self.conformance_window else {
            return (cw > 0 && ch > 0).then_some((cw, ch));
        };
        let c = self.chroma_array_type();
        let dx = win
            .left
            .checked_add(win.right)?
            .checked_mul(c.sub_width_c())?;
        let dy = win
            .top
            .checked_add(win.bottom)?
            .checked_mul(c.sub_height_c())?;
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
    /// §7.4.3.2 infers `chroma_sample_loc_type_top_field` to be 0 — which is
    /// "left" — whenever `chroma_loc_info_present_flag` is 0, regardless of the
    /// chroma format.
    ///
    /// `// D17:` the reference applies that inference **only for 4:2:0**.
    /// Probed on `x265` output at three chroma formats, all with
    /// `chroma_loc_info_present_flag = 0` (confirmed by `-bsf:v trace_headers`):
    ///
    /// ```text
    /// chroma_format_idc = 1 (4:2:0)  ->  chroma_location=left
    /// chroma_format_idc = 2 (4:2:2)  ->  chroma_location=unspecified
    /// chroma_format_idc = 3 (4:4:4)  ->  chroma_location=unspecified
    /// chroma_format_idc = 0 (mono)   ->  chroma_location=unspecified
    /// ```
    ///
    /// This is also where HEVC and H.264 diverge in the reference:
    /// `vaco-parse-h264` measured `left` for 4:2:2, 4:4:4 *and* monochrome. So
    /// the rule is per-codec, not a shared inference, and the two crates
    /// deliberately disagree. `chroma_location` is printed by `-show_streams`,
    /// so this is observable and D17 says to reproduce it.
    #[must_use]
    pub fn color_info(&self) -> ColorInfo {
        let Some(vui) = self.vui.as_ref() else {
            return ColorInfo::default();
        };
        let (p, t, m) = vui.colour_description.unwrap_or((2, 2, 2));
        let chroma_location = match vui.chroma_sample_loc {
            Some((top, _)) => {
                ChromaLocation::from_h264_loc_type(top as u8).unwrap_or(ChromaLocation::Unspecified)
            }
            // D17: the inference applies only to 4:2:0.
            None if self.chroma_format == ChromaFormat::Yuv420 => ChromaLocation::Left,
            None => ChromaLocation::Unspecified,
        };
        ColorInfo {
            primaries: ColorPrimaries::from_u8(p).unwrap_or_default(),
            transfer: TransferCharacteristic::from_u8(t).unwrap_or_default(),
            matrix: MatrixCoefficients::from_u8(m).unwrap_or_default(),
            range: vui
                .video_full_range
                .map_or(ColorRange::Unspecified, ColorRange::from_full_range_flag),
            chroma_location,
        }
    }

    /// The frame rate the VUI implies, or [`Rational::UNDEFINED`].
    ///
    /// **Not** halved; see [`VuiParameters::frame_rate`] for why HEVC differs
    /// from H.264 here.
    #[must_use]
    pub fn frame_rate(&self) -> Rational {
        self.vui
            .as_ref()
            .map_or(Rational::UNDEFINED, VuiParameters::frame_rate)
    }

    /// `sps_max_num_reorder_pics[ sps_max_sub_layers_minus1 ]` — the reorder
    /// depth at the highest temporal sub-layer, which is what `ffprobe` prints
    /// as `has_b_frames`.
    ///
    /// The *highest* entry, not the first: §7.4.3.2 makes the list monotonic
    /// and the highest sub-layer is the one a decoder of the whole stream sees.
    /// With `sps_sub_layer_ordering_info_present_flag` clear only one value is
    /// coded and it applies to every sub-layer, which this handles by storing
    /// that one value.
    ///
    /// `// D17:` the reference *raises* this if it observes deeper reordering
    /// while decoding. Probed by patching `sps_max_num_reorder_pics[0]` to each
    /// of 0..=5 in a stream that does reorder: `ffprobe` printed 1, 1, 2, 3, 4,
    /// 5 — the declared value except that 0 came back as 1. Raising it requires
    /// decoding and is therefore outside a parser's reach.
    #[must_use]
    pub fn max_num_reorder_pics(&self) -> u32 {
        self.max_num_reorder_pics.last().copied().unwrap_or(0)
    }

    /// `sps_max_dec_pic_buffering_minus1` at the highest sub-layer, plus one.
    #[must_use]
    pub fn max_dec_pic_buffering(&self) -> u32 {
        self.max_dec_pic_buffering_minus1
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// `MaxPicOrderCntLsb`, §7.4.3.2: `2^log2_max_pic_order_cnt_lsb`.
    #[must_use]
    pub const fn max_pic_order_cnt_lsb(&self) -> u32 {
        1u32.wrapping_shl(self.log2_max_pic_order_cnt_lsb as u32)
    }

    /// The profile's display name, as the reference prints it, or `None` for a
    /// profile nothing names.
    #[must_use]
    pub fn profile_name(&self) -> Option<&'static str> {
        let pt = self.ptl.general.as_ref()?;
        crate::profile::profile_name(pt.effective_profile_idc())
    }

    /// `general_tier_flag`, as a [`Tier`](crate::profile::Tier).
    #[must_use]
    pub fn tier(&self) -> crate::profile::Tier {
        crate::profile::Tier::from_flag(self.ptl.general.is_some_and(|p| p.tier_flag))
    }
}

// ------------------------------------------------------------------- parsing

impl Sps {
    /// Parse a sequence parameter set from a NAL unit's RBSP.
    ///
    /// `rbsp` is the whole NAL unit with emulation prevention already removed —
    /// both header bytes included, because that is where the specification's
    /// bit numbering starts.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a syntax element outside its permitted range
    /// or a structurally impossible codeword, [`Error::UnexpectedEof`] for a
    /// truncated unit, and [`Error::LimitExceeded`] when a declared count would
    /// exceed the budget.
    pub fn parse(rbsp: &[u8], budget: &mut Budget) -> Result<Self> {
        let header = HevcNalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if header.nal_unit_type != NalUnitType::SPS_NUT {
            return Err(Error::InvalidData("not a sequence parameter set"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(16); // the two NAL header bytes
        let sps = Self::parse_data(&mut reader, budget)?;
        reader.check()?;
        Ok(sps)
    }

    /// `seq_parameter_set_rbsp()`, §7.3.2.2, from a reader positioned just after
    /// the NAL header.
    ///
    /// Anything after the extension flags is `sps_extension_data_flag` and is
    /// ignored, per §7.3.2.2.
    ///
    /// # Errors
    ///
    /// As [`Sps::parse`].
    #[allow(clippy::too_many_lines, reason = "one specification syntax table")]
    pub fn parse_data(reader: &mut BitReader<'_>, budget: &mut Budget) -> Result<Self> {
        let max_dimension = budget.limits().max_dimension.max(1);
        let mut g = BoundedGolomb::new(reader, budget);

        let vps_id = g.u(4)? as u8;
        let max_sub_layers_minus1 = g.u(3)?;
        let temporal_id_nesting = g.u(1)? != 0;
        let ptl = ProfileTierLevel::parse(&mut g, true, max_sub_layers_minus1)?;
        // §7.4.3.2 bounds `sps_seq_parameter_set_id` at 15.
        let id = g.ue_v(15)? as u8;
        let chroma_format = ChromaFormat::from_idc(g.ue_v(3)?)
            .ok_or(Error::InvalidData("chroma_format_idc out of range"))?;
        let separate_colour_plane = if chroma_format == ChromaFormat::Yuv444 {
            g.u(1)? != 0
        } else {
            false
        };
        let pic_width_in_luma_samples = g.ue_v(max_dimension)?;
        let pic_height_in_luma_samples = g.ue_v(max_dimension)?;
        let conformance_window = if g.u(1)? != 0 {
            // The offsets are in chroma units, so bounding each by the picture
            // size in luma samples is looser than the specification's own
            // constraint but cannot overflow; `Sps::dimensions` rejects what is
            // actually out of range.
            Some(Window {
                left: g.ue_v(pic_width_in_luma_samples)?,
                right: g.ue_v(pic_width_in_luma_samples)?,
                top: g.ue_v(pic_height_in_luma_samples)?,
                bottom: g.ue_v(pic_height_in_luma_samples)?,
            })
        } else {
            None
        };
        // §7.4.3.2 caps both depths at 8, i.e. 16 bits.
        let bit_depth_luma = g.ue_v(8)? as u8 + 8;
        let bit_depth_chroma = g.ue_v(8)? as u8 + 8;
        // §7.4.3.2 caps the log2 field at 12.
        let log2_max_pic_order_cnt_lsb = g.ue_v(12)? as u8 + 4;

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
            // §7.4.3.2 bounds the buffering count by MaxDpbSize - 1, at most 15.
            max_dec_pic_buffering_minus1.push(g.ue_v(15)?);
            max_num_reorder_pics.push(g.ue_v(15)?);
            max_latency_increase_plus1.push(g.ue_v(u32::MAX - 1)?);
        }

        // §7.4.3.2 bounds the coding-block log2 fields so that CtbLog2SizeY is
        // at most 6 and MinCbLog2SizeY at least 3.
        let log2_min_cb_size = g.ue_v(3)? as u8 + 3;
        let log2_diff_max_min_cb_size = g.ue_v(3)? as u8;
        let log2_min_tb_size = g.ue_v(3)? as u8 + 2;
        let log2_diff_max_min_tb_size = g.ue_v(3)? as u8;
        let max_transform_hierarchy_depth_inter = g.ue_v(4)?;
        let max_transform_hierarchy_depth_intra = g.ue_v(4)?;

        let scaling_list_enabled = g.u(1)? != 0;
        let mut scaling_list = None;
        if scaling_list_enabled && g.u(1)? != 0 {
            scaling_list = Some(Box::new(read_scaling_list_data(&mut g)?));
        }
        let amp_enabled = g.u(1)? != 0;
        let sample_adaptive_offset_enabled = g.u(1)? != 0;
        let pcm = if g.u(1)? != 0 {
            Some(PcmParameters {
                sample_bit_depth_luma: g.u(4)? as u8 + 1,
                sample_bit_depth_chroma: g.u(4)? as u8 + 1,
                log2_min_cb_size: g.ue_v(3)? as u8 + 3,
                log2_diff_max_min_cb_size: g.ue_v(3)? as u8,
                loop_filter_disabled: g.u(1)? != 0,
            })
        } else {
            None
        };

        // §7.4.3.2 caps this at 64 outright.
        let num_short_term_ref_pic_sets = g.ue_v(MAX_SHORT_TERM_RPS)?;
        g.budget()
            .consume_fuel(u64::from(num_short_term_ref_pic_sets))?;
        let mut short_term_ref_pic_sets = Vec::new();
        for i in 0..num_short_term_ref_pic_sets {
            let set = parse_st_ref_pic_set(
                &mut g,
                i,
                &short_term_ref_pic_sets,
                num_short_term_ref_pic_sets,
            )?;
            short_term_ref_pic_sets.push(set);
        }

        let long_term_ref_pics_present = g.u(1)? != 0;
        let mut long_term_ref_pics = Vec::new();
        if long_term_ref_pics_present {
            // §7.4.3.2 caps this at 32.
            let n = g.ue_v(MAX_LONG_TERM_RPS)?;
            g.budget().consume_fuel(u64::from(n))?;
            for _ in 0..n {
                let poc_lsb = g.u(u32::from(log2_max_pic_order_cnt_lsb))?;
                long_term_ref_pics.push((poc_lsb, g.u(1)? != 0));
            }
        }

        let temporal_mvp_enabled = g.u(1)? != 0;
        let strong_intra_smoothing_enabled = g.u(1)? != 0;
        let vui = if g.u(1)? != 0 {
            Some(parse_vui(&mut g, max_sub_layers_minus1)?)
        } else {
            None
        };

        let mut range_extension = None;
        let mut scc_extension = None;
        if g.u(1)? != 0 {
            let range = g.u(1)? != 0;
            let multilayer = g.u(1)? != 0;
            let three_d = g.u(1)? != 0;
            let scc = g.u(1)? != 0;
            let _extension_4bits = g.u(4)?;
            if range {
                range_extension = Some(SpsRangeExtension {
                    transform_skip_rotation_enabled: g.u(1)? != 0,
                    transform_skip_context_enabled: g.u(1)? != 0,
                    implicit_rdpcm_enabled: g.u(1)? != 0,
                    explicit_rdpcm_enabled: g.u(1)? != 0,
                    extended_precision_processing: g.u(1)? != 0,
                    intra_smoothing_disabled: g.u(1)? != 0,
                    high_precision_offsets_enabled: g.u(1)? != 0,
                    persistent_rice_adaptation_enabled: g.u(1)? != 0,
                    cabac_bypass_alignment_enabled: g.u(1)? != 0,
                });
            }
            if multilayer {
                // §7.3.2.2.4: one bit, `inter_view_mv_vert_constraint_flag`.
                g.u(1)?;
            }
            // §7.3.2.2.5's `sps_3d_extension()` is only in multi-layer 3D-HEVC
            // streams, which this crate does not describe; reading past it
            // without implementing it would be reading rubbish, so the SCC
            // extension behind it is simply not reached. `scc_extension` stays
            // `None`, which is the conservative answer for the two flags a
            // slice header consults.
            if scc && !three_d {
                scc_extension = Some(parse_scc_extension(
                    &mut g,
                    chroma_format,
                    bit_depth_luma,
                    bit_depth_chroma,
                )?);
            }
        }
        // Anything left over is `sps_extension_data_flag`, which §7.3.2.2 says
        // a decoder ignores; the RBSP trailing bits end it.

        Self {
            vps_id,
            max_sub_layers: max_sub_layers_minus1 as u8 + 1,
            temporal_id_nesting,
            ptl,
            id,
            chroma_format,
            separate_colour_plane,
            pic_width_in_luma_samples,
            pic_height_in_luma_samples,
            conformance_window,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_pic_order_cnt_lsb,
            max_dec_pic_buffering_minus1,
            max_num_reorder_pics,
            max_latency_increase_plus1,
            log2_min_cb_size,
            log2_diff_max_min_cb_size,
            log2_min_tb_size,
            log2_diff_max_min_tb_size,
            max_transform_hierarchy_depth_inter,
            max_transform_hierarchy_depth_intra,
            scaling_list_enabled,
            scaling_list,
            amp_enabled,
            sample_adaptive_offset_enabled,
            pcm,
            short_term_ref_pic_sets,
            long_term_ref_pics_present,
            long_term_ref_pics,
            temporal_mvp_enabled,
            strong_intra_smoothing_enabled,
            vui,
            range_extension,
            scc_extension,
        }
        .checked(budget)
    }

    /// The geometry constraints of §7.4.3.2, checked once at the end.
    ///
    /// An SPS whose window leaves nothing has no usable geometry at all, and a
    /// zero coding-block size makes every CTB derivation meaningless — both are
    /// rejected here rather than left to surprise a caller.
    fn checked(self, budget: &mut Budget) -> Result<Self> {
        if self.pic_width_in_luma_samples == 0 || self.pic_height_in_luma_samples == 0 {
            return Err(Error::InvalidData("zero picture dimension"));
        }
        let min_cb = self.min_cb_size();
        if min_cb == 0
            || !self.pic_width_in_luma_samples.is_multiple_of(min_cb)
            || !self.pic_height_in_luma_samples.is_multiple_of(min_cb)
        {
            return Err(Error::InvalidData(
                "picture dimensions are not a multiple of MinCbSizeY",
            ));
        }
        let (w, h) = self.dimensions().ok_or(Error::InvalidData(
            "the conformance window leaves no picture",
        ))?;
        // Four bytes per pixel is the widest packed 8-bit layout; the real
        // pixel format tightens this once it is known.
        budget.check_frame(w.max(self.coded_width()), h.max(self.coded_height()), 4)?;
        Ok(self)
    }
}

/// `sps_scc_extension()`, §7.3.2.2.3.
///
/// Parsed rather than skipped because two of its fields change the *syntax* of
/// every subsequent slice segment header: `sps_curr_pic_ref_enabled_flag` adds
/// one to `NumPicTotalCurr`, which decides whether `ref_pic_lists_modification()`
/// is present at all, and a `motion_vector_resolution_control_idc` of 2 adds a
/// `use_integer_mv_flag`. Skipping it would make screen-content slice headers
/// unreadable rather than merely incompletely described.
fn parse_scc_extension(
    g: &mut BoundedGolomb<'_, '_, '_>,
    chroma_format: ChromaFormat,
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
) -> Result<SpsSccExtension> {
    let curr_pic_ref_enabled = g.u(1)? != 0;
    let palette_mode_enabled = g.u(1)? != 0;
    if palette_mode_enabled {
        // §7.4.3.2.3 bounds `palette_max_size` and the predictor delta at 64
        // and 128 respectively.
        let _palette_max_size = g.ue_v(64)?;
        let _delta_palette_max_predictor_size = g.ue_v(128)?;
        if g.u(1)? != 0 {
            // Bounded by `palette_max_size + delta_palette_max_predictor_size`,
            // which the two caps above put at 192.
            let n = g.ue_v(191)? + 1;
            let comps = if chroma_format == ChromaFormat::Monochrome {
                1u32
            } else {
                3
            };
            g.budget()
                .consume_fuel(u64::from(n).saturating_mul(u64::from(comps)))?;
            for comp in 0..comps {
                let bits = if comp == 0 {
                    u32::from(bit_depth_luma)
                } else {
                    u32::from(bit_depth_chroma)
                };
                for _ in 0..n {
                    g.u(bits)?;
                }
            }
        }
    }
    Ok(SpsSccExtension {
        curr_pic_ref_enabled,
        motion_vector_resolution_control_idc: g.u(2)? as u8,
        intra_boundary_filtering_disabled: g.u(1)? != 0,
        palette_mode_enabled,
    })
}

/// `scaling_list_data()`, §7.3.4.
///
/// Both loops are compile-time bounded — four sizes, six matrices, at most 64
/// coefficients — so nothing here can be driven by input.
pub(crate) fn read_scaling_list_data(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<ScalingListData> {
    let mut out = ScalingListData::default();
    for size_id in 0usize..4 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0usize;
        while matrix_id < 6 {
            let pred_mode = g.u(1)? != 0;
            if let Some(row) = out.pred_mode.get_mut(size_id)
                && let Some(slot) = row.get_mut(matrix_id)
            {
                *slot = pred_mode;
            }
            if pred_mode {
                let coef_num = 64usize.min(1 << (4 + (size_id << 1)));
                let mut next_coef = 8i32;
                if size_id > 1 {
                    // §7.4.5 bounds `scaling_list_dc_coef_minus8` to -7..=247.
                    let dc = g.se_v(-7, 247)? + 8;
                    next_coef = dc;
                    if let Some(row) = out.dc_coef.get_mut(size_id - 2)
                        && let Some(slot) = row.get_mut(matrix_id)
                    {
                        *slot = dc;
                    }
                }
                for i in 0..coef_num {
                    // §7.4.5 bounds `scaling_list_delta_coef` to -128..=127.
                    let delta = g.se_v(-128, 127)?;
                    next_coef = (next_coef + delta + 256).rem_euclid(256);
                    if let Some(m) = out.coef.get_mut(size_id)
                        && let Some(row) = m.get_mut(matrix_id)
                        && let Some(slot) = row.get_mut(i)
                    {
                        *slot = next_coef as u8;
                    }
                }
            } else {
                let delta = g.ue_v(matrix_id as u32)?;
                if let Some(row) = out.pred_matrix_id_delta.get_mut(size_id)
                    && let Some(slot) = row.get_mut(matrix_id)
                {
                    *slot = delta;
                }
            }
            matrix_id += step;
        }
    }
    Ok(out)
}

/// `vui_parameters()`, §E.2.1.
fn parse_vui(
    g: &mut BoundedGolomb<'_, '_, '_>,
    max_sub_layers_minus1: u32,
) -> Result<VuiParameters> {
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
        // §E.3.1 bounds both at 5.
        vui.chroma_sample_loc = Some((g.ue_v(5)?, g.ue_v(5)?));
    }
    vui.neutral_chroma_indication = g.u(1)? != 0;
    vui.field_seq = g.u(1)? != 0;
    vui.frame_field_info_present = g.u(1)? != 0;
    if g.u(1)? != 0 {
        vui.default_display_window = Some(Window {
            left: g.ue_v(u32::MAX - 1)?,
            right: g.ue_v(u32::MAX - 1)?,
            top: g.ue_v(u32::MAX - 1)?,
            bottom: g.ue_v(u32::MAX - 1)?,
        });
    }
    if g.u(1)? != 0 {
        let num_units_in_tick = g.u(32)?;
        let time_scale = g.u(32)?;
        let num_ticks_poc_diff_one_minus1 = if g.u(1)? != 0 {
            Some(g.ue_v(u32::MAX - 1)?)
        } else {
            None
        };
        vui.timing = Some(Timing {
            num_units_in_tick,
            time_scale,
            num_ticks_poc_diff_one_minus1,
        });
        if g.u(1)? != 0 {
            vui.hrd = Some(parse_hrd(g, true, max_sub_layers_minus1)?);
        }
    }
    if g.u(1)? != 0 {
        vui.bitstream_restriction = Some(BitstreamRestriction {
            tiles_fixed_structure: g.u(1)? != 0,
            motion_vectors_over_pic_boundaries: g.u(1)? != 0,
            restricted_ref_pic_lists: g.u(1)? != 0,
            // §E.3.1 bounds this at 4095.
            min_spatial_segmentation_idc: g.ue_v(4095)?,
            max_bytes_per_pic_denom: g.ue_v(16)?,
            max_bits_per_min_cu_denom: g.ue_v(16)?,
            log2_max_mv_length_horizontal: g.ue_v(16)?,
            log2_max_mv_length_vertical: g.ue_v(16)?,
        });
    }
    Ok(vui)
}

/// `hrd_parameters( commonInfPresentFlag, maxNumSubLayersMinus1 )`, §E.2.2.
pub(crate) fn parse_hrd(
    g: &mut BoundedGolomb<'_, '_, '_>,
    common_inf_present: bool,
    max_sub_layers_minus1: u32,
) -> Result<HrdParameters> {
    let mut hrd = HrdParameters::default();
    if common_inf_present {
        hrd.nal_hrd_present = g.u(1)? != 0;
        hrd.vcl_hrd_present = g.u(1)? != 0;
        if hrd.nal_hrd_present || hrd.vcl_hrd_present {
            if g.u(1)? != 0 {
                hrd.sub_pic = Some(SubPicHrd {
                    tick_divisor_minus2: g.u(8)? as u8,
                    du_cpb_removal_delay_increment_length_minus1: g.u(5)? as u8,
                    sub_pic_cpb_params_in_pic_timing_sei: g.u(1)? != 0,
                    dpb_output_delay_du_length_minus1: g.u(5)? as u8,
                });
            }
            hrd.bit_rate_scale = g.u(4)? as u8;
            hrd.cpb_size_scale = g.u(4)? as u8;
            if hrd.sub_pic.is_some() {
                hrd.cpb_size_du_scale = g.u(4)? as u8;
            }
            hrd.initial_cpb_removal_delay_length_minus1 = g.u(5)? as u8;
            hrd.au_cpb_removal_delay_length_minus1 = g.u(5)? as u8;
            hrd.dpb_output_delay_length_minus1 = g.u(5)? as u8;
        }
    }

    let sub_pic = hrd.sub_pic.is_some();
    for _ in 0..=max_sub_layers_minus1.min(MAX_SUB_LAYERS - 1) {
        let mut layer = SubLayerHrd {
            fixed_pic_rate_general: g.u(1)? != 0,
            ..SubLayerHrd::default()
        };
        // §E.3.2: `fixed_pic_rate_within_cvs_flag[i]` is inferred to equal
        // `fixed_pic_rate_general_flag[i]` when the latter is 1, and is coded
        // otherwise.
        layer.fixed_pic_rate_within_cvs = if layer.fixed_pic_rate_general {
            true
        } else {
            g.u(1)? != 0
        };
        if layer.fixed_pic_rate_within_cvs {
            // §E.3.2 bounds this at 2047.
            layer.elemental_duration_in_tc_minus1 = Some(g.ue_v(2047)?);
        } else {
            layer.low_delay_hrd = g.u(1)? != 0;
        }
        if !layer.low_delay_hrd {
            // §E.3.2 bounds `cpb_cnt_minus1` at 31.
            layer.cpb_cnt_minus1 = g.ue_v(31)?;
        }
        let cpb_cnt = layer.cpb_cnt_minus1.saturating_add(1);
        if hrd.nal_hrd_present {
            layer.nal_cpb = read_sub_layer_hrd(g, cpb_cnt, sub_pic)?;
        }
        if hrd.vcl_hrd_present {
            layer.vcl_cpb = read_sub_layer_hrd(g, cpb_cnt, sub_pic)?;
        }
        hrd.sub_layers.push(layer);
    }
    Ok(hrd)
}

/// `sub_layer_hrd_parameters( i )`, §E.2.3.
fn read_sub_layer_hrd(
    g: &mut BoundedGolomb<'_, '_, '_>,
    cpb_cnt: u32,
    sub_pic: bool,
) -> Result<Vec<CpbEntry>> {
    g.budget().consume_fuel(u64::from(cpb_cnt))?;
    let mut out = g.budget().alloc::<CpbEntry>(cpb_cnt as usize)?;
    out.clear();
    for _ in 0..cpb_cnt {
        // The values run to 2^32 - 2, which a `ue(v)` prefix of 31 zeros can
        // still express, so the bound is the type's rather than a policy.
        let bit_rate_value_minus1 = g.ue_v(u32::MAX - 1)?;
        let cpb_size_value_minus1 = g.ue_v(u32::MAX - 1)?;
        let (cpb_size_du_value_minus1, bit_rate_du_value_minus1) = if sub_pic {
            (g.ue_v(u32::MAX - 1)?, g.ue_v(u32::MAX - 1)?)
        } else {
            (0, 0)
        };
        out.push(CpbEntry {
            bit_rate_value_minus1,
            cpb_size_value_minus1,
            cpb_size_du_value_minus1,
            bit_rate_du_value_minus1,
            cbr: g.u(1)? != 0,
        });
    }
    Ok(out)
}
