//! H.264's implementation of [`CbsCodec`] — the read/modify/write face of the
//! crate, mirroring `vaco_parse_hevc::cbs`.
//!
//! # Scope
//!
//! [`H264Content`] types the two units a filter is most likely to edit — an
//! SPS and a PPS — plus [`H264Content::Raw`] for everything else (slices,
//! SEI, AUD, filler). SEI is intentionally left `Raw` here: H.264's SEI
//! payload catalogue is its own substantial parser
//! ([`crate::sei`]) and typing it as editable `Content` has no caller in this
//! crate yet — the same "no test can honestly exercise it" reasoning
//! `vaco-bsf-av1::metadata`'s docs already name for a different unit type.
//!
//! # The write path
//!
//! Unlike HEVC's [`vaco_parse_hevc::cbs::HevcCbs`] (write-unsupported for
//! every typed variant), [`H264Cbs::write_unit`] **does** re-encode an SPS or
//! PPS bit-exactly: `seq_parameter_set_data()` (§7.3.2.1.1), `vui_parameters()`
//! (§E.1.1), `hrd_parameters()` (§E.1.2) and `pic_parameter_set_rbsp()`
//! (§7.3.2.2), each written in the specification's own field order to match
//! [`crate::sps::Sps::parse_data`]/[`crate::pps::Pps::parse_data`] exactly.
//! Verified by round-tripping real encoder output byte-for-byte — see the
//! tests below.
//!
//! One documented, narrow exception: a *custom* scaling list
//! (`seq_scaling_list_present_flag[i] == 1`, `UseDefaultScalingMatrixFlag ==
//! 0`) is always re-encoded as one explicit `delta_scale` per entry, never
//! using the early-termination sentinel (a delta that drives `nextScale` to
//! 0 mid-list, which freezes every remaining entry at the prior value) that
//! an encoder *may* have used to shorten the list. Both encodings decode to
//! the identical [`crate::sps::ScalingLists`], so this is a small,
//! unstructured deviation confined to a rarely-used path (most encoders,
//! `libx264` included, never set a custom scaling list at all), not a
//! decode-affecting bug. See [`write_scaling_list`].

use vaco_bitstream::{BitWriter, RbspWriter};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, RbspBuf, units};
use vaco_limits::Budget;

use crate::nal::{H264NalHeader, NalUnitType};
use crate::params::ParameterSets;
use crate::pps::{Pps, SliceGroupMap};
use crate::sps::{
    BitstreamRestriction, ChromaFormat, CpbEntry, EXTENDED_SAR, HrdParameters, ScalingLists, Sps,
    VuiParameters,
};
use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit, UnitOrigin};

/// The typed content of one H.264 NAL unit.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum H264Content {
    /// A sequence parameter set, plus the header bits [`Sps`] itself does not
    /// carry.
    Sps {
        /// `nal_ref_idc`. The specification requires this to be nonzero for
        /// an SPS but does not fix its value, so it is carried rather than
        /// assumed.
        nal_ref_idc: u8,
        /// The parsed parameter set.
        sps: Box<Sps>,
    },
    /// A picture parameter set, plus its header bits.
    Pps {
        /// `nal_ref_idc`.
        nal_ref_idc: u8,
        /// The parsed parameter set.
        pps: Box<Pps>,
    },
    /// Anything else: the unit's bytes, escaping intact.
    Raw {
        /// The NAL unit type.
        nal_unit_type: NalUnitType,
        /// The bytes, header included.
        data: Vec<u8>,
    },
}

impl H264Content {
    /// The NAL unit type this content would be written as.
    #[must_use]
    pub const fn nal_unit_type(&self) -> NalUnitType {
        match self {
            Self::Sps { .. } => NalUnitType::Sps,
            Self::Pps { .. } => NalUnitType::Pps,
            Self::Raw { nal_unit_type, .. } => *nal_unit_type,
        }
    }
}

/// The H.264 [`CbsCodec`].
///
/// Holds an [`RbspBuf`] for de-escaping (as `HevcCbs` does) and a
/// [`ParameterSets`] store, because [`Pps::parse`] needs its SPS to size the
/// slice-group map and the tail's scaling lists — the same reason
/// [`crate::params::ParameterSets::add_pps`] keeps one.
#[derive(Debug, Default)]
pub struct H264Cbs {
    rbsp: RbspBuf,
    params: ParameterSets,
}

impl H264Cbs {
    /// A fresh codec, with no parameter sets seen yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rbsp: RbspBuf::new(),
            params: ParameterSets::new(),
        }
    }
}

impl CbsCodec for H264Cbs {
    type Content = H264Content;
    type Framing = Framing;
    const NAME: &'static str = "h264";

    fn split(
        &self,
        data: &[u8],
        framing: Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        for nal in units(data, framing) {
            let Some(header) = H264NalHeader::parse(nal.data) else {
                continue;
            };
            fragment.push(
                CbsUnit::from_source(
                    u32::from(header.nal_unit_type.to_u8()),
                    nal.data.to_vec(),
                    UnitOrigin {
                        offset: nal.offset,
                        framing_len: nal.start_code_len,
                    },
                ),
                budget,
            )?;
        }
        Ok(())
    }

    fn assemble(
        &self,
        fragment: &CbsFragment,
        framing: Framing,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        let total: usize = fragment
            .units()
            .iter()
            .map(|u| u.data.len().saturating_add(4))
            .sum();
        budget.check(total as u64)?;
        for unit in fragment.units() {
            match framing {
                Framing::AnnexB => {
                    // See `vaco_parse_hevc::cbs::HevcCbs::assemble`: three
                    // bytes only when the source used a three-byte start
                    // code, four otherwise.
                    if unit.origin.map_or(4, |o| o.framing_len) != 3 {
                        out.push(0);
                    }
                    out.extend_from_slice(&[0, 0, 1]);
                }
                Framing::LengthPrefixed(size) => {
                    let len = unit.data.len() as u64;
                    if len > size.max_unit_len() {
                        return Err(Error::InvalidData(
                            "NAL unit too long for this length prefix",
                        ));
                    }
                    for k in (0..size.len()).rev() {
                        out.push(((len >> (k * 8)) & 0xFF) as u8);
                    }
                }
            }
            out.extend_from_slice(&unit.data);
        }
        Ok(())
    }

    fn read_unit(&mut self, unit: &CbsUnit, budget: &mut Budget) -> Result<H264Content> {
        let header = H264NalHeader::parse(&unit.data).ok_or(Error::UnexpectedEof)?;
        let t = header.nal_unit_type;
        self.rbsp.fill(&unit.data, budget)?;
        let rbsp = self.rbsp.as_slice();
        Ok(match t {
            NalUnitType::Sps => {
                // Feed the store too, so a following PPS in the same
                // fragment can size its tail correctly.
                let id = self.params.add_sps(rbsp, budget)?;
                let sps = self
                    .params
                    .get_sps(id)
                    .ok_or(Error::InvalidData("sps vanished after being stored"))?
                    .clone();
                H264Content::Sps {
                    nal_ref_idc: header.nal_ref_idc,
                    sps: Box::new(sps),
                }
            }
            NalUnitType::Pps => {
                let id = self.params.add_pps(rbsp, budget)?;
                let pps = self
                    .params
                    .get_pps(id)
                    .ok_or(Error::InvalidData("pps vanished after being stored"))?
                    .clone();
                H264Content::Pps {
                    nal_ref_idc: header.nal_ref_idc,
                    pps: Box::new(pps),
                }
            }
            _ => H264Content::Raw {
                nal_unit_type: t,
                data: unit.data.clone(),
            },
        })
    }

    fn write_unit(
        &mut self,
        content: &H264Content,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match content {
            H264Content::Raw { data, .. } => {
                budget.check(data.len() as u64)?;
                out.extend_from_slice(data);
                Ok(())
            }
            H264Content::Sps { nal_ref_idc, sps } => {
                let mut w = RbspWriter::new();
                write_nal_header(w.bits(), *nal_ref_idc, NalUnitType::Sps);
                write_sps_data(sps, w.bits());
                let bytes = w.finish();
                budget.check(bytes.len() as u64)?;
                out.extend_from_slice(&bytes);
                Ok(())
            }
            H264Content::Pps { nal_ref_idc, pps } => {
                let mut w = RbspWriter::new();
                write_nal_header(w.bits(), *nal_ref_idc, NalUnitType::Pps);
                write_pps_data(pps, w.bits());
                let bytes = w.finish();
                budget.check(bytes.len() as u64)?;
                out.extend_from_slice(&bytes);
                Ok(())
            }
        }
    }

    fn content_unit_type(&self, content: &H264Content) -> u32 {
        u32::from(content.nal_unit_type().to_u8())
    }
}

/// `nal_unit_header()`, §7.3.1's first byte: `forbidden_zero_bit` (always 0 —
/// a stream that set it was already non-conforming, and nothing this crate
/// reads preserves the bit to round-trip it), `nal_ref_idc`, `nal_unit_type`.
fn write_nal_header(w: &mut BitWriter, nal_ref_idc: u8, t: NalUnitType) {
    w.put(1, 0);
    w.put(2, u32::from(nal_ref_idc));
    w.put(5, u32::from(t.to_u8()));
}

/// `seq_parameter_set_data()`, §7.3.2.1.1 — the inverse of
/// [`crate::sps::Sps::parse_data`], field for field.
fn write_sps_data(sps: &Sps, w: &mut BitWriter) {
    w.put(8, u32::from(sps.profile_idc));
    w.put(8, u32::from(sps.constraint_flags.bits()));
    w.put(8, u32::from(sps.level_idc));
    w.ue(u32::from(sps.id));

    if crate::sps::profile_has_chroma_block(sps.profile_idc) {
        w.ue(sps.chroma_format.idc());
        if sps.chroma_format == ChromaFormat::Yuv444 {
            w.put(1, u32::from(sps.separate_colour_plane));
        }
        w.ue(u32::from(sps.bit_depth_luma) - 8);
        w.ue(u32::from(sps.bit_depth_chroma) - 8);
        w.put(1, u32::from(sps.qpprime_y_zero_transform_bypass));
        match &sps.scaling_lists {
            Some(lists) => {
                w.put(1, 1);
                let count = if sps.chroma_format == ChromaFormat::Yuv444 {
                    12
                } else {
                    8
                };
                write_scaling_lists(w, lists, count);
            }
            None => w.put(1, 0),
        }
    }

    w.ue(u32::from(sps.log2_max_frame_num) - 4);
    w.ue(u32::from(sps.pic_order_cnt_type));
    match sps.pic_order_cnt_type {
        0 => w.ue(u32::from(sps.log2_max_pic_order_cnt_lsb) - 4),
        // A missing `poc_type1` here means the `Sps` was hand-built rather
        // than parsed (a real read always fills it for type 1); writing the
        // all-zero fields a fresh `PocType1` would hold keeps this function
        // total instead of panicking on a caller's construction bug.
        1 => {
            if let Some(p1) = &sps.poc_type1 {
                w.put(1, u32::from(p1.delta_pic_order_always_zero));
                w.se(p1.offset_for_non_ref_pic);
                w.se(p1.offset_for_top_to_bottom_field);
                w.ue(p1.offset_for_ref_frame.len() as u32);
                for &v in &p1.offset_for_ref_frame {
                    w.se(v);
                }
            } else {
                w.put(1, 0);
                w.se(0);
                w.se(0);
                w.ue(0);
            }
        }
        _ => {}
    }
    w.ue(sps.max_num_ref_frames);
    w.put(1, u32::from(sps.gaps_in_frame_num_value_allowed));
    w.ue(sps.pic_width_in_mbs - 1);
    w.ue(sps.pic_height_in_map_units - 1);
    w.put(1, u32::from(sps.frame_mbs_only));
    if !sps.frame_mbs_only {
        w.put(1, u32::from(sps.mb_adaptive_frame_field));
    }
    w.put(1, u32::from(sps.direct_8x8_inference));
    match sps.crop {
        Some(c) => {
            w.put(1, 1);
            w.ue(c.left);
            w.ue(c.right);
            w.ue(c.top);
            w.ue(c.bottom);
        }
        None => w.put(1, 0),
    }
    match &sps.vui {
        Some(vui) => {
            w.put(1, 1);
            write_vui(w, vui);
        }
        None => w.put(1, 0),
    }
}

/// `pic_parameter_set_rbsp()`, §7.3.2.2 — the inverse of
/// [`crate::pps::Pps::parse_data`].
fn write_pps_data(pps: &Pps, w: &mut BitWriter) {
    w.ue(u32::from(pps.id));
    w.ue(u32::from(pps.sps_id));
    w.put(1, u32::from(pps.entropy_coding_mode));
    w.put(1, u32::from(pps.bottom_field_pic_order_in_frame_present));
    w.ue(pps.num_slice_groups - 1);
    if let Some(map) = &pps.slice_group_map {
        write_slice_group_map(w, map, pps.num_slice_groups);
    }
    w.ue(pps.num_ref_idx_l0_default_active_minus1);
    w.ue(pps.num_ref_idx_l1_default_active_minus1);
    w.put(1, u32::from(pps.weighted_pred));
    w.put(2, u32::from(pps.weighted_bipred_idc));
    w.se(pps.pic_init_qp_minus26);
    w.se(pps.pic_init_qs_minus26);
    w.se(pps.chroma_qp_index_offset);
    w.put(1, u32::from(pps.deblocking_filter_control_present));
    w.put(1, u32::from(pps.constrained_intra_pred));
    w.put(1, u32::from(pps.redundant_pic_cnt_present));
    if pps.has_tail {
        w.put(1, u32::from(pps.transform_8x8_mode));
        match &pps.scaling_lists {
            Some(lists) => {
                w.put(1, 1);
                // §7.3.2.2: the SPS is needed only to *size* the list on
                // read; on write, the count the original bitstream actually
                // coded is `lists.count`, not `lists.present.len()` — that
                // array is a fixed 12 entries regardless of how many were
                // really read (see `ScalingLists::count`'s doc), so using its
                // length here silently over-writes `present` flags the
                // source never had, desyncing every field after the tail.
                write_scaling_lists(w, lists, lists.count as usize);
            }
            None => w.put(1, 0),
        }
        w.se(pps.second_chroma_qp_index_offset);
    }
}

/// The slice-group map, §7.3.2.2 — the inverse of `parse_slice_group_map`.
///
/// `num_slice_groups` (the PPS's own `num_slice_groups_minus1 + 1`, not
/// anything derived from `map`) sizes the `Explicit` arm's per-entry bit
/// width, exactly as `parse_slice_group_map` reads it — deriving the width
/// from the *values actually present* in `ids` instead is wrong whenever the
/// map does not happen to use every group index up to
/// `num_slice_groups - 1`, which under-writes bits and desyncs everything
/// after.
fn write_slice_group_map(w: &mut BitWriter, map: &SliceGroupMap, num_slice_groups: u32) {
    w.ue(u32::from(map.map_type()));
    match map {
        SliceGroupMap::Interleaved(runs) => {
            for &r in runs {
                w.ue(r);
            }
        }
        SliceGroupMap::Dispersed => {}
        SliceGroupMap::Foreground(boxes) => {
            for &(a, b) in boxes {
                w.ue(a);
                w.ue(b);
            }
        }
        SliceGroupMap::Changing {
            change_direction,
            change_rate_minus1,
            ..
        } => {
            w.put(1, u32::from(*change_direction));
            w.ue(*change_rate_minus1);
        }
        SliceGroupMap::Explicit(ids) => {
            w.ue(ids.len().saturating_sub(1) as u32);
            let bits = 32 - num_slice_groups.saturating_sub(1).leading_zeros();
            for &id in ids {
                w.put(bits.max(1), u32::from(id));
            }
        }
    }
}

/// `scaling_list()` written `count` times, §7.3.2.1.1.1 — the inverse of
/// `read_scaling_lists`. See the module doc for the one documented deviation
/// (no early-termination sentinel for a non-default custom list).
fn write_scaling_lists(w: &mut BitWriter, lists: &ScalingLists, count: usize) {
    for i in 0..count {
        let present = lists.present.get(i).copied().unwrap_or(false);
        w.put(1, u32::from(present));
        if !present {
            continue;
        }
        let use_default = lists.use_default.get(i).copied().unwrap_or(false);
        if i < 6 {
            let list = lists.list_4x4.get(i).copied().unwrap_or([0; 16]);
            write_scaling_list(w, &list, use_default);
        } else {
            let list = lists.list_8x8.get(i - 6).copied().unwrap_or([0; 64]);
            write_scaling_list(w, &list, use_default);
        }
    }
}

/// One `scaling_list()`, §7.3.2.1.1.1.
fn write_scaling_list(w: &mut BitWriter, list: &[u8], use_default: bool) {
    if use_default {
        // The one delta that drives `nextScale` to 0 at j == 0: `delta = -8`
        // (`last_scale` starts at 8), which is exactly
        // `UseDefaultScalingMatrixFlag`'s trigger and the only way
        // `read_scaling_list` ever sets it.
        w.se(-8);
        return;
    }
    let mut last_scale = 8i32;
    for &v in list {
        let target = i32::from(v);
        // `target` is never 0 (see the module doc): a decoded entry can only
        // ever be `last_scale` at the moment `nextScale` would have become
        // 0, and `last_scale` itself is never 0 by the same induction. So
        // `delta = target - last_scale` (wrapped into -128..=127) never
        // collides with the sentinel.
        let raw = target - last_scale;
        let delta = ((raw + 128).rem_euclid(256)) - 128;
        w.se(delta);
        last_scale = target;
    }
}

/// `vui_parameters()`, §E.1.1 — the inverse of `parse_vui`.
fn write_vui(w: &mut BitWriter, vui: &VuiParameters) {
    match vui.aspect_ratio_idc {
        Some(idc) => {
            w.put(1, 1);
            w.put(8, u32::from(idc));
            if idc == EXTENDED_SAR {
                let (sw, sh) = vui.sar.unwrap_or((0, 0));
                w.put(16, u32::from(sw));
                w.put(16, u32::from(sh));
            }
        }
        None => w.put(1, 0),
    }
    match vui.overscan_appropriate {
        Some(v) => {
            w.put(1, 1);
            w.put(1, u32::from(v));
        }
        None => w.put(1, 0),
    }
    match vui.video_format {
        Some(fmt) => {
            w.put(1, 1);
            w.put(3, u32::from(fmt));
            w.put(1, u32::from(vui.video_full_range.unwrap_or(false)));
            match vui.colour_description {
                Some((p, t, m)) => {
                    w.put(1, 1);
                    w.put(8, u32::from(p));
                    w.put(8, u32::from(t));
                    w.put(8, u32::from(m));
                }
                None => w.put(1, 0),
            }
        }
        None => w.put(1, 0),
    }
    match vui.chroma_sample_loc {
        Some((top, bottom)) => {
            w.put(1, 1);
            w.ue(top);
            w.ue(bottom);
        }
        None => w.put(1, 0),
    }
    match vui.timing {
        Some(t) => {
            w.put(1, 1);
            w.put(32, t.num_units_in_tick);
            w.put(32, t.time_scale);
            w.put(1, u32::from(t.fixed_frame_rate));
        }
        None => w.put(1, 0),
    }
    match &vui.nal_hrd {
        Some(h) => {
            w.put(1, 1);
            write_hrd(w, h);
        }
        None => w.put(1, 0),
    }
    match &vui.vcl_hrd {
        Some(h) => {
            w.put(1, 1);
            write_hrd(w, h);
        }
        None => w.put(1, 0),
    }
    if vui.nal_hrd.is_some() || vui.vcl_hrd.is_some() {
        w.put(1, u32::from(vui.low_delay_hrd.unwrap_or(false)));
    }
    w.put(1, u32::from(vui.pic_struct_present));
    match &vui.bitstream_restriction {
        Some(b) => {
            w.put(1, 1);
            write_bitstream_restriction(w, b);
        }
        None => w.put(1, 0),
    }
}

/// `hrd_parameters()`, §E.1.2 — the inverse of `parse_hrd`.
fn write_hrd(w: &mut BitWriter, h: &HrdParameters) {
    w.ue(h.cpb.len().saturating_sub(1) as u32);
    w.put(4, u32::from(h.bit_rate_scale));
    w.put(4, u32::from(h.cpb_size_scale));
    for e in &h.cpb {
        write_cpb_entry(w, e);
    }
    w.put(5, u32::from(h.initial_cpb_removal_delay_length_minus1));
    w.put(5, u32::from(h.cpb_removal_delay_length_minus1));
    w.put(5, u32::from(h.dpb_output_delay_length_minus1));
    w.put(5, u32::from(h.time_offset_length));
}

/// One CPB entry inside `hrd_parameters()`.
fn write_cpb_entry(w: &mut BitWriter, e: &CpbEntry) {
    w.ue(e.bit_rate_value_minus1);
    w.ue(e.cpb_size_value_minus1);
    w.put(1, u32::from(e.cbr));
}

/// `bitstream_restriction()`'s fields, tail of §E.1.1 — the inverse of the
/// closing block of `parse_vui`.
fn write_bitstream_restriction(w: &mut BitWriter, b: &BitstreamRestriction) {
    w.put(1, u32::from(b.motion_vectors_over_pic_boundaries));
    w.ue(b.max_bytes_per_pic_denom);
    w.ue(b.max_bits_per_mb_denom);
    w.ue(b.log2_max_mv_length_horizontal);
    w.ue(b.log2_max_mv_length_vertical);
    w.ue(b.max_num_reorder_frames);
    w.ue(b.max_dec_frame_buffering);
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
    use vaco_codec_cbs::Cbs;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// SPS + PPS, both real `libx264` output, byte for byte, emulation
    /// prevention intact — the same fixtures `crate::params` (`SD_SPS_EBSP`,
    /// for `testsrc2=s=640x360:r=24`) and `crate::pps` (`X264_PPS`) already
    /// pin.
    fn stream() -> Vec<u8> {
        const SPS: &[u8] = &[
            0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00,
            0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0F, 0x16, 0x2D, 0x96,
        ];
        const PPS: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
        let mut v = Vec::new();
        for nal in [SPS, PPS] {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    #[test]
    fn a_stream_splits_into_its_units() {
        let mut cbs = Cbs::new(H264Cbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&stream(), Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [7, 8]
        );
    }

    #[test]
    fn an_untouched_fragment_round_trips_byte_for_byte() {
        let data = stream();
        let mut cbs = Cbs::new(H264Cbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::AnnexB,
            &mut out,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("transform");
        assert_eq!(out, data);
    }

    /// The property the write path exists for: read an SPS/PPS to a typed
    /// value and write it straight back, with no edit at all, and get
    /// exactly the same bytes — over real `libx264` output.
    #[test]
    fn sps_and_pps_round_trip_bit_exactly_with_no_edit() {
        let data = stream();
        let mut cbs = Cbs::new(H264Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");

        let sps = cbs.read_unit(&f, 0, &mut b).expect("an sps");
        assert!(matches!(sps, H264Content::Sps { .. }));
        let before_sps = f.units()[0].data.clone();
        cbs.update_unit(&mut f, 0, &sps, &mut b).expect("rewrites");
        assert_eq!(f.units()[0].data, before_sps, "sps re-encodes identically");

        let pps = cbs.read_unit(&f, 1, &mut b).expect("a pps");
        assert!(matches!(pps, H264Content::Pps { .. }));
        let before_pps = f.units()[1].data.clone();
        cbs.update_unit(&mut f, 1, &pps, &mut b).expect("rewrites");
        assert_eq!(f.units()[1].data, before_pps, "pps re-encodes identically");

        f.release(&mut b);
    }

    /// A field edit through the typed representation — the whole point of a
    /// write path that is not merely "copy the bytes back".
    #[test]
    fn editing_a_typed_field_changes_only_that_field() {
        let data = stream();
        let mut cbs = Cbs::new(H264Cbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");

        let H264Content::Sps {
            nal_ref_idc,
            mut sps,
        } = cbs.read_unit(&f, 0, &mut b).expect("an sps")
        else {
            panic!("expected an sps");
        };
        let original_dims = sps.dimensions();
        sps.level_idc = 51;
        cbs.update_unit(&mut f, 0, &H264Content::Sps { nal_ref_idc, sps }, &mut b)
            .expect("rewrites");

        let H264Content::Sps { sps, .. } = cbs.read_unit(&f, 0, &mut b).expect("re-read") else {
            panic!("expected an sps");
        };
        assert_eq!(sps.level_idc, 51, "the edited field stuck");
        assert_eq!(sps.dimensions(), original_dims, "nothing else moved");
        f.release(&mut b);
    }

    #[test]
    fn every_truncation_splits_and_reads_without_panicking() {
        let data = stream();
        let mut cbs = Cbs::new(H264Cbs::new());
        let mut b = budget();
        for n in 0..data.len() {
            let mut f = CbsFragment::new();
            let _ = cbs.split(&data[..n], Framing::AnnexB, &mut f, &mut b);
            for i in 0..f.len() {
                let _ = cbs.read_unit(&f, i, &mut b);
            }
            f.release(&mut b);
        }
    }

    /// Regression for a bug `cbs_h264` fuzzing found: `write_slice_group_map`
    /// derived the `Explicit` variant's per-entry bit width from the values
    /// actually present in the map (`ids.iter().max()`) instead of from the
    /// PPS's own `num_slice_groups`. Both bugs agree exactly when every group
    /// index 0..num_slice_groups-1 appears in the map, which is why no
    /// existing fixture caught it — here `num_slice_groups` is 5 but the map
    /// only ever uses indices 0 and 1, so the old code wrote 1 bit per entry
    /// where the spec (`Ceil(Log2(5))`) requires 3, desyncing everything
    /// after and making the re-encoded PPS fail to parse at all.
    #[test]
    fn an_explicit_slice_group_map_that_skips_group_indices_round_trips() {
        let pps = Pps {
            id: 5,
            sps_id: 1,
            entropy_coding_mode: true,
            bottom_field_pic_order_in_frame_present: false,
            num_slice_groups: 5,
            slice_group_map: Some(SliceGroupMap::Explicit(vec![0, 0, 1])),
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred: true,
            weighted_bipred_idc: 0,
            pic_init_qp_minus26: 1,
            pic_init_qs_minus26: 0,
            chroma_qp_index_offset: 1,
            deblocking_filter_control_present: true,
            constrained_intra_pred: false,
            redundant_pic_cnt_present: false,
            transform_8x8_mode: false,
            scaling_lists: None,
            second_chroma_qp_index_offset: -1,
            has_tail: true,
        };

        let mut w = RbspWriter::new();
        write_nal_header(w.bits(), 1, NalUnitType::Pps);
        write_pps_data(&pps, w.bits());
        let bytes = w.finish();

        let mut b = budget();
        let reparsed = Pps::parse(&bytes, None, &mut b).expect("re-parses");
        assert_eq!(reparsed, pps, "round trip must reproduce the same Pps");
    }

    /// Regression for a second bug the same fuzzing run found:
    /// `write_pps_data`'s scaling-list tail used `lists.present.len()` as the
    /// count of `seq_scaling_list_present_flag` bits to write, but
    /// [`ScalingLists::present`] is a fixed 12-entry array regardless of how
    /// many the source bitstream actually coded (6, for a 4:2:0 PPS tail
    /// without `transform_8x8_mode_flag`) — the entries past the real count
    /// are just defaulted `false`, never read. Writing all 12 anyway
    /// double-codes the tail and desyncs `second_chroma_qp_index_offset` and
    /// everything after it. The fix threads the real count through as
    /// [`ScalingLists::count`].
    ///
    /// `pic_scaling_matrix_present_flag` unconditionally requires the SPS to
    /// parse (it sizes the tail even when every individual list's own
    /// `present` bit turns out to be 0), so this test — unlike the others in
    /// this module — supplies one.
    #[test]
    fn a_pps_tail_with_fewer_than_twelve_scaling_lists_round_trips() {
        const SPS: &[u8] = &[
            0x67, 0x64, 0x00, 0x1E, 0xAC, 0xD9, 0x40, 0xA0, 0x2F, 0xF9, 0x70, 0x11, 0x00, 0x00,
            0x03, 0x00, 0x01, 0x00, 0x00, 0x03, 0x00, 0x30, 0x0F, 0x16, 0x2D, 0x96,
        ];
        let sps = Sps::parse(SPS, &mut budget()).expect("a real sps");

        let pps = Pps {
            id: 47,
            sps_id: 1,
            entropy_coding_mode: false,
            bottom_field_pic_order_in_frame_present: false,
            num_slice_groups: 1,
            slice_group_map: None,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred: true,
            weighted_bipred_idc: 1,
            pic_init_qp_minus26: 0,
            pic_init_qs_minus26: -1,
            chroma_qp_index_offset: 0,
            deblocking_filter_control_present: true,
            constrained_intra_pred: true,
            redundant_pic_cnt_present: false,
            // `transform_8x8_mode: false` means the tail's scaling-list
            // count is 6 (§7.3.2.2), not the full 12 — the case the old code
            // got wrong.
            transform_8x8_mode: false,
            scaling_lists: Some(Box::new(ScalingLists {
                count: 6,
                ..ScalingLists::default()
            })),
            second_chroma_qp_index_offset: -3,
            has_tail: true,
        };

        let mut w = RbspWriter::new();
        write_nal_header(w.bits(), 0, NalUnitType::Pps);
        write_pps_data(&pps, w.bits());
        let bytes = w.finish();

        let mut b = budget();
        let reparsed = Pps::parse(&bytes, Some(&sps), &mut b).expect("re-parses");
        assert_eq!(reparsed, pps, "round trip must reproduce the same Pps");
    }
}
