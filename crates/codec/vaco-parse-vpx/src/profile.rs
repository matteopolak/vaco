//! VP9 profiles and levels, Annex A of the VP9 Bitstream & Decoding Process
//! Specification (v0.6, 8 Dec 2016).
//!
//! # Where the names came from
//!
//! Unlike H.264/HEVC/AV1, whose profiles have descriptive names ("High",
//! "Main 10"), VP9's four profiles are printed by the reference as the bare
//! ordinal — measured, `ffprobe -show_entries stream=profile` on
//! `libvpx-vp9` output at each of the four `-profile:v` values prints
//! `Profile 0`, `Profile 1`, `Profile 2`, `Profile 3` verbatim, never a
//! descriptive name. So [`PROFILE_NAMES`] is the whole of the naming
//! question; there is no Annex-A wording to reproduce.
//!
//! # `level` is never populated from the bitstream — measured, not assumed
//!
//! `// D17:` the uncompressed header (§6.2) carries `profile` but has **no
//! level syntax element at all**, unlike AV1's `seq_level_idx`. Probed
//! directly: `libvpx-vp9 -level 4.0`, remuxed through both `WebM` and MP4 (the
//! latter carrying a `vpcC` box that *does* state a level byte), still reports
//! `level=-99` from `ffprobe -show_entries stream=level` — because `ffprobe`'s
//! own VP9 reader never looks at `vpcC`'s level byte either. So
//! [`LEVELS`] exists to satisfy P-05's "a VP9 level table exists somewhere"
//! requirement and to give [`level_from_vpcc`] a name to attach to a `vpcC`
//! byte when a caller wants one, but nothing in [`crate::vp9`] calls it: the
//! parser reports what the reference reports, which is nothing.
//!
//! # The level table is transcribed, not probed
//!
//! Annex A's Table 3 states the numbers outright (D7/D15, merger), and there
//! is no way to probe it: with `level` never surfacing, no `ffprobe` output
//! can confirm a row. Cross-checked against a public secondary transcription
//! (the VP9 Wikipedia article's level table) rather than against any
//! decoder's source, for the same reason `vaco-parse-av1::profile` gives for
//! its own Annex A table — a transcription error in a decoder's source would
//! otherwise silently propagate here as if it were the specification. Flagged
//! as unverified-by-measurement in the crate's issue-closing comment.

use vaco_codec_core::{Level, LevelConstraints, LevelEntry, LevelTable, Profile};

/// `Profile 0` through `Profile 3`, indexed by the raw `profile` value.
///
/// A fixed array rather than a `format!` at the call site: every name is
/// `'static` so [`Profile::name`](vaco_codec_core::Profile) can borrow it
/// without allocating on the parser's read path.
pub const PROFILE_NAMES: [&str; 4] = ["Profile 0", "Profile 1", "Profile 2", "Profile 3"];

/// The [`Profile`] for a raw 2-bit `profile` value (0..=3; §6.2's
/// `profile_low_bit`/`profile_high_bit`/`reserved_zero` combine to exactly
/// this range, so every value this type can hold has a name).
#[must_use]
pub const fn profile(profile: u8) -> Profile {
    match profile {
        0 => Profile::new(0, PROFILE_NAMES[0]),
        1 => Profile::new(1, PROFILE_NAMES[1]),
        2 => Profile::new(2, PROFILE_NAMES[2]),
        3 => Profile::new(3, PROFILE_NAMES[3]),
        // Not reachable from a conforming 2-bit field; named rather than
        // panicking so a caller that hands in a corrupt value still gets an
        // answer.
        _ => Profile::new(Profile::UNKNOWN_VALUE, ""),
    }
}

/// The one width/height bound every level shares: §6.2's `frame_size()`
/// codes `frame_width_minus_1`/`frame_height_minus_1` as 16 bits each, so no
/// VP9 frame of any level can exceed this in either dimension.
///
/// Annex A.2's table has no per-level width/height column at all — only the
/// `MaxLumaPictureSize` area — so this is *not* a per-level number. It exists
/// because [`LevelConstraints::admits`] checks `max_h_size`/`max_v_size`
/// unconditionally (no "0 means unconstrained" case, unlike its bitrate and
/// tile fields), so leaving them at 0 would make every level reject every
/// query. Using the syntax's own ceiling keeps `admits` meaningful without
/// inventing a per-level split Annex A does not state.
const MAX_DIMENSION: u32 = 1 << 16;

const fn entry(idc: i32, name: &'static str, max_rate: u64, max_size: u64, br: u32) -> LevelEntry {
    LevelEntry {
        level: Level(idc),
        name,
        constraints: LevelConstraints {
            max_luma_picture_size: max_size,
            max_luma_sample_rate: max_rate,
            max_bitrate_kbps: br,
            // Annex A.2's `MaxRefFrameBuffers` column: 8 up to level 5.1, then
            // shrinks. Not exposed as a distinct field on `LevelConstraints`,
            // so it rides on `max_dpb_frames` — the closest existing meaning
            // (buffers a decoder must keep) even though VP9 does not call it
            // a "decoded picture buffer".
            max_dpb_frames: if idc <= 51 { 8 } else { 6 },
            max_h_size: MAX_DIMENSION,
            max_v_size: MAX_DIMENSION,
            max_tiles: 0,
            max_tile_cols: 0,
        },
    }
}

/// Annex A Table 3, in increasing-capability order (required by
/// [`LevelTable::smallest_for`]).
///
/// `MaxLumaSampleRate` and `MaxLumaPictureSize` are luma *samples*, matching
/// `LevelConstraints`' own units; `MaxBitrate` is Annex A's Main-tier column
/// converted from Mbit/s to kbit/s (VP9 has no separate High tier the way
/// HEVC and AV1 do — one column, one number).
pub const LEVELS: LevelTable = LevelTable(&[
    entry(10, "1", 829_440, 36_864, 200),
    entry(11, "1.1", 2_764_800, 73_728, 800),
    entry(20, "2", 4_608_000, 122_880, 1_800),
    entry(21, "2.1", 9_216_000, 245_760, 3_600),
    entry(30, "3", 20_736_000, 552_960, 7_200),
    entry(31, "3.1", 36_864_000, 983_040, 12_000),
    entry(40, "4", 83_558_400, 2_228_224, 18_000),
    entry(41, "4.1", 160_432_128, 2_228_224, 30_000),
    entry(50, "5", 311_951_360, 8_912_896, 60_000),
    entry(51, "5.1", 588_251_136, 8_912_896, 120_000),
    entry(52, "5.2", 1_176_502_272, 8_912_896, 180_000),
    entry(60, "6", 1_176_502_272, 35_651_584, 180_000),
    entry(61, "6.1", 2_353_004_544, 35_651_584, 240_000),
    entry(62, "6.2", 4_706_009_088, 35_651_584, 480_000),
]);

/// The raw level byte a `vpcC` box states — `level_idc`-style, level × 10 —
/// converted to the [`Level`] [`LEVELS`] indexes by.
///
/// Exists for a caller that has a `vpcC` record and wants a name for its
/// level byte (e.g. building a `vp09.PP.LL.DD` codec string). **Not** called
/// from [`crate::vp9::Vp9Parser`]: see the module-level `// D17:` note.
#[must_use]
pub const fn level_from_vpcc(level_byte: u8) -> Level {
    Level(level_byte as i32)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    #[test]
    fn profile_names_match_the_probed_reference() {
        // Measured against `ffprobe 8.1`: `-profile:v 0..3` on `libvpx-vp9`
        // prints `profile=Profile 0` .. `profile=Profile 3` verbatim.
        for p in 0u8..4 {
            assert_eq!(profile(p).name, PROFILE_NAMES[p as usize]);
            assert_eq!(profile(p).value, i32::from(p));
        }
    }

    #[test]
    fn an_out_of_range_profile_is_unknown_not_a_panic() {
        assert!(profile(4).is_unknown());
        assert!(profile(255).is_unknown());
    }

    #[test]
    fn the_level_table_is_ordered_by_capability() {
        let entries = LEVELS.0;
        for w in entries.windows(2) {
            assert!(
                w[0].constraints.max_luma_sample_rate <= w[1].constraints.max_luma_sample_rate,
                "{} then {} breaks the sample-rate order",
                w[0].name,
                w[1].name
            );
        }
    }

    #[test]
    fn level_names_round_trip() {
        assert_eq!(LEVELS.name(Level(40)), Some("4"));
        assert_eq!(LEVELS.from_name("5.1"), Some(Level(51)));
        assert_eq!(LEVELS.name(Level(9)), None);
    }

    #[test]
    fn level_from_vpcc_is_the_raw_byte() {
        assert_eq!(level_from_vpcc(10), Level(10));
    }
}
