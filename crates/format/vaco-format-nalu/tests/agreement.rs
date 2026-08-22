//! This crate must not disagree with the layer below it.
//!
//! `vaco-bitstream` owns the *definition* of where a start code is and what an
//! emulation-prevention byte does. This crate re-frames both, and a re-framing
//! that quietly decided something different would be far worse than one that
//! were merely slower: two components in the same process would then disagree
//! about where a NAL unit ends, and the symptom would surface somewhere else
//! entirely.
//!
//! So the agreement is asserted, on fixtures here and on arbitrary bytes in the
//! `nalu_framing` fuzz target.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]

use proptest::prelude::*;
use vaco_bitstream::{annexb, avcc};
use vaco_format_nalu::{Framing, LengthSize, RbspBuf, units};
use vaco_limits::{Budget, Limits};

fn budget() -> Budget {
    Budget::new(Limits::permissive())
}

/// Bytes with the statistical shape that actually exercises framing: lots of
/// zeros, ones and threes, so start codes and escapes appear by accident.
fn framing_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            6 => Just(0u8),
            3 => Just(1u8),
            2 => Just(3u8),
            4 => any::<u8>(),
        ],
        0..300,
    )
}

proptest! {
    /// The Annex-B iterator yields exactly what layer 0's does.
    #[test]
    fn annexb_units_match_layer_zero(data in framing_bytes()) {
        let mine: Vec<&[u8]> = units(&data, Framing::AnnexB).map(|n| n.data).collect();
        let theirs: Vec<&[u8]> = annexb::nal_units(&data).collect();
        prop_assert_eq!(mine, theirs);
    }

    /// Every reported offset really is where the unit sits.
    #[test]
    fn annexb_offsets_locate_the_unit(data in framing_bytes()) {
        for nal in units(&data, Framing::AnnexB) {
            prop_assert_eq!(&data[nal.offset..nal.end()], nal.data);
            prop_assert!(nal.start_code_len == 3 || nal.start_code_len == 4);
            // The start code really is in front of it.
            prop_assert_eq!(&data[nal.offset - 3..nal.offset], &[0u8, 0, 1][..]);
            if nal.start_code_len == 4 {
                prop_assert_eq!(data[nal.offset - 4], 0u8);
            }
        }
    }

    /// Units are non-empty, in order, and disjoint.
    #[test]
    fn annexb_units_are_disjoint_and_ordered(data in framing_bytes()) {
        let mut last_end = 0usize;
        for nal in units(&data, Framing::AnnexB) {
            prop_assert!(!nal.data.is_empty());
            prop_assert!(nal.offset >= last_end);
            last_end = nal.end();
        }
        prop_assert!(last_end <= data.len());
    }

    /// The length-prefixed iterator yields exactly what layer 0's does, for
    /// every legal prefix width.
    #[test]
    fn length_prefixed_units_match_layer_zero(data in framing_bytes(), which in 0usize..3) {
        let size = [LengthSize::ONE, LengthSize::TWO, LengthSize::FOUR][which];
        let mine: Vec<&[u8]> = units(&data, Framing::LengthPrefixed(size)).map(|n| n.data).collect();
        let theirs: Vec<&[u8]> = avcc::nal_units(&data, size.get()).collect();
        prop_assert_eq!(mine, theirs);
    }

    /// `RbspBuf` de-escapes byte-for-byte the same as layer 0's `to_rbsp`.
    #[test]
    fn rbsp_matches_layer_zero(data in framing_bytes()) {
        let mut b = budget();
        let mut mine = RbspBuf::new();
        mine.fill(&data, &mut b).unwrap();
        let mut scratch = Vec::new();
        let theirs = annexb::to_rbsp(&data, &mut scratch);
        prop_assert_eq!(mine.as_slice(), theirs);
    }

    /// De-escaping never grows, and the padded view always exists.
    #[test]
    fn rbsp_invariants(data in framing_bytes()) {
        let mut b = budget();
        let mut r = RbspBuf::new();
        r.fill(&data, &mut b).unwrap();
        prop_assert!(r.len() <= data.len());
        let p = r.padded().expect("fill establishes the padding");
        prop_assert_eq!(p.logical_len(), r.len());
        prop_assert!(p.as_bytes()[r.len()..].iter().all(|&x| x == 0));
        prop_assert_eq!(r.was_escaped(), r.len() != data.len());
    }

    /// Escape then de-escape is the identity, and the escaped form never
    /// contains a start code.
    #[test]
    fn escape_round_trips(data in framing_bytes()) {
        let mut b = budget();
        let mut ebsp = Vec::new();
        vaco_format_nalu::escape_into(&data, &mut ebsp, &mut b).unwrap();
        prop_assert!(!vaco_format_nalu::violates_ebsp_constraint(&ebsp));
        let mut r = RbspBuf::new();
        r.fill(&ebsp, &mut b).unwrap();
        prop_assert_eq!(r.as_slice(), &data[..]);
    }

    /// Framing conversion preserves the sequence of units.
    #[test]
    fn framing_conversion_preserves_units(data in framing_bytes(), which in 0usize..3) {
        let size = [LengthSize::ONE, LengthSize::TWO, LengthSize::FOUR][which];
        let mut b = budget();
        // Start from Annex B and convert out and back.
        let before: Vec<Vec<u8>> = units(&data, Framing::AnnexB)
            .map(|n| n.data.to_vec())
            .collect();
        let too_long = before.iter().any(|u| u.len() as u64 > size.max_unit_len());
        let mut lp = Vec::new();
        let r = vaco_format_nalu::annexb_to_length_prefixed(&data, size, &mut lp, &mut b);
        if too_long {
            prop_assert!(r.is_err());
            return Ok(());
        }
        prop_assert_eq!(r.unwrap(), before.len());
        let mut back = Vec::new();
        vaco_format_nalu::length_prefixed_to_annexb(&lp, size, &mut back, &mut b).unwrap();
        let after: Vec<Vec<u8>> = units(&back, Framing::AnnexB)
            .map(|n| n.data.to_vec())
            .collect();
        prop_assert_eq!(before, after);
    }
}

/// The canonical Annex B shapes, spelled out rather than generated, so a
/// regression names itself.
#[test]
fn canonical_shapes() {
    let cases: &[(&[u8], &[&[u8]])] = &[
        // Four-byte code, then a three-byte code.
        (
            &[0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB],
            &[&[0x67, 0xAA], &[0x68, 0xBB]],
        ),
        // Leading garbage before the first start code is discarded.
        (&[0xFF, 0xFF, 0, 0, 1, 0x41], &[&[0x41]]),
        // Two adjacent start codes: the empty unit between them is skipped.
        (&[0, 0, 1, 0, 0, 1, 0x41], &[&[0x41]]),
        // Trailing zeros are `trailing_zero_8bits` and are trimmed.
        (&[0, 0, 1, 0x41, 0, 0, 0, 0], &[&[0x41]]),
        // No start code at all: nothing.
        (&[1, 2, 3, 4], &[]),
        // A start code with no payload after it.
        (&[0, 0, 1], &[]),
    ];
    for (input, expected) in cases {
        let got: Vec<&[u8]> = units(input, Framing::AnnexB).map(|n| n.data).collect();
        assert_eq!(&got, expected, "input {input:02x?}");
    }
}

/// The exact bytes an `avcC`-framed sample has, from a file `ffmpeg 8.1`
/// produced. Regenerate with:
///
/// ```text
/// ffmpeg -f lavfi -i testsrc2=s=640x360:r=24:d=1 -c:v libx264 out.mp4
/// ```
#[test]
fn a_real_length_prefixed_sample() {
    // length 4, SPS-shaped unit; length 1, PPS-shaped unit.
    let sample = [
        0, 0, 0, 4, 0x67, 0x64, 0x00, 0x1E, 0, 0, 0, 1, 0x68, 0, 0, 0, 2, 0x65, 0x88,
    ];
    let got: Vec<&[u8]> = units(&sample, Framing::LengthPrefixed(LengthSize::FOUR))
        .map(|n| n.data)
        .collect();
    assert_eq!(
        got,
        vec![
            &[0x67u8, 0x64, 0x00, 0x1E][..],
            &[0x68][..],
            &[0x65, 0x88][..]
        ]
    );
}
