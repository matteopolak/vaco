//! `slice_segment_header()`, ITU-T H.265 §7.3.6.1, and its three
//! sub-structures.
//!
//! # HEVC makes picture boundaries easy, and that is the headline
//!
//! H.264 has no marker for "this slice starts a new picture"; §7.4.1.2.4 makes
//! a parser compare seven fields of consecutive slice headers to work it out,
//! and `vaco-parse-h264` implements exactly that. HEVC replaced the whole
//! problem with one bit: **`first_slice_segment_in_pic_flag`**, the first bit of
//! every slice segment header. So access-unit detection here is a bit test
//! rather than a comparison, and the parser is correspondingly simpler and
//! harder to get wrong.
//!
//! # One place a parser cannot be exact, and what is done about it
//!
//! `pred_weight_table()` (§7.3.6.3) reads `luma_weight_l0_flag[i]` only when the
//! *i*-th reference picture differs from the current picture in layer or
//! picture order count. That condition is a question about the reference
//! picture **list**, which is built from the decoded picture buffer — state a
//! header parser does not have and cannot get without decoding.
//!
//! The condition is false only when a reference *is* the current picture, which
//! requires `pps_curr_pic_ref_enabled_flag` — a screen-content-coding feature.
//! So the flags are read unconditionally here, which is exact for every stream
//! that is not SCC, and [`SliceHeader::weight_table_exact`] says which case
//! applies. A header whose tail is misread is reported as
//! [`Error::InvalidData`](vaco_core::Error::InvalidData) by the caller's
//! `check()`, not silently accepted.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::BoundedGolomb;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};
use crate::pps::Pps;
use crate::rps::{ShortTermRps, parse_st_ref_pic_set};
use crate::sps::{ChromaFormat, Sps};
use crate::util::{MAX_ENTRY_POINTS, MAX_SLICE_HEADER_EXTENSION, ceil_log2};

/// The bound §7.4.7.1 puts on `num_entry_point_offsets`, computed from the
/// picture geometry rather than taken as a flat ceiling.
///
/// The three cases are the specification's own:
///
/// ```text
///   tiles off, WPP on   ->  PicHeightInCtbsY - 1
///   tiles on,  WPP off  ->  num_tile_columns * num_tile_rows - 1
///   both on             ->  num_tile_columns * PicHeightInCtbsY - 1
/// ```
///
/// # Why this is computed rather than fixed, and what that did NOT buy
///
/// It was a flat 8192 first — a legitimate bound, since no real stream comes
/// near it. The geometry-derived version is strictly tighter: a 1080p wavefront
/// stream admits 16 entry points rather than 8192, so a hostile header is
/// refused 500x earlier.
///
/// **It is not a measured speedup, and the first version of this comment
/// claimed it was.** The hypothesis was that a random `ue(v)` under a ceiling of
/// 8192 would decode into the thousands and cost that many `u(v)` reads,
/// dominating the whole-stream benchmark. Measured A/B on the `slice_header`
/// bench — the same header with a random tail, once under each bound — the two
/// are **252 ns and 263 ns**: within noise, and if anything the looser bound was
/// marginally faster. The whole-stream number did not move either (1.365 ms
/// before, 1.348 ms after, on a megabyte).
///
/// Recorded because plan 12's PF-0.1 through PF-0.3 amendments exist for exactly
/// this: three confident performance assumptions on this project have measured
/// backwards. This is a fourth. The bound stays because it is the
/// specification's own and because refusing a hostile value 500x earlier is
/// worth having — but on hardening grounds, not on speed.
fn max_entry_points(sps: &Sps, pps: &Pps) -> u32 {
    let rows = sps.pic_height_in_ctbs();
    match pps.tiles.as_ref() {
        None => rows.saturating_sub(1),
        Some(t) if pps.entropy_coding_sync_enabled => {
            t.num_columns.saturating_mul(rows).saturating_sub(1)
        }
        Some(t) => t
            .num_columns
            .saturating_mul(t.num_rows)
            .saturating_sub(1),
    }
    // Never above the structural ceiling, whatever a malformed PPS declares.
    .min(MAX_ENTRY_POINTS)
}

/// `slice_type`, Table 7-7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SliceKind {
    /// 0 — B slice.
    B,
    /// 1 — P slice.
    P,
    /// 2 — I slice.
    #[default]
    I,
}

impl SliceKind {
    /// From `slice_type`.
    #[must_use]
    pub const fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => Self::B,
            1 => Self::P,
            2 => Self::I,
            _ => return None,
        })
    }

    /// The letter `ffprobe` prints for a frame of this type.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::B => 'B',
            Self::P => 'P',
            Self::I => 'I',
        }
    }

    /// Whether the slice uses inter prediction from list 0 — P or B.
    #[must_use]
    pub const fn is_inter(self) -> bool {
        matches!(self, Self::P | Self::B)
    }
}

/// One long-term reference picture named by a slice segment header, §7.3.6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LongTermRef {
    /// `poc_lsb_lt[i]`, either read directly or taken from the SPS's list.
    pub poc_lsb_lt: u32,
    /// `used_by_curr_pic_lt_flag[i]`.
    pub used_by_curr_pic: bool,
    /// `delta_poc_msb_cycle_lt[i]`, when present.
    pub delta_poc_msb_cycle: Option<u32>,
}

/// `ref_pic_lists_modification()`, §7.3.6.2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefPicListModification {
    /// `list_entry_l0[i]`, empty when `ref_pic_list_modification_flag_l0` was 0.
    pub list_entry_l0: Vec<u32>,
    /// `list_entry_l1[i]`.
    pub list_entry_l1: Vec<u32>,
}

/// `pred_weight_table()`, §7.3.6.3.
///
/// The weights themselves are kept because a `hevc_metadata` filter that
/// rewrites a header has to write them back; nothing in a stream description
/// uses them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom`.
    pub luma_log2_weight_denom: u32,
    /// `delta_chroma_log2_weight_denom`.
    pub delta_chroma_log2_weight_denom: i32,
    /// `(delta_luma_weight_lX[i], luma_offset_lX[i])` for the entries that had
    /// a weight, list 0 then list 1.
    pub luma: [Vec<Option<(i32, i32)>>; 2],
    /// `(delta_chroma_weight_lX[i][j], delta_chroma_offset_lX[i][j])`.
    pub chroma: [Vec<Option<[(i32, i32); 2]>>; 2],
}

/// A slice segment header: §7.3.6.1, in field order.
///
/// A **dependent** slice segment stops after `slice_segment_address`; every
/// field below that point keeps its default and
/// [`SliceHeader::dependent`] says so. That is not a shortcut — §7.4.7.1 says a
/// dependent segment inherits those values from the preceding independent one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct SliceHeader {
    /// The NAL unit type this header came from, which several of its own
    /// presence conditions depend on.
    pub nal_unit_type: NalUnitType,
    /// `first_slice_segment_in_pic_flag` — **the** picture boundary marker.
    pub first_slice_segment_in_pic: bool,
    /// `no_output_of_prior_pics_flag`, present only in an IRAP unit.
    pub no_output_of_prior_pics: bool,
    /// `slice_pic_parameter_set_id`.
    pub pps_id: u8,
    /// `dependent_slice_segment_flag`.
    pub dependent: bool,
    /// `slice_segment_address`, in coding tree blocks.
    pub slice_segment_address: u32,
    /// `slice_type`.
    pub kind: SliceKind,
    /// `pic_output_flag`; §7.4.7.1 infers 1 when absent.
    pub pic_output: bool,
    /// `colour_plane_id`, only with `separate_colour_plane_flag`.
    pub colour_plane_id: u8,
    /// `slice_pic_order_cnt_lsb`. Zero for an IDR, which codes none.
    pub pic_order_cnt_lsb: u32,
    /// `short_term_ref_pic_set_sps_flag`.
    pub short_term_ref_pic_set_sps: bool,
    /// The set the slice uses: the inline one, or the SPS's at
    /// `short_term_ref_pic_set_idx`.
    pub short_term_rps: Option<ShortTermRps>,
    /// `short_term_ref_pic_set_idx`.
    pub short_term_ref_pic_set_idx: u32,
    /// The long-term references, SPS-derived entries first.
    pub long_term_refs: Vec<LongTermRef>,
    /// `slice_temporal_mvp_enabled_flag`.
    pub temporal_mvp_enabled: bool,
    /// `slice_sao_luma_flag`.
    pub sao_luma: bool,
    /// `slice_sao_chroma_flag`.
    pub sao_chroma: bool,
    /// `num_ref_idx_l0_active_minus1`, from the header or the PPS default.
    pub num_ref_idx_l0_active_minus1: u32,
    /// `num_ref_idx_l1_active_minus1`.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `ref_pic_lists_modification()`.
    pub ref_pic_list_modification: Option<RefPicListModification>,
    /// `mvd_l1_zero_flag`.
    pub mvd_l1_zero: bool,
    /// `cabac_init_flag`.
    pub cabac_init: bool,
    /// `collocated_from_l0_flag`; §7.4.7.1 infers 1 when absent.
    pub collocated_from_l0: bool,
    /// `collocated_ref_idx`.
    pub collocated_ref_idx: u32,
    /// `pred_weight_table()`.
    pub pred_weight_table: Option<PredWeightTable>,
    /// `five_minus_max_num_merge_cand`.
    pub five_minus_max_num_merge_cand: u32,
    /// `use_integer_mv_flag`, only with `motion_vector_resolution_control_idc == 2`.
    pub use_integer_mv: bool,
    /// `slice_qp_delta`.
    pub qp_delta: i32,
    /// `slice_cb_qp_offset`.
    pub cb_qp_offset: i32,
    /// `slice_cr_qp_offset`.
    pub cr_qp_offset: i32,
    /// `cu_chroma_qp_offset_enabled_flag`.
    pub cu_chroma_qp_offset_enabled: bool,
    /// `slice_deblocking_filter_disabled_flag`, after the override rules.
    pub deblocking_filter_disabled: bool,
    /// `slice_beta_offset_div2`.
    pub beta_offset_div2: i32,
    /// `slice_tc_offset_div2`.
    pub tc_offset_div2: i32,
    /// `slice_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices_enabled: bool,
    /// `entry_point_offset_minus1[i] + 1`.
    pub entry_point_offsets: Vec<u32>,
    /// Whether [`PredWeightTable`] was read with the exact presence conditions
    /// of §7.3.6.3 or with the unconditional approximation. See the module
    /// documentation.
    pub weight_table_exact: bool,
}

impl SliceHeader {
    /// Whether this slice begins a new picture — §7.4.2.4.4's whole rule, in
    /// one bit.
    #[must_use]
    pub const fn starts_new_picture(&self) -> bool {
        self.first_slice_segment_in_pic
    }

    /// Whether the picture is an IRAP.
    #[must_use]
    pub const fn is_irap(&self) -> bool {
        self.nal_unit_type.is_irap()
    }

    /// Whether the picture is an IDR.
    #[must_use]
    pub const fn is_idr(&self) -> bool {
        self.nal_unit_type.is_idr()
    }

    /// `NumPicTotalCurr`, §7.4.7.2 — the number of pictures the current picture
    /// may reference, which decides whether `ref_pic_lists_modification()` is
    /// present at all.
    #[must_use]
    pub fn num_pic_total_curr(&self, curr_pic_ref_enabled: bool) -> u32 {
        let short = self
            .short_term_rps
            .as_ref()
            .map_or(0, ShortTermRps::num_used_by_curr_pic);
        let long = self
            .long_term_refs
            .iter()
            .filter(|r| r.used_by_curr_pic)
            .count() as u32;
        short + long + u32::from(curr_pic_ref_enabled)
    }

    /// Parse a slice segment header from a NAL unit's RBSP.
    ///
    /// `sps` and `pps` must be the ones the header names; a caller finds them
    /// with [`peek_pps_id`] first.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a syntax element out of range,
    /// [`Error::UnexpectedEof`] on truncation, or a budget error.
    pub fn parse(rbsp: &[u8], sps: &Sps, pps: &Pps, budget: &mut Budget) -> Result<Self> {
        let header = HevcNalHeader::parse(rbsp).ok_or(Error::UnexpectedEof)?;
        if !header.nal_unit_type.has_slice_header() {
            return Err(Error::InvalidData("not a slice segment"));
        }
        let mut reader = BitReader::new(rbsp);
        reader.skip(16);
        let h = Self::parse_data(&mut reader, header, sps, pps, budget)?;
        reader.check()?;
        Ok(h)
    }

    /// `slice_segment_header()`, §7.3.6.1, from a reader positioned just after
    /// the NAL header.
    ///
    /// # Errors
    ///
    /// As [`SliceHeader::parse`].
    #[allow(clippy::too_many_lines, reason = "one specification syntax table")]
    pub fn parse_data(
        reader: &mut BitReader<'_>,
        nal: HevcNalHeader,
        sps: &Sps,
        pps: &Pps,
        budget: &mut Budget,
    ) -> Result<Self> {
        let mut g = BoundedGolomb::new(reader, budget);
        let mut h = Self {
            nal_unit_type: nal.nal_unit_type,
            pic_output: true,
            collocated_from_l0: true,
            weight_table_exact: true,
            ..Self::default()
        };

        h.first_slice_segment_in_pic = g.u(1)? != 0;
        if nal.nal_unit_type.is_irap() {
            h.no_output_of_prior_pics = g.u(1)? != 0;
        }
        h.pps_id = g.ue_v(63)? as u8;
        if !h.first_slice_segment_in_pic {
            if pps.dependent_slice_segments_enabled {
                h.dependent = g.u(1)? != 0;
            }
            let bits = sps.slice_address_bits();
            h.slice_segment_address = if bits == 0 { 0 } else { g.u(bits)? };
        }
        if h.dependent {
            // §7.4.7.1: everything below is inherited from the preceding
            // independent slice segment. Stopping here is the syntax, not a
            // shortcut. The extension and byte-alignment syntax still
            // terminates this segment header, even though the slice state
            // fields between the address and it are inherited.
            if pps.has_entry_points() {
                let n = g.ue_v(max_entry_points(sps, pps))?;
                if n > 0 {
                    let len = g.ue_v(31)? + 1;
                    g.budget().consume_fuel(u64::from(n))?;
                    for _ in 0..n {
                        h.entry_point_offsets.push(g.u(len)?.saturating_add(1));
                    }
                }
            }
            if pps.slice_segment_header_extension_present {
                let n = g.ue_v(MAX_SLICE_HEADER_EXTENSION)?;
                g.budget().consume_fuel(u64::from(n))?;
                for _ in 0..n {
                    g.u(8)?;
                }
            }
            g.u(1)?;
            g.reader().align();
            return Ok(h);
        }

        for _ in 0..pps.num_extra_slice_header_bits {
            g.u(1)?;
        }
        h.kind =
            SliceKind::from_u32(g.ue_v(2)?).ok_or(Error::InvalidData("slice_type out of range"))?;
        if pps.output_flag_present {
            h.pic_output = g.u(1)? != 0;
        }
        if sps.separate_colour_plane {
            h.colour_plane_id = g.u(2)? as u8;
        }

        if !nal.nal_unit_type.is_idr() {
            h.pic_order_cnt_lsb = g.u(u32::from(sps.log2_max_pic_order_cnt_lsb))?;
            h.short_term_ref_pic_set_sps = g.u(1)? != 0;
            let num_sets = sps.short_term_ref_pic_sets.len() as u32;
            if h.short_term_ref_pic_set_sps {
                if num_sets > 1 {
                    h.short_term_ref_pic_set_idx = g.u(ceil_log2(u64::from(num_sets)))?;
                }
                h.short_term_rps = sps
                    .short_term_ref_pic_sets
                    .get(h.short_term_ref_pic_set_idx as usize)
                    .cloned();
            } else {
                h.short_term_rps = Some(parse_st_ref_pic_set(
                    &mut g,
                    num_sets,
                    &sps.short_term_ref_pic_sets,
                    num_sets,
                )?);
            }

            if sps.long_term_ref_pics_present {
                let sps_lt = sps.long_term_ref_pics.len() as u32;
                let num_long_term_sps = if sps_lt > 0 { g.ue_v(sps_lt)? } else { 0 };
                // §7.4.7.1 bounds the total by sps_max_dec_pic_buffering_minus1.
                let num_long_term_pics = g.ue_v(16)?;
                let total = num_long_term_sps.saturating_add(num_long_term_pics);
                g.budget().consume_fuel(u64::from(total))?;
                for i in 0..total {
                    let mut entry = LongTermRef::default();
                    if i < num_long_term_sps {
                        let idx = if sps_lt > 1 {
                            g.u(ceil_log2(u64::from(sps_lt)))?
                        } else {
                            0
                        };
                        if let Some(&(poc, used)) = sps.long_term_ref_pics.get(idx as usize) {
                            entry.poc_lsb_lt = poc;
                            entry.used_by_curr_pic = used;
                        }
                    } else {
                        entry.poc_lsb_lt = g.u(u32::from(sps.log2_max_pic_order_cnt_lsb))?;
                        entry.used_by_curr_pic = g.u(1)? != 0;
                    }
                    if g.u(1)? != 0 {
                        entry.delta_poc_msb_cycle = Some(g.ue_v(u32::MAX - 1)?);
                    }
                    h.long_term_refs.push(entry);
                }
            }
            if sps.temporal_mvp_enabled {
                h.temporal_mvp_enabled = g.u(1)? != 0;
            }
        }

        if sps.sample_adaptive_offset_enabled {
            h.sao_luma = g.u(1)? != 0;
            if sps.chroma_array_type() != ChromaFormat::Monochrome {
                h.sao_chroma = g.u(1)? != 0;
            }
        }

        h.num_ref_idx_l0_active_minus1 = pps.num_ref_idx_l0_default_active_minus1;
        h.num_ref_idx_l1_active_minus1 = pps.num_ref_idx_l1_default_active_minus1;
        if h.kind.is_inter() {
            if g.u(1)? != 0 {
                // §7.4.7.1 bounds both at 14.
                h.num_ref_idx_l0_active_minus1 = g.ue_v(14)?;
                if h.kind == SliceKind::B {
                    h.num_ref_idx_l1_active_minus1 = g.ue_v(14)?;
                }
            }
            let curr_pic_ref = pps.scc_extension.is_some_and(|s| s.curr_pic_ref_enabled)
                || sps.scc_extension.is_some_and(|s| s.curr_pic_ref_enabled);
            let num_pic_total_curr = h.num_pic_total_curr(curr_pic_ref);
            if pps.lists_modification_present && num_pic_total_curr > 1 {
                h.ref_pic_list_modification =
                    Some(read_list_modification(&mut g, &h, num_pic_total_curr)?);
            }
            if h.kind == SliceKind::B {
                h.mvd_l1_zero = g.u(1)? != 0;
            }
            if pps.cabac_init_present {
                h.cabac_init = g.u(1)? != 0;
            }
            if h.temporal_mvp_enabled {
                if h.kind == SliceKind::B {
                    h.collocated_from_l0 = g.u(1)? != 0;
                }
                let n = if h.collocated_from_l0 {
                    h.num_ref_idx_l0_active_minus1
                } else {
                    h.num_ref_idx_l1_active_minus1
                };
                if n > 0 {
                    h.collocated_ref_idx = g.ue_v(14)?;
                }
            }
            if (pps.weighted_pred && h.kind == SliceKind::P)
                || (pps.weighted_bipred && h.kind == SliceKind::B)
            {
                h.weight_table_exact = !curr_pic_ref;
                h.pred_weight_table = Some(read_pred_weight_table(&mut g, &h, sps)?);
            }
            h.five_minus_max_num_merge_cand = g.ue_v(4)?;
            if sps
                .scc_extension
                .is_some_and(|s| s.motion_vector_resolution_control_idc == 2)
            {
                h.use_integer_mv = g.u(1)? != 0;
            }
        }

        // §7.4.7.1: -QpBdOffsetY - 26 - init_qp_minus26 .. +25 - init_qp_minus26.
        h.qp_delta = g.se_v(-128, 128)?;
        if pps.slice_chroma_qp_offsets_present {
            h.cb_qp_offset = g.se_v(-12, 12)?;
            h.cr_qp_offset = g.se_v(-12, 12)?;
        }
        if pps
            .scc_extension
            .is_some_and(|s| s.slice_act_qp_offsets_present)
        {
            g.se_v(-12, 12)?;
            g.se_v(-12, 12)?;
            g.se_v(-12, 12)?;
        }
        if pps
            .range_extension
            .as_ref()
            .is_some_and(|r| r.chroma_qp_offset_list_enabled)
        {
            h.cu_chroma_qp_offset_enabled = g.u(1)? != 0;
        }

        let deblocking = pps.deblocking.unwrap_or_default();
        h.deblocking_filter_disabled = deblocking.disabled;
        h.beta_offset_div2 = deblocking.beta_offset_div2;
        h.tc_offset_div2 = deblocking.tc_offset_div2;
        let mut override_flag = false;
        if deblocking.override_enabled {
            override_flag = g.u(1)? != 0;
        }
        if override_flag {
            h.deblocking_filter_disabled = g.u(1)? != 0;
            if !h.deblocking_filter_disabled {
                h.beta_offset_div2 = g.se_v(-6, 6)?;
                h.tc_offset_div2 = g.se_v(-6, 6)?;
            }
        }
        h.loop_filter_across_slices_enabled = pps.loop_filter_across_slices_enabled;
        if pps.loop_filter_across_slices_enabled
            && (h.sao_luma || h.sao_chroma || !h.deblocking_filter_disabled)
        {
            h.loop_filter_across_slices_enabled = g.u(1)? != 0;
        }

        if pps.has_entry_points() {
            let n = g.ue_v(max_entry_points(sps, pps))?;
            if n > 0 {
                let len = g.ue_v(31)? + 1;
                g.budget().consume_fuel(u64::from(n))?;
                for _ in 0..n {
                    h.entry_point_offsets.push(g.u(len)?.saturating_add(1));
                }
            }
        }
        if pps.slice_segment_header_extension_present {
            let n = g.ue_v(MAX_SLICE_HEADER_EXTENSION)?;
            g.budget().consume_fuel(u64::from(n))?;
            for _ in 0..n {
                g.u(8)?;
            }
        }
        // `byte_alignment()`: a `1` bit then zeros to the byte boundary.
        g.u(1)?;
        g.reader().align();

        Ok(h)
    }
}

/// `ref_pic_lists_modification()`, §7.3.6.2.
fn read_list_modification(
    g: &mut BoundedGolomb<'_, '_, '_>,
    h: &SliceHeader,
    num_pic_total_curr: u32,
) -> Result<RefPicListModification> {
    let bits = ceil_log2(u64::from(num_pic_total_curr));
    let mut out = RefPicListModification::default();
    if g.u(1)? != 0 {
        let n = h.num_ref_idx_l0_active_minus1.saturating_add(1);
        g.budget().consume_fuel(u64::from(n))?;
        for _ in 0..n {
            out.list_entry_l0.push(g.u(bits)?);
        }
    }
    if h.kind == SliceKind::B && g.u(1)? != 0 {
        let n = h.num_ref_idx_l1_active_minus1.saturating_add(1);
        g.budget().consume_fuel(u64::from(n))?;
        for _ in 0..n {
            out.list_entry_l1.push(g.u(bits)?);
        }
    }
    Ok(out)
}

/// `pred_weight_table()`, §7.3.6.3.
///
/// The presence conditions on `luma_weight_lX_flag[i]` are approximated; see the
/// module documentation for why and for when the approximation is exact.
fn read_pred_weight_table(
    g: &mut BoundedGolomb<'_, '_, '_>,
    header: &SliceHeader,
    sps: &Sps,
) -> Result<PredWeightTable> {
    let chroma = sps.chroma_array_type() != ChromaFormat::Monochrome;
    // §7.4.7.3 widens the offsets when `high_precision_offsets_enabled_flag` is
    // set: the range becomes -(1 << (BitDepth - 1)) .. (1 << (BitDepth - 1)) - 1
    // rather than -128..=127.
    let high_precision = sps
        .range_extension
        .is_some_and(|r| r.high_precision_offsets_enabled);
    let (off_lo, off_hi) = if high_precision {
        let shift = u32::from(sps.bit_depth_luma).saturating_sub(1).min(30);
        (-(1i32 << shift), (1i32 << shift) - 1)
    } else {
        (-128, 127)
    };

    let mut table = PredWeightTable {
        luma_log2_weight_denom: g.ue_v(7)?,
        ..PredWeightTable::default()
    };
    if chroma {
        table.delta_chroma_log2_weight_denom = g.se_v(-7, 7)?;
    }

    let lists: [u32; 2] = [
        header.num_ref_idx_l0_active_minus1.saturating_add(1),
        if header.kind == SliceKind::B {
            header.num_ref_idx_l1_active_minus1.saturating_add(1)
        } else {
            0
        },
    ];
    for (list, &count) in lists.iter().enumerate() {
        if count == 0 {
            continue;
        }
        // `count` is at most 15 (§7.4.7.1 bounds `num_ref_idx_lX_active_minus1`
        // at 14), and the fuel charge is what bounds it against a malformed
        // PPS default.
        g.budget().consume_fuel(u64::from(count) * 2)?;
        let mut luma_flags = Vec::new();
        for _ in 0..count {
            luma_flags.push(g.u(1)? != 0);
        }
        let mut chroma_flags = vec![false; count as usize];
        if chroma {
            for slot in &mut chroma_flags {
                *slot = g.u(1)? != 0;
            }
        }
        let mut luma = Vec::new();
        let mut chroma_weights = Vec::new();
        for i in 0..count as usize {
            let luma_entry = if luma_flags.get(i).copied().unwrap_or(false) {
                Some((g.se_v(-128, 127)?, g.se_v(off_lo, off_hi)?))
            } else {
                None
            };
            let chroma_entry = if chroma_flags.get(i).copied().unwrap_or(false) {
                let mut pair = [(0i32, 0i32); 2];
                for slot in &mut pair {
                    *slot = (g.se_v(-128, 127)?, g.se_v(-512, 512)?);
                }
                Some(pair)
            } else {
                None
            };
            luma.push(luma_entry);
            chroma_weights.push(chroma_entry);
        }
        if let Some(slot) = table.luma.get_mut(list) {
            *slot = luma;
        }
        if let Some(slot) = table.chroma.get_mut(list) {
            *slot = chroma_weights;
        }
    }
    Ok(table)
}

/// Read a slice segment header's `slice_pic_parameter_set_id` without parsing
/// the rest.
///
/// The first three elements are `first_slice_segment_in_pic_flag`, an optional
/// `no_output_of_prior_pics_flag`, and `slice_pic_parameter_set_id` — none of
/// them dependent on any parameter set, which is exactly why the format puts
/// them first. That is what makes it possible to find the right SPS and PPS
/// *before* parsing a header whose remaining fields need them.
#[must_use]
pub fn peek_pps_id(rbsp: &[u8]) -> Option<u8> {
    use vaco_codec_golomb::GolombDecode;
    let nal = HevcNalHeader::parse(rbsp)?;
    if !nal.nal_unit_type.has_slice_header() {
        return None;
    }
    let mut r = BitReader::new(rbsp);
    r.skip(16);
    let _first = r.get_bit();
    if nal.nal_unit_type.is_irap() {
        let _no_output = r.get_bit();
    }
    let id = r.ue_v_max(63).ok()?;
    (!r.overrun()).then_some(id as u8)
}

/// Whether a slice segment NAL unit's first bit is
/// `first_slice_segment_in_pic_flag`, without any parameter set at all.
///
/// The cheapest possible picture-boundary test, and the reason HEVC's streaming
/// parser needs no slice-header comparison: the flag is bit 0 of the byte after
/// the two-byte NAL header.
#[must_use]
pub fn peek_first_slice_in_pic(rbsp: &[u8]) -> Option<bool> {
    let nal = HevcNalHeader::parse(rbsp)?;
    if !nal.nal_unit_type.has_slice_header() {
        return None;
    }
    Some(rbsp.get(2)? & 0x80 != 0)
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

    /// The SPS and PPS from `sd.265`, the same pair every other test uses.
    const SPS_EBSP: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32, 0xbc, 0x05,
        0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
    ];
    const PPS_EBSP: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40];

    fn sets() -> (Sps, Pps) {
        let mut budget = Budget::new(Limits::strict());
        let mut s1 = Vec::new();
        let mut s2 = Vec::new();
        let sps_rbsp = vaco_bitstream::annexb::to_rbsp(SPS_EBSP, &mut s1);
        let pps_rbsp = vaco_bitstream::annexb::to_rbsp(PPS_EBSP, &mut s2);
        (
            Sps::parse(sps_rbsp, &mut budget).expect("SPS"),
            Pps::parse(pps_rbsp, &mut budget).expect("PPS"),
        )
    }

    /// The first twenty bytes of a real `IDR_N_LP` slice from `sd.265`: NAL
    /// header `0x28 0x01`, then the slice segment header, then CABAC data the
    /// header parser stops before.
    const IDR_SLICE: &[u8] = &[
        0x28, 0x01, 0xaf, 0x1d, 0x30, 0xc6, 0x23, 0x40, 0xf2, 0xcd, 0x58, 0xb9, 0x5a, 0x80, 0x62,
        0x7c, 0x25, 0xcc, 0x46, 0x65,
    ];

    /// A `TRAIL_R` slice from the same stream — the inter case, which reaches
    /// the reference-picture-set and weighted-prediction branches.
    const TRAIL_SLICE: &[u8] = &[
        0x02, 0x01, 0xd0, 0x29, 0x4b, 0xe1, 0x0c, 0x63, 0x86, 0x16, 0xd0, 0x1e, 0x32, 0xc3, 0xc2,
        0x99, 0xee, 0x5f, 0x65, 0x1f,
    ];

    #[test]
    fn the_first_bit_is_the_picture_boundary() {
        assert_eq!(peek_first_slice_in_pic(IDR_SLICE), Some(true));
        // Clear the flag and the same unit says it continues a picture.
        let mut cont = IDR_SLICE.to_vec();
        cont[2] &= 0x7F;
        assert_eq!(peek_first_slice_in_pic(&cont), Some(false));
        // A parameter set has no slice header at all.
        assert_eq!(peek_first_slice_in_pic(SPS_EBSP), None);
        assert_eq!(peek_first_slice_in_pic(&[]), None);
        assert_eq!(peek_first_slice_in_pic(&[0x26]), None);
    }

    #[test]
    fn a_real_idr_slice_header() {
        let (sps, pps) = sets();
        let mut budget = Budget::new(Limits::strict());
        let h = SliceHeader::parse(IDR_SLICE, &sps, &pps, &mut budget).expect("parses");
        assert!(h.first_slice_segment_in_pic);
        assert_eq!(h.pps_id, 0);
        assert!(!h.dependent);
        assert_eq!(h.kind, SliceKind::I);
        assert!(h.is_idr());
        assert_eq!(h.nal_unit_type, NalUnitType::IDR_N_LP);
        assert!(h.is_irap());
        // An IDR codes no picture order count at all.
        assert_eq!(h.pic_order_cnt_lsb, 0);
        assert!(h.short_term_rps.is_none());
        // The SPS enables SAO, so the flags are present.
        assert!(sps.sample_adaptive_offset_enabled);
        assert!(h.weight_table_exact);
    }

    /// The inter case: a trailing slice carries a picture order count and a
    /// short-term reference picture set. `x265` declares **no** sets in the SPS
    /// (`num_short_term_ref_pic_sets = 0`), so every slice codes its own inline
    /// — which is the branch that needs §7.4.8's derivation.
    #[test]
    fn a_real_trailing_slice_header() {
        let (sps, pps) = sets();
        let mut budget = Budget::new(Limits::strict());
        let h = SliceHeader::parse(TRAIL_SLICE, &sps, &pps, &mut budget).expect("parses");
        assert!(h.first_slice_segment_in_pic);
        assert_eq!(h.nal_unit_type, NalUnitType::TRAIL_R);
        assert!(!h.is_idr());
        assert!(!h.is_irap());
        assert!(h.kind.is_inter(), "a trailing slice is P or B");
        assert!(
            h.short_term_rps.is_some(),
            "a non-IDR slice always names a reference picture set"
        );
        assert!(
            sps.short_term_ref_pic_sets.is_empty(),
            "x265 declares none in the SPS"
        );
        assert!(
            !h.short_term_ref_pic_set_sps,
            "so the slice codes one inline"
        );
    }

    #[test]
    fn peeking_the_pps_id_agrees_with_a_full_parse() {
        let (sps, pps) = sets();
        let mut budget = Budget::new(Limits::strict());
        let h = SliceHeader::parse(IDR_SLICE, &sps, &pps, &mut budget).expect("parses");
        assert_eq!(peek_pps_id(IDR_SLICE), Some(h.pps_id));
        assert_eq!(peek_pps_id(SPS_EBSP), None);
    }

    #[test]
    fn slice_kinds_map_to_the_letters_ffprobe_prints() {
        assert_eq!(SliceKind::from_u32(0), Some(SliceKind::B));
        assert_eq!(SliceKind::from_u32(1), Some(SliceKind::P));
        assert_eq!(SliceKind::from_u32(2), Some(SliceKind::I));
        assert_eq!(SliceKind::from_u32(3), None);
        assert_eq!(SliceKind::B.letter(), 'B');
        assert_eq!(SliceKind::P.letter(), 'P');
        assert_eq!(SliceKind::I.letter(), 'I');
        assert!(SliceKind::P.is_inter());
        assert!(!SliceKind::I.is_inter());
    }

    #[test]
    fn every_truncation_and_every_bit_flip_is_handled() {
        let (sps, pps) = sets();
        for base in [IDR_SLICE, TRAIL_SLICE] {
            for n in 0..base.len() {
                let mut budget = Budget::new(Limits::strict());
                let _ = SliceHeader::parse(&base[..n], &sps, &pps, &mut budget);
            }
            for byte in 2..base.len() {
                for bit in 0..8 {
                    let mut data = base.to_vec();
                    data[byte] ^= 1 << bit;
                    let mut budget = Budget::new(Limits::strict());
                    let _ = SliceHeader::parse(&data, &sps, &pps, &mut budget);
                }
            }
        }
    }

    #[test]
    fn a_dependent_slice_segment_stops_at_its_address() {
        let (sps, mut pps) = sets();
        pps.dependent_slice_segments_enabled = true;
        // first_slice_segment_in_pic = 0, dependent = 1, then the address.
        let mut data = IDR_SLICE.to_vec();
        data[2] = 0b0100_0000 | (data[2] & 0x3F);
        let mut budget = Budget::new(Limits::strict());
        if let Ok(h) = SliceHeader::parse(&data, &sps, &pps, &mut budget)
            && h.dependent
        {
            assert!(!h.first_slice_segment_in_pic);
            assert_eq!(h.kind, SliceKind::I, "left at its default");
        }
    }
}
