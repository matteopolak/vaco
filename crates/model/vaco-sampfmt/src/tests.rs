//! Unit and property tests.
//!
//! The table is small enough to assert exhaustively, so most of these are
//! "every format satisfies X" loops rather than spot checks. The values in
//! [`REFERENCE_TABLE`] are the recorded output of
//! `ffmpeg -hide_banner -sample_fmts` (`FFmpeg` 8.1) — the acceptance criterion
//! for this crate, per plan 11 §9.6.
#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test that indexes out of range or unwraps a None is a failing \
              test, which is the correct outcome; the lints exist to stop \
              library code panicking on hostile input"
)]

use proptest::prelude::*;

use super::{SampleFmt, SampleKind};

/// `ffmpeg -hide_banner -sample_fmts`, verbatim: `(name, depth)` in listing
/// order. Both columns and the order are observable output.
const REFERENCE_TABLE: [(&str, u32); 12] = [
    ("u8", 8),
    ("s16", 16),
    ("s32", 32),
    ("flt", 32),
    ("dbl", 64),
    ("u8p", 8),
    ("s16p", 16),
    ("s32p", 32),
    ("fltp", 32),
    ("dblp", 64),
    ("s64", 64),
    ("s64p", 64),
];

#[test]
fn matches_the_reference_listing() {
    let ours: Vec<(&str, u32)> = SampleFmt::ALL
        .iter()
        .map(|f| (f.name(), f.bits_per_sample()))
        .collect();
    assert_eq!(ours, REFERENCE_TABLE.to_vec());
}

#[test]
fn all_covers_every_variant_exactly_once() {
    let mut seen = SampleFmt::ALL.to_vec();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), SampleFmt::ALL.len());
}

#[test]
fn every_name_round_trips() {
    for fmt in SampleFmt::ALL {
        let name = fmt.name();
        assert_eq!(
            SampleFmt::from_name(name).ok(),
            Some(fmt),
            "`{name}` did not round-trip"
        );
        assert_eq!(fmt.to_string(), name);
        assert_eq!(name.parse::<SampleFmt>().ok(), Some(fmt));
    }
}

#[test]
fn names_are_unique() {
    let mut names: Vec<&str> = SampleFmt::ALL.iter().map(|f| f.name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), SampleFmt::ALL.len());
}

#[test]
fn unknown_names_are_rejected_not_guessed() {
    // The reference rejects every one of these; see the D17 note on `from_name`.
    for bad in [
        "", "none", "NONE", "S16", "s16 ", " s16", "u16", "s8", "f32", "float", "double", "0", "1",
        "-1", "s16P", "S16P",
    ] {
        assert!(
            SampleFmt::from_name(bad).is_err(),
            "`{bad}` should not have parsed"
        );
    }
}

#[test]
fn planar_and_packed_are_a_bijection() {
    for fmt in SampleFmt::ALL {
        let planar = fmt.to_planar();
        let packed = fmt.to_packed();
        assert!(planar.is_planar(), "{fmt} -> {planar} is not planar");
        assert!(!packed.is_planar(), "{fmt} -> {packed} is planar");
        // The pair agrees on everything except storage.
        assert_eq!(planar.bytes_per_sample(), fmt.bytes_per_sample());
        assert_eq!(packed.bytes_per_sample(), fmt.bytes_per_sample());
        assert_eq!(planar.kind(), fmt.kind());
        assert_eq!(packed.kind(), fmt.kind());
        // Idempotent, and mutually inverse.
        assert_eq!(planar.to_planar(), planar);
        assert_eq!(packed.to_packed(), packed);
        assert_eq!(planar.to_packed(), packed);
        assert_eq!(packed.to_planar(), planar);
    }
}

#[test]
fn planar_names_are_the_packed_name_plus_p() {
    // Not a coincidence worth relying on in code, but a real invariant of the
    // reference's naming that a typo in the table would break.
    for fmt in SampleFmt::ALL {
        assert_eq!(
            fmt.to_planar().name(),
            format!("{}p", fmt.to_packed().name()),
            "planar name for {fmt} is not the packed name plus `p`"
        );
    }
}

#[test]
fn kinds_are_what_the_names_say() {
    assert_eq!(SampleFmt::U8.kind(), SampleKind::Unsigned);
    assert_eq!(SampleFmt::U8P.kind(), SampleKind::Unsigned);
    for fmt in [
        SampleFmt::S16,
        SampleFmt::S32,
        SampleFmt::S64,
        SampleFmt::S16P,
        SampleFmt::S32P,
        SampleFmt::S64P,
    ] {
        assert_eq!(fmt.kind(), SampleKind::Signed);
        assert!(!fmt.is_float());
    }
    for fmt in [
        SampleFmt::F32,
        SampleFmt::F64,
        SampleFmt::F32P,
        SampleFmt::F64P,
    ] {
        assert_eq!(fmt.kind(), SampleKind::Float);
        assert!(fmt.is_float());
    }
}

#[test]
fn widths_are_powers_of_two_and_match_the_depth_column() {
    for fmt in SampleFmt::ALL {
        let bytes = fmt.bytes_per_sample();
        assert!(bytes.is_power_of_two() && (1..=8).contains(&bytes));
        assert_eq!(fmt.bits_per_sample() as usize, bytes * 8);
    }
}

#[test]
fn accessors_are_const() {
    // Losing `const fn` here is a silent performance regression inside every
    // monomorphised resampling kernel, so make it a compile error instead.
    const BYTES: usize = SampleFmt::S16.bytes_per_sample();
    const PLANAR: bool = SampleFmt::F32P.is_planar();
    const NAME: &str = SampleFmt::F64.name();
    const PAIR: SampleFmt = SampleFmt::S32.to_planar();
    const SIZE: Option<usize> = SampleFmt::S16.plane_size(2, 1024);
    assert_eq!(BYTES, 2);
    const { assert!(PLANAR) };
    assert_eq!(NAME, "dbl");
    assert_eq!(PAIR, SampleFmt::S32P);
    assert_eq!(SIZE, Some(4096));
}

#[test]
fn plane_arithmetic_by_hand() {
    // Packed: one plane holding channels * samples * width.
    assert_eq!(SampleFmt::S16.plane_count(6), 1);
    assert_eq!(SampleFmt::S16.plane_size(6, 1024), Some(6 * 1024 * 2));
    assert_eq!(SampleFmt::S16.buffer_size(6, 1024).ok(), Some(6 * 1024 * 2));

    // Planar: one plane per channel, each holding samples * width.
    assert_eq!(SampleFmt::S16P.plane_count(6), 6);
    assert_eq!(SampleFmt::S16P.plane_size(6, 1024), Some(1024 * 2));
    assert_eq!(
        SampleFmt::S16P.buffer_size(6, 1024).ok(),
        Some(6 * 1024 * 2)
    );

    // Zero channels is representable (an unspecified layout can carry it) and
    // must not divide by zero or allocate a plane.
    assert_eq!(SampleFmt::F32P.plane_count(0), 0);
    assert_eq!(SampleFmt::F32.buffer_size(0, 1024).ok(), Some(0));
}

#[test]
fn oversized_frames_are_an_error_not_a_wrap() {
    let huge = u32::MAX;
    let r = SampleFmt::F64.buffer_size(huge, huge);
    if usize::BITS <= 64 {
        // 8 * 2^32 * 2^32 = 2^67, which does not fit a 64-bit usize either.
        assert!(r.is_err());
    }
    assert!(SampleFmt::F64P.plane_size(huge, huge).is_some());
}

fn arb_fmt() -> impl Strategy<Value = SampleFmt> {
    (0usize..SampleFmt::ALL.len()).prop_map(|i| SampleFmt::ALL[i])
}

proptest! {
    #[test]
    fn name_round_trips(fmt in arb_fmt()) {
        prop_assert_eq!(SampleFmt::from_name(fmt.name()).ok(), Some(fmt));
    }

    #[test]
    fn arbitrary_text_never_panics(s in ".{0,32}") {
        // Whatever it does, it returns; and if it succeeds the name it echoes
        // back must be the input, since there are no aliases.
        if let Ok(fmt) = SampleFmt::from_name(&s) {
            prop_assert_eq!(fmt.name(), s.as_str());
        }
    }

    #[test]
    fn buffer_size_is_planes_times_plane(
        fmt in arb_fmt(),
        channels in 0u32..64,
        samples in 0u32..65_536,
    ) {
        let plane = fmt.plane_size(channels, samples).expect("small enough");
        let planes = fmt.plane_count(channels).max(1) as usize;
        prop_assert_eq!(fmt.buffer_size(channels, samples).ok(), Some(plane * planes));
    }

    #[test]
    fn total_bytes_are_storage_independent(
        fmt in arb_fmt(),
        channels in 1u32..64,
        samples in 0u32..65_536,
    ) {
        // Packing does not change how much data there is, only where it sits.
        prop_assert_eq!(
            fmt.to_planar().buffer_size(channels, samples).ok(),
            fmt.to_packed().buffer_size(channels, samples).ok()
        );
    }
}
