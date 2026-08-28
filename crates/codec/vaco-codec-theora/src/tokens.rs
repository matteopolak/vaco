//! DCT token decode (`Vaco-Spec-Ref: theora-spec-20170603 section 7.7`):
//! EOB run tokens (7.7.1) and coefficient tokens (7.7.2), both operating on
//! one block's 64-entry zig-zag-order coefficient buffer.

use vaco_bitstream::BitReader;

/// Section 2.6, Figure 2.8: the zig-zag scan position of each natural
/// (row-major) coefficient index. The same table appears, independently
/// derived, in JPEG (Annex A) and MPEG video — this is Theora's own copy of
/// it, transcribed directly from the spec's figure.
pub(crate) const NATURAL_TO_ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, //
    2, 4, 7, 13, 16, 26, 29, 42, //
    3, 8, 12, 17, 25, 30, 41, 43, //
    9, 11, 18, 24, 31, 40, 44, 53, //
    10, 19, 23, 32, 39, 45, 52, 54, //
    20, 22, 33, 38, 46, 51, 55, 60, //
    21, 34, 37, 47, 50, 56, 59, 61, //
    35, 36, 48, 49, 57, 58, 62, 63,
];

/// Expand an EOB token (value `0..=6`) into a run length (section 7.7.1,
/// steps 1-7). `remaining` is the number of coded blocks whose token index
/// is still below 64 *including the current block*, needed only for token
/// 6's "zero means all remaining blocks" special case.
pub(crate) fn expand_eob_run(token: u8, r: &mut BitReader<'_>, remaining: u32) -> u32 {
    match token {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => r.get(2) + 4,
        4 => r.get(3) + 8,
        5 => r.get(4) + 16,
        _ => {
            let v = r.get(12);
            if v == 0 { remaining.max(1) } else { v }
        }
    }
}

/// The result of decoding one coefficient token (section 7.7.2).
pub(crate) struct CoeffToken {
    /// New value of `TIS[bi]` — the token index just past the coefficients
    /// (and any zero run) this token wrote.
    pub new_ti: u32,
    /// New value of `NCOEFFS[bi]`, or `None` for a pure zero run (tokens 7
    /// and 8), which the spec deliberately does not count (section 7.7.3's
    /// note on mimicking VP3's accounting).
    pub new_ncoeffs: Option<u32>,
}

/// Decode one coefficient token (`TOKEN` in `7..=31`) into `coeffs`, a
/// block's 64-entry zig-zag-order buffer, starting at zig-zag index `ti`.
///
/// Every token but a handful of leading zeros followed by one signed value;
/// `coeffs` slots the token does not touch are left as they were (the
/// frame-decode loop pre-zeroes each block once, so this never needs to zero
/// anything it is not explicitly told to by the token itself).
#[allow(
    clippy::too_many_lines,
    reason = "one 25-way match transcribed directly from the spec's Table 7.38; splitting it up would separate cases that read as a single lookup table"
)]
pub(crate) fn decode_coeff_token(
    token: u8,
    r: &mut BitReader<'_>,
    ti: u32,
    coeffs: &mut [i32; 64],
) -> CoeffToken {
    let set = |coeffs: &mut [i32; 64], idx: u32, v: i32| {
        if let Some(slot) = coeffs.get_mut(idx as usize) {
            *slot = v;
        }
    };
    match token {
        7 => {
            let rlen = r.get(3) + 1;
            CoeffToken {
                new_ti: ti + rlen,
                new_ncoeffs: None,
            }
        }
        8 => {
            let rlen = r.get(6) + 1;
            CoeffToken {
                new_ti: ti + rlen,
                new_ncoeffs: None,
            }
        }
        9..=12 => {
            let v = match token {
                9 => 1,
                10 => -1,
                11 => 2,
                _ => -2,
            };
            set(coeffs, ti, v);
            CoeffToken {
                new_ti: ti + 1,
                new_ncoeffs: Some(ti + 1),
            }
        }
        13..=16 => {
            let base = match token {
                13 => 3,
                14 => 4,
                15 => 5,
                _ => 6,
            };
            let sign = r.get(1);
            let v = if sign == 0 { base } else { -base };
            set(coeffs, ti, v);
            CoeffToken {
                new_ti: ti + 1,
                new_ncoeffs: Some(ti + 1),
            }
        }
        17..=22 => {
            let (extra_bits, offset) = match token {
                17 => (1, 7),
                18 => (2, 9),
                19 => (3, 13),
                20 => (4, 21),
                21 => (5, 37),
                _ => (9, 69),
            };
            let sign = r.get(1);
            let mag = i32::try_from(r.get(extra_bits)).unwrap_or(i32::MAX).saturating_add(offset);
            set(coeffs, ti, if sign == 0 { mag } else { -mag });
            CoeffToken {
                new_ti: ti + 1,
                new_ncoeffs: Some(ti + 1),
            }
        }
        23..=27 => {
            // One zero followed by +-1, with `zeros` zeros in front (token
            // 23 = 1 zero, ..., token 27 = 5 zeros).
            let zeros = u32::from(token - 23) + 1;
            for z in 0..zeros {
                set(coeffs, ti + z, 0);
            }
            let sign = r.get(1);
            let v = if sign == 0 { 1 } else { -1 };
            set(coeffs, ti + zeros, v);
            let new_ti = ti + zeros + 1;
            CoeffToken {
                new_ti,
                new_ncoeffs: Some(new_ti),
            }
        }
        28 | 29 => {
            let (extra_bits, offset) = if token == 28 { (2, 6) } else { (3, 10) };
            let sign = r.get(1);
            let rlen = r.get(extra_bits) + offset;
            for z in 0..rlen {
                set(coeffs, ti + z, 0);
            }
            let v = if sign == 0 { 1 } else { -1 };
            set(coeffs, ti + rlen, v);
            let new_ti = ti + rlen + 1;
            CoeffToken {
                new_ti,
                new_ncoeffs: Some(new_ti),
            }
        }
        30 => {
            let sign = r.get(1);
            let mag = i32::try_from(r.get(1)).unwrap_or(i32::MAX).saturating_add(2);
            set(coeffs, ti, 0);
            set(coeffs, ti + 1, if sign == 0 { mag } else { -mag });
            let new_ti = ti + 2;
            CoeffToken {
                new_ti,
                new_ncoeffs: Some(new_ti),
            }
        }
        _ => {
            // token 31
            let sign = r.get(1);
            let mag = i32::try_from(r.get(1)).unwrap_or(i32::MAX).saturating_add(2);
            let rlen = r.get(1) + 2;
            for z in 0..rlen {
                set(coeffs, ti + z, 0);
            }
            set(coeffs, ti + rlen, if sign == 0 { mag } else { -mag });
            let new_ti = ti + rlen + 1;
            CoeffToken {
                new_ti,
                new_ncoeffs: Some(new_ti),
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn token_9_is_a_single_plus_one() {
        let mut coeffs = [0i32; 64];
        let mut r = BitReader::new(&[]);
        let out = decode_coeff_token(9, &mut r, 3, &mut coeffs);
        assert_eq!(coeffs[3], 1);
        assert_eq!(out.new_ti, 4);
        assert_eq!(out.new_ncoeffs, Some(4));
    }

    #[test]
    fn token_7_short_zero_run_does_not_update_ncoeffs() {
        // 3 extra bits = 0b010 => RLEN = 2+1 = 3.
        let mut coeffs = [5i32; 64];
        let mut r = BitReader::new(&[0b0100_0000]);
        let out = decode_coeff_token(7, &mut r, 0, &mut coeffs);
        assert_eq!(out.new_ti, 3);
        assert_eq!(out.new_ncoeffs, None);
    }

    #[test]
    fn token_23_is_one_zero_then_signed_one() {
        let mut coeffs = [9i32; 64];
        let mut r = BitReader::new(&[0b1000_0000]); // sign=1 => -1
        let out = decode_coeff_token(23, &mut r, 2, &mut coeffs);
        assert_eq!(coeffs[2], 0);
        assert_eq!(coeffs[3], -1);
        assert_eq!(out.new_ti, 4);
        assert_eq!(out.new_ncoeffs, Some(4));
    }

    #[test]
    fn zigzag_table_is_a_permutation_of_0_to_63() {
        let mut seen = [false; 64];
        for &z in &NATURAL_TO_ZIGZAG {
            assert!(!seen[z], "duplicate zig-zag index {z}");
            seen[z] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }
}
