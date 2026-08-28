//! VP9 Bitstream & Decoding Process Specification v0.6 §8.7 — the inverse
//! transform process: the butterfly-network inverse DCT (4/8/16/32-point),
//! the inverse ADST (4/8/16-point), the inverse Walsh-Hadamard transform
//! (lossless 4-point), and the 2-D row/column combination that ties them
//! together.
//!
//! # Why this lives here and not in `vaco-codec-vp9`
//!
//! Per this crate's own module doc: a pure function of already-dequantised
//! coefficients, with no codec-specific context, is exactly what this crate
//! exists to hold once (D19) rather than let a future AV1 decoder (which
//! shares VP9's DCT/ADST family almost exactly) re-derive independently.
//!
//! # Why everything is `i64`
//!
//! §8.7.1.1 requires the working array `T` to fit "8 + `BitDepth`" bits and a
//! second array `S` (used only by the ADST8/16 high-precision steps) to fit
//! "24 + `BitDepth`" bits. For 12-bit content that is up to 36 bits — past
//! `i32`. Rather than track two different integer widths (and risk a subtle
//! width-mismatch bug the way an under-sized accumulator would silently
//! wrap), every intermediate here is `i64`, which comfortably holds both
//! bounds for every bit depth VP9 defines (8/10/12). This is strictly wider
//! than the specification's own minimum, which only ever makes a conforming
//! computation *more* exact, never different — the bit-exact contract is
//! about the final values, not the internal register width.
//!
//! # Untrusted input
//!
//! Coefficients arrive from an entropy decoder fed attacker-controlled
//! bytes. Every function here is total: out-of-range angles wrap via `& 127`
//! exactly as the specification defines, and there is no operation in this
//! module that can panic on any `i64` input (`i64` arithmetic here never
//! approaches overflow even for `i32::MAX`-magnitude coefficients multiplied
//! by the largest table entry, `16384`, and summed across at most 32 terms).
//!
//! # Provenance
//!
//! Transcribed from VP9 Bitstream & Decoding Process Specification v0.6
//! §8.7 (`vp9-bitstream-spec-v0.6`), which expresses this process as ordered
//! algorithmic steps rather than prose — unavoidably the specification text
//! itself for this format (see `vaco-codec-vp8`'s doc for the identical
//! situation with RFC 6386 §16). `COS64_LOOKUP` is the one format-dictated
//! numeric table here (D7/D15).

/// §8.7.1.1's `cos64_lookup[33]`.
const COS64_LOOKUP: [i64; 33] = [
    16384, 16364, 16305, 16207, 16069, 15893, 15679, 15426, 15137, 14811, 14449, 14053, 13623,
    13160, 12665, 12140, 11585, 11003, 10394, 9760, 9102, 8423, 7723, 7005, 6270, 5520, 4756,
    3981, 3196, 2404, 1606, 804, 0,
];

/// §8.7.1.1's `cos64(angle)`.
fn cos64(angle: i32) -> i64 {
    let angle2 = angle & 127;
    let lookup = |i: i32| COS64_LOOKUP.get(i.max(0) as usize).copied().unwrap_or(0);
    if (0..=32).contains(&angle2) {
        lookup(angle2)
    } else if angle2 <= 64 {
        -lookup(64 - angle2)
    } else if angle2 <= 96 {
        -lookup(angle2 - 64)
    } else {
        lookup(128 - angle2)
    }
}

/// §8.7.1.1's `sin64(angle) = cos64(angle - 32)`.
fn sin64(angle: i32) -> i64 {
    cos64(angle - 32)
}

/// §8.7.1.1's `Round2(x, n)` for `n >= 1`.
fn round2(x: i64, n: u32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

fn get(t: &[i64], i: usize) -> i64 {
    t.get(i).copied().unwrap_or(0)
}

fn set(t: &mut [i64], i: usize, v: i64) {
    if let Some(slot) = t.get_mut(i) {
        *slot = v;
    }
}

/// §8.7.1.1's `brev(numBits, x)`: bit-reversal of the low `num_bits` bits of `x`.
fn brev(num_bits: u32, x: usize) -> usize {
    let mut t = 0usize;
    for i in 0..num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
    }
    t
}

/// §8.7.1.1's `B(a, b, angle, flip)` butterfly rotation (optionally flipped).
fn b(t: &mut [i64], a: usize, bb: usize, angle: i32, flip: bool) {
    let ta = get(t, a);
    let tb = get(t, bb);
    let x = ta * cos64(angle) - tb * sin64(angle);
    let y = ta * sin64(angle) + tb * cos64(angle);
    let (ra, rb) = (round2(x, 14), round2(y, 14));
    if flip {
        set(t, a, rb);
        set(t, bb, ra);
    } else {
        set(t, a, ra);
        set(t, bb, rb);
    }
}

/// §8.7.1.1's `H(a, b, flip)` Hadamard rotation.
fn h(t: &mut [i64], a: usize, bb: usize, flip: bool) {
    let (a, bb) = if flip { (bb, a) } else { (a, bb) };
    let x = get(t, a);
    let y = get(t, bb);
    set(t, a, x + y);
    set(t, bb, x - y);
}

/// §8.7.1.1's `SB(a, b, angle, flip)`: butterfly into the high-precision `S`
/// array, reading from `T`.
fn sb(t: &[i64], s: &mut [i64], a: usize, bb: usize, angle: i32, flip: bool) {
    let ta = get(t, a);
    let tb = get(t, bb);
    let sa = ta * cos64(angle) - tb * sin64(angle);
    let sb_ = ta * sin64(angle) + tb * cos64(angle);
    if flip {
        set(s, a, sb_);
        set(s, bb, sa);
    } else {
        set(s, a, sa);
        set(s, bb, sb_);
    }
}

/// §8.7.1.1's `SH(a, b)`: Hadamard rotation and rounding, `S` -> `T`.
fn sh(t: &mut [i64], s: &[i64], a: usize, bb: usize) {
    let sa = get(s, a);
    let sbv = get(s, bb);
    set(t, a, round2(sa + sbv, 14));
    set(t, bb, round2(sa - sbv, 14));
}

/// §8.7.1.2's inverse DCT array permutation: an in-place bit-reversal
/// permutation of `t[0..2^n]`.
fn idct_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i64> = (0..n0).map(|i| get(t, i)).collect();
    for i in 0..n0 {
        set(t, i, copy.get(brev(n, i)).copied().unwrap_or(0));
    }
}

/// §8.7.1.3's inverse DCT process on the already-permuted array `t[0..2^n]`,
/// `2 <= n <= 5`.
#[allow(clippy::many_single_char_names, reason = "mirrors the spec's own n0/n1/n2/n3 names")]
fn idct(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let n2 = if n >= 2 { 1usize << (n - 2) } else { 0 };
    let n3 = if n >= 3 { 1usize << (n - 3) } else { 0 };

    // 1. Base case or recurse.
    if n == 2 {
        b(t, 0, 1, 16, true);
    } else {
        idct(t, n - 1);
    }

    // 2.
    for i in 0..n2 {
        let angle = 32 - i32::try_from(brev(5, n1 + i)).unwrap_or(0);
        b(t, n1 + i, n0 - 1 - i, angle, false);
    }

    // 3.
    if n >= 3 {
        for i in 0..n3 {
            for j in 0..2 {
                h(t, n1 + 4 * i + 2 * j, n1 + 1 + 4 * i + 2 * j, j == 1);
            }
        }
    }

    // 4.
    if n == 5 {
        for i in 0..2 {
            for j in 0..2 {
                let a = n0 - n as usize + 3 - n2 * j - 4 * i;
                let bb = n1 + n as usize - 4 + n2 * j + 4 * i;
                let angle = 28 - 16 * i32::try_from(i).unwrap_or(0) + 56 * i32::try_from(j).unwrap_or(0);
                b(t, a, bb, angle, true);
            }
        }
        for i in 0..2 {
            for j in 0..4 {
                let a = n1 + n3 * j + i;
                let bb = n1 + n2 - 5 + n3 * j - i;
                h(t, a, bb, j & 1 == 1);
            }
        }
    }

    // 5.
    if n >= 4 {
        let imax = usize::from(n == 5);
        for i in 0..=imax {
            for j in 0..2 {
                let a = n0 - n as usize + 2 - i - n2 * j;
                let bb = n1 + n as usize - 3 + i + n2 * j;
                let angle = 24 + 48 * i32::try_from(j).unwrap_or(0);
                b(t, a, bb, angle, true);
            }
        }
        let imax2 = if n == 5 { 3usize } else { 1usize };
        for i in 0..=imax2 {
            for j in 0..2 {
                let a = n1 + n2 * j + i;
                let bb = n1 + n2 - 1 + n2 * j - i;
                h(t, a, bb, j & 1 == 1);
            }
        }
    }

    // 6.
    if n >= 3 {
        for i in 0..n3 {
            b(t, n0 - n3 - 1 - i, n1 + n3 + i, 16, true);
        }
    }

    // 7.
    for i in 0..n1 {
        h(t, i, n0 - 1 - i, false);
    }
}

/// §8.7.1.4's inverse ADST input array permutation on `t[0..2^n]`.
fn adst_input_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let n1 = 1usize << (n - 1);
    let copy: Vec<i64> = (0..n0).map(|i| get(t, i)).collect();
    for i in 0..n1 {
        set(t, 2 * i, copy.get(n0 - 1 - 2 * i).copied().unwrap_or(0));
        set(t, 2 * i + 1, copy.get(2 * i).copied().unwrap_or(0));
    }
}

/// §8.7.1.5's inverse ADST output array permutation on `t[0..2^n]`, `n` in `{3,4}`.
fn adst_output_permute(t: &mut [i64], n: u32) {
    let copy: Vec<i64> = (0..(1usize << n)).map(|i| get(t, i)).collect();
    let at = |idx: usize| copy.get(idx).copied().unwrap_or(0);
    if n == 4 {
        for a in 0..2usize {
            for bb in 0..2usize {
                for c in 0..2usize {
                    for d in 0..2usize {
                        let dst = 8 * a + 4 * bb + 2 * c + d;
                        let src = 8 * (d ^ c) + 4 * (c ^ bb) + 2 * (bb ^ a) + a;
                        set(t, dst, at(src));
                    }
                }
            }
        }
    } else {
        for a in 0..2usize {
            for bb in 0..2usize {
                for c in 0..2usize {
                    let dst = 4 * a + 2 * bb + c;
                    let src = 4 * (c ^ bb) + 2 * (bb ^ a) + a;
                    set(t, dst, at(src));
                }
            }
        }
    }
}

const SINPI_1_9: i64 = 5283;
const SINPI_2_9: i64 = 9929;
const SINPI_3_9: i64 = 13377;
const SINPI_4_9: i64 = 15212;

/// §8.7.1.6's inverse ADST4 process on `t[0..4]`.
fn iadst4(t: &mut [i64]) {
    let t0 = get(t, 0);
    let t1 = get(t, 1);
    let t2 = get(t, 2);
    let t3 = get(t, 3);
    let s0 = SINPI_1_9 * t0;
    let s1 = SINPI_2_9 * t0;
    let s2 = SINPI_3_9 * t1;
    let s3 = SINPI_4_9 * t2;
    let s4 = SINPI_1_9 * t2;
    let s5 = SINPI_2_9 * t3;
    let s6 = SINPI_4_9 * t3;
    let v = t0 - t2 + t3;
    let s7 = SINPI_3_9 * v;
    let x0 = s0 + s3 + s5;
    let x1 = s1 - s4 - s6;
    let x2 = s7;
    let x3 = s2;
    let o0 = x0 + x3;
    let o1 = x1 + x3;
    let o2 = x2;
    let o3 = x0 + x1 - x3;
    set(t, 0, round2(o0, 14));
    set(t, 1, round2(o1, 14));
    set(t, 2, round2(o2, 14));
    set(t, 3, round2(o3, 14));
}

/// §8.7.1.7's inverse ADST8 process on `t[0..8]`, using a high-precision
/// scratch array for the intermediate `SB`/`SH` steps.
fn iadst8(t: &mut [i64]) {
    adst_input_permute(t, 3);
    let mut s = [0i64; 8];
    for i in 0..4 {
        sb(t, &mut s, 2 * i, 1 + 2 * i, 30 - 8 * i32::try_from(i).unwrap_or(0), true);
    }
    for i in 0..4 {
        sh(t, &s, i, 4 + i);
    }
    for i in 0..2 {
        sb(t, &mut s, 4 + 3 * i, 5 + i, 24 - 16 * i32::try_from(i).unwrap_or(0), true);
    }
    for i in 0..2 {
        sh(t, &s, 4 + i, 6 + i);
    }
    for i in 0..2 {
        h(t, i, 2 + i, false);
    }
    for i in 0..2 {
        b(t, 2 + 4 * i, 3 + 4 * i, 16, true);
    }
    adst_output_permute(t, 3);
    for i in 0..4 {
        set(t, 1 + 2 * i, -get(t, 1 + 2 * i));
    }
}

/// §8.7.1.8's inverse ADST16 process on `t[0..16]`.
fn iadst16(t: &mut [i64]) {
    adst_input_permute(t, 4);
    let mut s = [0i64; 16];
    for i in 0..8 {
        sb(t, &mut s, 2 * i, 1 + 2 * i, 31 - 4 * i32::try_from(i).unwrap_or(0), true);
    }
    for i in 0..8 {
        sh(t, &s, i, 8 + i);
    }
    for i in 0..4 {
        sb(t, &mut s, 8 + 2 * i, 9 + 2 * i, 28 - 16 * i32::try_from(i).unwrap_or(0), true);
    }
    for i in 0..4 {
        sh(t, &s, 8 + i, 12 + i);
    }
    for i in 0..4 {
        h(t, i, 4 + i, false);
    }
    for i in 0..2 {
        for j in 0..2 {
            sb(
                t,
                &mut s,
                4 + 8 * i + 3 * j,
                5 + 8 * i + j,
                24 - 16 * i32::try_from(j).unwrap_or(0),
                true,
            );
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            sh(t, &s, 4 + 8 * j + i, 6 + 8 * j + i);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            h(t, 8 * j + i, 2 + 8 * j + i, false);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            let angle = 48 + 64 * i32::try_from(i ^ j).unwrap_or(0);
            b(t, 2 + 4 * j + 8 * i, 3 + 4 * j + 8 * i, angle, false);
        }
    }
    adst_output_permute(t, 4);
    for i in 0..2 {
        for j in 0..2 {
            let idx = 1 + 12 * j + 2 * i;
            set(t, idx, -get(t, idx));
        }
    }
}

/// §8.7.1.9's inverse ADST dispatch, `2 <= n <= 4`.
fn iadst(t: &mut [i64], n: u32) {
    match n {
        2 => iadst4(t),
        3 => iadst8(t),
        _ => iadst16(t),
    }
}

/// §8.7.1.10's inverse Walsh-Hadamard transform on `t[0..4]` (lossless).
#[allow(clippy::many_single_char_names, reason = "mirrors the spec's own a/b/c/d/e names")]
fn iwht4(t: &mut [i64], shift: u32) {
    let mut a = get(t, 0) >> shift;
    let mut c = get(t, 1) >> shift;
    let mut d = get(t, 2) >> shift;
    let mut b_ = get(t, 3) >> shift;
    a += c;
    d -= b_;
    let e = (a - d) >> 1;
    b_ = e - b_;
    c = e - c;
    a -= b_;
    d += c;
    set(t, 0, a);
    set(t, 1, b_);
    set(t, 2, c);
    set(t, 3, d);
}

/// The four `TxType` values §8.7.2 dispatches on — named
/// `{column-transform}_{row-transform}` per the specification's own naming
/// (`ADST_DCT`: rows use DCT, columns use ADST — confirmed against both the
/// §3 constants table's prose *and* the §8.7.2 process text, which
/// disagree in naming intuition but agree numerically).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxType {
    DctDct,
    AdstDct,
    DctAdst,
    AdstAdst,
}

impl TxType {
    fn row_is_adst(self) -> bool {
        matches!(self, TxType::DctAdst | TxType::AdstAdst)
    }
    fn col_is_adst(self) -> bool {
        matches!(self, TxType::AdstDct | TxType::AdstAdst)
    }
}

/// §8.7.2's 2D inverse transform, applied in place to `dequant`, a
/// row-major `2^n x 2^n` array (`dequant.len() == (1 << n) * (1 << n)`, `2
/// <= n <= 5`). `lossless` selects the Walsh-Hadamard path (`tx_type` is
/// ignored in that case, matching the specification).
pub fn inverse_transform_2d(dequant: &mut [i64], n: u32, tx_type: TxType, lossless: bool) {
    let n0 = 1usize << n;
    let mut t = [0i64; 32];
    let row = t.get_mut(..n0).unwrap_or(&mut []);

    // Row transforms.
    for i in 0..n0 {
        for j in 0..n0 {
            set(row, j, get(dequant, i * n0 + j));
        }
        if lossless {
            iwht4(row, 2);
        } else if tx_type.row_is_adst() {
            iadst(row, n);
        } else {
            idct_permute(row, n);
            idct(row, n);
        }
        for j in 0..n0 {
            set(dequant, i * n0 + j, get(row, j));
        }
    }

    // Column transforms.
    let col = t.get_mut(..n0).unwrap_or(&mut []);
    let out_shift = (n + 2).min(6); // Min(6, n + 2), per §8.7.2.
    for j in 0..n0 {
        for i in 0..n0 {
            set(col, i, get(dequant, i * n0 + j));
        }
        if lossless {
            iwht4(col, 0);
        } else if tx_type.col_is_adst() {
            iadst(col, n);
        } else {
            idct_permute(col, n);
            idct(col, n);
        }
        for i in 0..n0 {
            let v = get(col, i);
            let out = if lossless { v } else { round2(v, out_shift) };
            set(dequant, i * n0 + j, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_dct_produces_a_flat_block() {
        // A pure-DC 4x4 DCT input should reconstruct to (near-)constant
        // output: every output sample equal, since a DC basis function is
        // literally the constant function.
        let mut d = [0i64; 16];
        d[0] = 400;
        inverse_transform_2d(&mut d, 2, TxType::DctDct, false);
        let first = d[0];
        assert!(d.iter().all(|&v| v == first), "{d:?}");
    }

    #[test]
    fn all_zero_input_is_all_zero_output() {
        for n in 2..=5u32 {
            let size = (1usize << n) * (1usize << n);
            let mut d = vec![0i64; size];
            inverse_transform_2d(&mut d, n, TxType::DctDct, false);
            assert!(d.iter().all(|&v| v == 0));
            let mut d2 = vec![0i64; size];
            inverse_transform_2d(&mut d2, n, TxType::AdstAdst, false);
            assert!(d2.iter().all(|&v| v == 0));
        }
    }

    #[test]
    fn wht_all_zero_is_all_zero() {
        let mut d = [0i64; 16];
        inverse_transform_2d(&mut d, 2, TxType::DctDct, true);
        assert_eq!(d, [0i64; 16]);
    }

    #[test]
    fn every_size_and_type_runs_without_panicking_on_extreme_input() {
        for n in 2..=5u32 {
            let size = (1usize << n) * (1usize << n);
            for tx_type in [TxType::DctDct, TxType::AdstDct, TxType::DctAdst, TxType::AdstAdst] {
                let mut d = vec![i64::from(i32::MAX); size];
                inverse_transform_2d(&mut d, n, tx_type, false);
                let mut d2 = vec![i64::from(i32::MIN); size];
                inverse_transform_2d(&mut d2, n, tx_type, false);
            }
        }
    }

    #[test]
    fn cos64_matches_known_values() {
        assert_eq!(cos64(0), 16384);
        assert_eq!(cos64(32), 0);
        assert_eq!(cos64(64), -16384);
        assert_eq!(sin64(0), cos64(-32));
    }

    #[test]
    fn brev_reverses_bits() {
        assert_eq!(brev(3, 0b001), 0b100);
        assert_eq!(brev(3, 0b110), 0b011);
        assert_eq!(brev(5, 0), 0);
    }
}
