//! Content probing: is this an ISOBMFF file, and how sure are we.
//!
//! # Measured, not guessed
//!
//! `planning/18-formats.md` §3.1.3 predicts a tiered score — 100 for a
//! recognised major brand, 90 for an unknown one, 75 for a leading
//! `moov`/`mdat`. **Three of those four rows are wrong.** Measured against
//! `ffprobe 8.1` by mutating one calibration file four ways and reading
//! `format=probe_score`:
//!
//! | File | Prediction | `probe_score` |
//! |---|---:|---:|
//! | `ftyp` with brand `isom` | 100 | **100** |
//! | `ftyp` with brand `zzzz` (all brands overwritten) | 90 | **100** |
//! | `ftyp` removed, file starts with `moov` | 75 | **100** |
//! | `ftyp` removed, `mdat` first and `moov` last | 75 | **100** |
//!
//! ```sh
//! ffprobe -v quiet -show_entries format=probe_score -of default=nw=1:nk=1 brandless.mp4
//! ```
//!
//! So the reference's ISOBMFF probe does not grade brands at all; a
//! recognisable top-level box is enough. A degenerate file — `ftyp` alone, a
//! lone `free`, a `wide`+`mdat` pair — is still *detected* (the error carries
//! the `mov,mp4,m4a,3gp,3g2,mj2` context) but has no streams, so `ffprobe`
//! prints no `FORMAT` section and the score cannot be read from it. Those rows
//! are marked as choices below rather than presented as reproductions.

use vaco_format_core::probe::{ProbeData, ProbeScore};

use crate::fourcc::{FourCc, boxes};

/// Brands this crate recognises, from ISO/IEC 14496-12 Annex E, 14496-14,
/// 3GPP TS 26.244, the AVIF and HEIF specifications, and the MP4 Registration
/// Authority's brand list.
///
/// Recognition changes **no score** — the measurements above show the reference
/// does not grade brands — so the list exists for callers that want to report
/// or filter on it, not for probing.
pub const KNOWN_BRANDS: &[&[u8; 4]] = &[
    b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"iso7", b"iso8", b"iso9", b"mp41",
    b"mp42", b"mp71", b"avc1", b"qt  ", b"3gp4", b"3gp5", b"3gp6", b"3gp7", b"3g2a", b"3g2b",
    b"3g2c", b"M4V ", b"M4A ", b"M4P ", b"M4B ", b"M4VP", b"mif1", b"msf1", b"heic", b"heix",
    b"hevc", b"hevx", b"avif", b"avis", b"crx ", b"isml", b"ccff", b"dash", b"cmfc", b"caqv",
    b"da0a", b"jp2 ", b"mjp2", b"f4v ", b"MSNV", b"NDAS", b"piff", b"iso1",
];

/// Top-level box types whose presence at offset zero identifies the format.
const OPENING_TYPES: &[FourCc] = &[
    boxes::FTYP,
    boxes::STYP,
    boxes::MOOV,
    boxes::MOOF,
    boxes::MDAT,
    boxes::PNOT,
    boxes::WIDE,
    boxes::FREE,
    boxes::SKIP,
    boxes::JUNK,
    boxes::SIDX,
];

/// Types that carry real structure, as opposed to padding.
const STRUCTURAL_TYPES: &[FourCc] = &[
    boxes::FTYP,
    boxes::STYP,
    boxes::MOOV,
    boxes::MOOF,
    boxes::MDAT,
    boxes::SIDX,
];

/// Whether `brand` is in [`KNOWN_BRANDS`].
#[must_use]
pub fn is_known_brand(brand: FourCc) -> bool {
    KNOWN_BRANDS.iter().any(|b| **b == brand.0)
}

/// Score `data` as an ISOBMFF file.
///
/// Returns [`ProbeScore::MAGIC_CHECKED`] — the reference's measured 100 — for
/// any of the four measured cases, and [`ProbeScore::CONTENT`] for a file that
/// opens with padding only, which is a **choice** (the reference detects those
/// too but prints no score for them).
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let Some(kind) = data.tag(4).map(FourCc) else {
        return ProbeScore::NONE;
    };
    if !OPENING_TYPES.contains(&kind) {
        return ProbeScore::NONE;
    }
    // The size word must be plausible: zero (to end of file), one (largesize)
    // or at least a header. `free\0\0\0\x02` is not a box, and treating it as
    // one is how a probe claims a file it cannot open.
    let Some(size) = data.rb32(0) else {
        return ProbeScore::NONE;
    };
    if size != 0 && size != 1 && size < 8 {
        return ProbeScore::NONE;
    }
    if STRUCTURAL_TYPES.contains(&kind) {
        // Measured: 100 whether or not the brand is recognised, and 100 with
        // no `ftyp` at all.
        return ProbeScore::MAGIC_CHECKED;
    }
    // Padding-only opening. Detected by the reference; score unobservable.
    // Chosen at CONTENT so a file that is genuinely something else, with a
    // stronger claim, still wins.
    ProbeScore::CONTENT
}

/// The `ftyp` major brand, when the buffer reaches it.
#[must_use]
pub fn major_brand(data: &ProbeData<'_>) -> Option<FourCc> {
    if data.tag(4).map(FourCc) != Some(boxes::FTYP) {
        return None;
    }
    data.tag(8).map(FourCc)
}

/// The extensions the reference's `mov,mp4,m4a,3gp,3g2,mj2` demuxer claims.
///
/// Interface facts (D9), reproduced verbatim so `-f mp4` and extension-based
/// selection behave the same way.
pub const EXTENSIONS: &[&str] = &[
    "mov", "mp4", "m4a", "3gp", "3g2", "mj2", "psp", "m4b", "ism", "ismv", "isma", "f4v", "avif",
    "heic", "heif",
];

/// MIME types for the same family.
pub const MIME_TYPES: &[&str] = &[
    "video/mp4",
    "audio/mp4",
    "application/mp4",
    "video/quicktime",
    "video/3gpp",
    "video/3gpp2",
    "image/avif",
    "image/heic",
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::testutil::bx;

    fn score(data: &[u8]) -> ProbeScore {
        probe(&ProbeData::new(data))
    }

    #[test]
    fn the_four_measured_cases_all_score_one_hundred() {
        // 1. ftyp with a known brand.
        assert_eq!(
            score(&bx(b"ftyp", b"isom\0\0\x02\0isom")),
            ProbeScore::MAGIC_CHECKED
        );
        // 2. ftyp with an unknown brand — the reference does not grade brands.
        assert_eq!(
            score(&bx(b"ftyp", b"zzzz\0\0\x02\0zzzz")),
            ProbeScore::MAGIC_CHECKED
        );
        // 3. moov first, no ftyp.
        assert_eq!(score(&bx(b"moov", &[0; 32])), ProbeScore::MAGIC_CHECKED);
        // 4. mdat first, no ftyp.
        assert_eq!(score(&bx(b"mdat", &[0; 32])), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn fragmented_segment_openings_also_score_one_hundred() {
        assert_eq!(score(&bx(b"styp", b"msdh")), ProbeScore::MAGIC_CHECKED);
        assert_eq!(score(&bx(b"moof", &[0; 16])), ProbeScore::MAGIC_CHECKED);
        assert_eq!(score(&bx(b"sidx", &[0; 24])), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn padding_only_openings_score_lower_by_choice() {
        assert_eq!(score(&bx(b"free", &[0; 8])), ProbeScore::CONTENT);
        assert_eq!(score(&bx(b"wide", &[])), ProbeScore::CONTENT);
        assert_eq!(score(&bx(b"skip", &[0; 4])), ProbeScore::CONTENT);
    }

    #[test]
    fn anything_else_scores_nothing() {
        assert_eq!(score(b"RIFF\0\0\0\0WAVEfmt "), ProbeScore::NONE);
        assert_eq!(score(b"\x1aE\xdf\xa3"), ProbeScore::NONE);
        assert_eq!(score(&[]), ProbeScore::NONE);
        assert_eq!(score(&[0; 4]), ProbeScore::NONE);
    }

    #[test]
    fn an_impossible_size_word_disqualifies_a_recognised_type() {
        let mut data = 2u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"moov");
        assert_eq!(score(&data), ProbeScore::NONE);
        // Zero and one are both legal.
        let mut zero = 0u32.to_be_bytes().to_vec();
        zero.extend_from_slice(b"moov");
        assert_eq!(score(&zero), ProbeScore::MAGIC_CHECKED);
        let mut one = 1u32.to_be_bytes().to_vec();
        one.extend_from_slice(b"mdat");
        assert_eq!(score(&one), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn a_short_buffer_is_scored_from_what_it_has() {
        // ProbeData zero-pads, so a four-byte buffer reads a zero type.
        assert_eq!(score(&[0, 0, 0, 32]), ProbeScore::NONE);
    }

    #[test]
    fn the_major_brand_is_readable_when_present() {
        let raw = bx(b"ftyp", b"isom\0\0\x02\0");
        assert_eq!(
            major_brand(&ProbeData::new(&raw)),
            Some(FourCc::new(b"isom"))
        );
        let moov = bx(b"moov", &[0; 8]);
        assert_eq!(major_brand(&ProbeData::new(&moov)), None);
    }

    #[test]
    fn the_brand_table_holds_what_it_claims() {
        assert!(is_known_brand(FourCc::new(b"isom")));
        assert!(is_known_brand(FourCc::new(b"qt  ")));
        assert!(is_known_brand(FourCc::new(b"avif")));
        assert!(!is_known_brand(FourCc::new(b"zzzz")));
    }
}
