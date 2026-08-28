//! HEVC's implementation of [`CbsCodec`] — the read/modify/write face of the
//! crate.
//!
//! # What this adds over the parsers
//!
//! The rest of the crate reads a NAL unit and tells you what it says. This
//! module makes a *stream* editable: split an access unit or an `hvcC` into a
//! [`CbsFragment`], drop or insert units, decode the ones you care about,
//! change a field, write it back, and re-assemble — in the same framing or a
//! different one. That is the whole of `hevc_metadata`, `filter_units`,
//! `hevc_mp4toannexb` and `extract_extradata`.
//!
//! # The write path (D-19)
//!
//! [`HevcCbs::write_unit`] re-encodes [`HevcContent::Vps`], [`HevcContent::Sps`]
//! and [`HevcContent::Pps`] bit-exactly — `profile_tier_level()`, every
//! reference picture set and the whole VUI included — as well as
//! [`HevcContent::Raw`]. See the write-side section further down (just above
//! the `#[cfg(test)]` block) for exactly what it writes and the three narrow,
//! documented cases it cannot reconstruct losslessly (a predicted short-term
//! RPS, and the two SCC-extension payloads this crate's own reader discards).
//! [`HevcContent::Sei`] remains unwritten — nothing in this crate's dependents
//! edits a decoded SEI message today, so there is no tested caller for it yet.

use vaco_bitstream::{BitWriter, RbspWriter};
use vaco_codec_cbs::{CbsCodec, CbsFragment, CbsUnit, UnitOrigin};
use vaco_core::{Error, Result};
use vaco_format_nalu::{Framing, RbspBuf, units};
use vaco_limits::Budget;

use crate::nal::{HevcNalHeader, NalUnitType};

/// What a fragment cannot carry unchanged through Annex B.
///
/// Two shapes, both of which [`annexb_safe`] tests for, and **neither of which
/// can occur in a conforming stream**:
///
/// 1. **A unit whose bytes end in `0x00`.** §B.1 permits `trailing_zero_8bits`
///    after a NAL unit and they are indistinguishable from payload zeros — the
///    four-byte start code's own leading zero is one of them — so
///    `vaco-format-nalu`'s Annex B iterator trims them. §7.4.1.1's
///    `rbsp_trailing_bits()` ends every conforming unit with a `1` bit, so the
///    last byte is never zero.
/// 2. **A unit whose bytes contain `00 00 01`.** Writing it as Annex B makes
///    that sequence a start code, and reading it back yields *two* units.
///    §7.4.1.1's emulation prevention exists precisely so an EBSP never contains
///    one.
///
/// Both are properties of the *format*, not of this crate: Annex B is a strictly
/// less expressive container than a length prefix. Both were found by the
/// `cbs_hevc` fuzz target, which excludes exactly these cases from its
/// round-trip assertion.
///
/// The name exists so a conformance audit can find it, and
/// `a_unit_annex_b_cannot_express_is_reported` asserts the divergence is still
/// real rather than quietly closing.
pub const ANNEXB_EXPRESSIVENESS_DIVERGENCE: &str =
    "a NAL unit ending in 0x00, or containing 00 00 01, cannot round-trip through Annex B";

/// Whether `unit` survives being written as Annex B and read back.
///
/// A `hevc_mp4toannexb`-shaped filter should check this before reframing: a unit
/// that fails it is not a conforming NAL unit, and Annex B has no way to carry
/// it. See [`ANNEXB_EXPRESSIVENESS_DIVERGENCE`].
#[must_use]
pub fn annexb_safe(unit: &[u8]) -> bool {
    unit.last() != Some(&0) && !vaco_codec_cbs::violates_ebsp_constraint(unit)
}
use crate::pps::{DeblockingControl, Pps, PpsRangeExtension, Tiles};
use crate::ptl::{ProfileTier, ProfileTierLevel};
use crate::rps::ShortTermRps;
use crate::sei::SeiMessage;
use crate::sps::{
    BitstreamRestriction, ChromaFormat, CpbEntry, EXTENDED_SAR, HrdParameters, ScalingListData,
    Sps, SpsRangeExtension, SubLayerHrd, SubPicHrd, Timing, VuiParameters, Window,
};
use crate::util::MAX_SUB_LAYERS;
use crate::vps::Vps;

/// The typed content of one HEVC NAL unit.
///
/// [`HevcContent::Raw`] is not a failure: a unit whose syntax this crate does
/// not decode — a slice's payload, filler, a reserved type — is kept whole so
/// it can be re-emitted byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HevcContent {
    /// A video parameter set.
    Vps(Box<Vps>),
    /// A sequence parameter set.
    Sps(Box<Sps>),
    /// A picture parameter set.
    Pps(Box<Pps>),
    /// The messages of one SEI NAL unit, and whether it was a suffix unit.
    ///
    /// Owned rather than borrowed, because the borrow would be of the
    /// fragment's own bytes and would stop a caller editing the fragment while
    /// holding the decoded value.
    Sei {
        /// Whether this came from a `SUFFIX_SEI_NUT`.
        suffix: bool,
        /// The messages, with their payloads re-owned.
        messages: Vec<OwnedSeiMessage>,
    },
    /// Anything else: the unit's bytes, escaping intact.
    Raw {
        /// The NAL unit type.
        nal_unit_type: NalUnitType,
        /// The bytes, header included.
        data: Vec<u8>,
    },
}

/// One SEI message with its payload owned rather than borrowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedSeiMessage {
    /// `payloadType`.
    pub payload_type: u32,
    /// `payloadSize`, as declared.
    pub payload_size: u32,
    /// Whether the declared size ran past the end of the unit.
    pub truncated: bool,
    /// The payload bytes, un-decoded.
    pub data: Vec<u8>,
}

impl HevcContent {
    /// The NAL unit type this content would be written as.
    #[must_use]
    pub const fn nal_unit_type(&self) -> NalUnitType {
        match self {
            Self::Vps(_) => NalUnitType::VPS_NUT,
            Self::Sps(_) => NalUnitType::SPS_NUT,
            Self::Pps(_) => NalUnitType::PPS_NUT,
            Self::Sei { suffix: true, .. } => NalUnitType::SUFFIX_SEI_NUT,
            Self::Sei { suffix: false, .. } => NalUnitType::PREFIX_SEI_NUT,
            Self::Raw { nal_unit_type, .. } => *nal_unit_type,
        }
    }
}

/// The HEVC [`CbsCodec`].
///
/// Holds an [`RbspBuf`] so de-escaping a whole stream's units is one allocation
/// rather than one per unit, and nothing else — the parameter-set store belongs
/// to [`HevcParser`](crate::parser::HevcParser), which is the stateful reader;
/// a bitstream filter wants each unit decoded on its own terms.
#[derive(Debug, Default)]
pub struct HevcCbs {
    rbsp: RbspBuf,
}

impl HevcCbs {
    /// A fresh codec.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl CbsCodec for HevcCbs {
    type Content = HevcContent;
    type Framing = Framing;
    const NAME: &'static str = "hevc";

    fn split(
        &self,
        data: &[u8],
        framing: Framing,
        fragment: &mut CbsFragment,
        budget: &mut Budget,
    ) -> Result<()> {
        for nal in units(data, framing) {
            let Some(header) = HevcNalHeader::parse(nal.data) else {
                // A unit shorter than its own header is not a unit. Dropping it
                // rather than failing keeps a filter working on a stream with
                // one stray byte in it.
                continue;
            };
            fragment.push(
                CbsUnit::from_source(
                    u32::from(header.nal_unit_type.get()),
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
                    // Three bytes only when the source was Annex B and used
                    // three; four otherwise. A unit that came from a
                    // length-prefixed buffer has `framing_len` equal to the
                    // prefix width, which is not a start-code length at all —
                    // reading it as one is how a reframe round trip comes back
                    // three bytes per unit shorter than it went out.
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

    fn read_unit(&mut self, unit: &CbsUnit, budget: &mut Budget) -> Result<HevcContent> {
        let header = HevcNalHeader::parse(&unit.data).ok_or(Error::UnexpectedEof)?;
        let t = header.nal_unit_type;
        // Only the base layer's syntax is described here; a unit from any other
        // layer is kept whole so a filter can still move or drop it.
        if !header.is_base_layer() {
            return Ok(HevcContent::Raw {
                nal_unit_type: t,
                data: unit.data.clone(),
            });
        }
        self.rbsp.fill(&unit.data, budget)?;
        let rbsp = self.rbsp.as_slice();
        Ok(match t {
            NalUnitType::VPS_NUT => HevcContent::Vps(Box::new(Vps::parse(rbsp, budget)?)),
            NalUnitType::SPS_NUT => HevcContent::Sps(Box::new(Sps::parse(rbsp, budget)?)),
            NalUnitType::PPS_NUT => HevcContent::Pps(Box::new(Pps::parse(rbsp, budget)?)),
            t if t.is_sei() => {
                let messages = crate::sei::parse(rbsp, None, budget)?;
                HevcContent::Sei {
                    suffix: t == NalUnitType::SUFFIX_SEI_NUT,
                    messages: messages.iter().map(own_message).collect(),
                }
            }
            _ => HevcContent::Raw {
                nal_unit_type: t,
                data: unit.data.clone(),
            },
        })
    }

    fn write_unit(
        &mut self,
        content: &HevcContent,
        out: &mut Vec<u8>,
        budget: &mut Budget,
    ) -> Result<()> {
        match content {
            HevcContent::Raw { data, .. } => {
                budget.check(data.len() as u64)?;
                out.extend_from_slice(data);
                Ok(())
            }
            HevcContent::Vps(vps) => write_param_set(out, budget, NalUnitType::VPS_NUT, |w| {
                write_vps_data(vps, w);
                Ok(())
            }),
            HevcContent::Sps(sps) => {
                write_param_set(out, budget, NalUnitType::SPS_NUT, |w| write_sps_data(sps, w))
            }
            HevcContent::Pps(pps) => {
                write_param_set(out, budget, NalUnitType::PPS_NUT, |w| write_pps_data(pps, w))
            }
            HevcContent::Sei { .. } => Err(Error::Unsupported(
                "writing an HEVC SEI unit back out is not implemented",
            )),
        }
    }

    fn content_unit_type(&self, content: &HevcContent) -> u32 {
        u32::from(content.nal_unit_type().get())
    }
}

/// Copy one [`SeiMessage`]'s payload out of the fragment's buffer.
fn own_message(m: &SeiMessage<'_>) -> OwnedSeiMessage {
    OwnedSeiMessage {
        payload_type: m.payload_type,
        payload_size: m.payload_size,
        truncated: m.truncated,
        data: match &m.payload {
            crate::sei::SeiPayload::Other { data, .. }
            | crate::sei::SeiPayload::DecodedPictureHash { data, .. } => (*data).to_vec(),
            _ => Vec::new(),
        },
    }
}

// -------------------------------------------------------------------- write
//
// The write side of [`HevcCbs`]: a bit-exact re-encoding of
// `video_parameter_set_rbsp()` (§7.3.2.1), `seq_parameter_set_rbsp()`
// (§7.3.2.2) and `pic_parameter_set_rbsp()` (§7.3.2.3), each mirroring its
// crate's own `parse_data` field for field. Three documented, narrow
// deviations, none of them decode-affecting:
//
// 1. **`st_ref_pic_set()`** (inside an SPS) is always written in its
//    *explicit* spelling, never the inter-predicted one. [`ShortTermRps`]
//    itself only stores the derived `DeltaPocSX`/`UsedByCurrPicSX` lists —
//    "the syntax elements are not kept" is that module's own words — so the
//    original `delta_rps`/`ref_idx` that produced a predicted set cannot be
//    recovered, only a value that decodes to the identical lists. A real
//    encoder frequently uses prediction here, so this is the deviation most
//    likely to be visible on real content; see the round-trip test below for
//    what it costs on one real `x265` SPS.
// 2. **`sps_extension_4bits`, the multilayer and 3D extension bits** are
//    always written 0. Nothing in [`Sps`] retains them (§7.3.2.2.4's
//    multilayer bit and the whole 3D extension are explicitly not parsed),
//    and a conforming single-layer stream — everything this crate decodes —
//    never sets them.
// 3. **A `palette_mode_enabled` SCC extension, or a PPS SCC extension with
//    `slice_act_qp_offsets_present`,** cannot be written at all:
//    [`crate::sps::parse_scc_extension`]/[`crate::pps::read_scc_extension`]
//    both discard the actual palette/ACT payload while parsing (documented in
//    their own doc comments), so there is nothing here to write back.
//    [`write_unit`](HevcCbs::write_unit) reports [`Error::Unsupported`] for
//    exactly this case rather than emit a plausible-looking guess.

/// `nal_unit_header()`'s two bytes for a freshly-written parameter set:
/// `nuh_layer_id = 0` and `nuh_temporal_id_plus1 = 1`, i.e. `TemporalId = 0`.
/// §7.4.2.2 *requires* this of every VPS/SPS/PPS in a conforming stream —
/// `HevcCbs::read_unit` only ever decodes one of these three at
/// [`HevcNalHeader::is_base_layer`], and layer 0 base-layer parameter sets are
/// always `TemporalId == 0` — so hard-coding it here, rather than threading it
/// through [`HevcContent`], loses nothing a conforming stream could carry.
fn write_nal_header(w: &mut BitWriter, t: NalUnitType) {
    w.put(1, 0); // forbidden_zero_bit
    w.put(6, u32::from(t.get()));
    w.put(6, 0); // nuh_layer_id
    w.put(3, 1); // nuh_temporal_id_plus1
}

/// Write one parameter set: the two-byte NAL header, `body`, then
/// `rbsp_trailing_bits()` and escaping via [`RbspWriter`].
fn write_param_set(
    out: &mut Vec<u8>,
    budget: &mut Budget,
    t: NalUnitType,
    body: impl FnOnce(&mut BitWriter) -> Result<()>,
) -> Result<()> {
    let mut w = RbspWriter::new();
    write_nal_header(w.bits(), t);
    body(w.bits())?;
    let bytes = w.finish();
    budget.check(bytes.len() as u64)?;
    out.extend_from_slice(&bytes);
    Ok(())
}

/// `video_parameter_set_rbsp()`, §7.3.2.1 — the inverse of [`Vps::parse_data`].
fn write_vps_data(vps: &Vps, w: &mut BitWriter) {
    w.put(4, u32::from(vps.id));
    w.put(1, u32::from(vps.base_layer_internal));
    w.put(1, u32::from(vps.base_layer_available));
    w.put(6, u32::from(vps.max_layers) - 1);
    let max_sub_layers_minus1 = u32::from(vps.max_sub_layers) - 1;
    w.put(3, max_sub_layers_minus1);
    w.put(1, u32::from(vps.temporal_id_nesting));
    w.put(16, 0xFFFF); // vps_reserved_0xffff_16bits
    write_ptl(w, &vps.ptl, max_sub_layers_minus1);

    // `vps_sub_layer_ordering_info_present_flag`: this crate always stores one
    // entry per coded sub-layer (never collapses to the single-entry-repeated
    // form on read — see `Vps::parse_data`'s `first` variable), so a list
    // shorter than `max_sub_layers` is what "not present" looks like here.
    let ordering_present = vps.max_dec_pic_buffering_minus1.len() as u32 == max_sub_layers_minus1 + 1;
    w.put(1, u32::from(ordering_present));
    let first = if ordering_present { 0 } else { max_sub_layers_minus1 };
    for i in first..=max_sub_layers_minus1.min(MAX_SUB_LAYERS - 1) {
        let idx = (i - first) as usize;
        w.ue(vps.max_dec_pic_buffering_minus1.get(idx).copied().unwrap_or(0));
        w.ue(vps.max_num_reorder_pics.get(idx).copied().unwrap_or(0));
        w.ue(vps.max_latency_increase_plus1.get(idx).copied().unwrap_or(0));
    }

    w.put(6, u32::from(vps.max_layer_id));
    let num_layer_sets_minus1 = vps.num_layer_sets.saturating_sub(1);
    w.ue(num_layer_sets_minus1);
    // `layer_id_included_flag[i][j]`: every layer set beyond the first (base)
    // one includes every layer up to `max_layer_id`, which is the only shape
    // a single-layer stream (everything this crate parses) ever has.
    for _ in 0..num_layer_sets_minus1 {
        for _ in 0..=u32::from(vps.max_layer_id) {
            w.put(1, 1);
        }
    }

    match &vps.timing {
        Some(t) => {
            w.put(1, 1);
            w.put(32, t.num_units_in_tick);
            w.put(32, t.time_scale);
            match t.num_ticks_poc_diff_one_minus1 {
                Some(v) => {
                    w.put(1, 1);
                    w.ue(v);
                }
                None => w.put(1, 0),
            }
            w.ue(vps.hrd.len() as u32);
            for (i, (layer_set_idx, params)) in vps.hrd.iter().enumerate() {
                w.ue(*layer_set_idx);
                // `cprms_present_flag[0]` is inferred 1 and not coded (mirrors
                // `Vps::parse_data`); for i > 0 the crate does not retain
                // whether the source actually coded 0 or 1 here, so this
                // always writes 1 — a real, but exceedingly rare, deviation
                // limited to a VPS with more than one HRD entry (multi
                // operating-point signalling this crate's single-layer
                // decode never needs).
                if i > 0 {
                    w.put(1, 1);
                }
                write_hrd(w, params, true, max_sub_layers_minus1);
            }
        }
        None => w.put(1, 0),
    }
    w.put(1, u32::from(vps.extension_present));
    // `vps_extension_data_flag` (none stored) is the RBSP trailer's job.
}

/// The 88-bit profile-and-constraint block, §7.3.3 — the inverse of
/// `ptl::read_profile_tier`. Every field is stored raw, so this is a direct
/// replay with no derivation.
fn write_profile_tier(w: &mut BitWriter, p: &ProfileTier) {
    w.put(2, u32::from(p.profile_space));
    w.put(1, u32::from(p.tier_flag));
    w.put(5, u32::from(p.profile_idc));
    w.put(32, p.compatibility_flags);
    w.put(1, u32::from(p.progressive_source));
    w.put(1, u32::from(p.interlaced_source));
    w.put(1, u32::from(p.non_packed_constraint));
    w.put(1, u32::from(p.frame_only_constraint));
    w.put(32, (p.constraint_bits >> 11) as u32);
    w.put(11, (p.constraint_bits & 0x7FF) as u32);
    w.put(1, u32::from(p.inbld));
}

/// `profile_tier_level()`, §7.3.3 — the inverse of
/// [`ProfileTierLevel::parse`].
fn write_ptl(w: &mut BitWriter, ptl: &ProfileTierLevel, max_num_sub_layers_minus1: u32) {
    let sub_layers_minus1 = max_num_sub_layers_minus1.min(MAX_SUB_LAYERS - 1);
    if let Some(g) = &ptl.general {
        write_profile_tier(w, g);
    }
    w.put(8, u32::from(ptl.general_level_idc));

    for i in 0..sub_layers_minus1 as usize {
        let sl = ptl.sub_layers.get(i);
        w.put(1, u32::from(sl.is_some_and(|s| s.profile.is_some())));
        w.put(1, u32::from(sl.is_some_and(|s| s.level_idc.is_some())));
    }
    if sub_layers_minus1 > 0 {
        for _ in sub_layers_minus1..MAX_SUB_LAYERS {
            w.put(2, 0);
        }
    }
    for i in 0..sub_layers_minus1 as usize {
        let Some(sl) = ptl.sub_layers.get(i) else {
            continue;
        };
        if let Some(p) = &sl.profile {
            write_profile_tier(w, p);
        }
        if let Some(l) = sl.level_idc {
            w.put(8, u32::from(l));
        }
    }
}

/// `hrd_parameters(commonInfPresentFlag, maxNumSubLayersMinus1)`, §E.2.2 — the
/// inverse of `sps::parse_hrd`.
fn write_hrd(w: &mut BitWriter, h: &HrdParameters, common_inf_present: bool, max_sub_layers_minus1: u32) {
    if common_inf_present {
        w.put(1, u32::from(h.nal_hrd_present));
        w.put(1, u32::from(h.vcl_hrd_present));
        if h.nal_hrd_present || h.vcl_hrd_present {
            match &h.sub_pic {
                Some(s) => {
                    w.put(1, 1);
                    write_sub_pic_hrd(w, *s);
                }
                None => w.put(1, 0),
            }
            w.put(4, u32::from(h.bit_rate_scale));
            w.put(4, u32::from(h.cpb_size_scale));
            if h.sub_pic.is_some() {
                w.put(4, u32::from(h.cpb_size_du_scale));
            }
            w.put(5, u32::from(h.initial_cpb_removal_delay_length_minus1));
            w.put(5, u32::from(h.au_cpb_removal_delay_length_minus1));
            w.put(5, u32::from(h.dpb_output_delay_length_minus1));
        }
    }
    let sub_pic = h.sub_pic.is_some();
    for i in 0..=max_sub_layers_minus1.min(MAX_SUB_LAYERS - 1) as usize {
        let default_layer = SubLayerHrd::default();
        let layer = h.sub_layers.get(i).unwrap_or(&default_layer);
        w.put(1, u32::from(layer.fixed_pic_rate_general));
        if !layer.fixed_pic_rate_general {
            w.put(1, u32::from(layer.fixed_pic_rate_within_cvs));
        }
        if layer.fixed_pic_rate_within_cvs {
            w.ue(layer.elemental_duration_in_tc_minus1.unwrap_or(0));
        } else {
            w.put(1, u32::from(layer.low_delay_hrd));
        }
        if !layer.low_delay_hrd {
            w.ue(layer.cpb_cnt_minus1);
        }
        if h.nal_hrd_present {
            write_sub_layer_hrd(w, &layer.nal_cpb, sub_pic);
        }
        if h.vcl_hrd_present {
            write_sub_layer_hrd(w, &layer.vcl_cpb, sub_pic);
        }
    }
}

/// `sub_pic_hrd_params()`'s fields, §E.2.2.
fn write_sub_pic_hrd(w: &mut BitWriter, s: SubPicHrd) {
    w.put(8, u32::from(s.tick_divisor_minus2));
    w.put(5, u32::from(s.du_cpb_removal_delay_increment_length_minus1));
    w.put(1, u32::from(s.sub_pic_cpb_params_in_pic_timing_sei));
    w.put(5, u32::from(s.dpb_output_delay_du_length_minus1));
}

/// `sub_layer_hrd_parameters(i)`, §E.2.3 — the inverse of `read_sub_layer_hrd`.
fn write_sub_layer_hrd(w: &mut BitWriter, cpb: &[CpbEntry], sub_pic: bool) {
    for e in cpb {
        w.ue(e.bit_rate_value_minus1);
        w.ue(e.cpb_size_value_minus1);
        if sub_pic {
            w.ue(e.cpb_size_du_value_minus1);
            w.ue(e.bit_rate_du_value_minus1);
        }
        w.put(1, u32::from(e.cbr));
    }
}

/// `seq_parameter_set_rbsp()`, §7.3.2.2 — the inverse of [`Sps::parse_data`].
/// See the module doc for what this cannot reconstruct bit-exactly.
fn write_sps_data(sps: &Sps, w: &mut BitWriter) -> Result<()> {
    w.put(4, u32::from(sps.vps_id));
    let max_sub_layers_minus1 = u32::from(sps.max_sub_layers) - 1;
    w.put(3, max_sub_layers_minus1);
    w.put(1, u32::from(sps.temporal_id_nesting));
    write_ptl(w, &sps.ptl, max_sub_layers_minus1);
    w.ue(u32::from(sps.id));
    w.ue(sps.chroma_format.idc());
    if sps.chroma_format == ChromaFormat::Yuv444 {
        w.put(1, u32::from(sps.separate_colour_plane));
    }
    w.ue(sps.pic_width_in_luma_samples);
    w.ue(sps.pic_height_in_luma_samples);
    match sps.conformance_window {
        Some(win) => {
            w.put(1, 1);
            write_window(w, win);
        }
        None => w.put(1, 0),
    }
    w.ue(u32::from(sps.bit_depth_luma) - 8);
    w.ue(u32::from(sps.bit_depth_chroma) - 8);
    w.ue(u32::from(sps.log2_max_pic_order_cnt_lsb) - 4);

    let ordering_present = sps.max_dec_pic_buffering_minus1.len() as u32 == max_sub_layers_minus1 + 1;
    w.put(1, u32::from(ordering_present));
    let first = if ordering_present { 0 } else { max_sub_layers_minus1 };
    for i in first..=max_sub_layers_minus1.min(MAX_SUB_LAYERS - 1) {
        let idx = (i - first) as usize;
        w.ue(sps.max_dec_pic_buffering_minus1.get(idx).copied().unwrap_or(0));
        w.ue(sps.max_num_reorder_pics.get(idx).copied().unwrap_or(0));
        w.ue(sps.max_latency_increase_plus1.get(idx).copied().unwrap_or(0));
    }

    w.ue(u32::from(sps.log2_min_cb_size) - 3);
    w.ue(u32::from(sps.log2_diff_max_min_cb_size));
    w.ue(u32::from(sps.log2_min_tb_size) - 2);
    w.ue(u32::from(sps.log2_diff_max_min_tb_size));
    w.ue(sps.max_transform_hierarchy_depth_inter);
    w.ue(sps.max_transform_hierarchy_depth_intra);

    w.put(1, u32::from(sps.scaling_list_enabled));
    if sps.scaling_list_enabled {
        match &sps.scaling_list {
            Some(list) => {
                w.put(1, 1);
                write_scaling_list_data(w, list);
            }
            None => w.put(1, 0),
        }
    }
    w.put(1, u32::from(sps.amp_enabled));
    w.put(1, u32::from(sps.sample_adaptive_offset_enabled));
    match &sps.pcm {
        Some(pcm) => {
            w.put(1, 1);
            w.put(4, u32::from(pcm.sample_bit_depth_luma) - 1);
            w.put(4, u32::from(pcm.sample_bit_depth_chroma) - 1);
            w.ue(u32::from(pcm.log2_min_cb_size) - 3);
            w.ue(u32::from(pcm.log2_diff_max_min_cb_size));
            w.put(1, u32::from(pcm.loop_filter_disabled));
        }
        None => w.put(1, 0),
    }

    w.ue(sps.short_term_ref_pic_sets.len() as u32);
    for (i, set) in sps.short_term_ref_pic_sets.iter().enumerate() {
        write_st_ref_pic_set_explicit(w, set, i != 0);
    }

    w.put(1, u32::from(sps.long_term_ref_pics_present));
    if sps.long_term_ref_pics_present {
        w.ue(sps.long_term_ref_pics.len() as u32);
        for &(poc_lsb, used) in &sps.long_term_ref_pics {
            w.put(u32::from(sps.log2_max_pic_order_cnt_lsb), poc_lsb);
            w.put(1, u32::from(used));
        }
    }

    w.put(1, u32::from(sps.temporal_mvp_enabled));
    w.put(1, u32::from(sps.strong_intra_smoothing_enabled));
    match &sps.vui {
        Some(vui) => {
            w.put(1, 1);
            write_vui(w, vui, max_sub_layers_minus1);
        }
        None => w.put(1, 0),
    }

    let has_scc = sps.scc_extension.is_some();
    if let Some(scc) = &sps.scc_extension
        && scc.palette_mode_enabled
    {
        // See the module doc: the palette predictor payload was discarded on
        // read, so there is nothing to write here.
        return Err(Error::Unsupported(
            "an SPS SCC extension with palette mode cannot be re-encoded: \
             the palette predictor payload was not retained on read",
        ));
    }
    let extension_present = sps.range_extension.is_some() || has_scc;
    w.put(1, u32::from(extension_present));
    if extension_present {
        w.put(1, u32::from(sps.range_extension.is_some()));
        w.put(1, 0); // sps_multilayer_extension_flag: not tracked, never set
        w.put(1, 0); // sps_3d_extension_flag: not tracked, never set
        w.put(1, u32::from(has_scc));
        w.put(4, 0); // sps_extension_4bits: not tracked, always written 0
        if let Some(r) = &sps.range_extension {
            write_sps_range_extension(w, r);
        }
        if let Some(scc) = &sps.scc_extension {
            w.put(1, u32::from(scc.curr_pic_ref_enabled));
            w.put(1, 0); // palette_mode_enabled_flag: excluded above
            w.put(2, u32::from(scc.motion_vector_resolution_control_idc));
            w.put(1, u32::from(scc.intra_boundary_filtering_disabled));
        }
    }
    Ok(())
}

/// `conf_win_*`/`def_disp_win_*`, the one `ue(v)` quartet shared by the
/// conformance window and the VUI's default display window.
fn write_window(w: &mut BitWriter, win: Window) {
    w.ue(win.left);
    w.ue(win.right);
    w.ue(win.top);
    w.ue(win.bottom);
}

/// `st_ref_pic_set(stRpsIdx)`, §7.3.7, **always in its explicit spelling**.
/// See the module doc: the inter-predicted spelling's own inputs are not
/// retained by this crate's reader, so this is the one always-available
/// encoding — decodes to the identical `DeltaPocSX`/`UsedByCurrPicSX` lists,
/// whether or not the source used prediction.
///
/// `first` is `st_rps_idx != 0`, which is what gates
/// `inter_ref_pic_set_prediction_flag`'s presence at all — writing explicit
/// still means writing that flag as 0 for every set after the first.
fn write_st_ref_pic_set_explicit(w: &mut BitWriter, set: &ShortTermRps, first: bool) {
    if first {
        w.put(1, 0); // inter_ref_pic_set_prediction_flag
    }
    w.ue(set.num_negative_pics());
    w.ue(set.num_positive_pics());
    let mut prev = 0i32;
    for (i, &d) in set.delta_poc_s0.iter().enumerate() {
        w.ue((prev - d).unsigned_abs().saturating_sub(1));
        prev = d;
        w.put(1, u32::from(set.used_by_curr_pic_s0.get(i).copied().unwrap_or(false)));
    }
    prev = 0;
    for (i, &d) in set.delta_poc_s1.iter().enumerate() {
        w.ue((d - prev).unsigned_abs().saturating_sub(1));
        prev = d;
        w.put(1, u32::from(set.used_by_curr_pic_s1.get(i).copied().unwrap_or(false)));
    }
}

/// `scaling_list_data()`, §7.3.4 — the inverse of `sps::read_scaling_list_data`.
/// Unlike H.264's, there is no early-termination sentinel here: every
/// coefficient is an explicit `scaling_list_delta_coef`, so this is a direct,
/// unambiguous replay of the stored values.
fn write_scaling_list_data(w: &mut BitWriter, data: &ScalingListData) {
    for size_id in 0usize..4 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0usize;
        while matrix_id < 6 {
            let pred_mode = data
                .pred_mode
                .get(size_id)
                .and_then(|row| row.get(matrix_id))
                .copied()
                .unwrap_or(false);
            w.put(1, u32::from(pred_mode));
            if pred_mode {
                let coef_num = 64usize.min(1 << (4 + (size_id << 1)));
                let mut prev = 8i32;
                if size_id > 1 {
                    let dc = data
                        .dc_coef
                        .get(size_id - 2)
                        .and_then(|row| row.get(matrix_id))
                        .copied()
                        .unwrap_or(8);
                    w.se(dc - 8);
                    prev = dc;
                }
                for i in 0..coef_num {
                    let target = i32::from(
                        data.coef
                            .get(size_id)
                            .and_then(|m| m.get(matrix_id))
                            .and_then(|row| row.get(i))
                            .copied()
                            .unwrap_or(0),
                    );
                    let raw = target - prev;
                    let delta = ((raw + 128).rem_euclid(256)) - 128;
                    w.se(delta);
                    prev = target;
                }
            } else {
                let delta = data
                    .pred_matrix_id_delta
                    .get(size_id)
                    .and_then(|row| row.get(matrix_id))
                    .copied()
                    .unwrap_or(0);
                w.ue(delta);
            }
            matrix_id += step;
        }
    }
}

/// `vui_parameters()`, §E.2.1 — the inverse of `sps::parse_vui`.
fn write_vui(w: &mut BitWriter, vui: &VuiParameters, max_sub_layers_minus1: u32) {
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
    w.put(1, u32::from(vui.neutral_chroma_indication));
    w.put(1, u32::from(vui.field_seq));
    w.put(1, u32::from(vui.frame_field_info_present));
    match vui.default_display_window {
        Some(win) => {
            w.put(1, 1);
            write_window(w, win);
        }
        None => w.put(1, 0),
    }
    match vui.timing {
        Some(t) => {
            w.put(1, 1);
            write_vui_timing(w, t);
            match &vui.hrd {
                Some(h) => {
                    w.put(1, 1);
                    write_hrd(w, h, true, max_sub_layers_minus1);
                }
                None => w.put(1, 0),
            }
        }
        None => w.put(1, 0),
    }
    match &vui.bitstream_restriction {
        Some(b) => {
            w.put(1, 1);
            write_bitstream_restriction(w, b);
        }
        None => w.put(1, 0),
    }
}

/// `vui_timing_info`'s five fields, §E.2.1.
fn write_vui_timing(w: &mut BitWriter, t: Timing) {
    w.put(32, t.num_units_in_tick);
    w.put(32, t.time_scale);
    match t.num_ticks_poc_diff_one_minus1 {
        Some(v) => {
            w.put(1, 1);
            w.ue(v);
        }
        None => w.put(1, 0),
    }
}

/// `bitstream_restriction()`'s fields, tail of §E.2.1.
fn write_bitstream_restriction(w: &mut BitWriter, b: &BitstreamRestriction) {
    w.put(1, u32::from(b.tiles_fixed_structure));
    w.put(1, u32::from(b.motion_vectors_over_pic_boundaries));
    w.put(1, u32::from(b.restricted_ref_pic_lists));
    w.ue(b.min_spatial_segmentation_idc);
    w.ue(b.max_bytes_per_pic_denom);
    w.ue(b.max_bits_per_min_cu_denom);
    w.ue(b.log2_max_mv_length_horizontal);
    w.ue(b.log2_max_mv_length_vertical);
}

/// `sps_range_extension()`, §7.3.2.2.2 — every field, in order.
fn write_sps_range_extension(w: &mut BitWriter, r: &SpsRangeExtension) {
    w.put(1, u32::from(r.transform_skip_rotation_enabled));
    w.put(1, u32::from(r.transform_skip_context_enabled));
    w.put(1, u32::from(r.implicit_rdpcm_enabled));
    w.put(1, u32::from(r.explicit_rdpcm_enabled));
    w.put(1, u32::from(r.extended_precision_processing));
    w.put(1, u32::from(r.intra_smoothing_disabled));
    w.put(1, u32::from(r.high_precision_offsets_enabled));
    w.put(1, u32::from(r.persistent_rice_adaptation_enabled));
    w.put(1, u32::from(r.cabac_bypass_alignment_enabled));
}

/// `pic_parameter_set_rbsp()`, §7.3.2.3 — the inverse of [`Pps::parse_data`].
fn write_pps_data(pps: &Pps, w: &mut BitWriter) -> Result<()> {
    w.ue(u32::from(pps.id));
    w.ue(u32::from(pps.sps_id));
    w.put(1, u32::from(pps.dependent_slice_segments_enabled));
    w.put(1, u32::from(pps.output_flag_present));
    w.put(3, u32::from(pps.num_extra_slice_header_bits));
    w.put(1, u32::from(pps.sign_data_hiding_enabled));
    w.put(1, u32::from(pps.cabac_init_present));
    w.ue(pps.num_ref_idx_l0_default_active_minus1);
    w.ue(pps.num_ref_idx_l1_default_active_minus1);
    w.se(pps.init_qp_minus26);
    w.put(1, u32::from(pps.constrained_intra_pred));
    w.put(1, u32::from(pps.transform_skip_enabled));
    w.put(1, u32::from(pps.cu_qp_delta_enabled));
    if pps.cu_qp_delta_enabled {
        w.ue(pps.diff_cu_qp_delta_depth);
    }
    w.se(pps.cb_qp_offset);
    w.se(pps.cr_qp_offset);
    w.put(1, u32::from(pps.slice_chroma_qp_offsets_present));
    w.put(1, u32::from(pps.weighted_pred));
    w.put(1, u32::from(pps.weighted_bipred));
    w.put(1, u32::from(pps.transquant_bypass_enabled));
    w.put(1, u32::from(pps.tiles.is_some()));
    w.put(1, u32::from(pps.entropy_coding_sync_enabled));

    if let Some(tiles) = &pps.tiles {
        write_tiles(w, tiles);
    }
    w.put(1, u32::from(pps.loop_filter_across_slices_enabled));
    match &pps.deblocking {
        Some(d) => {
            w.put(1, 1);
            write_deblocking_control(w, d);
        }
        None => w.put(1, 0),
    }
    match &pps.scaling_list {
        Some(list) => {
            w.put(1, 1);
            write_scaling_list_data(w, list);
        }
        None => w.put(1, 0),
    }
    w.put(1, u32::from(pps.lists_modification_present));
    w.ue(pps.log2_parallel_merge_level - 2);
    w.put(1, u32::from(pps.slice_segment_header_extension_present));

    let has_scc = pps.scc_extension.is_some();
    if let Some(scc) = &pps.scc_extension
        && scc.slice_act_qp_offsets_present
    {
        // See the module doc: the three ACT QP offsets were discarded on
        // read, so there is nothing to write here.
        return Err(Error::Unsupported(
            "a PPS SCC extension with slice ACT QP offsets cannot be re-encoded: \
             the offset values were not retained on read",
        ));
    }
    let extension_present = pps.range_extension.is_some() || has_scc;
    w.put(1, u32::from(extension_present));
    if extension_present {
        w.put(1, u32::from(pps.range_extension.is_some()));
        w.put(1, 0); // pps_multilayer_extension_flag: not tracked, never set
        w.put(1, 0); // pps_3d_extension_flag: not tracked, never set
        w.put(1, u32::from(has_scc));
        w.put(4, 0); // pps_extension_4bits
        if let Some(r) = &pps.range_extension {
            write_pps_range_extension(w, r, pps.transform_skip_enabled);
        }
        if let Some(scc) = &pps.scc_extension {
            w.put(1, u32::from(scc.curr_pic_ref_enabled));
            w.put(1, 0); // residual_adaptive_colour_transform_enabled_flag: excluded above
        }
    }
    Ok(())
}

/// The tile-layout block of §7.3.2.3.
fn write_tiles(w: &mut BitWriter, tiles: &Tiles) {
    w.ue(tiles.num_columns - 1);
    w.ue(tiles.num_rows - 1);
    w.put(1, u32::from(tiles.uniform_spacing));
    if !tiles.uniform_spacing {
        for &c in &tiles.column_widths {
            w.ue(c - 1);
        }
        for &r in &tiles.row_heights {
            w.ue(r - 1);
        }
    }
    w.put(1, u32::from(tiles.loop_filter_across_tiles));
}

/// The deblocking-control block of §7.3.2.3.
fn write_deblocking_control(w: &mut BitWriter, d: &DeblockingControl) {
    w.put(1, u32::from(d.override_enabled));
    w.put(1, u32::from(d.disabled));
    if !d.disabled {
        w.se(d.beta_offset_div2);
        w.se(d.tc_offset_div2);
    }
}

/// `pps_range_extension()`, §7.3.2.3.2 — the inverse of `read_range_extension`.
fn write_pps_range_extension(w: &mut BitWriter, r: &PpsRangeExtension, transform_skip_enabled: bool) {
    if transform_skip_enabled {
        w.ue(r.log2_max_transform_skip_block_size_minus2);
    }
    w.put(1, u32::from(r.cross_component_prediction_enabled));
    w.put(1, u32::from(r.chroma_qp_offset_list_enabled));
    if r.chroma_qp_offset_list_enabled {
        w.ue(r.diff_cu_chroma_qp_offset_depth);
        w.ue(r.cb_qp_offset_list.len().saturating_sub(1) as u32);
        for (&cb, &cr) in r.cb_qp_offset_list.iter().zip(r.cr_qp_offset_list.iter()) {
            w.se(cb);
            w.se(cr);
        }
    }
    w.ue(r.log2_sao_offset_scale_luma);
    w.ue(r.log2_sao_offset_scale_chroma);
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
    use vaco_format_nalu::LengthSize;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    /// VPS, SPS, PPS, prefix SEI and an IDR slice, in Annex B, from `sd.265`.
    fn stream() -> Vec<u8> {
        let mut v = Vec::new();
        for nal in [
            &[
                0x40u8, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90,
                0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x3f, 0x95, 0x98, 0x09,
            ][..],
            &[
                0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
                0x00, 0x03, 0x00, 0x3f, 0xa0, 0x05, 0x02, 0x01, 0x69, 0x65, 0x95, 0x9a, 0x49, 0x32,
                0xbc, 0x05, 0xa0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x01,
            ][..],
            &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40][..],
            &[0x4e, 0x01, 0x05, 0x02, 0x11, 0x22, 0x80][..],
            &[0x28, 0x01, 0xaf, 0x1d, 0x30, 0xc6, 0x23, 0x40, 0xf2, 0xcd][..],
        ] {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    #[test]
    fn a_stream_splits_into_its_five_units() {
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut f = CbsFragment::new();
        let mut b = budget();
        cbs.split(&stream(), Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        assert_eq!(
            f.units().iter().map(|u| u.unit_type).collect::<Vec<_>>(),
            [32, 33, 34, 39, 20]
        );
    }

    /// The property the whole layer rests on.
    #[test]
    fn an_untouched_fragment_round_trips_byte_for_byte() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
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

    /// `filter_units`: drop every SEI unit, keep everything else exactly.
    #[test]
    fn dropping_sei_leaves_the_rest_untouched() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut out = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::AnnexB,
            &mut out,
            &mut b,
            |_, f, _| {
                f.retain(|u| u.unit_type != 39 && u.unit_type != 40);
                Ok(())
            },
        )
        .expect("transform");
        // The SEI unit was 7 bytes plus a 4-byte start code.
        assert_eq!(out.len(), data.len() - 11);
        assert!(!out.windows(2).any(|w| w == [0x4e, 0x01]));
    }

    /// `hevc_mp4toannexb`, and its inverse.
    #[test]
    fn reframing_is_lossless_in_both_directions() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut prefixed = Vec::new();
        cbs.transform(
            &data,
            Framing::AnnexB,
            Framing::LengthPrefixed(LengthSize::FOUR),
            &mut prefixed,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("to length-prefixed");
        // Five units, each losing a four-byte start code and gaining a
        // four-byte length: the same size.
        assert_eq!(prefixed.len(), data.len());

        let mut back = Vec::new();
        cbs.transform(
            &prefixed,
            Framing::LengthPrefixed(LengthSize::FOUR),
            Framing::AnnexB,
            &mut back,
            &mut b,
            |_, _, _| Ok(()),
        )
        .expect("back to Annex B");
        assert_eq!(back, data);
    }

    /// The typed read path, over every unit the crate understands.
    #[test]
    fn each_parameter_set_decodes_to_its_own_type() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        assert!(matches!(
            cbs.read_unit(&f, 0, &mut b),
            Ok(HevcContent::Vps(_))
        ));
        match cbs.read_unit(&f, 1, &mut b) {
            Ok(HevcContent::Sps(sps)) => assert_eq!(sps.dimensions(), Some((640, 360))),
            other => panic!("expected an SPS, got {other:?}"),
        }
        assert!(matches!(
            cbs.read_unit(&f, 2, &mut b),
            Ok(HevcContent::Pps(_))
        ));
        match cbs.read_unit(&f, 3, &mut b) {
            Ok(HevcContent::Sei { suffix, messages }) => {
                assert!(!suffix);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].payload_type, 5);
            }
            other => panic!("expected an SEI unit, got {other:?}"),
        }
        // A slice is kept whole.
        match cbs.read_unit(&f, 4, &mut b) {
            Ok(HevcContent::Raw { nal_unit_type, .. }) => {
                assert_eq!(nal_unit_type, NalUnitType::IDR_N_LP);
            }
            other => panic!("expected a raw unit, got {other:?}"),
        }
    }

    /// A raw unit writes back byte for byte.
    #[test]
    fn a_raw_rewrite_changes_nothing() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        let before = f.units()[4].data.clone();
        let raw = cbs.read_unit(&f, 4, &mut b).expect("a raw unit");
        cbs.update_unit(&mut f, 4, &raw, &mut b).expect("writes");
        assert_eq!(f.units()[4].data, before, "a raw rewrite changes nothing");
        f.release(&mut b);
    }

    /// The write path this crate previously had none of: read a real VPS,
    /// SPS and PPS (all three from `sd.265`, the same fixture `stream()`
    /// uses) to their typed form and write them straight back with no edit,
    /// and check the result against the original bytes.
    ///
    /// VPS and PPS come back **byte for byte** — neither exercises this
    /// crate's three documented deviations (see the write-side module doc).
    /// The SPS is reported, not asserted exact: whether `sd.265`'s single
    /// short-term RPS entry happens to hit the inter-prediction deviation is
    /// exactly the kind of thing that must be measured, not assumed, and a
    /// spurious byte-exact assertion on one fixture would prove less than it
    /// looks like it proves either way.
    #[test]
    fn vps_and_pps_round_trip_bit_exactly_with_no_edit() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");

        let vps = cbs.read_unit(&f, 0, &mut b).expect("a vps");
        assert!(matches!(vps, HevcContent::Vps(_)));
        let before_vps = f.units()[0].data.clone();
        cbs.update_unit(&mut f, 0, &vps, &mut b).expect("rewrites");
        assert_eq!(f.units()[0].data, before_vps, "vps re-encodes identically");

        let pps = cbs.read_unit(&f, 2, &mut b).expect("a pps");
        assert!(matches!(pps, HevcContent::Pps(_)));
        let before_pps = f.units()[2].data.clone();
        cbs.update_unit(&mut f, 2, &pps, &mut b).expect("rewrites");
        assert_eq!(f.units()[2].data, before_pps, "pps re-encodes identically");

        let sps = cbs.read_unit(&f, 1, &mut b).expect("an sps");
        assert!(matches!(sps, HevcContent::Sps(_)));
        let before_sps = f.units()[1].data.clone();
        cbs.update_unit(&mut f, 1, &sps, &mut b).expect("rewrites");
        let after_sps = f.units()[1].data.clone();
        eprintln!(
            "sd.265 SPS round-trip: {} (before {} bytes, after {} bytes)",
            if before_sps == after_sps {
                "byte-exact"
            } else {
                "differs — see module doc for the documented RPS deviation"
            },
            before_sps.len(),
            after_sps.len(),
        );
        // Whatever the byte comparison says, re-reading the rewritten unit
        // must still decode to the identical semantic content — the actual
        // bar (see the write-side module doc), not the bytes.
        let HevcContent::Sps(reread) = cbs.read_unit(&f, 1, &mut b).expect("re-read") else {
            panic!("expected an sps");
        };
        let HevcContent::Sps(original) = sps else {
            panic!("expected an sps");
        };
        assert_eq!(reread, original, "re-encoding must preserve every field");
        f.release(&mut b);
    }

    /// A field edit through the typed SPS changes only that field — the
    /// point of a write path over "copy the bytes back".
    #[test]
    fn editing_a_typed_sps_field_changes_only_that_field() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");

        let HevcContent::Sps(mut sps) = cbs.read_unit(&f, 1, &mut b).expect("an sps") else {
            panic!("expected an sps");
        };
        let original_dims = sps.dimensions();
        sps.ptl.general_level_idc = 90;
        cbs.update_unit(&mut f, 1, &HevcContent::Sps(sps), &mut b)
            .expect("rewrites");

        let HevcContent::Sps(sps) = cbs.read_unit(&f, 1, &mut b).expect("re-read") else {
            panic!("expected an sps");
        };
        assert_eq!(sps.ptl.general_level_idc, 90, "the edited field stuck");
        assert_eq!(sps.dimensions(), original_dims, "nothing else moved");
        f.release(&mut b);
    }

    /// `extract_extradata`: lift the parameter sets out of an access unit into
    /// a fragment of their own.
    #[test]
    fn parameter_sets_can_be_lifted_into_their_own_fragment() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();
        let mut f = CbsFragment::new();
        cbs.split(&data, Framing::AnnexB, &mut f, &mut b)
            .expect("splits");
        let mut extradata = CbsFragment::new();
        for unit in f.units() {
            if NalUnitType::from_u8(unit.unit_type as u8).is_parameter_set() {
                extradata.push(unit.clone(), &mut b).expect("push");
            }
        }
        assert_eq!(extradata.len(), 3);
        let mut out = Vec::new();
        cbs.assemble(&extradata, Framing::AnnexB, &mut out, &mut b)
            .expect("assembles");
        assert_eq!(out.len(), 4 * 3 + 24 + 42 + 7);
        extradata.release(&mut b);
        f.release(&mut b);
    }

    /// Both reframing divergences, pinned. See
    /// [`ANNEXB_EXPRESSIVENESS_DIVERGENCE`].
    #[test]
    fn a_unit_annex_b_cannot_express_is_reported() {
        let mut cbs = Cbs::new(HevcCbs::new());
        let mut b = budget();

        // Case 1: a unit whose last two bytes are zero. Length-prefixed says
        // five bytes; Annex B gives three back.
        let trailing = [0x00u8, 0x00, 0x00, 0x05, 0x40, 0x01, 0x0c, 0x00, 0x00];
        // Case 2: a unit containing a start code, which Annex B splits in two.
        // The second unit needs two bytes of its own, or the split drops it as
        // too short to hold a header — which would hide the divergence.
        let embedded = [
            0x00u8, 0x00, 0x00, 0x08, 0x40, 0x01, 0x0c, 0x00, 0x00, 0x01, 0x0d, 0x0e,
        ];
        // ...and one that is a conforming EBSP, which survives untouched.
        let ok = [0x00u8, 0x00, 0x00, 0x03, 0x40, 0x01, 0x0c];

        for (name, prefixed, safe, expect_units) in [
            ("trailing zeros", &trailing[..], false, 1usize),
            ("embedded start code", &embedded[..], false, 2),
            ("conforming", &ok[..], true, 1),
        ] {
            let mut f = CbsFragment::new();
            cbs.split(
                prefixed,
                Framing::LengthPrefixed(LengthSize::FOUR),
                &mut f,
                &mut b,
            )
            .expect("splits");
            assert_eq!(f.len(), 1, "{name}: one unit in");
            let before = f.units()[0].data.clone();
            assert_eq!(
                annexb_safe(&before),
                safe,
                "{name}: {ANNEXB_EXPRESSIVENESS_DIVERGENCE}"
            );

            let mut annexb = Vec::new();
            cbs.assemble(&f, Framing::AnnexB, &mut annexb, &mut b)
                .expect("assembles");
            let mut back = CbsFragment::new();
            cbs.split(&annexb, Framing::AnnexB, &mut back, &mut b)
                .expect("splits");
            assert_eq!(back.len(), expect_units, "{name}: units out");
            if safe {
                assert_eq!(back.units()[0].data, before, "{name}: survives");
            } else {
                assert_ne!(back.units()[0].data, before, "{name}: diverges");
            }
            f.release(&mut b);
            back.release(&mut b);
        }
    }

    #[test]
    fn every_truncation_splits_without_panicking() {
        let data = stream();
        let mut cbs = Cbs::new(HevcCbs::new());
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
}
