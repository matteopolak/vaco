//! Profiles, tiers and levels, ITU-T H.265 Annex A.
//!
//! Two tables and the rules that select a row. Both are format-dictated: Annex A
//! states the profile numbers and the level limits outright, so any conforming
//! implementation contains the same numbers (D7/D15, merger).
//!
//! # Where the profile names came from
//!
//! The *names* are not spec text — they are what the reference tool prints, and
//! `-show_streams` prints them, so D6 makes them part of the output contract.
//! They were recovered by black-box probe: a `x265` SPS had its
//! `general_profile_idc` and its 32 compatibility flags patched to each value
//! in turn, at the RBSP level with the emulation prevention recomputed, and was
//! fed back through `ffprobe`. Twenty-four rows; the full result is in
//! `docs/codec/vaco-parse-hevc.md`.
//!
//! Six values have names and the rest print as numbers, including 5, 7, 8, 10
//! and 11 — which Annexes G, H and I *do* name. That is reproduced, not fixed.
//!
//! # Levels are ×30, and the tier changes what they mean
//!
//! `general_level_idc` is thirty times the level number: 4.1 is 123 and 2.1 is
//! 63. The reference prints the raw value, so `ffprobe` says `level=123` where a
//! human says "level 4.1". [`level_name`] converts.
//!
//! A level is only half the limit: `general_tier_flag` selects between the Main
//! and High tier bit-rate and CPB caps, which differ by up to 3x. Level 4 at
//! High tier permits 30 Mbit/s where Main tier permits 12.

use vaco_codec_core::{Level, LevelConstraints, LevelEntry, LevelTable, Profile, ProfileTable};

use crate::ptl::ProfileTier;

/// The display name for an effective `general_profile_idc`, or `None` when the
/// reference prints the number instead.
///
/// `// D17:` five profiles Annexes G/H/I name are **not** named here, because
/// the reference does not name them: probed with `general_profile_idc` patched
/// to each of 0..=11, `ffprobe 8.1` printed `5`, `7`, `8`, `10` and `11`
/// verbatim where the specification says Scalable Main, 3D Main,
/// Screen-Extended Main and the two Multiview/Scalable 10 profiles.
#[must_use]
pub const fn profile_name(profile_idc: u8) -> Option<&'static str> {
    Some(match profile_idc {
        1 => "Main",
        2 => "Main 10",
        3 => "Main Still Picture",
        // A.3.5 and onwards call this family the "format range extensions
        // profiles"; the reference prints this five-letter spelling for all of
        // them, without distinguishing Main 4:2:2 10 from Main 4:4:4 12.
        4 => "Rext",
        6 => "Multiview Main",
        9 => "Scc",
        _ => return None,
    })
}

/// The [`Profile`] a `profile_tier_level()`'s general layer describes.
///
/// `value` is the **effective** profile idc — see
/// [`ProfileTier::effective_profile_idc`] for why that is not simply
/// `general_profile_idc` — and `name` is `""` for a profile nothing names,
/// which is the signal for a caller to print the number instead. That is what
/// the reference does.
#[must_use]
pub fn profile(pt: &ProfileTier) -> Profile {
    let idc = pt.effective_profile_idc();
    Profile::new(i32::from(idc), profile_name(idc).unwrap_or(""))
}

/// The profile table, in the reference's spelling.
pub const PROFILES: ProfileTable = ProfileTable(&[
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(1, "Main"),
        subsumes: &[],
    },
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(2, "Main 10"),
        subsumes: &[1, 3],
    },
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(3, "Main Still Picture"),
        subsumes: &[],
    },
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(4, "Rext"),
        subsumes: &[1, 2, 3],
    },
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(6, "Multiview Main"),
        subsumes: &[1],
    },
    vaco_codec_core::ProfileEntry {
        profile: Profile::new(9, "Scc"),
        subsumes: &[1, 2, 3, 4],
    },
]);

// --------------------------------------------------------------------- levels

/// The tier a level is being read at, §A.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// `general_tier_flag == 0`. The consumer tier.
    #[default]
    Main,
    /// `general_tier_flag == 1`. Defined only for level 4 and above.
    High,
}

impl Tier {
    /// From `general_tier_flag`.
    #[must_use]
    pub const fn from_flag(flag: bool) -> Self {
        if flag { Self::High } else { Self::Main }
    }
}

/// One row of Table A.6 / A.8 / A.9, in the specification's own units.
struct Row {
    /// `general_level_idc`, thirty times the level number.
    idc: i32,
    /// Display name, `"4.1"`.
    name: &'static str,
    /// `MaxLumaPs`, luma samples per picture (Table A.6).
    max_luma_ps: u64,
    /// `MaxLumaSr`, luma samples per second (Table A.6).
    max_luma_sr: u64,
    /// `MaxBR` at Main tier, in units of 1000 bits/s for a `CpbBrVclFactor` of
    /// 1000 (Table A.8/A.9 — the Main and Main 10 columns).
    max_br_main: u32,
    /// `MaxBR` at High tier. Zero where the tier is not defined, which is every
    /// level below 4.
    max_br_high: u32,
}

/// `MaxDpbSize` and the picture-dimension limits both derive from `MaxLumaPs`,
/// §A.4.1, so neither is a table column.
///
/// §A.4.1's dimension bound is `Sqrt( MaxLumaPs * 8 )` for both axes. An
/// integer square root by binary search, so it is `const` and exact —
/// `f64::sqrt` would be neither.
const fn max_dimension(max_luma_ps: u64) -> u64 {
    let target = max_luma_ps * 8;
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

/// Build one [`LevelEntry`] from Table A.6's numbers.
///
/// `max_tiles` and `max_tile_cols` are Table A.6's `MaxTileRows * MaxTileCols`
/// and `MaxTileCols`; they step at levels 3, 3.1, 4 and 6.
const fn entry_of(
    idc: i32,
    name: &'static str,
    max_luma_ps: u64,
    max_luma_sr: u64,
    max_br: u32,
) -> LevelEntry {
    let dim = max_dimension(max_luma_ps) as u32;
    let (rows, cols): (u16, u16) = if idc < 90 {
        (1, 1)
    } else if idc < 93 {
        (2, 2)
    } else if idc < 120 {
        (5, 5)
    } else if idc < 156 {
        (11, 10)
    } else {
        (22, 20)
    };
    LevelEntry {
        level: Level(idc),
        name,
        constraints: LevelConstraints {
            max_luma_picture_size: max_luma_ps,
            max_luma_sample_rate: max_luma_sr,
            max_bitrate_kbps: max_br,
            // §A.4.2's MaxDpbSize depends on the picture size, so no single
            // number belongs here; 16 is the ceiling that clause imposes.
            // `max_dpb_size` computes the real one.
            max_dpb_frames: 16,
            max_h_size: dim,
            max_v_size: dim,
            max_tiles: rows * cols,
            max_tile_cols: cols,
        },
    }
}

/// Table A.6 (`MaxLumaPs`, `MaxLumaSr`) and Tables A.8/A.9 (`MaxBR` in kbit/s
/// at a `CpbBrVclFactor` of 1000, i.e. the Main and Main 10 columns), written
/// once and expanded into both the ordered [`LevelTable`] the framework
/// consumes and the side table the tier-dependent helpers need.
///
/// A macro rather than two literal arrays because the numbers are the
/// standard's and a transcription that exists twice will eventually disagree
/// with itself.
macro_rules! level_table {
    ($($idc:literal, $name:literal, $ps:literal, $sr:literal, $main:literal, $high:literal;)*) => {
        const LEVEL_ROWS: &[Row] = &[$(Row {
            idc: $idc, name: $name, max_luma_ps: $ps, max_luma_sr: $sr,
            max_br_main: $main, max_br_high: $high,
        }),*];
        /// Table A.6 / A.8 at **Main** tier, ordered by increasing capability.
        ///
        /// Main tier because that is the one every consumer stream declares;
        /// [`level_constraints`] takes a [`Tier`] for the other.
        pub const LEVELS: LevelTable = LevelTable(&[
            $(entry_of($idc, $name, $ps, $sr, $main)),*
        ]);
    };
}

level_table! {
    30,  "1",   36_864,     552_960,       128,     0;
    60,  "2",   122_880,    3_686_400,     1_500,   0;
    63,  "2.1", 245_760,    7_372_800,     3_000,   0;
    90,  "3",   552_960,    16_588_800,    6_000,   0;
    93,  "3.1", 983_040,    33_177_600,    10_000,  0;
    120, "4",   2_228_224,  66_846_720,    12_000,  30_000;
    123, "4.1", 2_228_224,  133_693_440,   20_000,  50_000;
    150, "5",   8_912_896,  267_386_880,   25_000,  100_000;
    153, "5.1", 8_912_896,  534_773_760,   40_000,  160_000;
    156, "5.2", 8_912_896,  1_069_547_520, 60_000,  240_000;
    180, "6",   35_651_584, 1_069_547_520, 60_000,  240_000;
    183, "6.1", 35_651_584, 2_139_095_040, 120_000, 480_000;
    186, "6.2", 35_651_584, 4_278_190_080, 240_000, 800_000;
}

/// The level a `general_level_idc` denotes.
///
/// The raw value, unscaled: `Level(123)` is level 4.1. Never normalised, so a
/// container's value round-trips out byte-identically (`vaco_codec_core::Level`
/// documents this as the rule for every codec).
#[must_use]
pub const fn level(level_idc: u8) -> Level {
    Level(level_idc as i32)
}

/// The display name for a `general_level_idc`, or `None` for a value Annex A
/// does not define.
#[must_use]
pub fn level_name(level_idc: u8) -> Option<&'static str> {
    LEVEL_ROWS
        .iter()
        .find(|r| r.idc == i32::from(level_idc))
        .map(|r| r.name)
}

/// What a level caps, at a given tier.
///
/// `None` for a `general_level_idc` Annex A does not define.
#[must_use]
pub fn level_constraints(level_idc: u8, tier: Tier) -> Option<LevelConstraints> {
    let row = LEVEL_ROWS.iter().find(|r| r.idc == i32::from(level_idc))?;
    let br = if tier == Tier::High && row.max_br_high != 0 {
        row.max_br_high
    } else {
        row.max_br_main
    };
    Some(entry_of(row.idc, row.name, row.max_luma_ps, row.max_luma_sr, br).constraints)
}

/// `MaxDpbSize`, §A.4.2 — the decoded picture buffer size a level permits for a
/// picture of `luma_samples` samples.
///
/// The rule is a four-step staircase in `MaxLumaPs / PicSizeInSamplesY`:
///
/// ```text
///   PicSizeInSamplesY <= MaxLumaPs / 4   ->  Min( 4 * MaxDpbPicBuf, 16 )
///   ...              <= MaxLumaPs / 2    ->  Min( 2 * MaxDpbPicBuf, 16 )
///   ...              <= 3 * MaxLumaPs / 4 -> Min( 4 * MaxDpbPicBuf / 3, 16 )
///   otherwise                            ->  MaxDpbPicBuf
/// ```
///
/// with `MaxDpbPicBuf` = 6 for every profile this crate reports. Returns `None`
/// for an unknown level or a zero-sized picture.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "§A.4.2's own arithmetic; MaxDpbPicBuf is 6, so 4 * 6 / 3 is exact"
)]
pub fn max_dpb_size(level_idc: u8, luma_samples: u64) -> Option<u32> {
    const MAX_DPB_PIC_BUF: u64 = 6;
    let row = LEVEL_ROWS.iter().find(|r| r.idc == i32::from(level_idc))?;
    if luma_samples == 0 {
        return None;
    }
    let ps = row.max_luma_ps;
    let n = if luma_samples.saturating_mul(4) <= ps {
        4 * MAX_DPB_PIC_BUF
    } else if luma_samples.saturating_mul(2) <= ps {
        2 * MAX_DPB_PIC_BUF
    } else if luma_samples.saturating_mul(4) <= ps.saturating_mul(3) {
        // §A.4.2's `4 * MaxDpbPicBuf / 3` — exact here, since MaxDpbPicBuf is 6.
        4 * MAX_DPB_PIC_BUF / 3
    } else {
        MAX_DPB_PIC_BUF
    };
    Some(n.min(16) as u32)
}

/// `MaxBR` in kbit/s, §A.4.2, scaled by the profile's `CpbBrVclFactor`.
///
/// Table A.8's numbers are for `CpbBrVclFactor` 1000, which covers Main, Main 10
/// and Main Still Picture. The range-extension profiles raise it, and the factor
/// depends on the bit depth and chroma format rather than on the profile number
/// alone (§A.4.2 Table A.10), so it is the caller's to supply: pass 1000 for
/// Main and Main 10.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "Table A.8's values are in units of 1000 bit/s and CpbBrVclFactor \
              is a multiple of 1000, so the division is exact"
)]
pub fn max_bit_rate_kbps(level_idc: u8, tier: Tier, cpb_br_vcl_factor: u32) -> Option<u64> {
    let row = LEVEL_ROWS.iter().find(|r| r.idc == i32::from(level_idc))?;
    let base = u64::from(if tier == Tier::High && row.max_br_high != 0 {
        row.max_br_high
    } else {
        row.max_br_main
    });
    base.checked_mul(u64::from(cpb_br_vcl_factor))
        .map(|v| v / 1000)
}

/// The smallest level that admits a picture of `luma_samples` at
/// `luma_sample_rate` — what `-level auto` picks.
///
/// No [`Tier`] parameter, because Table A.6's picture-size and sample-rate
/// limits are the same at both tiers; only the bit rate differs, and that is
/// [`max_bit_rate_kbps`]'s question.
#[must_use]
pub fn smallest_level_for(luma_samples: u64, luma_sample_rate: u64) -> Option<Level> {
    LEVEL_ROWS
        .iter()
        .find(|r| luma_samples <= r.max_luma_ps && luma_sample_rate <= r.max_luma_sr)
        .map(|r| Level(r.idc))
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

    /// Every profile name the reference prints, from the patched-SPS probe.
    #[test]
    fn the_profile_names_are_the_references() {
        let expected: &[(u8, Option<&str>)] = &[
            (0, None),
            (1, Some("Main")),
            (2, Some("Main 10")),
            (3, Some("Main Still Picture")),
            (4, Some("Rext")),
            (5, None),
            (6, Some("Multiview Main")),
            (7, None),
            (8, None),
            (9, Some("Scc")),
            (10, None),
            (11, None),
        ];
        for &(idc, name) in expected {
            assert_eq!(profile_name(idc), name, "profile_idc {idc}");
        }
    }

    /// The level names the reference prints unscaled, and the ×30 relationship.
    #[test]
    fn levels_are_thirty_times_the_level_number() {
        for (idc, name) in [
            (30u8, "1"),
            (60, "2"),
            (63, "2.1"),
            (90, "3"),
            (93, "3.1"),
            (120, "4"),
            (123, "4.1"),
            (150, "5"),
            (153, "5.1"),
            (156, "5.2"),
            (180, "6"),
            (186, "6.2"),
        ] {
            assert_eq!(level_name(idc), Some(name), "level_idc {idc}");
            assert_eq!(level(idc).raw(), i32::from(idc));
        }
        assert_eq!(level_name(31), None, "not a defined level");
    }

    /// The High tier raises the bit rate cap where it is defined, and is the
    /// Main tier's value where it is not.
    #[test]
    fn the_tier_changes_the_bit_rate_cap() {
        assert_eq!(max_bit_rate_kbps(120, Tier::Main, 1000), Some(12_000));
        assert_eq!(max_bit_rate_kbps(120, Tier::High, 1000), Some(30_000));
        // Below level 4 the High tier is not defined; the Main value stands.
        assert_eq!(max_bit_rate_kbps(90, Tier::Main, 1000), Some(6_000));
        assert_eq!(max_bit_rate_kbps(90, Tier::High, 1000), Some(6_000));
        assert_eq!(max_bit_rate_kbps(31, Tier::Main, 1000), None);
    }

    /// The picture sizes each level admits, at the boundaries that matter.
    #[test]
    fn the_luma_picture_size_limits_place_real_resolutions() {
        // 1920x1080 = 2_073_600 <= level 4's 2_228_224, and above 3.1's.
        assert_eq!(
            smallest_level_for(1920 * 1080, 1920 * 1080 * 25),
            Some(Level(120))
        );
        // 3840x2160 needs level 5.
        assert_eq!(
            smallest_level_for(3840 * 2160, 3840 * 2160 * 30),
            Some(Level(150))
        );
        // 640x360 at 24 fps fits level 2.1 — which is what x265 chose.
        assert_eq!(
            smallest_level_for(640 * 360, 640 * 360 * 24),
            Some(Level(63))
        );
    }

    /// §A.4.2's DPB staircase, at all four steps.
    #[test]
    #[allow(
        clippy::integer_division,
        reason = "exact divisions of a known constant"
    )]
    fn the_dpb_size_staircase() {
        // Level 4: MaxLumaPs 2_228_224.
        assert_eq!(max_dpb_size(120, 2_228_224 / 4), Some(16)); // 4*6 capped
        assert_eq!(max_dpb_size(120, 2_228_224 / 2), Some(12));
        assert_eq!(max_dpb_size(120, 3 * 2_228_224 / 4), Some(8));
        assert_eq!(max_dpb_size(120, 2_228_224), Some(6));
        assert_eq!(max_dpb_size(120, 0), None);
        assert_eq!(max_dpb_size(31, 1000), None);
    }

    /// The dimension bound is `Sqrt( MaxLumaPs * 8 )`, so a level admits a
    /// picture far wider than it is tall — and the constraint table has to say
    /// so, or a 8192-wide level-5 stream is rejected as out of level.
    #[test]
    fn the_dimension_bound_is_the_square_root_of_eight_times_the_area() {
        let c = level_constraints(150, Tier::Main).expect("level 5");
        assert_eq!(c.max_h_size, 8444, "Sqrt( 8912896 * 8 )");
        assert_eq!(c.max_v_size, 8444);
        assert!(c.max_h_size > 3840, "4K is comfortably inside level 5");
    }

    #[test]
    fn the_profile_table_resolves_by_name_and_by_value() {
        assert_eq!(PROFILES.from_name("main 10").map(|p| p.value), Some(2));
        assert_eq!(PROFILES.from_value(4).map(|p| p.name), Some("Rext"));
        let main = PROFILES.from_value(1).expect("Main");
        let main10 = PROFILES.from_value(2).expect("Main 10");
        assert!(PROFILES.subsumes(main10, main), "Main 10 decodes Main");
        assert!(!PROFILES.subsumes(main, main10));
    }
}
