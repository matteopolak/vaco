//! Building an `avcC` Configuration Record (ISO/IEC 14496-15 §5.3.3.1.2)
//! from H.264 SPS/PPS units already in hand — the write-side mirror of
//! `vaco-mux-smoothstreaming::avcc::avcc_to_annexb`'s read side, for the
//! opposite gap: a container carrying H.264 in-band (MPEG-TS, AVI, raw
//! Annex B) has SPS/PPS `assemble_extradata` can already turn into an
//! Annex-B buffer for `-show_streams`'s benefit, but a `-c copy` target
//! that needs a *real* length-prefixed configuration record (Matroska's
//! `V_MPEG4/ISO/AVC` `CodecPrivate`, today) cannot use that buffer as-is —
//! `avcC` is a different, structured record, not just a reframing.
//!
//! # Why this is not `vaco-parse-h264`
//!
//! D14.1: a format/mux crate reaches codec-level parsing only through the
//! injected `ParserProvider` seam, never a direct crate dependency.
//! `AVCProfileIndication`/`profile_compatibility`/`AVCLevelIndication` are a
//! straight byte copy of the SPS's own first three bytes — no parsing at
//! all — and the "high profile" extension fields
//! (`chroma_format_idc`/`bit_depth_luma_minus8`/`bit_depth_chroma_minus8`)
//! need only `seq_parameter_set_id` and three more Exp-Golomb reads off the
//! front of the RBSP, both already reachable through this crate's own
//! [`crate::RbspBuf`] and [`vaco_bitstream::GolombRead`]. Hand-parsing this
//! much is the same "less machinery than routing through the seam" call
//! `vaco-mux-smoothstreaming::avcc` already made for the opposite
//! direction, for the same reason.
//!
//! # Specification
//!
//! ISO/IEC 14496-15 §5.3.3.1.2 for the record's own layout; ITU-T H.264
//! §7.3.2.1.1 for `seq_parameter_set_data()`'s high-profile extension
//! fields and the exact `profile_idc` set that carries them.

use vaco_bitstream::GolombRead;
use vaco_limits::{Budget, Limits};

use crate::header::{HeaderKind, NalHeader};
use crate::framing::Framing;
use crate::rbsp::RbspBuf;

/// `profile_idc` values whose `seq_parameter_set_data()` carries
/// `chroma_format_idc`/`bit_depth_luma_minus8`/`bit_depth_chroma_minus8`
/// (ITU-T H.264 §7.3.2.1.1's own condition on `seq_parameter_set_data()`,
/// transcribed from the spec, not guessed).
const HIGH_PROFILE_IDC: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

/// Build an `avcC` record from one or more SPS units and zero or more PPS
/// units — Annex-B [`crate::Nal`] payloads (header byte first, emulation
/// prevention intact), exactly what [`crate::units`] yields and
/// [`crate::parameter_sets`] already filters down to "parameter sets" —
/// this function's own job is only to separate SPS from PPS and lay the
/// record out.
///
/// `lengthSizeMinusOne` is always written as 3 (4-byte NAL lengths): the
/// only width every length-prefixed writer in this workspace produces (see
/// [`crate::annexb_to_length_prefixed`]'s own callers), so there is nothing
/// else for a record built here to declare.
///
/// `None` when `sps` is empty — nothing to derive
/// `AVCProfileIndication`/level from at all — or the first SPS is too
/// short to hold even the three fixed bytes this needs.
#[must_use]
pub fn build_h264_avcc(sps: &[&[u8]], pps: &[&[u8]]) -> Option<Vec<u8>> {
    let first_sps = *sps.first()?;
    let &[_header, profile_idc, profile_compatibility, level_idc, ..] = first_sps else {
        return None;
    };

    let mut out = vec![
        1, // configurationVersion
        profile_idc,
        profile_compatibility,
        level_idc,
        0xFF, // reserved(6)=111111, lengthSizeMinusOne(2)=11 (4 bytes)
        0xE0 | u8::try_from(sps.len()).unwrap_or(31).min(31), // reserved(3)=111, numSPS(5)
    ];
    for s in sps {
        out.extend_from_slice(&u16::try_from(s.len()).unwrap_or(u16::MAX).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(u8::try_from(pps.len()).unwrap_or(u8::MAX));
    for p in pps {
        out.extend_from_slice(&u16::try_from(p.len()).unwrap_or(u16::MAX).to_be_bytes());
        out.extend_from_slice(p);
    }

    if HIGH_PROFILE_IDC.contains(&profile_idc)
        && let Some(ext) = high_profile_extension(first_sps)
    {
        out.extend_from_slice(&ext);
    }

    Some(out)
}

/// The four trailing bytes a high-profile `avcC` carries: reserved(6) +
/// `chroma_format_idc`(2), reserved(5) + `bit_depth_luma_minus8`(3),
/// reserved(5) + `bit_depth_chroma_minus8`(3), `numOfSequenceParameterSetExt`
/// (always 0 — nothing in this workspace emits an SPS extension NAL unit,
/// and no real encoder measured for it does either). `None` on a truncated
/// RBSP rather than a guess: [`build_h264_avcc`] still produces a legal
/// (if minimal, non-high-profile-shaped) record without it.
fn high_profile_extension(sps_nal: &[u8]) -> Option<[u8; 4]> {
    let mut buf = RbspBuf::default();
    let mut budget = Budget::new(Limits::permissive());
    // Skip the one-byte NAL header — `RbspBuf` de-escapes the rest exactly
    // as every other H.264 RBSP reader in this workspace does (ITU-T H.264
    // §7.3.1).
    buf.fill(sps_nal.get(1..)?, &mut budget).ok()?;
    let mut r = buf.reader();
    let _profile_idc = r.get(8);
    let _constraint_flags_and_reserved = r.get(8);
    let _level_idc = r.get(8);
    let _seq_parameter_set_id = r.ue();
    let chroma_format_idc = r.ue();
    if chroma_format_idc == 3 {
        let _separate_colour_plane_flag = r.get(1);
    }
    let bit_depth_luma_minus8 = r.ue();
    let bit_depth_chroma_minus8 = r.ue();
    r.check().ok()?;
    Some([
        0xFC | (chroma_format_idc as u8 & 0x03),
        0xF8 | (bit_depth_luma_minus8 as u8 & 0x07),
        0xF8 | (bit_depth_chroma_minus8 as u8 & 0x07),
        0,
    ])
}


/// The HEVC fields an `hvcC` header needs that only the SPS carries — the
/// `profile_tier_level()` block verbatim plus the four values that follow it
/// far enough into the RBSP to need real Exp-Golomb parsing.
struct HevcSpsInfo {
    /// `general_profile_space`/`tier`/`profile_idc` (1), the 32-bit
    /// compatibility flags (4), the 48-bit constraint flags (6) and
    /// `general_level_idc` (1) — the twelve RBSP bytes ISO/IEC 14496-15
    /// §8.3.3.1.2 copies into the record unchanged.
    ptl: [u8; 12],
    num_temporal_layers: u8,
    temporal_id_nested: u8,
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

impl HevcSpsInfo {
    /// `None` on a truncated or unparsable SPS, so [`build_hevc_hvcc`] can
    /// refuse rather than write a record with guessed profile/level bytes.
    fn parse(sps_nal: &[u8]) -> Option<Self> {
        let mut buf = RbspBuf::default();
        let mut budget = Budget::new(Limits::permissive());
        // The HEVC NAL header is two bytes (ITU-T H.265 §7.3.1.2); the
        // emulation-prevention bytes that follow are why the twelve PTL bytes
        // cannot simply be sliced out of `sps_nal` — a `general_constraint_
        // indicator_flags` field that is 43 reserved zero bits produces
        // `00 00 03` escapes in practice, not in theory.
        buf.fill(sps_nal.get(2..)?, &mut budget).ok()?;
        let rbsp = buf.as_slice();
        let first = *rbsp.first()?;
        let max_sub_layers_minus1 = (first >> 1) & 0x07;
        let ptl: [u8; 12] = rbsp.get(1..13)?.try_into().ok()?;

        let mut r = buf.reader();
        // `sps_video_parameter_set_id`/`sps_max_sub_layers_minus1`/
        // `sps_temporal_id_nesting_flag`, then the 96 general bits already
        // captured in `ptl`.
        r.skip(8 + 96);
        let mut sub_profile = [false; 7];
        let mut sub_level = [false; 7];
        for i in 0..usize::from(max_sub_layers_minus1) {
            let profile = r.get_bit() == 1;
            let level = r.get_bit() == 1;
            if let (Some(p), Some(l)) = (sub_profile.get_mut(i), sub_level.get_mut(i)) {
                *p = profile;
                *l = level;
            }
        }
        if max_sub_layers_minus1 > 0 {
            r.skip(u32::from(8 - max_sub_layers_minus1) * 2);
        }
        for i in 0..usize::from(max_sub_layers_minus1) {
            if sub_profile.get(i).copied().unwrap_or(false) {
                r.skip(88);
            }
            if sub_level.get(i).copied().unwrap_or(false) {
                r.skip(8);
            }
        }
        let _sps_seq_parameter_set_id = r.ue();
        let chroma_format_idc = r.ue();
        if chroma_format_idc == 3 {
            let _separate_colour_plane_flag = r.get_bit();
        }
        let _pic_width_in_luma_samples = r.ue();
        let _pic_height_in_luma_samples = r.ue();
        if r.get_bit() == 1 {
            for _ in 0..4 {
                let _conf_win_offset = r.ue();
            }
        }
        let bit_depth_luma_minus8 = r.ue();
        let bit_depth_chroma_minus8 = r.ue();
        r.check().ok()?;
        Some(Self {
            ptl,
            num_temporal_layers: max_sub_layers_minus1.saturating_add(1),
            temporal_id_nested: first & 1,
            chroma_format_idc: chroma_format_idc as u8,
            bit_depth_luma_minus8: bit_depth_luma_minus8 as u8,
            bit_depth_chroma_minus8: bit_depth_chroma_minus8 as u8,
        })
    }
}

/// Build an `hvcC` record (ISO/IEC 14496-15 §8.3.3.1.2) from VPS/SPS/PPS
/// units in Annex-B payload form — [`build_h264_avcc`]'s HEVC mirror, for
/// Matroska's `V_MPEGH/ISO/HEVC` `CodecPrivate` and MP4's `hev1` sample
/// entry.
///
/// `lengthSizeMinusOne` is 3 for the same reason it is in `avcC`: four bytes
/// is the only width [`crate::annexb_to_length_prefixed`]'s callers produce.
/// `min_spatial_segmentation_idc`, `parallelismType` and `avgFrameRate` are
/// written as 0 — the values the spec defines as "no information", and
/// exactly what `ffmpeg 9.0.1` writes for a real `libx265` stream (measured,
/// see the test).
///
/// `None` when there is no SPS, or the first SPS cannot be parsed: the
/// profile/tier/level bytes have no other source, and a record that states
/// them wrongly is worse than none at all.
#[must_use]
pub fn build_hevc_hvcc(vps: &[&[u8]], sps: &[&[u8]], pps: &[&[u8]]) -> Option<Vec<u8>> {
    let info = HevcSpsInfo::parse(sps.first()?)?;

    let mut out = vec![1u8]; // configurationVersion
    out.extend_from_slice(&info.ptl);
    out.extend_from_slice(&[0xF0, 0x00]); // reserved(4) + min_spatial_segmentation_idc = 0
    out.push(0xFC); // reserved(6) + parallelismType = 0
    out.push(0xFC | (info.chroma_format_idc & 0x03));
    out.push(0xF8 | (info.bit_depth_luma_minus8 & 0x07));
    out.push(0xF8 | (info.bit_depth_chroma_minus8 & 0x07));
    out.extend_from_slice(&0u16.to_be_bytes()); // avgFrameRate
    // constantFrameRate(2) = 0, numTemporalLayers(3), temporalIdNested(1),
    // lengthSizeMinusOne(2) = 3.
    out.push(((info.num_temporal_layers & 0x07) << 3) | ((info.temporal_id_nested & 1) << 2) | 0x03);

    let arrays: [(u8, &[&[u8]]); 3] = [(32, vps), (33, sps), (34, pps)];
    out.push(u8::try_from(arrays.iter().filter(|(_, u)| !u.is_empty()).count()).unwrap_or(3));
    for (nal_unit_type, units) in arrays {
        if units.is_empty() {
            continue;
        }
        // `array_completeness` = 0, `reserved` = 0, then `NAL_unit_type`.
        out.push(nal_unit_type);
        out.extend_from_slice(&u16::try_from(units.len()).unwrap_or(u16::MAX).to_be_bytes());
        for unit in units {
            out.extend_from_slice(&u16::try_from(unit.len()).unwrap_or(u16::MAX).to_be_bytes());
            out.extend_from_slice(unit);
        }
    }
    Some(out)
}

/// What a container that stores **length-prefixed** NAL units (MP4's `avc1`/
/// `hev1`, Matroska's `V_MPEG4/ISO/AVC` and `V_MPEGH/ISO/HEVC`) must do with
/// an `extradata` buffer that may have arrived in either form.
///
/// # Why this returns both halves at once
///
/// The configuration record and the sample framing are one decision, not
/// two, and splitting them is exactly how every H.264/HEVC file this
/// workspace encoded came out malformed: the container advertised
/// `is_avc=true, nal_length_size=4` beside an Annex-B `mdat`. A caller that
/// takes [`Self::record`] cannot forget [`Self::repack`], because there is
/// no way to ask for one without the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LengthPrefixed {
    /// The `avcC`/`hvcC` payload to store out of band.
    pub record: Vec<u8>,
    /// Whether the packets themselves are still Annex-B and need
    /// [`crate::annexb_to_length_prefixed`] with [`crate::LengthSize::FOUR`]
    /// before they are written.
    pub repack: bool,
}

/// Decide [`LengthPrefixed`] for `extradata`.
///
/// A real configuration record opens with `configurationVersion = 1`; an
/// Annex-B buffer opens with a start code, whose first byte is always zero.
/// That is the whole discriminator, and it is checked in exactly one place
/// so two containers cannot disagree about it.
///
/// `None` when nothing can be decided — an empty buffer, or an Annex-B
/// buffer carrying no SPS for [`build_h264_avcc`]/[`build_hevc_hvcc`] to
/// derive profile and level from. A caller that gets `None` has no record to
/// write and must say so rather than write an empty box.
#[must_use]
pub fn length_prefixed_config(kind: HeaderKind, extradata: &[u8]) -> Option<LengthPrefixed> {
    match extradata.first()? {
        0 => {
            let mut sets: [Vec<&[u8]>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for nal in crate::units(extradata, Framing::AnnexB) {
                let Some(header) = NalHeader::parse(kind, nal.data) else {
                    continue;
                };
                let slot = match (kind, header.nal_unit_type) {
                    (HeaderKind::H264, 7) | (HeaderKind::H265, 33) => 1,
                    (HeaderKind::H264, 8) | (HeaderKind::H265, 34) => 2,
                    (HeaderKind::H265, 32) => 0,
                    _ => continue,
                };
                if let Some(v) = sets.get_mut(slot) {
                    v.push(nal.data);
                }
            }
            let [vps, sps, pps] = &sets;
            let record = match kind {
                HeaderKind::H264 => build_h264_avcc(sps, pps),
                HeaderKind::H265 => build_hevc_hvcc(vps, sps, pps),
                HeaderKind::H266 => None,
            }?;
            Some(LengthPrefixed {
                record,
                repack: true,
            })
        }
        // Already a configuration record, which in every container that
        // carries one also means the samples are already length-prefixed.
        _ => Some(LengthPrefixed {
            record: extradata.to_vec(),
            repack: false,
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// A real `ffmpeg 9.0.1 -c:v libx264` High-profile SPS/PPS pair,
    /// extracted from a real MPEG-TS-to-Matroska `-c copy`'s own reference
    /// output (`CodecPrivate`, walked out of the EBML directly) — the exact
    /// bytes this function needs to reproduce to close the gap it exists
    /// for. Table format matches [`build_h264_avcc`]'s own doc: `avcC`
    /// bytes 0-4 are the fixed header, 5 is `numSPS`, then the SPS itself.
    const SPS: [u8; 25] = [
        0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41, 0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
        0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1, 0x42, 0x99, 0x60,
    ];
    const PPS: [u8; 6] = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

    /// The exact 46-byte `avcC` `ffmpeg 9.0.1` wrote for the SPS/PPS above,
    /// measured directly (see the module doc for why this crate builds its
    /// own rather than trusting a hand count).
    const EXPECTED: [u8; 46] = [
        0x01, 0x64, 0x00, 0x0d, 0xff, 0xe1, 0x00, 0x19, 0x67, 0x64, 0x00, 0x0d, 0xac, 0xd9, 0x41,
        0x41, 0xfb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0x20, 0xf1,
        0x42, 0x99, 0x60, 0x01, 0x00, 0x06, 0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0, 0xfd, 0xf8, 0xf8,
        0x00,
    ];

    #[test]
    fn a_real_high_profile_record_matches_ffmpeg_byte_for_byte() {
        let built = build_h264_avcc(&[&SPS], &[&PPS]).unwrap();
        assert_eq!(built, EXPECTED);
    }

    #[test]
    fn no_sps_at_all_builds_nothing() {
        assert!(build_h264_avcc(&[], &[&PPS]).is_none());
    }

    #[test]
    fn a_truncated_sps_builds_nothing() {
        assert!(build_h264_avcc(&[&[0x67, 0x64]], &[]).is_none());
    }

    #[test]
    fn no_pps_still_builds_a_record_with_zero_pps_entries() {
        let built = build_h264_avcc(&[&SPS], &[]).unwrap();
        // Byte 33 (right after the SPS) is `numOfPictureParameterSets`.
        assert_eq!(built.get(33), Some(&0));
    }

    /// A baseline-profile SPS (`profile_idc = 66`, not in the high-profile
    /// set) must not grow the four extension bytes — those are only legal
    /// (and only meaningful) for the profiles ITU-T H.264 §7.3.2.1.1 names.
    #[test]
    fn a_baseline_profile_record_has_no_high_profile_extension() {
        // A hand-built minimal baseline SPS: real bit content does not
        // matter here since `profile_idc = 66` alone must short-circuit the
        // extension before this function ever reads it.
        let baseline_sps: [u8; 8] = [0x67, 66, 0x00, 0x0a, 0, 0, 0, 0];
        let built = build_h264_avcc(&[&baseline_sps], &[]).unwrap();
        // Fixed header(5) + numSPS(1) + sps_len(2) + sps(8) + numPPS(1) = 17.
        assert_eq!(built.len(), 17);
    }
    /// A real `ffmpeg 9.0.1 -c:v libx265` VPS/SPS/PPS trio, lifted straight
    /// out of the `hvcC` of its own MP4 output (320x240, Main profile,
    /// level 6.0) — kept in Annex-B payload form, which is what this
    /// function is given.
    const HEVC_VPS: [u8; 24] = [
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x3c, 0x95, 0x98, 0x09,
    ];
    const HEVC_SPS: [u8; 42] = [
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3c, 0xa0, 0x0a, 0x08, 0x0f, 0x16, 0x59, 0x59, 0xa4, 0x93, 0x2b, 0xc0, 0x5a,
        0x02, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x32, 0x10,
    ];
    const HEVC_PPS: [u8; 7] = [0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40];

    /// The 23-byte fixed header `ffmpeg 9.0.1` wrote for the trio above,
    /// read back out of its own `hvcC` box. Everything after it is the three
    /// NAL arrays, checked separately below — the reference also appends a
    /// fourth array for the prefix SEI carrying x265's option string, which
    /// this function has no parameter sets to build from and deliberately
    /// does not write.
    const HEVC_HEADER: [u8; 23] = [
        0x01, 0x01, 0x60, 0x00, 0x00, 0x00, 0x90, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3c, 0xf0, 0x00,
        0xfc, 0xfd, 0xf8, 0xf8, 0x00, 0x00, 0x0f, 0x03,
    ];

    #[test]
    fn a_real_hvcc_header_matches_ffmpeg_byte_for_byte() {
        let built = build_hevc_hvcc(&[&HEVC_VPS], &[&HEVC_SPS], &[&HEVC_PPS]).unwrap();
        // Byte 22 is `numOfArrays`: the reference wrote 4 (it had a prefix
        // SEI too), this writes 3, so only the 22 bytes before it are a
        // byte-for-byte match.
        assert_eq!(built.get(..22), Some(&HEVC_HEADER[..22]));
        assert_eq!(built.get(22), Some(&3), "VPS, SPS and PPS arrays");
    }

    /// The arrays themselves: `array_completeness(1)=0 reserved(1)=0
    /// NAL_unit_type(6)`, `numNalus(16)`, then each unit length-prefixed.
    #[test]
    fn hvcc_arrays_carry_each_parameter_set_length_prefixed() {
        let built = build_hevc_hvcc(&[&HEVC_VPS], &[&HEVC_SPS], &[&HEVC_PPS]).unwrap();
        let mut expected = HEVC_HEADER[..22].to_vec();
        expected.push(3);
        for (t, unit) in [
            (32u8, HEVC_VPS.as_slice()),
            (33, HEVC_SPS.as_slice()),
            (34, HEVC_PPS.as_slice()),
        ] {
            expected.push(t);
            expected.extend_from_slice(&1u16.to_be_bytes());
            expected.extend_from_slice(&(unit.len() as u16).to_be_bytes());
            expected.extend_from_slice(unit);
        }
        assert_eq!(built, expected);
    }

    #[test]
    fn an_hvcc_needs_an_sps_to_state_profile_and_level() {
        assert!(build_hevc_hvcc(&[&HEVC_VPS], &[], &[&HEVC_PPS]).is_none());
        assert!(build_hevc_hvcc(&[], &[&[0x42, 0x01, 0x01]], &[]).is_none());
    }

    /// The whole point of [`length_prefixed_config`]: one call answers both
    /// "what record do I store" and "do the samples still need reframing",
    /// so a container cannot write one form's record beside the other form's
    /// samples — the exact shape of the H.264 encode bug this closed.
    #[test]
    fn annexb_extradata_yields_a_record_and_asks_for_a_repack() {
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(&SPS);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&PPS);
        let got = length_prefixed_config(HeaderKind::H264, &annexb).unwrap();
        assert!(got.repack, "Annex-B samples must be reframed");
        assert_eq!(got.record, EXPECTED, "and the record derived from them");
    }

    #[test]
    fn a_record_shaped_extradata_is_passed_through_and_asks_for_no_repack() {
        let got = length_prefixed_config(HeaderKind::H264, &EXPECTED).unwrap();
        assert!(!got.repack);
        assert_eq!(got.record, EXPECTED);
    }

    #[test]
    fn hevc_annexb_extradata_yields_an_hvcc() {
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(&HEVC_VPS);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&HEVC_SPS);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&HEVC_PPS);
        let got = length_prefixed_config(HeaderKind::H265, &annexb).unwrap();
        assert!(got.repack);
        assert_eq!(
            got.record,
            build_hevc_hvcc(&[&HEVC_VPS], &[&HEVC_SPS], &[&HEVC_PPS]).unwrap()
        );
    }

    /// Empty, and Annex-B with nothing a record can be built from, are both
    /// "no answer" — never an empty record, which is the box a real
    /// `ffprobe` refuses.
    #[test]
    fn nothing_to_build_from_answers_none() {
        assert!(length_prefixed_config(HeaderKind::H264, &[]).is_none());
        assert!(length_prefixed_config(HeaderKind::H264, &[0, 0, 0, 1, 0x68, 0xeb]).is_none());
    }
}
