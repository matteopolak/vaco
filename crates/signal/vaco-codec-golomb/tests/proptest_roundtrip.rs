//! Round-trip and invariant properties.
//!
//! The spec tests in `spec.rs` pin the coding to the standard at specific
//! points. These cover the space between those points: every code must survive
//! a write/read cycle, every cost function must agree with the writer, and no
//! input at all may panic or fail to terminate.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use proptest::prelude::*;
use vaco_bitstream::{BitReader, BitWriter, GolombRead};
use vaco_codec_golomb::{
    ChromaArrayType, GolombDecode, GolombEncode, MbPartPredMode, cbp_code_num_count,
    cbp_from_code_num, code_num_from_cbp, map,
};

proptest! {
    #[test]
    fn ue_v_round_trips(v in 0u32..u32::MAX) {
        let mut w = BitWriter::new();
        w.put_ue_v(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.ue_v(), v);
        prop_assert!(!r.overrun());
    }

    #[test]
    fn se_v_round_trips(v in -(i32::MAX)..=i32::MAX) {
        let mut w = BitWriter::new();
        w.put_se_v(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.se_v(), v);
        prop_assert!(!r.overrun());
    }

    #[test]
    fn ue_v64_round_trips(v in 0u64..(1u64 << 62)) {
        let mut w = BitWriter::new();
        w.put_ue_v64(v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.ue_v64(), v);
        prop_assert!(!r.overrun());
    }

    /// Order-k round-trip, over the values the matching reader can actually
    /// return — `lz + k <= 32`, which `ue_k_bit_len` expresses as a length cap.
    #[test]
    fn ue_k_round_trips(v in 0u32..(1u32 << 24), k in 0u32..8) {
        prop_assume!(map::ue_k_bit_len(v, k) <= 63);
        let mut w = BitWriter::new();
        w.put_ue_k(k, v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.ue_k(k), v);
        prop_assert!(!r.overrun());
    }

    #[test]
    fn se_k_round_trips(v in -(1i32 << 20)..(1i32 << 20), k in 0u32..8) {
        prop_assume!(map::se_k_bit_len(v, k) <= 63);
        let mut w = BitWriter::new();
        w.put_se_k(k, v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.se_k(k), v);
        prop_assert!(!r.overrun());
    }

    #[test]
    fn te_v_round_trips(c_max in 0u32..1024, v in 0u32..1024) {
        prop_assume!(v <= c_max);
        let mut w = BitWriter::new();
        w.put_te_v(c_max, v);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.te_v(c_max), v);
    }

    /// The signed mapping is a bijection on the range it claims.
    #[test]
    fn signed_mapping_is_a_bijection(k in 0u32..(u32::MAX - 1)) {
        let v = map::se_value(k);
        prop_assert_eq!(map::se_code_num(v), k);
    }

    #[test]
    fn signed_mapping_inverse(v in -(i32::MAX)..=i32::MAX) {
        let k = map::se_code_num(v);
        prop_assert_eq!(map::se_value(k), v);
    }

    /// Cost functions must agree with what the writer actually emits — a
    /// rate-distortion loop that mispredicts its own output is worse than one
    /// that does not exist.
    #[test]
    fn bit_len_agrees_with_the_writer(v in 0u32..(u32::MAX - 1)) {
        let mut w = BitWriter::new();
        w.put_ue_v(v);
        prop_assert_eq!(w.bit_len(), u64::from(map::ue_bit_len(v)));
    }

    #[test]
    fn ue_k_bit_len_agrees_with_the_writer(v in 0u32..(1u32 << 24), k in 0u32..8) {
        prop_assume!(map::ue_k_bit_len(v, k) <= 63);
        let mut w = BitWriter::new();
        w.put_ue_k(k, v);
        prop_assert_eq!(w.bit_len(), u64::from(map::ue_k_bit_len(v, k)));
    }

    #[test]
    fn ue_k_bit_len_order_zero_is_ue_bit_len(v in 0u32..u32::MAX) {
        prop_assert_eq!(map::ue_k_bit_len(v, 0), map::ue_bit_len(v));
    }

    /// Table 9-4 is invertible for every code number in range.
    #[test]
    fn me_v_round_trips(code_num in 0u32..48, inter: bool, chroma_48: bool) {
        let chroma = if chroma_48 { ChromaArrayType::WithChroma } else { ChromaArrayType::Monochrome };
        let pred = if inter { MbPartPredMode::Inter } else { MbPartPredMode::Intra };
        prop_assume!(code_num < cbp_code_num_count(chroma));

        let cbp = cbp_from_code_num(code_num, chroma, pred).unwrap();
        prop_assert_eq!(code_num_from_cbp(cbp, chroma, pred), Some(code_num));

        let mut w = BitWriter::new();
        prop_assert!(w.put_me_v(chroma, pred, cbp));
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        prop_assert_eq!(r.me_v(chroma, pred), cbp);
        prop_assert!(!r.overrun());
    }

    /// Every read over arbitrary bytes must terminate, must not panic, and must
    /// either consume bits or flag the reader. A read that does neither is the
    /// shape of an infinite loop.
    #[test]
    fn arbitrary_bytes_always_progress(data: Vec<u8>, k in 0u32..34, c_max: u32) {
        let mut r = BitReader::new(&data);
        for _ in 0..64 {
            let before = (r.bit_pos(), r.overrun());
            let _ = r.ue_v();
            let _ = r.se_v();
            let _ = r.te_v(c_max);
            let _ = r.ue_k(k);
            let _ = r.ue_v64();
            let _ = r.me_v(ChromaArrayType::WithChroma, MbPartPredMode::Inter);
            let after = (r.bit_pos(), r.overrun());
            prop_assert!(after != before || after.1, "no progress and no flag");
            if after.1 {
                break;
            }
        }
    }

    /// The two `ue(v)` implementations must agree bit for bit on any input,
    /// valid or not.
    #[test]
    fn differential_against_vaco_bitstream(data: Vec<u8>) {
        let mut a = BitReader::new(&data);
        let mut b = BitReader::new(&data);
        for _ in 0..32 {
            prop_assert_eq!(GolombDecode::ue_v(&mut a), GolombRead::ue(&mut b));
            prop_assert_eq!(a.bit_pos(), b.bit_pos());
            prop_assert_eq!(a.overrun(), b.overrun());
        }
    }

    /// Bounded reads never panic and always report, whatever the input.
    #[test]
    fn bounded_reads_are_total(data: Vec<u8>, max: u32, lo: i32, hi: i32) {
        use vaco_codec_golomb::BoundedGolomb;
        use vaco_limits::{Budget, Limits};

        let mut reader = BitReader::new(&data);
        let mut budget = Budget::new(Limits::tiny());
        let mut g = BoundedGolomb::new(&mut reader, &mut budget);
        for _ in 0..32 {
            let _ = g.ue_v(max);
            let _ = g.se_v(lo, hi);
            let _ = g.te_v(max);
            let _ = g.ue_k(3, max);
            let _ = g.u(17);
        }
    }
}
