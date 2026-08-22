//! Profiles and levels, ITU-T H.264 Annex A.
//!
//! Two tables and the rules that select a row. Both are format-dictated: Annex A
//! states the profile numbers, the constraint-flag meanings and the level limits
//! outright, so any conforming implementation contains the same numbers
//! (D7/D15, merger).
//!
//! # Where the names came from
//!
//! The *names* are not spec text — they are what the reference tool prints, and
//! `-show_streams` prints them, so D6 makes them part of the output contract.
//! They were recovered by black-box probe: an SPS from `libx264` had its
//! `profile_idc` and constraint byte patched to each value in turn and was fed
//! back through `ffprobe`. The command, so the table can be re-derived when the
//! pinned reference moves:
//!
//! ```text
//! # byte 5 of a raw Annex B stream is profile_idc, byte 6 the constraint flags
//! printf '...' | ffprobe -v error -f h264 -show_entries stream=profile -of csv=p=0 -
//! ```
//!
//! The full result is in `docs/codec/vaco-parse-h264.md`.

use vaco_codec_core::{Level, LevelConstraints, LevelEntry, LevelTable, Profile};

/// The `constraint_setN_flag` bits and the two reserved bits, as one byte.
///
/// Stored as the raw byte because that is what `avcC`'s
/// `profile_compatibility` field and the `avc1.PPCCLL` MIME parameter both
/// carry verbatim, and reconstructing it from six booleans is a chance to get
/// the bit order wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct ConstraintFlags(u8);

impl ConstraintFlags {
    /// Wrap the raw byte from the bitstream. Bit 7 is `constraint_set0_flag`.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw byte.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// `constraint_setN_flag` for `n` in 0..=5. Any other `n` is false.
    #[must_use]
    pub const fn get(self, n: u8) -> bool {
        if n > 5 {
            return false;
        }
        self.0 & (0x80 >> n) != 0
    }

    /// The two `reserved_zero_2bits`. Non-zero means the stream is not
    /// conforming, though nothing here rejects it.
    #[must_use]
    pub const fn reserved(self) -> u8 {
        self.0 & 0x03
    }
}

/// Set in [`Profile::value`] when the constraint flags select a *constrained*
/// variant.
///
/// The specification gives the variant a name but not a number, so this
/// encoding is ours. Bit 9 rather than an arbitrary bit because it is the one
/// the reference tool uses internally, which costs nothing and keeps the two
/// numbering schemes from diverging if the value ever becomes observable.
pub const PROFILE_CONSTRAINED: i32 = 1 << 9;

/// Set in [`Profile::value`] for an *Intra* variant. See
/// [`PROFILE_CONSTRAINED`].
pub const PROFILE_INTRA: i32 = 1 << 11;

/// The display name for a `profile_idc` and its constraint flags, or `None`
/// when neither the specification nor the reference names it.
///
/// # The two selection rules, as measured
///
/// The constraint flags do not modify every profile — only the ones the
/// reference has a name for:
///
/// * `constraint_set1_flag` produces a *Constrained* variant for
///   `profile_idc == 66` only. Probed: `77` with `cs1` still prints `Main`,
///   not a number, so the flag is not being composed into the value at all.
/// * `constraint_set3_flag` produces an *Intra* variant for 110, 122 and 244
///   only. Probed: `100` with `cs3` prints `High`, and `44`, `118` and `128`
///   with `cs3` print their plain names.
///
/// `// D17:` two of these disagree with Annex A and are reproduced anyway:
///
/// * `profile_idc == 44` prints **`CAVLC 4:4:4`**; Annex A.2.11 names it the
///   *CAVLC 4:4:4 Intra profile*.
/// * `profile_idc == 100` with `constraint_set4_flag` is the *Progressive High
///   profile* (A.2.4.1), and with `constraint_set4` and `constraint_set5` the
///   *Constrained High profile* (A.2.4.2). The reference prints `High` for
///   both; probed with the constraint byte set to `0x08` and `0x0c`.
#[must_use]
pub fn profile_name(profile_idc: u8, flags: ConstraintFlags) -> Option<&'static str> {
    let constrained = flags.get(1);
    let intra = flags.get(3);
    Some(match profile_idc {
        66 if constrained => "Constrained Baseline",
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 if intra => "High 10 Intra",
        110 => "High 10",
        122 if intra => "High 4:2:2 Intra",
        122 => "High 4:2:2",
        244 if intra => "High 4:4:4 Intra",
        244 => "High 4:4:4 Predictive",
        // D17: the standard calls this the CAVLC 4:4:4 *Intra* profile.
        44 => "CAVLC 4:4:4",
        118 => "Multiview High",
        128 => "Stereo High",
        // A.2.9, withdrawn in the 2009 revision but still found in the wild.
        144 => "High 4:4:4",
        _ => return None,
    })
}

/// The [`Profile`] a `profile_idc` and constraint byte describe.
///
/// The name is `""` for a `profile_idc` nothing names — which is what the
/// reference falls back to printing the number for, so a caller that reports
/// profiles checks for the empty name and prints [`Profile::value`] masked to
/// its low bits instead.
#[must_use]
pub fn profile(profile_idc: u8, flags: ConstraintFlags) -> Profile {
    let mut value = i32::from(profile_idc);
    match profile_idc {
        66 if flags.get(1) => value |= PROFILE_CONSTRAINED,
        110 | 122 | 244 if flags.get(3) => value |= PROFILE_INTRA,
        _ => {}
    }
    Profile::new(value, profile_name(profile_idc, flags).unwrap_or(""))
}

/// The `profile_idc` inside a [`Profile::value`] produced by [`profile`].
#[must_use]
pub const fn profile_idc_of(p: Profile) -> u8 {
    (p.value & 0xFF) as u8
}

// --------------------------------------------------------------------- levels

/// One row of Table A-1, in the specification's own units.
struct Row {
    /// `level_idc`, or the pseudo-value 9 used for level 1b.
    idc: i32,
    /// `MaxDpbMbs`, macroblocks of decoded picture buffer.
    max_dpb_mbs: u64,
    /// `MaxBR`, in units of 1000 bits/s for profiles with `cpbBrVclFactor`
    /// 1000. See [`max_bit_rate_kbps`] for the profile-dependent scaling.
    max_br: u32,
}

/// Largest picture dimension a level permits, §A.3.1:
/// `PicWidthInMbs <= sqrt(MaxFS * 8)` and the same for the height.
///
/// An integer square root by binary search over 20 bits, so it is `const` and
/// exact — `f64::sqrt` would be neither.
const fn max_dimension_mbs(max_fs: u64) -> u64 {
    let target = max_fs * 8;
    let mut lo = 0u64;
    let mut hi = 1u64 << 20;
    while lo < hi {
        let mid = (lo + hi + 1) >> 1;
        if mid * mid <= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

const fn entry_of(
    idc: i32,
    name: &'static str,
    max_mbps: u64,
    max_fs: u64,
    max_br: u32,
) -> LevelEntry {
    let dim = max_dimension_mbs(max_fs) * 16;
    LevelEntry {
        level: Level(idc),
        name,
        constraints: LevelConstraints {
            max_luma_picture_size: max_fs * 256,
            max_luma_sample_rate: max_mbps * 256,
            max_bitrate_kbps: max_br,
            // §A.3.1's `MaxDpbFrames` depends on the picture size, so no single
            // number belongs here; 16 is the absolute ceiling the same clause
            // imposes. `max_dpb_frames` computes the real one.
            max_dpb_frames: 16,
            max_h_size: dim as u32,
            max_v_size: dim as u32,
            // H.264 has no tiles.
            max_tiles: 0,
            max_tile_cols: 0,
        },
    }
}

/// ITU-T H.264 Table A-1, written once and expanded into both the ordered
/// [`LevelTable`] the framework consumes and the side table the two
/// picture-size-dependent helpers need.
///
/// A macro rather than two literal arrays because the numbers are the
/// standard's and a transcription that exists twice will eventually disagree
/// with itself. Ordered by increasing capability, which is what
/// [`LevelTable::smallest_for`] requires — so level 1b sits between 1 and 1.1
/// even though its `level_idc` of 9 sorts before 10.
macro_rules! level_table {
    ($($idc:literal, $name:literal, $mbps:literal, $fs:literal, $dpb:literal, $br:literal;)*) => {
        const ROWS: &[Row] = &[$(Row { idc: $idc, max_dpb_mbs: $dpb, max_br: $br }),*];
        /// The level table, ordered by increasing capability.
        pub const LEVELS: LevelTable = LevelTable(&[$(entry_of($idc, $name, $mbps, $fs, $br)),*]);
    };
}

level_table! {
    10, "1",   1_485,      99,      396,     64;
    // 1b is `level_idc == 11` with `constraint_set3_flag`, or the value 9 in
    // an `avcC`. Both spellings mean this row; see `is_level_1b`.
    9,  "1b",  1_485,      99,      396,     128;
    11, "1.1", 3_000,      396,     900,     192;
    12, "1.2", 6_000,      396,     2_376,   384;
    13, "1.3", 11_880,     396,     2_376,   768;
    20, "2",   11_880,     396,     2_376,   2_000;
    21, "2.1", 19_800,     792,     4_752,   4_000;
    22, "2.2", 20_250,     1_620,   8_100,   4_000;
    30, "3",   40_500,     1_620,   8_100,   10_000;
    31, "3.1", 108_000,    3_600,   18_000,  14_000;
    32, "3.2", 216_000,    5_120,   20_480,  20_000;
    40, "4",   245_760,    8_192,   32_768,  20_000;
    41, "4.1", 245_760,    8_192,   32_768,  50_000;
    42, "4.2", 522_240,    8_704,   34_816,  50_000;
    50, "5",   589_824,    22_080,  110_400, 135_000;
    51, "5.1", 983_040,    36_864,  184_320, 240_000;
    52, "5.2", 2_073_600,  36_864,  184_320, 240_000;
    60, "6",   4_177_920,  139_264, 696_320, 240_000;
    61, "6.1", 8_355_840,  139_264, 696_320, 480_000;
    62, "6.2", 16_711_680, 139_264, 696_320, 800_000;
}

/// The [`Level`] a `level_idc` denotes.
///
/// `// D17:` level 1b is **not** folded in. Annex A.3.1 gives level 1b two
/// spellings — `level_idc == 11` with `constraint_set3_flag` set for the
/// non-High profiles, and `level_idc == 9` for the High ones — and a tool that
/// resolved them would report 9 for both. The reference does not: probed with
/// the constraint byte set to `0x10` and `level_idc` 11, `ffprobe` prints
/// `level=11`, and with `level_idc` 9 it prints `level=9`. So `level_idc` is
/// passed through verbatim and the 1b question is the caller's, through
/// [`is_level_1b`].
#[must_use]
pub const fn level(level_idc: u8) -> Level {
    Level(level_idc as i32)
}

/// Whether a `level_idc` and constraint byte denote level 1b, §A.3.1.
#[must_use]
pub const fn is_level_1b(level_idc: u8, profile_idc: u8, flags: ConstraintFlags) -> bool {
    match level_idc {
        9 => true,
        // For Baseline, Constrained Baseline, Main and Extended,
        // `constraint_set3_flag` at level 11 selects 1b.
        11 => matches!(profile_idc, 66 | 77 | 88) && flags.get(3),
        _ => false,
    }
}

/// `MaxDpbFrames`, §A.3.1:
/// `Min(MaxDpbMbs / (PicWidthInMbs * FrameHeightInMbs), 16)`.
///
/// Depends on the picture size, which is why it is a function rather than a
/// table column. Returns `None` for an unknown level or a zero-sized picture.
#[must_use]
pub fn max_dpb_frames(
    level_idc: u8,
    pic_width_in_mbs: u32,
    frame_height_in_mbs: u32,
) -> Option<u32> {
    let row = ROWS.iter().find(|r| r.idc == i32::from(level_idc))?;
    let mbs = u64::from(pic_width_in_mbs).checked_mul(u64::from(frame_height_in_mbs))?;
    let frames = row.max_dpb_mbs.checked_div(mbs)?;
    Some(frames.min(16) as u32)
}

/// `MaxBR` scaled by the profile's `cpbBrVclFactor`, §A.3.1, in kbit/s.
///
/// The High profiles raise the limit: 1.25x for High, 3x for High 10, and 4x
/// for High 4:2:2, High 4:4:4 Predictive and CAVLC 4:4:4. Everything else uses
/// the table value unchanged.
#[must_use]
pub fn max_bit_rate_kbps(level_idc: u8, profile_idc: u8) -> Option<u64> {
    let row = ROWS.iter().find(|r| r.idc == i32::from(level_idc))?;
    let base = u64::from(row.max_br);
    // Expressed as a fraction so the 1.25x case stays exact in integers.
    let (num, den) = match profile_idc {
        100 => (5u64, 4u64),
        110 => (3, 1),
        122 | 244 | 44 => (4, 1),
        _ => (1, 1),
    };
    base.checked_mul(num)?.checked_div(den)
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

    fn flags(bits: u8) -> ConstraintFlags {
        ConstraintFlags::from_bits(bits)
    }

    #[test]
    fn constraint_bit_order_matches_the_bitstream() {
        // constraint_set0_flag is the most significant bit of the byte.
        assert!(flags(0x80).get(0));
        assert!(flags(0x40).get(1));
        assert!(flags(0x10).get(3));
        assert!(flags(0x08).get(4));
        assert!(flags(0x04).get(5));
        assert_eq!(flags(0x03).reserved(), 3);
        assert!(!flags(0xFC).get(6));
    }

    /// Every row of this table was read back from `ffprobe 8.1`. If one of
    /// these ever changes, the reference changed, not the standard.
    #[test]
    fn profile_names_match_the_probed_reference() {
        let cases: &[(u8, u8, Option<&str>)] = &[
            (66, 0x00, Some("Baseline")),
            (66, 0x40, Some("Constrained Baseline")),
            (66, 0xC0, Some("Constrained Baseline")),
            (66, 0x10, Some("Baseline")),
            (77, 0x00, Some("Main")),
            (77, 0x40, Some("Main")),
            (88, 0x00, Some("Extended")),
            (100, 0x00, Some("High")),
            (100, 0x10, Some("High")),
            (100, 0x08, Some("High")),
            (100, 0x0C, Some("High")),
            (110, 0x00, Some("High 10")),
            (110, 0x10, Some("High 10 Intra")),
            (110, 0x40, Some("High 10")),
            (122, 0x00, Some("High 4:2:2")),
            (122, 0x10, Some("High 4:2:2 Intra")),
            (244, 0x00, Some("High 4:4:4 Predictive")),
            (244, 0x10, Some("High 4:4:4 Intra")),
            (44, 0x00, Some("CAVLC 4:4:4")),
            (44, 0x10, Some("CAVLC 4:4:4")),
            (118, 0x00, Some("Multiview High")),
            (118, 0x10, Some("Multiview High")),
            (128, 0x00, Some("Stereo High")),
            (144, 0x00, Some("High 4:4:4")),
            (83, 0x00, None),
            (86, 0x00, None),
            (134, 0x00, None),
            (135, 0x00, None),
            (138, 0x00, None),
            (139, 0x00, None),
        ];
        for &(idc, bits, expected) in cases {
            assert_eq!(
                profile_name(idc, flags(bits)),
                expected,
                "profile_idc {idc} constraints {bits:#04x}"
            );
        }
    }

    #[test]
    fn the_composed_value_keeps_the_idc_recoverable() {
        for idc in [66u8, 77, 100, 110, 122, 244, 44] {
            for bits in [0x00u8, 0x40, 0x10] {
                let p = profile(idc, flags(bits));
                assert_eq!(profile_idc_of(p), idc);
            }
        }
        assert_eq!(
            profile(66, flags(0x40)).value,
            0x42 | PROFILE_CONSTRAINED,
            "constrained baseline"
        );
        assert_eq!(profile(110, flags(0x10)).value, 0x6E | PROFILE_INTRA);
        assert_eq!(profile(100, flags(0x10)).value, 100, "no intra for High");
    }

    #[test]
    fn the_level_table_is_ordered_by_capability() {
        let entries = LEVELS.0;
        for w in entries.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            assert!(
                a.constraints.max_luma_picture_size <= b.constraints.max_luma_picture_size,
                "{} then {} breaks the picture-size order",
                a.name,
                b.name
            );
        }
    }

    #[test]
    fn level_1b_has_both_spellings() {
        assert!(is_level_1b(9, 100, flags(0x00)));
        assert!(is_level_1b(11, 66, flags(0x10)));
        assert!(!is_level_1b(11, 66, flags(0x00)));
        assert!(!is_level_1b(11, 100, flags(0x10)), "High uses level_idc 9");
        assert!(!is_level_1b(30, 66, flags(0x10)));
    }

    #[test]
    fn max_dimension_is_the_integer_square_root() {
        // MaxFS 8192 (level 4) -> sqrt(65536) = 256 macroblocks = 4096 luma.
        assert_eq!(max_dimension_mbs(8_192), 256);
        // MaxFS 1620 (level 3) -> sqrt(12960) = 113 (113^2 = 12769).
        assert_eq!(max_dimension_mbs(1_620), 113);
        assert_eq!(
            LEVELS.constraints(Level(40)).map(|c| c.max_h_size),
            Some(4_096)
        );
    }

    /// The classic worked example: 1920x1088 at level 4 holds four frames.
    #[test]
    fn dpb_frames_for_1080p_at_level_4() {
        // 120 x 68 macroblocks = 8160; 32768 / 8160 = 4.
        assert_eq!(max_dpb_frames(40, 120, 68), Some(4));
        // A tiny picture is capped at 16 rather than the arithmetic result.
        assert_eq!(max_dpb_frames(40, 1, 1), Some(16));
        assert_eq!(max_dpb_frames(40, 0, 0), None);
        assert_eq!(max_dpb_frames(255, 120, 68), None);
    }

    #[test]
    fn the_high_profiles_raise_the_bit_rate_limit() {
        assert_eq!(max_bit_rate_kbps(40, 66), Some(20_000));
        assert_eq!(max_bit_rate_kbps(40, 100), Some(25_000));
        assert_eq!(max_bit_rate_kbps(40, 110), Some(60_000));
        assert_eq!(max_bit_rate_kbps(40, 122), Some(80_000));
        assert_eq!(max_bit_rate_kbps(99, 100), None);
    }

    #[test]
    fn level_names_round_trip() {
        assert_eq!(LEVELS.name(Level(40)), Some("4"));
        assert_eq!(LEVELS.name(Level(9)), Some("1b"));
        assert_eq!(LEVELS.name(Level(51)), Some("5.1"));
        assert_eq!(LEVELS.from_name("4.1"), Some(Level(41)));
        assert_eq!(LEVELS.name(Level(255)), None);
    }
}
