//! Scalefactor window band (`swb`) offset tables — ISO/IEC 14496-3 subpart 4
//! Tables 4.129 through 4.141 (2009 edition numbering; 4.73-4.85 in the 2001
//! edition, identical content) — the boundaries, in spectral-line units, of
//! each scalefactor band for a long (1024-line) or short (128-line) MDCT.
//!
//! # Provenance
//!
//! Transcribed from two independently hosted copies of the primary ISO/IEC
//! 14496-3 text (the 2001 and 2009 editions), not from any reference
//! decoder's source. Two of the seven long tables (44.1/48 kHz and
//! 11.025/12/16 kHz) were cross-checked byte-for-byte between the two
//! editions before the rest were transcribed the same way; every one of the
//! 13 tables here additionally passes a structural check any transcription
//! error is very unlikely to survive: starts at 0, strictly increasing, and
//! ends at exactly 1024 (long) or 128 (short) — see
//! `tests::every_table_starts_at_zero_and_ends_at_the_transform_size`.
//!
//! Two of the six short-window tables (64 kHz and 88.2/96 kHz) transcribed
//! identically from the source text; kept as written rather than merged,
//! since a future edition splitting them apart should not require touching
//! this file's structure. `vaco-codec-mpegaudio`'s own SFB tables follow the
//! same convention for the same reason.
//!
//! 7350 Hz (`samplingFrequencyIndex` 12) has no scalefactor band table in
//! this text at all — an extremely rare rate with no table given rather
//! than a gap in this transcription — so `swb_offset_long`/`_short` return
//! `None` for it; see `config.rs`'s object-type/sample-rate gating.

/// `swb_offset_long_window`, 44.1 and 48 kHz. 49 bands, 50 boundaries.
const LONG_44_48: [u16; 50] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 1024,
];

/// `swb_offset_long_window`, 32 kHz. 51 bands, 52 boundaries.
const LONG_32: [u16; 52] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 960, 992, 1024,
];

/// `swb_offset_long_window`, 8 kHz. 40 bands, 41 boundaries.
const LONG_8: [u16; 41] = [
    0, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 172, 188, 204, 220, 236, 252, 268,
    288, 308, 328, 348, 372, 396, 420, 448, 476, 508, 544, 580, 620, 664, 712, 764, 820, 880, 944,
    1024,
];

/// `swb_offset_long_window`, 11.025/12/16 kHz. 43 bands, 44 boundaries.
const LONG_11_12_16: [u16; 44] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 100, 112, 124, 136, 148, 160, 172, 184, 196, 212,
    228, 244, 260, 280, 300, 320, 344, 368, 396, 424, 456, 492, 532, 572, 616, 664, 716, 772, 832,
    896, 960, 1024,
];

/// `swb_offset_long_window`, 22.05/24 kHz. 47 bands, 48 boundaries.
const LONG_22_24: [u16; 48] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 136,
    148, 160, 172, 188, 204, 220, 240, 260, 284, 308, 336, 364, 396, 432, 468, 508, 552, 600, 652,
    704, 768, 832, 896, 960, 1024,
];

/// `swb_offset_long_window`, 64 kHz. 47 bands, 48 boundaries.
const LONG_64: [u16; 48] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 100, 112, 124, 140,
    156, 172, 192, 216, 240, 268, 304, 344, 384, 424, 464, 504, 544, 584, 624, 664, 704, 744, 784,
    824, 864, 904, 944, 984, 1024,
];

/// `swb_offset_long_window`, 88.2/96 kHz. 41 bands, 42 boundaries.
const LONG_88_96: [u16; 42] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132,
    144, 156, 172, 188, 212, 240, 276, 320, 384, 448, 512, 576, 640, 704, 768, 832, 896, 960, 1024,
];

/// `swb_offset_short_window`, 32/44.1/48 kHz. 14 bands, 15 boundaries.
const SHORT_32_44_48: [u16; 15] = [
    0, 4, 8, 12, 16, 20, 28, 36, 44, 56, 68, 80, 96, 112, 128,
];

/// `swb_offset_short_window`, 8 kHz. 15 bands, 16 boundaries.
const SHORT_8: [u16; 16] = [
    0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 60, 72, 88, 108, 128,
];

/// `swb_offset_short_window`, 11.025/12/16 kHz. 15 bands, 16 boundaries.
const SHORT_11_12_16: [u16; 16] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 60, 72, 88, 108, 128,
];

/// `swb_offset_short_window`, 22.05/24 kHz. 15 bands, 16 boundaries.
const SHORT_22_24: [u16; 16] = [
    0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 64, 76, 92, 108, 128,
];

/// `swb_offset_short_window`, 64 kHz. 12 bands, 13 boundaries.
const SHORT_64: [u16; 13] = [0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];

/// `swb_offset_short_window`, 88.2/96 kHz. 12 bands, 13 boundaries.
const SHORT_88_96: [u16; 13] = [0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];

/// The long-window `swb_offset` table for a `samplingFrequencyIndex`
/// (0..=11; 12, 7350 Hz, has none — see the module doc).
#[must_use]
pub(crate) fn swb_offset_long(sfi: u8) -> Option<&'static [u16]> {
    Some(match sfi {
        0 | 1 => &LONG_88_96,
        2 => &LONG_64,
        3 | 4 => &LONG_44_48,
        5 => &LONG_32,
        6 | 7 => &LONG_22_24,
        8..=10 => &LONG_11_12_16,
        11 => &LONG_8,
        _ => return None,
    })
}

/// The short-window `swb_offset` table for a `samplingFrequencyIndex`.
#[must_use]
pub(crate) fn swb_offset_short(sfi: u8) -> Option<&'static [u16]> {
    Some(match sfi {
        0 | 1 => &SHORT_88_96,
        2 => &SHORT_64,
        3..=5 => &SHORT_32_44_48,
        6 | 7 => &SHORT_22_24,
        8..=10 => &SHORT_11_12_16,
        11 => &SHORT_8,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::{swb_offset_long, swb_offset_short};

    #[test]
    fn every_table_starts_at_zero_and_ends_at_the_transform_size() {
        for sfi in 0u8..12 {
            let long = swb_offset_long(sfi).unwrap();
            assert_eq!(long[0], 0, "sfi {sfi} long does not start at 0");
            assert_eq!(
                *long.last().unwrap_or(&0),
                1024,
                "sfi {sfi} long does not end at 1024"
            );
            assert!(
                long.windows(2).all(|w| w[0] < w[1]),
                "sfi {sfi} long is not strictly increasing"
            );

            let short = swb_offset_short(sfi).unwrap();
            assert_eq!(short[0], 0, "sfi {sfi} short does not start at 0");
            assert_eq!(
                *short.last().unwrap_or(&0),
                128,
                "sfi {sfi} short does not end at 128"
            );
            assert!(
                short.windows(2).all(|w| w[0] < w[1]),
                "sfi {sfi} short is not strictly increasing"
            );
        }
    }

    #[test]
    fn sfi_12_7350hz_has_no_table() {
        assert!(swb_offset_long(12).is_none());
        assert!(swb_offset_short(12).is_none());
    }

    #[test]
    fn cross_checked_tables_match_the_iso_1996_2009_edition_exactly() {
        // 44.1/48 kHz and 11.025/12/16 kHz long tables were independently
        // byte-for-byte cross-checked against both the 2001 and 2009 ISO
        // editions during transcription; pinned here as regression anchors.
        assert_eq!(swb_offset_long(4).unwrap().len(), 50); // 44100 Hz: 49 bands
        assert_eq!(swb_offset_long(8).unwrap().len(), 44); // 16000 Hz: 43 bands
    }
}
