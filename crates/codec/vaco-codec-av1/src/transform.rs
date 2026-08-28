//! Inverse transforms, AV1 spec §7.13, plus the axis dispatch §7.12.3's
//! reconstruct process needs to pick a `PlaneTxType` apart into a row kind,
//! a column kind, and the two flip flags.
//!
//! # `FLIPADST` is not a fourth transform kind
//!
//! Only three 1D transforms exist here — DCT, ADST, identity — because
//! §7.13.3's own dispatch table routes `FLIPADST_DCT`/`DCT_FLIPADST`/etc.
//! through the same "invoke the inverse ADST process" step as plain ADST;
//! the flip is applied afterwards, as an index reversal on the *finished*
//! 2D residual, by §7.12.3's reconstruct process (`flipUD`/`flipLR`). That
//! step lives in `crate::decode`, not here, since it operates on
//! `CurrFrame`, which this module never touches. [`Av1TxType::flip_ud`] and
//! [`Av1TxType::flip_lr`] expose exactly the two booleans that process needs.
//!
//! `Vaco-Spec-Ref: aom-av1-spec 7.12.3, 7.13 (inverse transform + reconstruct)`.

use crate::tables::Tx1D;

/// `PlaneTxType`, §3: which pair of 1D transforms (and which flips) apply to
/// a residual block. Ordinal values match the specification's own table in
/// §3 exactly (`DCT_DCT` = 0 through `H_FLIPADST` = 15), which is not load
/// bearing for this crate's own code but keeps a debug print recognisable
/// against the specification text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Av1TxType {
    DctDct = 0,
    AdstDct = 1,
    DctAdst = 2,
    AdstAdst = 3,
    FlipadstDct = 4,
    DctFlipadst = 5,
    FlipadstFlipadst = 6,
    AdstFlipadst = 7,
    FlipadstAdst = 8,
    Idtx = 9,
    VDct = 10,
    HDct = 11,
    VAdst = 12,
    HAdst = 13,
    VFlipadst = 14,
    HFlipadst = 15,
}

impl Av1TxType {
    /// From the raw 0..16 ordinal `compute_tx_type`/`transform_type` produce.
    /// Any out-of-range value (never produced by this crate's own callers)
    /// falls back to `DctDct` rather than panicking.
    #[must_use]
    pub const fn from_ordinal(v: u8) -> Self {
        match v {
            1 => Self::AdstDct,
            2 => Self::DctAdst,
            3 => Self::AdstAdst,
            4 => Self::FlipadstDct,
            5 => Self::DctFlipadst,
            6 => Self::FlipadstFlipadst,
            7 => Self::AdstFlipadst,
            8 => Self::FlipadstAdst,
            9 => Self::Idtx,
            10 => Self::VDct,
            11 => Self::HDct,
            12 => Self::VAdst,
            13 => Self::HAdst,
            14 => Self::VFlipadst,
            15 => Self::HFlipadst,
            _ => Self::DctDct,
        }
    }

    /// The row transform (applied along the width axis, `n = log2W`) —
    /// §7.13.3's row-transform dispatch, folding `FLIPADST` into `Adst`.
    #[must_use]
    pub const fn row_kind(self) -> Tx1D {
        match self {
            Self::DctDct | Self::AdstDct | Self::FlipadstDct | Self::HDct => Tx1D::Dct,
            Self::DctAdst
            | Self::AdstAdst
            | Self::DctFlipadst
            | Self::FlipadstFlipadst
            | Self::AdstFlipadst
            | Self::FlipadstAdst
            | Self::HAdst
            | Self::HFlipadst => Tx1D::Adst,
            Self::Idtx | Self::VDct | Self::VAdst | Self::VFlipadst => Tx1D::Identity,
        }
    }

    /// The column transform (applied along the height axis, `n = log2H`).
    #[must_use]
    pub const fn col_kind(self) -> Tx1D {
        match self {
            Self::DctDct | Self::DctAdst | Self::DctFlipadst | Self::VDct => Tx1D::Dct,
            Self::AdstDct
            | Self::AdstAdst
            | Self::FlipadstDct
            | Self::FlipadstFlipadst
            | Self::AdstFlipadst
            | Self::FlipadstAdst
            | Self::VAdst
            | Self::VFlipadst => Tx1D::Adst,
            Self::Idtx | Self::HDct | Self::HAdst | Self::HFlipadst => Tx1D::Identity,
        }
    }

    /// `flipUD`, §7.12.3.
    #[must_use]
    pub const fn flip_ud(self) -> bool {
        matches!(self, Self::FlipadstDct | Self::FlipadstAdst | Self::VFlipadst | Self::FlipadstFlipadst)
    }

    /// `flipLR`, §7.12.3.
    #[must_use]
    pub const fn flip_lr(self) -> bool {
        matches!(self, Self::DctFlipadst | Self::AdstFlipadst | Self::HFlipadst | Self::FlipadstFlipadst)
    }
}

/// `Cos128_Lookup[65]`, §7.13.2.1: `4096 * cos(angle * pi / 128)` rounded,
/// for `angle` in `0..=64`.
const COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

fn cos128(angle: i32) -> i64 {
    let angle2 = angle.rem_euclid(256);
    let v = if angle2 <= 64 {
        COS128_LOOKUP.get(usize::try_from(angle2).unwrap_or(0)).copied().unwrap_or(0)
    } else if angle2 <= 128 {
        -COS128_LOOKUP.get(usize::try_from(128 - angle2).unwrap_or(0)).copied().unwrap_or(0)
    } else if angle2 <= 192 {
        -COS128_LOOKUP.get(usize::try_from(angle2 - 128).unwrap_or(0)).copied().unwrap_or(0)
    } else {
        COS128_LOOKUP.get(usize::try_from(256 - angle2).unwrap_or(0)).copied().unwrap_or(0)
    };
    i64::from(v)
}

fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

fn round2(x: i64, n: u32) -> i64 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

fn clip3(low: i64, high: i64, x: i64) -> i64 {
    x.clamp(low, high)
}

/// `brev(numBits, x)`, §7.13.2.1.
const fn brev(num_bits: u32, x: u32) -> u32 {
    let mut t = 0u32;
    let mut i = 0u32;
    while i < num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
        i += 1;
    }
    t
}

fn get(t: &[i64], i: usize) -> i64 {
    t.get(i).copied().unwrap_or(0)
}

fn set(t: &mut [i64], i: usize, v: i64) {
    if let Some(slot) = t.get_mut(i) {
        *slot = v;
    }
}

/// `B(a, b, angle, flip, r)`, §7.13.2.1: a butterfly rotation (and, if
/// `flip`, an exchange of the two results).
#[allow(
    clippy::many_single_char_names,
    reason = "the specification's own B(a, b, angle, flip, r) argument names, kept for line-by-line comparison with \
              the spec text"
)]
fn butterfly(t: &mut [i64], a: usize, b: usize, angle: i32, flip: bool) {
    let ta = get(t, a);
    let tb = get(t, b);
    let x = ta * cos128(angle) - tb * sin128(angle);
    let y = ta * sin128(angle) + tb * cos128(angle);
    let (ra, rb) = (round2(x, 12), round2(y, 12));
    if flip {
        set(t, a, rb);
        set(t, b, ra);
    } else {
        set(t, a, ra);
        set(t, b, rb);
    }
}

/// `H(a, b, flip, r)`, §7.13.2.1: a Hadamard rotation, clamped to `r` bits.
#[allow(
    clippy::many_single_char_names,
    reason = "the specification's own H(a, b, flip, r) argument names"
)]
fn hadamard(t: &mut [i64], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = get(t, a);
    let y = get(t, b);
    let hi = (1i64 << (r - 1)) - 1;
    let lo = -(1i64 << (r - 1));
    set(t, a, clip3(lo, hi, x + y));
    set(t, b, clip3(lo, hi, x - y));
}

/// §7.13.2.2: the bit-reversal permutation required before [`idct`].
fn idct_permute(t: &mut [i64], n: u32) {
    let len = 1usize << n;
    let mut copy = [0i64; 64];
    for (i, slot) in copy.iter_mut().enumerate().take(len) {
        *slot = get(t, i);
    }
    for i in 0..len {
        let idx = usize::try_from(brev(n, u32::try_from(i).unwrap_or(0))).unwrap_or(0);
        set(t, i, copy.get(idx).copied().unwrap_or(0));
    }
}

/// §7.13.2.3: the inverse DCT of length `2^n`, `2 <= n <= 6`, transcribed as
/// its 31 ordered steps, each named after the step number in the
/// specification so a reviewer can match this function to the text line by
/// line rather than trusting a restructured equivalent.
#[allow(clippy::many_single_char_names, reason = "the specification's own loop variable names (i, j)")]
fn idct(t: &mut [i64], n: u32, r: u32) {
    idct_permute(t, n);
    if n == 6 {
        for i in 0..16 {
            butterfly(t, 32 + i, 63 - i, 63 - 4 * i32::try_from(brev(4, u32::try_from(i).unwrap_or(0))).unwrap_or(0), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            let a = i32::try_from(brev(3, 7 - u32::try_from(i).unwrap_or(0))).unwrap_or(0);
            butterfly(t, 16 + i, 31 - i, 6 + (a << 3), false);
        }
    }
    if n == 6 {
        for i in 0..16 {
            hadamard(t, 32 + i * 2, 33 + i * 2, i & 1 != 0, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            let a = i32::try_from(brev(2, 3 - u32::try_from(i).unwrap_or(0))).unwrap_or(0);
            butterfly(t, 8 + i, 15 - i, 12 + (a << 4), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hadamard(t, 16 + 2 * i, 17 + 2 * i, i & 1 != 0, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                let ii = i32::try_from(i).unwrap_or(0);
                let jj = i32::try_from(j).unwrap_or(0);
                let a = i32::try_from(brev(2, u32::try_from(i).unwrap_or(0))).unwrap_or(0);
                butterfly(t, 62 - i * 4 - j, 33 + i * 4 + j, 60 - 16 * a + 64 * jj, true);
                let _ = ii;
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            let ii = i32::try_from(i).unwrap_or(0);
            butterfly(t, 4 + i, 7 - i, 56 - 32 * ii, false);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hadamard(t, 8 + 2 * i, 9 + 2 * i, i & 1 != 0, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                let jj = i32::try_from(j).unwrap_or(0);
                let ii = i32::try_from(i).unwrap_or(0);
                butterfly(t, 30 - 4 * i - j, 17 + 4 * i + j, 24 + (jj << 6) + ((1 - ii) << 5), true);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 != 0, r);
            }
        }
    }
    for i in 0..2 {
        let ii = i32::try_from(i).unwrap_or(0);
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * ii, i != 1);
    }
    if n >= 3 {
        for i in 0..2 {
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i != 0, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            let ii = i32::try_from(i).unwrap_or(0);
            butterfly(t, 14 - i, 9 + i, 48 + 64 * ii, true);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 != 0, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                let ii = i32::try_from(i).unwrap_or(0);
                let jj = i32::try_from(j).unwrap_or(0);
                butterfly(t, 61 - i * 8 - j, 34 + i * 8 + j, 56 - ii * 32 + (jj >> 1) * 64, true);
            }
        }
    }
    for i in 0..2 {
        hadamard(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        butterfly(t, 6, 5, 32, true);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i != 0, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            let ii = i32::try_from(i).unwrap_or(0);
            butterfly(t, 29 - i, 18 + i, 48 + (ii >> 1) * 64, true);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 != 0, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hadamard(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 13 - i, 10 + i, 32, true);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i != 0, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hadamard(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 27 - i, 20 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hadamard(t, 32 + i, 47 - i, false, r);
            hadamard(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hadamard(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 55 - i, 40 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hadamard(t, i, 63 - i, false, r);
        }
    }
}

/// §7.13.2.4: the ADST input permutation, `3 <= n <= 4`.
fn adst_input_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i64; 16];
    for (i, slot) in copy.iter_mut().enumerate().take(n0) {
        *slot = get(t, i);
    }
    for i in 0..n0 {
        let idx = if i & 1 != 0 { i - 1 } else { n0 - i - 1 };
        set(t, i, copy.get(idx).copied().unwrap_or(0));
    }
}

/// §7.13.2.5: the ADST output permutation, `3 <= n <= 4`.
#[allow(
    clippy::many_single_char_names,
    reason = "the specification's own a/b/c/d bit-permutation variable names, section 7.13.2.5"
)]
fn adst_output_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i64; 16];
    for (i, slot) in copy.iter_mut().enumerate().take(n0) {
        *slot = get(t, i);
    }
    for i in 0..n0 {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        let v = copy.get(idx).copied().unwrap_or(0);
        set(t, i, if i & 1 != 0 { -v } else { v });
    }
}

const SINPI_1_9: i64 = 1321;
const SINPI_2_9: i64 = 2482;
const SINPI_3_9: i64 = 3344;
const SINPI_4_9: i64 = 3803;

/// §7.13.2.6: the inverse ADST4, a direct (non-butterfly) closed form.
fn adst4(t: &mut [i64]) {
    let t0 = get(t, 0);
    let t1 = get(t, 1);
    let t2 = get(t, 2);
    let t3 = get(t, 3);
    let mut s = [0i64; 7];
    s[0] = SINPI_1_9 * t0;
    s[1] = SINPI_2_9 * t0;
    s[2] = SINPI_3_9 * t1;
    s[3] = SINPI_4_9 * t2;
    s[4] = SINPI_1_9 * t2;
    s[5] = SINPI_2_9 * t3;
    s[6] = SINPI_4_9 * t3;
    let a7 = t0 - t2;
    let b7 = a7 + t3;
    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = SINPI_3_9 * b7;
    s[0] += s[5];
    s[1] -= s[6];
    let x0 = s[0] + s[3];
    let x1 = s[1] + s[3];
    let x2 = s[2];
    let x3 = (s[0] + s[1]) - s[3];
    set(t, 0, round2(x0, 12));
    set(t, 1, round2(x1, 12));
    set(t, 2, round2(x2, 12));
    set(t, 3, round2(x3, 12));
}

/// §7.13.2.7: the inverse ADST8.
fn adst8(t: &mut [i64], r: u32) {
    adst_input_permute(t, 3);
    for i in 0..4 {
        let ii = i32::try_from(i).unwrap_or(0);
        butterfly(t, 2 * i, 2 * i + 1, 60 - 16 * ii, true);
    }
    for i in 0..4 {
        hadamard(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        let ii = i32::try_from(i).unwrap_or(0);
        butterfly(t, 4 + 3 * i, 5 + i, 48 - 32 * ii, true);
    }
    for j in 0..2 {
        for i in 0..2 {
            hadamard(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_output_permute(t, 3);
}

/// §7.13.2.8: the inverse ADST16.
fn adst16(t: &mut [i64], r: u32) {
    adst_input_permute(t, 4);
    for i in 0..8 {
        let ii = i32::try_from(i).unwrap_or(0);
        butterfly(t, 2 * i, 2 * i + 1, 62 - 8 * ii, true);
    }
    for i in 0..8 {
        hadamard(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        let ii = i32::try_from(i).unwrap_or(0);
        butterfly(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * ii, true);
        butterfly(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * ii, true);
    }
    for j in 0..2 {
        for i in 0..4 {
            hadamard(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for j in 0..2 {
        for i in 0..2 {
            let ii = i32::try_from(i).unwrap_or(0);
            butterfly(t, 4 + 8 * j + 3 * i, 5 + 8 * j + i, 48 - 32 * ii, true);
        }
    }
    for j in 0..4 {
        for i in 0..2 {
            hadamard(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_output_permute(t, 4);
}

/// §7.13.2.9: dispatch by size.
fn adst(t: &mut [i64], n: u32, r: u32) {
    match n {
        2 => adst4(t),
        3 => adst8(t, r),
        _ => adst16(t, r),
    }
}

/// §7.13.2.10: the inverse Walsh-Hadamard transform (lossless coding only).
#[allow(
    clippy::many_single_char_names,
    reason = "the specification's own a/b/c/d/e variable names, section 7.13.2.10"
)]
fn iwht(t: &mut [i64], shift: u32) {
    let mut a = get(t, 0) >> shift;
    let mut c = get(t, 1) >> shift;
    let d0 = get(t, 2) >> shift;
    let mut b = get(t, 3) >> shift;
    a += c;
    let mut d = d0 - b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    set(t, 0, a);
    set(t, 1, b);
    set(t, 2, c);
    set(t, 3, d);
}

/// §7.13.2.11–§7.13.2.15: the size-dependent scaled identity transform.
fn identity(t: &mut [i64], n: u32) {
    match n {
        2 => {
            for i in 0..4 {
                set(t, i, round2(get(t, i) * 5793, 12));
            }
        }
        3 => {
            for i in 0..8 {
                set(t, i, get(t, i) * 2);
            }
        }
        4 => {
            for i in 0..16 {
                set(t, i, round2(get(t, i) * 11586, 12));
            }
        }
        _ => {
            for i in 0..32 {
                set(t, i, get(t, i) * 4);
            }
        }
    }
}

fn apply_1d(t: &mut [i64], kind: Tx1D, n: u32, r: u32, lossless: bool, lossless_shift: u32) {
    if lossless {
        iwht(t, lossless_shift);
        return;
    }
    match kind {
        Tx1D::Dct => idct(t, n, r),
        Tx1D::Adst => adst(t, n, r),
        Tx1D::Identity => identity(t, n),
    }
}

/// The 2D inverse transform, §7.13.3: dequantized coefficients (already
/// laid out row-major over the populated `th x tw` region — `tw = min(32,
/// w)`, `th = min(32, h)`, the specification's own "coefficients beyond 32
/// are zero" rule folded in by the caller never populating that region) to
/// a `w x h` residual, row-major.
///
/// `tx_width_log2`/`tx_height_log2` are `Tx_Width_Log2[txSz]`/
/// `Tx_Height_Log2[txSz]`; `bit_depth` feeds `rowClampRange`/`colClampRange`.
///
/// # Panics
/// Never: every array access is bounds-checked, and a `residual_out` shorter
/// than `w * h` simply receives a partial write.
pub fn inverse_transform_2d(
    tx_type: Av1TxType,
    tx_width_log2: u32,
    tx_height_log2: u32,
    lossless: bool,
    bit_depth: u8,
    dequant: &[i32],
    residual_out: &mut [i32],
) {
    let (log2_w, log2_h) = (tx_width_log2, tx_height_log2);
    let w = 1usize << log2_w;
    let h = 1usize << log2_h;
    let tw = w.min(32);

    let row_shift = if lossless { 0 } else { transform_row_shift(log2_w, log2_h) };
    let col_shift = if lossless { 0 } else { 4 };
    let row_clamp = u32::from(bit_depth) + 8;
    let col_clamp = (u32::from(bit_depth) + 6).max(16);
    let rescale = log2_w.abs_diff(log2_h) == 1;

    // Row transforms: one length-w 1D transform per row i, reading the
    // populated tw x th block of `dequant` (zero elsewhere) and writing an
    // intermediate w x h buffer.
    let mut residual = vec![0i64; w.saturating_mul(h)];
    let mut row_buf = [0i64; 64];
    let row_kind = tx_type.row_kind();
    for i in 0..h {
        for slot in row_buf.iter_mut().take(w) {
            *slot = 0;
        }
        if i < 32 {
            for j in 0..tw.min(w) {
                let v = dequant.get(i * tw + j).copied().unwrap_or(0);
                if let Some(slot) = row_buf.get_mut(j) {
                    *slot = i64::from(v);
                }
            }
        }
        if rescale {
            for slot in row_buf.iter_mut().take(w) {
                *slot = round2(*slot * 2896, 12);
            }
        }
        apply_1d(&mut row_buf, row_kind, log2_w, row_clamp, lossless, 2);
        for j in 0..w {
            if let Some(dst) = residual.get_mut(i * w + j) {
                *dst = round2(row_buf.get(j).copied().unwrap_or(0), row_shift);
            }
        }
    }

    let hi = (1i64 << (col_clamp - 1)) - 1;
    let lo = -(1i64 << (col_clamp - 1));
    for v in &mut residual {
        *v = clip3(lo, hi, *v);
    }

    // Column transforms: one length-h 1D transform per column j.
    let mut col_buf = [0i64; 64];
    let col_kind = tx_type.col_kind();
    for j in 0..w {
        for (i, slot) in col_buf.iter_mut().enumerate().take(h) {
            *slot = residual.get(i * w + j).copied().unwrap_or(0);
        }
        apply_1d(&mut col_buf, col_kind, log2_h, col_clamp, lossless, 0);
        for i in 0..h {
            let v = round2(col_buf.get(i).copied().unwrap_or(0), col_shift);
            if let Some(dst) = residual_out.get_mut(i * w + j) {
                *dst = i32::try_from(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0);
            }
        }
    }
}

/// `Transform_Row_Shift[txSz]`, §7.13.3, addressed by `(log2W, log2H)`
/// rather than a `TxSize` ordinal so [`inverse_transform_2d`] does not need
/// its own copy of the size table — every `(log2W, log2H)` pair AV1 defines
/// maps to exactly one row of the specification's table.
fn transform_row_shift(log2_w: u32, log2_h: u32) -> u32 {
    for i in 0..crate::tables::TX_SIZES_ALL {
        if u32::from(crate::tables::TX_WIDTH_LOG2.get(i).copied().unwrap_or(0)) == log2_w
            && u32::from(crate::tables::TX_HEIGHT_LOG2.get(i).copied().unwrap_or(0)) == log2_h
        {
            return u32::from(crate::tables::TRANSFORM_ROW_SHIFT.get(i).copied().unwrap_or(0));
        }
    }
    0
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    #[test]
    fn dc_only_dct_dct_reconstructs_a_uniform_block() {
        // A pure-DC coefficient must reconstruct a spatially flat residual:
        // any non-uniform block from a DC-only input is a real bug (wrong
        // butterfly wiring, wrong shift), not a rounding nuance -- the
        // property `crate`'s own "an oracle you wrote shares your
        // misreading" guidance asks for when the specification is the only
        // source: a fact the output must have, not a second transcription.
        for log2 in [2u32, 3, 4, 5] {
            let n = 1usize << log2;
            let mut dequant = vec![0i32; n * n];
            dequant[0] = 4096;
            let mut out = vec![0i32; n * n];
            inverse_transform_2d(Av1TxType::DctDct, log2, log2, false, 8, &dequant, &mut out);
            let first = out[0];
            assert!(first != 0, "DC-only input produced an all-zero residual at size {n}");
            for &v in &out {
                assert_eq!(v, first, "DC-only DCT_DCT must be spatially flat at size {n}: {out:?}");
            }
        }
    }

    #[test]
    fn identity_transform_does_not_spread_a_single_coefficient() {
        // Unlike DCT_DCT, IDTX performs no spatial mixing at all -- a single
        // nonzero coefficient at (0,0) must stay isolated at pixel (0,0),
        // not spread across the block the way a DC coefficient does under a
        // real frequency transform. Getting this backwards (treating IDTX
        // like a DC spreader) is exactly the kind of structured error the
        // project's own ship-bar guidance calls a real defect.
        let mut dequant = vec![0i32; 64];
        dequant[0] = 1000;
        let mut out = vec![0i32; 64];
        inverse_transform_2d(Av1TxType::Idtx, 3, 3, false, 8, &dequant, &mut out);
        assert_ne!(out[0], 0, "the one nonzero input must produce a nonzero (0,0) output");
        for (i, &v) in out.iter().enumerate().skip(1) {
            assert_eq!(v, 0, "IDTX must not mix position 0 into position {i}: {out:?}");
        }
    }

    #[test]
    fn cos128_matches_its_own_symmetry_requirements() {
        // cos128(0) == 4096 (angle 0), cos128(64) == 0 (angle pi/2),
        // cos128(128) == -4096 (angle pi) -- exact checkpoints the lookup
        // table's own four-branch dispatch must reproduce.
        assert_eq!(cos128(0), 4096);
        assert_eq!(cos128(64), 0);
        assert_eq!(cos128(128), -4096);
        assert_eq!(cos128(192), 0);
        assert_eq!(cos128(256), 4096);
    }

    #[test]
    fn every_transform_size_runs_without_panicking_on_arbitrary_coefficients() {
        for log2w in [2u32, 3, 4, 5, 6] {
            for log2h in [2u32, 3, 4, 5, 6] {
                if log2w.abs_diff(log2h) > 2 {
                    continue; // AV1 never pairs these; not a real input.
                }
                let w = 1usize << log2w;
                let h = 1usize << log2h;
                let tw = w.min(32);
                let th = h.min(32);
                let mut dequant = vec![0i32; tw * th];
                for (i, v) in dequant.iter_mut().enumerate() {
                    *v = i32::try_from(i).unwrap_or(0) * 37 - 500;
                }
                let mut out = vec![0i32; w * h];
                for tt in 0..16u8 {
                    inverse_transform_2d(Av1TxType::from_ordinal(tt), log2w, log2h, false, 8, &dequant, &mut out);
                }
            }
        }
    }

    #[test]
    fn lossless_wht_round_trips_a_dc_only_block_to_a_flat_residual() {
        let mut dequant = vec![0i32; 16];
        dequant[0] = 400;
        let mut out = vec![0i32; 16];
        inverse_transform_2d(Av1TxType::DctDct, 2, 2, true, 8, &dequant, &mut out);
        let first = out[0];
        assert!(out.iter().all(|&v| v == first), "lossless WHT DC-only must be flat: {out:?}");
    }
}
