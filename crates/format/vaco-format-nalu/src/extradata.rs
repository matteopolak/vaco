//! Assembling `extradata` from in-band parameter sets.
//!
//! `planning/CONFORMANCE-FINDINGS.md` finding 26: a container with no
//! out-of-band configuration record — AVI, MPEG-TS, raw Annex B — carries an
//! H.264/HEVC stream's SPS/PPS/VPS *inside* the access units, and the
//! reference nonetheless reports a non-empty `extradata_size`, because
//! `avformat_find_stream_info` runs its `extract_extradata` bitstream filter
//! over the probe window and stores whatever it collects.
//!
//! This module is the one place that assembly rule lives (D19). Two call
//! sites need it and neither may keep its own copy:
//!
//! * `vaco-bsf-generic`'s `extract_extradata` filter, the write side — a
//!   caller runs this explicitly, typically because a muxer wants an
//!   out-of-band record and the stream has none.
//! * `vaco-format-core`'s stream discovery, the read side — this is what
//!   closes finding 26 for `-show_streams`, without discovery depending on
//!   `vaco-bsf-generic` or on any codec crate (D14.1: a `vaco-format-*` crate
//!   may not name `vaco-parse-*`; this crate is the shared floor both sides
//!   already stand on instead).
//!
//! # What is measured, not assumed
//!
//! Checked against `ffmpeg 8.1` (D7 — the reference is probed, never read):
//! a `testsrc` clip encoded with `libx264`, muxed to AVI, produces a
//! 37-byte `extradata`:
//!
//! ```text
//! 00 00 01 67 64 00 0a ac d9 44 26 c0 44 00 00 03 00 04 00 00 03 00 c8 3c 48 96 58
//! 00 00 00 01 68 eb e3 cb 22 c0
//! ```
//!
//! **The first unit gets a three-byte start code; every unit after it gets
//! four.** A four-byte start code exists to stop a run of RBSP trailing zero
//! bits at the end of one unit from being misread as the *next* unit's start
//! code once units are glued together with nothing else between them — the
//! exact situation concatenating parameter sets creates — and a buffer that
//! opens at offset zero has no such ambiguity to guard against, so the first
//! unit does not pay for it. Reproduced identically for HEVC's VPS/SPS/PPS.
//!
//! # Rejected alternatives
//!
//! 1. **Route stream discovery through a `BsfProvider`**, symmetrical with
//!    `ParserProvider`, and have it run the real `extract_extradata` filter.
//!    Rejected for now: `Discovery` has no such seam today, and adding one
//!    is an interface change (see `planning/INTERFACE-GAPS.md`) for a filter
//!    whose entire observable behaviour, on the read side, is this one pure
//!    function of "ordered parameter-set NAL units in, Annex-B buffer out".
//!    Building a provider, a registry lookup and a `BitstreamFilter`
//!    instance to reach a function with no state of its own is machinery
//!    the problem does not need. If a second read-side consumer ever needs
//!    the *filter's* other behaviour — `remove=1`, or a codec this module
//!    does not cover — that is the point to revisit this.
//! 2. **Have `vaco-parse-h264`/`-hevc` expose the parameter sets they already
//!    store internally**, and assemble from those. Rejected because it is
//!    strictly more surface for the same answer: this module already gets
//!    the NAL units straight from the packet bytes discovery is holding
//!    anyway (the same bytes it is about to hand the parser), with no need
//!    to reach into a parser's private state or widen `Parser` to expose it.

use crate::{Framing, HeaderKind, NalHeader, units};
use vaco_codec_core::CodecId;

/// Which [`HeaderKind`] framing `codec`'s bitstream uses for in-band
/// parameter sets, if any.
///
/// The single definition D19 requires for a question three call sites used
/// to each answer with their own copy of the same two-armed match:
/// [`crate::extradata`]'s own assembly rule already had exactly one home for
/// "how are H.264/HEVC parameter sets laid out"; this is the same discipline
/// applied to "which codecs *have* parameter sets laid out this way at all".
/// A real bug shipped from the missing third copy: `vaco-format-core::mux`'s
/// `global_header_action` (M16) asked for `extract_extradata` — the write
/// side named just below — for *every* codec with empty extradata in a
/// `GLOBALHEADER` container, not only the two this module and
/// `vaco-bsf-generic::extract_extradata` actually implement. `vaco -i
/// in.mkv -c:v ffv1 out.mp4` (and every non-H.264/HEVC video codec into
/// MP4) failed outright: `check_bitstream` asked for a filter that then
/// refused to build itself for that codec, well before `write_packet` ever
/// ran. `None` here is what lets a caller tell "this codec cannot supply
/// extradata this way" apart from "this codec's stream happens to have
/// none yet" — the distinction `global_header_action` was missing.
///
/// VVC is deliberately absent from the two codecs this maps, for the same
/// reason [`is_parameter_set`] always answers `false` for
/// [`HeaderKind::H266`]: this workspace has no NAL-level VVC parser, so
/// nothing downstream of a `Some` answer here could actually assemble
/// anything from it.
#[must_use]
pub const fn header_kind_for(codec: CodecId) -> Option<HeaderKind> {
    match codec {
        CodecId::H264 => Some(HeaderKind::H264),
        CodecId::Hevc => Some(HeaderKind::H265),
        _ => None,
    }
}

/// Whether `nal_unit_type` is a parameter set worth collecting into
/// extradata, for the codec `kind` names.
///
/// VVC is not covered — this workspace has no NAL-level VVC parser to hand
/// the bytes to, so guessing at a start-code convention nobody has measured
/// (D17) would be worse than refusing. `H266` therefore always answers
/// `false`, matching [`HeaderKind::H266`]'s treatment everywhere else this
/// module is used.
#[must_use]
pub const fn is_parameter_set(kind: HeaderKind, nal_unit_type: u8) -> bool {
    match kind {
        HeaderKind::H264 => matches!(nal_unit_type, 7 | 8), // SPS, PPS
        HeaderKind::H265 => matches!(nal_unit_type, 32..=34), // VPS, SPS, PPS
        HeaderKind::H266 => false,
    }
}

/// Collect the parameter-set NAL units present in `payload`, in the order
/// they appear, under `framing`.
///
/// A unit whose header does not parse (too short) is silently skipped rather
/// than treated as an error: `payload` is one access unit out of an
/// untrusted stream, and a malformed unit here is a reason to not collect
/// it, not a reason to stop discovery.
#[must_use]
pub fn parameter_sets(payload: &[u8], framing: Framing, kind: HeaderKind) -> Vec<&[u8]> {
    units(payload, framing)
        .filter_map(|nal| {
            let header = NalHeader::parse(kind, nal.data)?;
            is_parameter_set(kind, header.nal_unit_type).then_some(nal.data)
        })
        .collect()
}

/// Assemble Annex-B-framed extradata from an ordered list of parameter-set
/// NAL units, matching what the reference's `extract_extradata` bitstream
/// filter writes — see the module docs for the start-code convention this
/// encodes and why it is not the "obvious" spelling.
///
/// An empty input yields an empty buffer; a caller deciding whether there is
/// anything to store should check [`parameter_sets`]'s result before calling
/// this, not the other way round.
#[must_use]
pub fn assemble_extradata<'a>(sets: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, unit) in sets.into_iter().enumerate() {
        if i == 0 {
            out.extend_from_slice(&[0, 0, 1]);
        } else {
            out.extend_from_slice(&[0, 0, 0, 1]);
        }
        out.extend_from_slice(unit);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    const SPS: [u8; 24] = [
        0x67, 0x64, 0x00, 0x0a, 0xac, 0xd9, 0x44, 0x26, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04,
        0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x48, 0x96, 0x58,
    ];
    const PPS: [u8; 6] = [0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

    /// The real bug this function's own doc names: every codec that is not
    /// H.264/HEVC must answer `None`, not just "whatever `_ =>` happened to
    /// be convenient" -- `vaco-format-core::mux::global_header_action`
    /// trusted this exact distinction to stop asking for `extract_extradata`
    /// on a codec that filter cannot help (FFV1, VP8, VP9, `ProRes`, MPEG-2,
    /// every image codec), and the bug this closes was a `None` case
    /// nothing checked.
    #[test]
    fn only_h264_and_hevc_have_a_header_kind() {
        assert_eq!(header_kind_for(CodecId::H264), Some(HeaderKind::H264));
        assert_eq!(header_kind_for(CodecId::Hevc), Some(HeaderKind::H265));
        for codec in [
            CodecId::Ffv1,
            CodecId::Vp8,
            CodecId::Vp9,
            CodecId::Av1,
            CodecId::Prores,
            CodecId::Mpeg2video,
            CodecId::Png,
            CodecId::Mp3,
        ] {
            assert_eq!(
                header_kind_for(codec),
                None,
                "{codec:?} must not claim a HeaderKind"
            );
        }
    }

    /// The reference-measured 37-byte example from the module docs.
    #[test]
    fn h264_sps_pps_reproduce_the_measured_bytes() {
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend_from_slice(&SPS);
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(&PPS);
        annexb.extend_from_slice(&[0, 0, 1, 0x65, 0xAA]); // a slice, ignored

        let sets = parameter_sets(&annexb, Framing::AnnexB, HeaderKind::H264);
        assert_eq!(sets, vec![&SPS[..], &PPS[..]]);

        let mut expected = vec![0, 0, 1];
        expected.extend_from_slice(&SPS);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&PPS);
        assert_eq!(assemble_extradata(sets), expected);
    }

    /// Falsifies the start-code convention: the "obvious" all-four-byte
    /// spelling is a different, wrong, answer — not a stylistic variant of
    /// the right one. If this assertion ever fails, the real test above is
    /// passing by coincidence.
    #[test]
    fn the_naive_all_four_byte_spelling_is_a_different_answer() {
        let naive: Vec<u8> = [0, 0, 0, 1].iter().chain(SPS.iter()).copied().collect();
        let measured = assemble_extradata([&SPS[..]]);
        assert_ne!(naive, measured);
        assert_eq!(measured, [&[0, 0, 1][..], &SPS[..]].concat());
    }

    #[test]
    fn hevc_vps_sps_pps_all_collected() {
        let vps = [0x40, 0x01, 0x0c, 0x01];
        let sps = [0x42, 0x01, 0x01, 0x02];
        let pps = [0x44, 0x01, 0xc0];
        let mut annexb = Vec::new();
        for unit in [&vps[..], &sps[..], &pps[..]] {
            annexb.extend_from_slice(&[0, 0, 0, 1]);
            annexb.extend_from_slice(unit);
        }
        // A trailing slice NAL (type 1, not a parameter set) must not appear.
        annexb.extend_from_slice(&[0, 0, 0, 1, 0x02, 0x01]);

        let sets = parameter_sets(&annexb, Framing::AnnexB, HeaderKind::H265);
        assert_eq!(sets, vec![&vps[..], &sps[..], &pps[..]]);

        let extra = assemble_extradata(sets);
        let mut expected = vec![0, 0, 1];
        expected.extend_from_slice(&vps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&sps);
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(&pps);
        assert_eq!(extra, expected);
    }

    #[test]
    fn a_slice_only_payload_yields_nothing() {
        let annexb = [0, 0, 0, 1, 0x65, 0xAA, 0xBB];
        assert!(parameter_sets(&annexb, Framing::AnnexB, HeaderKind::H264).is_empty());
        assert!(assemble_extradata(Vec::<&[u8]>::new()).is_empty());
    }

    #[test]
    fn length_prefixed_framing_is_honoured() {
        let sps = [0x67, 0x42, 0xC0, 0x1E];
        let mut lp = Vec::new();
        lp.extend_from_slice(&(sps.len() as u32).to_be_bytes());
        lp.extend_from_slice(&sps);

        let framing = Framing::length_prefixed(4).unwrap();
        let sets = parameter_sets(&lp, framing, HeaderKind::H264);
        assert_eq!(sets, vec![&sps[..]]);
        assert_eq!(
            assemble_extradata(sets),
            [&[0, 0, 1][..], &sps[..]].concat()
        );
    }

    /// VVC has no measured convention yet, so it must not silently guess one.
    #[test]
    fn h266_is_never_treated_as_carrying_parameter_sets() {
        assert!(!is_parameter_set(HeaderKind::H266, 33)); // VVC's own VPS number
        let annexb = [0, 0, 0, 1, 0x00, 0b0100_0001]; // layer 0, type 32
        assert!(parameter_sets(&annexb, Framing::AnnexB, HeaderKind::H266).is_empty());
    }
}
