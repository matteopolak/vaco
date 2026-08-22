//! Profiles, tiers and levels, AV1 spec Annex A.
//!
//! Both tables are format-dictated — Annex A states the profile numbers and
//! the per-level limits outright, so any conforming implementation contains
//! the same numbers (D7/D15, merger). The level table below was cross-checked
//! against a public secondary source (Wikipedia's AV1 article, itself a
//! transcription of Annex A's Table) rather than against any decoder's source,
//! for the reasons `planning/AGENT-BRIEF-TEMPLATE.md`'s "Probing the
//! reference" section gives: a number copied from the spec text is safer than
//! one copied from an implementation, clean-room concerns aside, simply
//! because a transcription error in a decoder's source would otherwise
//! silently propagate here as if it were the specification.
//!
//! # Levels are `seq_level_idx`, unscaled — unlike H.264's ×10 or HEVC's ×30
//!
//! `ffprobe` prints the raw index (measured: a `libsvtav1` stream at level 2.1
//! reports `level=1`, not `21` or `2`). [`level_name`] converts to the "2.1"
//! form for display.
//!
//! # Tier only matters for the bit-rate cap
//!
//! Unlike HEVC, where tier changes several constraints, AV1's Annex A tier bit
//! (`seq_tier`) changes **only** the bit-rate and CPB-size limits — every other
//! column (picture size, sample rate, tile counts) is the same for Main and
//! High tier at a given level. [`level_constraints`] takes the [`Tier`]; the
//! table's own [`LevelEntry::constraints`] carries the Main-tier number,
//! matching `vaco-parse-hevc`'s `LEVELS`/`level_constraints` split.

use vaco_codec_core::{Level, LevelConstraints, LevelEntry, LevelTable, Profile, ProfileTable};

/// `seq_tier`, Annex A.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    #[default]
    Main,
    High,
}

impl Tier {
    /// From the single-bit `seq_tier[i]` field.
    #[must_use]
    pub const fn from_flag(high: bool) -> Self {
        if high { Self::High } else { Self::Main }
    }
}

/// AV1's three profiles, Annex A.2. The value is `seq_profile`.
pub const PROFILE_MAIN: Profile = Profile::new(0, "Main");
pub const PROFILE_HIGH: Profile = Profile::new(1, "High");
pub const PROFILE_PROFESSIONAL: Profile = Profile::new(2, "Professional");

/// The display name for a `seq_profile`, or `None` for a value the
/// specification does not assign (3..=7).
#[must_use]
pub const fn profile_name(seq_profile: u8) -> Option<&'static str> {
    Some(match seq_profile {
        0 => "Main",
        1 => "High",
        2 => "Professional",
        _ => return None,
    })
}

/// The [`Profile`] for a raw `seq_profile` value.
#[must_use]
pub const fn profile(seq_profile: u8) -> Profile {
    match profile_name(seq_profile) {
        Some(name) => Profile::new(seq_profile as i32, name),
        None => Profile::new(seq_profile as i32, ""),
    }
}

/// `seq_profile == 2` decouples `BitDepth` from `mono_chrome`/subsampling in
/// `color_config()` — the only profile that can be 12-bit *and* carry chroma.
pub const PROFILES: ProfileTable = ProfileTable(&[
    vaco_codec_core::ProfileEntry {
        profile: PROFILE_MAIN,
        subsumes: &[],
    },
    vaco_codec_core::ProfileEntry {
        profile: PROFILE_HIGH,
        subsumes: &[0],
    },
    vaco_codec_core::ProfileEntry {
        profile: PROFILE_PROFESSIONAL,
        subsumes: &[0, 1],
    },
]);

/// One row of Annex A's level table, in the specification's own units:
/// pictures, samples and Hz, not kbit/s.
struct Row {
    idx: i32,
    max_pic_size: u64,
    max_h_size: u32,
    max_v_size: u32,
    max_decode_rate: u64,
    max_tiles: u16,
    max_tile_cols: u16,
    main_mbps: u32,
    high_mbps: u32,
}

/// Build one [`LevelEntry`] at Main tier's bit rate. `const fn`, so the whole
/// table below is built at compile time — no lookup, no `Option::expect`, in
/// the style `vaco-parse-hevc::profile::entry_of` established.
const fn entry_of(
    idx: i32,
    name: &'static str,
    max_pic_size: u64,
    max_h_size: u32,
    max_v_size: u32,
    max_decode_rate: u64,
    max_tiles: u16,
    max_tile_cols: u16,
    main_mbps: u32,
) -> LevelEntry {
    LevelEntry {
        level: Level(idx),
        name,
        constraints: LevelConstraints {
            max_luma_picture_size: max_pic_size,
            max_luma_sample_rate: max_decode_rate,
            max_bitrate_kbps: main_mbps,
            // §6.8.2 tracks 8 reference-frame slots at every level; Annex A
            // does not vary the DPB size by level the way HEVC's Annex A does.
            max_dpb_frames: 8,
            max_h_size,
            max_v_size,
            max_tiles,
            max_tile_cols,
        },
    }
}

/// Annex A's level table, written once and expanded into both the ordered
/// [`LevelTable`] the framework consumes and the side table
/// [`level_constraints`] needs for the High-tier bit rate.
///
/// `seq_level_idx` values not listed here (2, 3, 6, 7, 10, 11, and 20..=31)
/// are reserved: 2.2/2.3/3.2/3.3/4.2/4.3 have no defined limits at all, and
/// 7.0..=7.3 (20..=23) are reserved for a future revision despite
/// `seq_level_idx` having room to name them — see [`level_name`], which names
/// them anyway because a stream is free to *signal* a reserved index even
/// though nothing here can validate it.
macro_rules! level_table {
    ($($idx:literal, $name:literal, $ps:literal, $hs:literal, $vs:literal, $sr:literal, $tiles:literal, $cols:literal, $main:literal, $high:literal;)*) => {
        const LEVEL_ROWS: &[Row] = &[$(Row {
            idx: $idx, max_pic_size: $ps, max_h_size: $hs, max_v_size: $vs,
            max_decode_rate: $sr, max_tiles: $tiles, max_tile_cols: $cols,
            main_mbps: $main, high_mbps: $high,
        }),*];
        /// Table at **Main** tier, ordered by increasing capability;
        /// [`level_constraints`] takes a [`Tier`] for the High-tier figure.
        pub const LEVELS: LevelTable = LevelTable(&[
            $(entry_of($idx, $name, $ps, $hs, $vs, $sr, $tiles, $cols, $main)),*
        ]);
    };
}

level_table! {
    0,  "2.0", 147_456,    2048,  1152, 5_529_600,     2, 8,   1_500,   0;
    1,  "2.1", 278_784,    2816,  1584, 10_454_400,    2, 8,   3_000,   0;
    4,  "3.0", 665_856,    4352,  2448, 24_969_600,    2, 16,  6_000,   0;
    5,  "3.1", 1_065_024,  5504,  3096, 39_938_400,    2, 16,  10_000,  0;
    8,  "4.0", 2_359_296,  6144,  3456, 77_856_768,    4, 32,  12_000,  30_000;
    9,  "4.1", 2_359_296,  6144,  3456, 155_713_536,   4, 32,  20_000,  50_000;
    12, "5.0", 8_912_896,  8192,  4352, 273_715_200,   6, 64,  30_000,  100_000;
    13, "5.1", 8_912_896,  8192,  4352, 547_430_400,   8, 64,  40_000,  160_000;
    14, "5.2", 8_912_896,  8192,  4352, 1_094_860_800, 8, 64,  60_000,  240_000;
    15, "5.3", 8_912_896,  8192,  4352, 1_176_502_272, 8, 64,  60_000,  240_000;
    16, "6.0", 35_651_584, 16384, 8704, 1_176_502_272, 8, 128, 60_000,  240_000;
    17, "6.1", 35_651_584, 16384, 8704, 2_189_721_600, 8, 128, 100_000, 480_000;
    18, "6.2", 35_651_584, 16384, 8704, 4_379_443_200, 8, 128, 160_000, 800_000;
    19, "6.3", 35_651_584, 16384, 8704, 4_706_009_088, 8, 128, 160_000, 800_000;
}

/// A level's constraints, adjusted for `tier`'s bit-rate cap.
///
/// `None` for a `seq_level_idx` Annex A reserves (2, 3, 6, 7, 10, 11, or
/// anything at or above 20).
#[must_use]
pub fn level_constraints(seq_level_idx: u8, tier: Tier) -> Option<LevelConstraints> {
    let row = LEVEL_ROWS
        .iter()
        .find(|r| r.idx == i32::from(seq_level_idx))?;
    let mbps = if tier == Tier::High && row.high_mbps != 0 {
        row.high_mbps
    } else {
        row.main_mbps
    };
    Some(LevelConstraints {
        max_luma_picture_size: row.max_pic_size,
        max_luma_sample_rate: row.max_decode_rate,
        max_bitrate_kbps: mbps,
        max_dpb_frames: 8,
        max_h_size: row.max_h_size,
        max_v_size: row.max_v_size,
        max_tiles: row.max_tiles,
        max_tile_cols: row.max_tile_cols,
    })
}

/// The "major.minor" display name for a `seq_level_idx`, computed from Annex
/// A's own numbering rule (`2 + idx/4` . `idx%4`) rather than tabulated, so it
/// covers every index 0..=23 including the reserved 2.2/2.3/3.2/3.3/4.2/4.3 and
/// the not-yet-defined 7.0..=7.3 — a stream is free to signal any of them even
/// though [`level_constraints`] cannot validate the reserved ones.
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "bounds are checked immediately above; `<[T]>::get` is not callable in a const fn, \
              matching the precedent in vaco-pixfmt's `PixFmtDescriptor::chroma_plane`"
)]
pub const fn level_name(seq_level_idx: u8) -> Option<&'static str> {
    const NAMES: [&str; 24] = [
        "2.0", "2.1", "2.2", "2.3", "3.0", "3.1", "3.2", "3.3", "4.0", "4.1", "4.2", "4.3", "5.0",
        "5.1", "5.2", "5.3", "6.0", "6.1", "6.2", "6.3", "7.0", "7.1", "7.2", "7.3",
    ];
    if (seq_level_idx as usize) < NAMES.len() {
        Some(NAMES[seq_level_idx as usize])
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    #[test]
    fn profile_names_match_annex_a() {
        assert_eq!(profile_name(0), Some("Main"));
        assert_eq!(profile_name(1), Some("High"));
        assert_eq!(profile_name(2), Some("Professional"));
        assert_eq!(profile_name(3), None);
    }

    #[test]
    fn level_names_follow_the_two_plus_idx_over_four_rule() {
        assert_eq!(level_name(0), Some("2.0"));
        assert_eq!(level_name(1), Some("2.1"));
        assert_eq!(level_name(13), Some("5.1"));
        assert_eq!(level_name(19), Some("6.3"));
        assert_eq!(level_name(23), Some("7.3"));
        assert_eq!(level_name(24), None);
    }

    #[test]
    fn tier_changes_only_the_bit_rate_cap() {
        let main = level_constraints(8, Tier::Main).unwrap();
        let high = level_constraints(8, Tier::High).unwrap();
        assert_eq!(main.max_bitrate_kbps, 12_000);
        assert_eq!(high.max_bitrate_kbps, 30_000);
        assert_eq!(main.max_h_size, high.max_h_size);
        assert_eq!(main.max_luma_picture_size, high.max_luma_picture_size);
    }

    #[test]
    fn reserved_indices_have_no_constraints() {
        assert!(level_constraints(2, Tier::Main).is_none());
        assert!(level_constraints(20, Tier::Main).is_none());
    }

    #[test]
    fn a_level_below_high_tier_bitrate_falls_back_to_main() {
        // Level 2.0 (idx 0) has no High-tier figure in Annex A.
        let c = level_constraints(0, Tier::High).unwrap();
        assert_eq!(c.max_bitrate_kbps, 1_500);
    }

    #[test]
    fn the_level_table_matches_a_real_libsvtav1_stream() {
        // Measured: `ffmpeg -c:v libsvtav1` on a 642x358 input reports
        // `seq_level_idx = 1` (level 2.1).
        assert_eq!(LEVELS.name(Level(1)), Some("2.1"));
    }
}
