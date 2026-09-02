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
}
