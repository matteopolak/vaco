//! Properties that must hold for every input, not just the fixtures.
//!
//! Three of the four here are round trips — the shape plan 19's brief singles
//! out for `proptest` — and the fourth is the geometry invariant the fuzzer also
//! asserts, stated here where it is cheap to shrink a counterexample.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::{ChromaFormat, NalUnitType, ProfileTier, Sps, Window, codec_parameters};

proptest! {
    /// Every six-bit `nal_unit_type` survives the newtype, and the derived
    /// predicates agree with Table 7-1's ranges.
    #[test]
    fn nal_unit_types_round_trip_and_classify(v in 0u8..64) {
        let t = NalUnitType::from_u8(v);
        prop_assert_eq!(t.get(), v);
        prop_assert_eq!(t.is_vcl(), v < 32);
        prop_assert_eq!(t.is_irap(), (16..=23).contains(&v));
        prop_assert_eq!(t.is_idr(), v == 19 || v == 20);
        prop_assert_eq!(t.is_bla(), (16..=18).contains(&v));
        prop_assert_eq!(t.is_sei(), v == 39 || v == 40);
        // An IDR, a BLA and a CRA are all IRAPs, and no VCL unit above 23 is.
        prop_assert!(!t.is_idr() || t.is_irap());
        prop_assert!(!t.is_bla() || t.is_irap());
        prop_assert!(!t.is_cra() || t.is_irap());
        // Every IRAP is a VCL unit.
        prop_assert!(!t.is_irap() || t.is_vcl());
        // Nothing is both a parameter set and a VCL unit.
        prop_assert!(!(t.is_parameter_set() && t.is_vcl()));
    }

    /// The 48 constraint bits `hvcC` carries round-trip through the accessor and
    /// its inverse, for every value the field can hold.
    ///
    /// This is the property that lets an `hvcC` and the SPS it carries be
    /// compared without a second code path, and the test that would catch a
    /// one-bit shift in either direction.
    #[test]
    fn the_hvcc_constraint_block_round_trips(bits in 0u64..(1u64 << 48)) {
        let pt = ProfileTier::default().with_constraint_indicator_flags(bits);
        prop_assert_eq!(pt.constraint_indicator_flags(), bits);
    }

    /// The effective-profile rule is total: it never panics, and it agrees with
    /// its own definition for every `(profile_idc, compatibility_flags)` pair.
    #[test]
    fn the_effective_profile_is_total(idc in 0u8..32, flags in any::<u32>()) {
        let pt = ProfileTier {
            profile_idc: idc,
            compatibility_flags: flags,
            ..ProfileTier::default()
        };
        let effective = pt.effective_profile_idc();
        if idc != 0 {
            prop_assert_eq!(effective, idc);
        } else if flags == 0 {
            prop_assert_eq!(effective, 0);
        } else {
            // The lowest set flag, and nothing below it is set.
            prop_assert!(pt.compatible_with(effective));
            for j in 0..effective {
                prop_assert!(!pt.compatible_with(j));
            }
        }
        // `claims_profile` is reflexive on whichever answer came out.
        prop_assert!(pt.claims_profile(effective) || (idc == 0 && flags == 0));
    }

    /// The conformance window can shrink a picture and can reject it, but it can
    /// never *grow* one — and a zero dimension never escapes.
    ///
    /// The offsets are in chroma units, so the arithmetic scales by `SubWidthC`
    /// and `SubHeightC`; getting that scaling wrong in the direction that
    /// under-subtracts would show up here as a reported size larger than the
    /// coded one.
    #[test]
    fn the_conformance_window_never_grows_the_picture(
        width_cbs in 1u32..512,
        height_cbs in 1u32..512,
        left in 0u32..64,
        right in 0u32..64,
        top in 0u32..64,
        bottom in 0u32..64,
        chroma in 0u32..4,
    ) {
        let mut sps = Sps {
            chroma_format: ChromaFormat::from_idc(chroma).unwrap(),
            // A multiple of MinCbSizeY, which the parser requires.
            pic_width_in_luma_samples: width_cbs * 8,
            pic_height_in_luma_samples: height_cbs * 8,
            log2_min_cb_size: 3,
            log2_diff_max_min_cb_size: 3,
            conformance_window: Some(Window { left, right, top, bottom }),
            ..Sps::default()
        };
        if let Some((w, h)) = sps.dimensions() {
            prop_assert!(w > 0 && h > 0, "a zero dimension escaped");
            prop_assert!(w <= sps.coded_width(), "the window widened the picture");
            prop_assert!(h <= sps.coded_height(), "the window heightened it");
        } else {
            // Rejected, which is only correct if the window really does remove
            // at least the whole picture on one axis.
            let c = sps.chroma_array_type();
            let dx = (left + right) * c.sub_width_c();
            let dy = (top + bottom) * c.sub_height_c();
            prop_assert!(
                dx >= sps.coded_width() || dy >= sps.coded_height(),
                "a usable window was rejected"
            );
        }
        // With no window at all the two sizes are equal, always.
        sps.conformance_window = None;
        prop_assert_eq!(
            sps.dimensions(),
            Some((sps.coded_width(), sps.coded_height()))
        );
    }

    /// Arbitrary bytes never panic any of the parameter-set parsers, and
    /// whatever comes out of an SPS is self-consistent.
    ///
    /// The fuzzer covers this far more thoroughly; the value of having it here
    /// too is that `proptest` shrinks, so a regression arrives as a
    /// twelve-byte counterexample rather than a corpus file.
    #[test]
    fn arbitrary_bytes_never_panic(
        data in proptest::collection::vec(
            prop_oneof![
                3 => Just(0u8),
                2 => Just(0xFFu8),
                8 => any::<u8>(),
            ],
            0..256usize,
        ),
        header in prop_oneof![Just(0x40u8), Just(0x42), Just(0x44), Just(0x4e), Just(0x28)],
    ) {
        let mut nal = vec![header, 0x01];
        nal.extend_from_slice(&data);
        let mut budget = Budget::new(Limits::strict());
        let _ = vaco_parse_hevc::Vps::parse(&nal, &mut budget);
        let _ = vaco_parse_hevc::Pps::parse(&nal, &mut budget);
        let _ = vaco_parse_hevc::sei::parse(&nal, None, &mut budget);
        let _ = vaco_parse_hevc::HevcDecoderConfigurationRecord::parse(&nal, &mut budget);
        if let Ok(sps) = Sps::parse(&nal, &mut budget) {
            if let Some((w, h)) = sps.dimensions() {
                prop_assert!(w > 0 && h > 0);
                prop_assert!(w <= sps.coded_width());
                prop_assert!(h <= sps.coded_height());
            }
            let params = codec_parameters(&sps);
            let v = params.video.as_ref().unwrap();
            // A reported frame rate is either undefined or strictly positive;
            // a zero denominator would divide by zero downstream.
            prop_assert!(v.frame_rate.is_undefined() || v.frame_rate.den > 0);
            prop_assert!(
                v.sample_aspect_ratio.is_undefined() || v.sample_aspect_ratio.den > 0
            );
        }
    }
}
