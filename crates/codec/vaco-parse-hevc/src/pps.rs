//! The picture parameter set, ITU-T H.265 §7.3.2.3.
//!
//! Almost every field here exists because a *slice segment header* consults it:
//! `dependent_slice_segments_enabled_flag` decides whether the header has a
//! `dependent_slice_segment_flag`, `num_extra_slice_header_bits` inserts that
//! many bits, `tiles_enabled_flag` and `entropy_coding_sync_enabled_flag`
//! together decide whether entry point offsets follow. A PPS that has not been
//! read is a slice header that cannot be.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};
use crate::sps::{ScalingListData, read_scaling_list_data};

/// The largest `num_tile_columns_minus1` or `num_tile_rows_minus1` accepted.
///
/// §7.4.3.3 bounds each by `PicWidthInCtbsY - 1` / `PicHeightInCtbsY - 1`, which
/// needs the SPS. This is the loose structural bound instead — Annex A's largest
/// `MaxTileCols` is 20 and `MaxTileRows` 22, so 1024 is far past anything real
/// and small enough that a hostile count cannot allocate.
const MAX_TILES: u32 = 1024;

/// The `tiles_enabled_flag` block, §7.3.2.3.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tiles {
    /// `num_tile_columns_minus1 + 1`.
    pub num_columns: u32,
    /// `num_tile_rows_minus1 + 1`.
    pub num_rows: u32,
    /// `uniform_spacing_flag`.
    pub uniform_spacing: bool,
    /// `column_width_minus1[i] + 1`, empty when the spacing is uniform.
    pub column_widths: Vec<u32>,
    /// `row_height_minus1[i] + 1`, empty when the spacing is uniform.
    pub row_heights: Vec<u32>,
    /// `loop_filter_across_tiles_enabled_flag`.
    pub loop_filter_across_tiles: bool,
}

/// The `deblocking_filter_control_present_flag` block, §7.3.2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeblockingControl {
    /// `deblocking_filter_override_enabled_flag`. Adds
    /// `deblocking_filter_override_flag` to every independent slice header.
    pub override_enabled: bool,
    /// `pps_deblocking_filter_disabled_flag`.
    pub disabled: bool,
    /// `pps_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `pps_tc_offset_div2`.
    pub tc_offset_div2: i32,
}

/// `pps_range_extension()`, §7.3.2.3.2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PpsRangeExtension {
    /// `log2_max_transform_skip_block_size_minus2`.
    pub log2_max_transform_skip_block_size_minus2: u32,
    /// `cross_component_prediction_enabled_flag`.
    pub cross_component_prediction_enabled: bool,
    /// `chroma_qp_offset_list_enabled_flag`. Adds
    /// `cu_chroma_qp_offset_enabled_flag` to every P/B slice header.
    pub chroma_qp_offset_list_enabled: bool,
    /// `diff_cu_chroma_qp_offset_depth`.
    pub diff_cu_chroma_qp_offset_depth: u32,
    /// `cb_qp_offset_list[i]`.
    pub cb_qp_offset_list: Vec<i32>,
    /// `cr_qp_offset_list[i]`.
    pub cr_qp_offset_list: Vec<i32>,
    /// `log2_sao_offset_scale_luma`.
    pub log2_sao_offset_scale_luma: u32,
    /// `log2_sao_offset_scale_chroma`.
    pub log2_sao_offset_scale_chroma: u32,
}

/// The one `pps_scc_extension()` field a slice segment header consults,
/// §7.3.2.3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PpsSccExtension {
    /// `pps_curr_pic_ref_enabled_flag`. Adds one to `NumPicTotalCurr`.
    pub curr_pic_ref_enabled: bool,
    /// `pps_slice_act_qp_offsets_present_flag`. Adds three `se(v)` to every
    /// independent slice header.
    pub slice_act_qp_offsets_present: bool,
}

/// A picture parameter set: §7.3.2.3, in field order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct Pps {
    /// `pps_pic_parameter_set_id`, 0..=63.
    pub id: u8,
    /// `pps_seq_parameter_set_id`, 0..=15.
    pub sps_id: u8,
    /// `dependent_slice_segments_enabled_flag`.
    pub dependent_slice_segments_enabled: bool,
    /// `output_flag_present_flag`.
    pub output_flag_present: bool,
    /// `num_extra_slice_header_bits`, 0..=7.
    pub num_extra_slice_header_bits: u8,
    /// `sign_data_hiding_enabled_flag`.
    pub sign_data_hiding_enabled: bool,
    /// `cabac_init_present_flag`.
    pub cabac_init_present: bool,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u32,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u32,
    /// `init_qp_minus26`.
    pub init_qp_minus26: i32,
    /// `constrained_intra_pred_flag`.
    pub constrained_intra_pred: bool,
    /// `transform_skip_enabled_flag`.
    pub transform_skip_enabled: bool,
    /// `cu_qp_delta_enabled_flag`.
    pub cu_qp_delta_enabled: bool,
    /// `diff_cu_qp_delta_depth`, 0 when the flag above is clear.
    pub diff_cu_qp_delta_depth: u32,
    /// `pps_cb_qp_offset`.
    pub cb_qp_offset: i32,
    /// `pps_cr_qp_offset`.
    pub cr_qp_offset: i32,
    /// `pps_slice_chroma_qp_offsets_present_flag`.
    pub slice_chroma_qp_offsets_present: bool,
    /// `weighted_pred_flag`.
    pub weighted_pred: bool,
    /// `weighted_bipred_flag`.
    pub weighted_bipred: bool,
    /// `transquant_bypass_enabled_flag`.
    pub transquant_bypass_enabled: bool,
    /// The tile layout, or `None` when `tiles_enabled_flag` was 0.
    pub tiles: Option<Tiles>,
    /// `entropy_coding_sync_enabled_flag` — wavefront parallel processing.
    pub entropy_coding_sync_enabled: bool,
    /// `pps_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices_enabled: bool,
    /// The deblocking control block, or `None`.
    pub deblocking: Option<DeblockingControl>,
    /// The raw `scaling_list_data()`, when the PPS carried one.
    pub scaling_list: Option<Box<ScalingListData>>,
    /// `lists_modification_present_flag`.
    pub lists_modification_present: bool,
    /// `log2_parallel_merge_level_minus2 + 2`.
    pub log2_parallel_merge_level: u32,
    /// `slice_segment_header_extension_present_flag`.
    pub slice_segment_header_extension_present: bool,
    /// `pps_range_extension()`.
    pub range_extension: Option<PpsRangeExtension>,
    /// `pps_scc_extension()`.
    pub scc_extension: Option<PpsSccExtension>,
}

impl Pps {
    /// Whether a slice segment header for this PPS carries entry point offsets,
    /// §7.3.6.1.
    #[must_use]
    pub const fn has_entry_points(&self) -> bool {
        self.tiles.is_some() || self.entropy_coding_sync_enabled
    }

    /// Parse a picture parameter set from a NAL unit's RBSP.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the unit is not a PPS or a syntax element is
    /// out of range, [`Error::UnexpectedEof`] on truncation, or a budget error.
    pub fn parse(rbsp: &[u8], budget: &mut Budget) -> Result<Self> {
        let header = HevcNalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if header.nal_unit_type != NalUnitType::PPS_NUT {
            return Err(Error::InvalidData("not a picture parameter set"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(16);
        let pps = Self::parse_data(&mut reader, budget)?;
        reader.check()?;
        Ok(pps)
    }

    /// `pic_parameter_set_rbsp()`, §7.3.2.3, from a reader positioned just after
    /// the NAL header.
    ///
    /// Unlike H.264's, HEVC's PPS needs **no** SPS to parse: nothing in it is
    /// sized by a sequence-level field. That is why this takes no parameter-set
    /// store and why the store can accept parameter sets in any order.
    ///
    /// # Errors
    ///
    /// As [`Pps::parse`].
    #[allow(clippy::too_many_lines, reason = "one specification syntax table")]
    pub fn parse_data(reader: &mut BitReader<'_>, budget: &mut Budget) -> Result<Self> {
        let mut g = BoundedGolomb::new(reader, budget);
        // §7.4.3.3 bounds the two ids at 63 and 15.
        let id = g.ue_v(63)? as u8;
        let sps_id = g.ue_v(15)? as u8;
        let dependent_slice_segments_enabled = g.u(1)? != 0;
        let output_flag_present = g.u(1)? != 0;
        let num_extra_slice_header_bits = g.u(3)? as u8;
        let sign_data_hiding_enabled = g.u(1)? != 0;
        let cabac_init_present = g.u(1)? != 0;
        // §7.4.3.3 bounds both at 14.
        let num_ref_idx_l0_default_active_minus1 = g.ue_v(14)?;
        let num_ref_idx_l1_default_active_minus1 = g.ue_v(14)?;
        // §7.4.3.3: -(26 + QpBdOffsetY) .. +25, and QpBdOffsetY is at most 48.
        let init_qp_minus26 = g.se_v(-74, 25)?;
        let constrained_intra_pred = g.u(1)? != 0;
        let transform_skip_enabled = g.u(1)? != 0;
        let cu_qp_delta_enabled = g.u(1)? != 0;
        let diff_cu_qp_delta_depth = if cu_qp_delta_enabled { g.ue_v(3)? } else { 0 };
        // §7.4.3.3 bounds both chroma offsets to -12..=12.
        let cb_qp_offset = g.se_v(-12, 12)?;
        let cr_qp_offset = g.se_v(-12, 12)?;
        let slice_chroma_qp_offsets_present = g.u(1)? != 0;
        let weighted_pred = g.u(1)? != 0;
        let weighted_bipred = g.u(1)? != 0;
        let transquant_bypass_enabled = g.u(1)? != 0;
        let tiles_enabled = g.u(1)? != 0;
        let entropy_coding_sync_enabled = g.u(1)? != 0;

        let tiles = if tiles_enabled {
            let num_columns = g.ue_v(MAX_TILES)? + 1;
            let num_rows = g.ue_v(MAX_TILES)? + 1;
            let uniform_spacing = g.u(1)? != 0;
            let mut column_widths = Vec::new();
            let mut row_heights = Vec::new();
            if !uniform_spacing {
                // Charge both loops before either runs.
                g.budget()
                    .consume_fuel(u64::from(num_columns) + u64::from(num_rows))?;
                for _ in 1..num_columns {
                    column_widths.push(g.ue_v(u32::MAX - 1)? + 1);
                }
                for _ in 1..num_rows {
                    row_heights.push(g.ue_v(u32::MAX - 1)? + 1);
                }
            }
            Some(Tiles {
                num_columns,
                num_rows,
                uniform_spacing,
                column_widths,
                row_heights,
                loop_filter_across_tiles: g.u(1)? != 0,
            })
        } else {
            None
        };

        let loop_filter_across_slices_enabled = g.u(1)? != 0;
        let deblocking = if g.u(1)? != 0 {
            let override_enabled = g.u(1)? != 0;
            let disabled = g.u(1)? != 0;
            let (beta_offset_div2, tc_offset_div2) = if disabled {
                (0, 0)
            } else {
                // §7.4.3.3 bounds both to -6..=6.
                (g.se_v(-6, 6)?, g.se_v(-6, 6)?)
            };
            Some(DeblockingControl {
                override_enabled,
                disabled,
                beta_offset_div2,
                tc_offset_div2,
            })
        } else {
            None
        };

        let scaling_list = if g.u(1)? != 0 {
            Some(Box::new(read_scaling_list_data(&mut g)?))
        } else {
            None
        };
        let lists_modification_present = g.u(1)? != 0;
        // §7.4.3.3 bounds this so that the merge level is at most CtbLog2SizeY.
        let log2_parallel_merge_level = g.ue_v(4)? + 2;
        let slice_segment_header_extension_present = g.u(1)? != 0;

        let mut range_extension = None;
        let mut scc_extension = None;
        if g.u(1)? != 0 {
            let range = g.u(1)? != 0;
            let multilayer = g.u(1)? != 0;
            let three_d = g.u(1)? != 0;
            let scc = g.u(1)? != 0;
            let _extension_4bits = g.u(4)?;
            if range {
                range_extension = Some(read_range_extension(&mut g, transform_skip_enabled)?);
            }
            // The multilayer and 3D extensions carry syntax this crate does not
            // describe, so anything behind them is unreachable; see the same
            // note in `sps::Sps::parse_data`.
            if scc && !multilayer && !three_d {
                scc_extension = Some(read_scc_extension(&mut g)?);
            }
        }

        Ok(Self {
            id,
            sps_id,
            dependent_slice_segments_enabled,
            output_flag_present,
            num_extra_slice_header_bits,
            sign_data_hiding_enabled,
            cabac_init_present,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            init_qp_minus26,
            constrained_intra_pred,
            transform_skip_enabled,
            cu_qp_delta_enabled,
            diff_cu_qp_delta_depth,
            cb_qp_offset,
            cr_qp_offset,
            slice_chroma_qp_offsets_present,
            weighted_pred,
            weighted_bipred,
            transquant_bypass_enabled,
            tiles,
            entropy_coding_sync_enabled,
            loop_filter_across_slices_enabled,
            deblocking,
            scaling_list,
            lists_modification_present,
            log2_parallel_merge_level,
            slice_segment_header_extension_present,
            range_extension,
            scc_extension,
        })
    }
}

/// `pps_range_extension()`, §7.3.2.3.2.
fn read_range_extension(
    g: &mut BoundedGolomb<'_, '_, '_>,
    transform_skip_enabled: bool,
) -> Result<PpsRangeExtension> {
    let log2_max_transform_skip_block_size_minus2 = if transform_skip_enabled {
        g.ue_v(3)?
    } else {
        0
    };
    let cross_component_prediction_enabled = g.u(1)? != 0;
    let chroma_qp_offset_list_enabled = g.u(1)? != 0;
    let mut diff_cu_chroma_qp_offset_depth = 0;
    let mut cb_qp_offset_list = Vec::new();
    let mut cr_qp_offset_list = Vec::new();
    if chroma_qp_offset_list_enabled {
        diff_cu_chroma_qp_offset_depth = g.ue_v(3)?;
        // §7.4.3.3.2 bounds `chroma_qp_offset_list_len_minus1` at 5.
        let len = g.ue_v(5)? + 1;
        for _ in 0..len {
            cb_qp_offset_list.push(g.se_v(-12, 12)?);
            cr_qp_offset_list.push(g.se_v(-12, 12)?);
        }
    }
    Ok(PpsRangeExtension {
        log2_max_transform_skip_block_size_minus2,
        cross_component_prediction_enabled,
        chroma_qp_offset_list_enabled,
        diff_cu_chroma_qp_offset_depth,
        cb_qp_offset_list,
        cr_qp_offset_list,
        // §7.4.3.3.2 bounds both by BitDepth - 10, so at most 6.
        log2_sao_offset_scale_luma: g.ue_v(6)?,
        log2_sao_offset_scale_chroma: g.ue_v(6)?,
    })
}

/// `pps_scc_extension()`, §7.3.2.3.3 — read as far as the two fields a slice
/// segment header needs, then the palette block that follows them.
fn read_scc_extension(g: &mut BoundedGolomb<'_, '_, '_>) -> Result<PpsSccExtension> {
    let curr_pic_ref_enabled = g.u(1)? != 0;
    let residual_adaptive_colour_transform_enabled = g.u(1)? != 0;
    let mut slice_act_qp_offsets_present = false;
    if residual_adaptive_colour_transform_enabled {
        slice_act_qp_offsets_present = g.u(1)? != 0;
        // §7.4.3.3.3 bounds the three ACT offsets to -12..=12.
        g.se_v(-12, 12)?;
        g.se_v(-12, 12)?;
        g.se_v(-12, 12)?;
    }
    Ok(PpsSccExtension {
        curr_pic_ref_enabled,
        slice_act_qp_offsets_present,
    })
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

    /// The PPS `x265` writes for a Main-profile stream, byte for byte from
    /// `sd.265`.
    const REAL_PPS_EBSP: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40];

    fn parse(ebsp: &[u8]) -> Pps {
        let mut scratch = Vec::new();
        let rbsp = vaco_bitstream::annexb::to_rbsp(ebsp, &mut scratch);
        let mut budget = Budget::new(Limits::strict());
        Pps::parse(rbsp, &mut budget).expect("a real PPS parses")
    }

    /// Every field, against `ffmpeg -bsf:v trace_headers` on the same file.
    #[test]
    fn a_real_pps_field_by_field() {
        let pps = parse(REAL_PPS_EBSP);
        assert_eq!(pps.id, 0);
        assert_eq!(pps.sps_id, 0);
        assert!(!pps.dependent_slice_segments_enabled);
        assert!(!pps.output_flag_present);
        assert_eq!(pps.num_extra_slice_header_bits, 0);
        assert!(pps.sign_data_hiding_enabled);
        assert!(!pps.cabac_init_present);
        assert_eq!(pps.num_ref_idx_l0_default_active_minus1, 0);
        assert_eq!(pps.num_ref_idx_l1_default_active_minus1, 0);
        assert_eq!(pps.init_qp_minus26, 0);
        assert!(!pps.constrained_intra_pred);
        assert!(!pps.transform_skip_enabled);
        assert!(pps.cu_qp_delta_enabled);
        assert_eq!(pps.diff_cu_qp_delta_depth, 1);
        assert_eq!(pps.cb_qp_offset, 0);
        assert_eq!(pps.cr_qp_offset, 0);
        assert!(!pps.slice_chroma_qp_offsets_present);
        assert!(pps.weighted_pred);
        assert!(!pps.weighted_bipred);
        assert!(!pps.transquant_bypass_enabled);
        assert!(pps.tiles.is_none());
        assert!(pps.entropy_coding_sync_enabled);
        assert!(pps.loop_filter_across_slices_enabled);
        assert!(pps.deblocking.is_none());
        assert!(pps.scaling_list.is_none());
        assert!(!pps.lists_modification_present);
        assert_eq!(pps.log2_parallel_merge_level, 2);
        assert!(!pps.slice_segment_header_extension_present);
        assert!(pps.range_extension.is_none());
        // Wavefront parallel processing alone still means entry points.
        assert!(pps.has_entry_points());
    }

    #[test]
    fn a_unit_of_the_wrong_type_is_refused() {
        let mut data = REAL_PPS_EBSP.to_vec();
        data[0] = 0x40;
        let mut budget = Budget::new(Limits::strict());
        assert!(matches!(
            Pps::parse(&data, &mut budget),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncation_is_handled() {
        for n in 0..REAL_PPS_EBSP.len() {
            let mut budget = Budget::new(Limits::strict());
            let _ = Pps::parse(&REAL_PPS_EBSP[..n], &mut budget);
        }
    }
}
