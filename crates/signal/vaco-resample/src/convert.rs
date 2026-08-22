//! Sample-format conversion: the numeric contract, and the kernels that hold it.
//!
//! # The factorisation
//!
//! Twelve formats give 144 ordered pairs. Implementing 144 kernels is what the
//! reference does and what we explicitly do not (§B.3.1):
//!
//! ```text
//! conversion(src → dst) = gather? ∘ element_convert ∘ scatter?
//! ```
//!
//! — thirty element converters (six types square, minus the diagonal) plus one
//! strided walk. Packed ↔ planar is a stride change, not a separate kernel.
//!
//! # The numeric contract
//!
//! Every rule below is a **measurement** of `FFmpeg` 8.1, recorded in
//! `docs/signal/vaco-resample.md` §Provenance with the exact command. Where
//! plan 17 §B.3.2 predicted something different, the measurement wins (D17).
//!
//! | Direction | Rule | Plan 17 said |
//! |---|---|---|
//! | integer → wider integer | `x << (m − n)` | same |
//! | integer → narrower integer | `x >> (n − m)`, **arithmetic shift, no rounding** | round-half-up — **wrong** |
//! | `u8` ↔ signed | offset binary, bias 128 | same |
//! | integer → float | `x / 2^(n−1)` | same |
//! | **`f32` → `s16`** | **`floor(x·32768 + 0.5)`** — half toward +∞ | half-away-from-zero — **wrong** |
//! | every other float → integer | `round_ties_even(x · 2^(m−1))` | half-away-from-zero — **wrong** |
//!
//! ## The `f32 → s16` asymmetry is real, and it is not a function of the value
//!
//! See [`F32_TO_S16_TAIL_DIVERGENCE`].

// Every integer division in this module has a divisor that is a
// `bytes_per_sample()` (1, 2, 4 or 8) or a channel count already checked
// non-zero by the buffer constructors.
#![allow(
    clippy::integer_division,
    reason = "divisors are sample widths or channel counts already proven non-zero"
)]

use vaco_core::Error;
use vaco_sampfmt::SampleFmt;

use crate::buf::{AudioMut, AudioRef};

// ---------------------------------------------------------------------------
// The element type lattice
// ---------------------------------------------------------------------------

/// One of the six numeric element types, independent of planar/packed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum Elem {
    U8,
    S16,
    S32,
    S64,
    F32,
    F64,
}

impl Elem {
    /// The element type of a [`SampleFmt`].
    #[must_use]
    pub const fn of(fmt: SampleFmt) -> Self {
        match fmt {
            SampleFmt::U8 | SampleFmt::U8P => Self::U8,
            SampleFmt::S16 | SampleFmt::S16P => Self::S16,
            SampleFmt::S32 | SampleFmt::S32P => Self::S32,
            SampleFmt::S64 | SampleFmt::S64P => Self::S64,
            SampleFmt::F32 | SampleFmt::F32P => Self::F32,
            SampleFmt::F64 | SampleFmt::F64P => Self::F64,
        }
    }

    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::S16 => 2,
            Self::S32 | Self::F32 => 4,
            Self::S64 | Self::F64 => 8,
        }
    }

    /// Significant bits, for choosing an internal working precision.
    #[must_use]
    pub const fn precision_bits(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::S16 => 16,
            Self::S32 => 32,
            Self::S64 => 64,
            // f32 carries 24 bits of mantissa; f64 carries 53.
            Self::F32 => 24,
            Self::F64 => 53,
        }
    }
}

/// The reference's `f32 → s16` rounding depends on where the sample sits in the
/// buffer, and we deliberately do not reproduce that.
///
/// # What was measured
///
/// Feeding 65 536 exact half-LSB ties (`(k + 0.5)/32768` for every
/// `k ∈ [−32768, 32767)`) through
/// `ffmpeg -f f32le -ar 48000 -ac 1 -i - -af aresample=out_sample_fmt=s16 -f s16le -`
/// gives `floor(q + 0.5)` for **every** one — round half toward +∞, not
/// half-away-from-zero and not ties-to-even.
///
/// But truncating the *same* input to 7, 31, 37 or 127 samples changes the
/// answer for some of them. Cross-referencing which indices move against which
/// stay put identifies the rule exactly: the reference processes whole blocks of
/// **16 samples** in a vector kernel that rounds half-up, and the trailing
/// `len % 16` samples in a scalar kernel that rounds ties-to-even.
///
/// ```text
/// len=801  ties at 4,12,20,…  all half-up   (800 = 50·16 vectorised, index 800 is not a tie)
/// len=31   tie at 20 → ties-even            (16 vectorised, 17..30 scalar)
/// len=37   tie at 36 → ties-even            (32 vectorised, 33..36 scalar)
/// len=7    tie at  4 → ties-even            (nothing vectorised)
/// ```
///
/// # Why we do not reproduce it
///
/// Reproducing it would make our output a function of how the caller chunked the
/// stream. Chunk-invariance — the same stream fed in one call and in many small
/// ones producing byte-identical output — is this crate's central contract
/// (§B.11) and the single highest-value test it has. A rounding mode that
/// depends on a SIMD block boundary is incompatible with it.
///
/// So we round **half-up unconditionally**, which is what the reference produces
/// for every sample in a full 16-block: at least 15 of every 16 samples, and all
/// of them whenever the caller's buffer length is a multiple of 16. The
/// divergence is confined to exact half-LSB ties in a trailing partial block,
/// where it is one LSB.
///
/// This is the D17.1 shape: not "matching would be awkward", but "matching would
/// require abandoning a stronger guarantee".
pub const F32_TO_S16_TAIL_DIVERGENCE: &str = "f32->s16 ties in the trailing len%16 samples: reference rounds ties-to-even there \
     and half-up elsewhere; we round half-up everywhere to keep chunk-invariance";

// ---------------------------------------------------------------------------
// Element conversion — the thirty converters, factored into four families
// ---------------------------------------------------------------------------

/// Scalar element converters. Public because they are the crate's numeric
/// contract and every kernel here is differentially tested against them.
pub mod elem {
    // --- integer -> integer: shifts only, never a multiply ------------------
    //
    // MEASURED: narrowing is an arithmetic right shift. `s32 -> s16` of
    // -32768 gives -1, of 32767 gives 0, of -65536 gives -1 — floor division by
    // 2^16, not round-half-up. Plan 17 §B.3.2 is wrong about this.

    #[must_use]
    pub const fn u8_to_i16(x: u8) -> i16 {
        ((x as i16) - 128) << 8
    }
    #[must_use]
    pub const fn u8_to_i32(x: u8) -> i32 {
        ((x as i32) - 128) << 24
    }
    #[must_use]
    pub const fn u8_to_i64(x: u8) -> i64 {
        ((x as i64) - 128) << 56
    }
    #[must_use]
    pub const fn i16_to_u8(x: i16) -> u8 {
        (((x >> 8) + 128) & 0xff) as u8
    }
    #[must_use]
    pub const fn i16_to_i32(x: i16) -> i32 {
        (x as i32) << 16
    }
    #[must_use]
    pub const fn i16_to_i64(x: i16) -> i64 {
        (x as i64) << 48
    }
    #[must_use]
    pub const fn i32_to_u8(x: i32) -> u8 {
        (((x >> 24) + 128) & 0xff) as u8
    }
    #[must_use]
    pub const fn i32_to_i16(x: i32) -> i16 {
        (x >> 16) as i16
    }
    #[must_use]
    pub const fn i32_to_i64(x: i32) -> i64 {
        (x as i64) << 32
    }
    #[must_use]
    pub const fn i64_to_u8(x: i64) -> u8 {
        (((x >> 56) + 128) & 0xff) as u8
    }
    #[must_use]
    pub const fn i64_to_i16(x: i64) -> i16 {
        (x >> 48) as i16
    }
    #[must_use]
    pub const fn i64_to_i32(x: i64) -> i32 {
        (x >> 32) as i32
    }

    // --- integer -> float: scale by a negative power of two ----------------
    //
    // Dividing by 2^(n-1) rather than 2^(n-1)-1 means full-scale negative maps
    // to exactly -1.0 and full-scale positive to 32767/32768. MEASURED:
    // `s16 -> flt` of -32768 gives -1.0 and of 32767 gives 0.999969482421875.

    pub(crate) const S16_SCALE: f64 = 1.0 / 32768.0;
    pub(crate) const S32_SCALE: f64 = 1.0 / 2_147_483_648.0;
    pub(crate) const S64_SCALE: f64 = 1.0 / 9_223_372_036_854_775_808.0;
    pub(crate) const U8_SCALE: f64 = 1.0 / 128.0;

    #[must_use]
    pub fn u8_to_f32(x: u8) -> f32 {
        (f32::from(x) - 128.0) * (U8_SCALE as f32)
    }
    #[must_use]
    pub fn u8_to_f64(x: u8) -> f64 {
        (f64::from(x) - 128.0) * U8_SCALE
    }
    #[must_use]
    pub fn i16_to_f32(x: i16) -> f32 {
        f32::from(x) * (S16_SCALE as f32)
    }
    #[must_use]
    pub fn i16_to_f64(x: i16) -> f64 {
        f64::from(x) * S16_SCALE
    }
    #[must_use]
    pub fn i32_to_f32(x: i32) -> f32 {
        // The i32 -> f32 conversion itself rounds; the multiply is exact.
        // MEASURED: i32::MAX maps to exactly 1.0.
        (x as f32) * (S32_SCALE as f32)
    }
    #[must_use]
    pub fn i32_to_f64(x: i32) -> f64 {
        f64::from(x) * S32_SCALE
    }
    #[must_use]
    pub fn i64_to_f32(x: i64) -> f32 {
        (x as f32) * (S64_SCALE as f32)
    }
    #[must_use]
    pub fn i64_to_f64(x: i64) -> f64 {
        (x as f64) * S64_SCALE
    }

    // --- float -> integer --------------------------------------------------
    //
    // Two independent decisions, both measured:
    //
    // 1. ROUNDING. `f32 -> s16` rounds half toward +infinity. Every other
    //    float-to-integer pair rounds ties-to-even. See
    //    `super::F32_TO_S16_TAIL_DIVERGENCE` for why the first is not a
    //    function of the value alone and what we do about it.
    //
    // 2. OVERFLOW. For `u8` and `s16` targets the reference's clip helper takes
    //    a C `int`, so an out-of-range float saturates to `i64` range, wraps
    //    into `i32`, and only then clamps. MEASURED: `flt -> s16` of `1e30`
    //    gives -1, of `-1e30` gives 0, of `inf` gives -1, of `NaN` gives 0 —
    //    and of `2.0` gives 32767, which is a clamp. Nothing but that two-step
    //    reproduces all four. For `s32` the helper takes an `int64_t`, so it
    //    clamps outright: MEASURED `flt -> s32` of `1e30` gives i32::MAX.
    //
    // `f32 as i64` in Rust is a saturating conversion with NaN mapping to 0,
    // which is exactly the C library's `llrintf` behaviour on this target;
    // `i64 as i32` truncates. So the emulation is two casts, not a branch.

    #[must_use]
    pub fn f32_to_i16(x: f32) -> i16 {
        clip_i16((x * 32768.0 + 0.5).floor() as i64)
    }
    #[must_use]
    pub fn f64_to_i16(x: f64) -> i16 {
        clip_i16((x * 32768.0).round_ties_even() as i64)
    }
    #[must_use]
    pub fn f32_to_u8(x: f32) -> u8 {
        clip_u8((x * 128.0).round_ties_even() as i64)
    }
    #[must_use]
    pub fn f64_to_u8(x: f64) -> u8 {
        clip_u8((x * 128.0).round_ties_even() as i64)
    }
    #[must_use]
    pub fn f32_to_i32(x: f32) -> i32 {
        clip_i32((x * 2_147_483_648.0).round_ties_even() as i64)
    }
    #[must_use]
    pub fn f64_to_i32(x: f64) -> i32 {
        clip_i32((x * 2_147_483_648.0).round_ties_even() as i64)
    }
    /// `s64` output is **unprobed**: the reference has no `s64le` raw muxer, so
    /// there is no direct entry point to measure through. Defined by symmetry
    /// with `s32` and marked as such in the fidelity table.
    #[must_use]
    pub fn f32_to_i64(x: f32) -> i64 {
        (x * 9_223_372_036_854_775_808.0).round_ties_even() as i64
    }
    #[must_use]
    pub fn f64_to_i64(x: f64) -> i64 {
        (x * 9_223_372_036_854_775_808.0).round_ties_even() as i64
    }

    /// Saturate to `i64`, truncate to `i32`, then clamp — see the note above.
    #[must_use]
    pub const fn clip_i16(wide: i64) -> i16 {
        let narrow = wide as i32;
        if narrow < -32768 {
            -32768
        } else if narrow > 32767 {
            32767
        } else {
            narrow as i16
        }
    }

    #[must_use]
    pub const fn clip_u8(wide: i64) -> u8 {
        let narrow = wide as i32;
        let clamped = if narrow < -128 {
            -128
        } else if narrow > 127 {
            127
        } else {
            narrow
        };
        (clamped + 128) as u8
    }

    /// `s32` clamps from the full `i64`, with no intermediate truncation.
    #[must_use]
    pub const fn clip_i32(wide: i64) -> i32 {
        if wide < i32::MIN as i64 {
            i32::MIN
        } else if wide > i32::MAX as i64 {
            i32::MAX
        } else {
            wide as i32
        }
    }
}

// ---------------------------------------------------------------------------
// The internal working type
// ---------------------------------------------------------------------------

/// A float type the crate can work in internally.
///
/// Implemented for `f32` and `f64` only. Every method is an element converter
/// from [`elem`], so the internal path and the direct path cannot disagree —
/// they call the same function.
pub trait Internal: Copy + Default + PartialOrd + core::fmt::Debug + Send + Sync + 'static {
    const ZERO: Self;
    const ONE: Self;

    fn from_f64(v: f64) -> Self;
    fn to_f64(self) -> f64;
    #[must_use]
    fn add(self, other: Self) -> Self;
    #[must_use]
    fn sub(self, other: Self) -> Self;
    #[must_use]
    fn mul(self, other: Self) -> Self;

    /// The convolution kernel.
    ///
    /// **Eight** accumulators, not four, and not one. `benches/resample.rs`
    /// compares all three at 32, 50 and 256 taps in both precisions; see
    /// [`crate::rate::kernel`] for the numbers and why the plan's rule does not
    /// hold here.
    fn dot(x: &[Self], h: &[Self]) -> Self {
        crate::rate::kernel::dot8(x, h)
    }

    fn of_u8(x: u8) -> Self;
    fn of_i16(x: i16) -> Self;
    fn of_i32(x: i32) -> Self;
    fn of_i64(x: i64) -> Self;
    fn of_f32(x: f32) -> Self;
    fn of_f64(x: f64) -> Self;

    fn into_u8(self) -> u8;
    fn into_i16(self) -> i16;
    fn into_i32(self) -> i32;
    fn into_i64(self) -> i64;
    fn into_f32(self) -> f32;
    fn into_f64(self) -> f64;

    /// Which element type this is, for the "internal format equals endpoint
    /// format" no-op check.
    const ELEM: Elem;
}

impl Internal for f32 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    const ELEM: Elem = Elem::F32;

    fn from_f64(v: f64) -> Self {
        v as Self
    }
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn sub(self, other: Self) -> Self {
        self - other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }

    fn of_u8(x: u8) -> Self {
        elem::u8_to_f32(x)
    }
    fn of_i16(x: i16) -> Self {
        elem::i16_to_f32(x)
    }
    fn of_i32(x: i32) -> Self {
        elem::i32_to_f32(x)
    }
    fn of_i64(x: i64) -> Self {
        elem::i64_to_f32(x)
    }
    fn of_f32(x: f32) -> Self {
        x
    }
    fn of_f64(x: f64) -> Self {
        x as Self
    }

    fn into_u8(self) -> u8 {
        elem::f32_to_u8(self)
    }
    fn into_i16(self) -> i16 {
        elem::f32_to_i16(self)
    }
    fn into_i32(self) -> i32 {
        elem::f32_to_i32(self)
    }
    fn into_i64(self) -> i64 {
        elem::f32_to_i64(self)
    }
    fn into_f32(self) -> f32 {
        self
    }
    fn into_f64(self) -> f64 {
        f64::from(self)
    }
}

impl Internal for f64 {
    const ZERO: Self = 0.0;
    const ONE: Self = 1.0;
    const ELEM: Elem = Elem::F64;

    fn from_f64(v: f64) -> Self {
        v
    }
    fn to_f64(self) -> f64 {
        self
    }
    fn add(self, other: Self) -> Self {
        self + other
    }
    fn sub(self, other: Self) -> Self {
        self - other
    }
    fn mul(self, other: Self) -> Self {
        self * other
    }

    fn of_u8(x: u8) -> Self {
        elem::u8_to_f64(x)
    }
    fn of_i16(x: i16) -> Self {
        elem::i16_to_f64(x)
    }
    fn of_i32(x: i32) -> Self {
        elem::i32_to_f64(x)
    }
    fn of_i64(x: i64) -> Self {
        elem::i64_to_f64(x)
    }
    fn of_f32(x: f32) -> Self {
        Self::from(x)
    }
    fn of_f64(x: f64) -> Self {
        x
    }

    fn into_u8(self) -> u8 {
        elem::f64_to_u8(self)
    }
    fn into_i16(self) -> i16 {
        elem::f64_to_i16(self)
    }
    fn into_i32(self) -> i32 {
        elem::f64_to_i32(self)
    }
    fn into_i64(self) -> i64 {
        elem::f64_to_i64(self)
    }
    fn into_f32(self) -> f32 {
        self as f32
    }
    fn into_f64(self) -> f64 {
        self
    }
}

// ---------------------------------------------------------------------------
// Strided element walks
// ---------------------------------------------------------------------------

/// One conversion loop.
///
/// `$sw`/`$dw` are the byte widths, `$sty`/`$dty` the element types and `$f` the
/// converter. Strides are in elements; the `1, 1` case is specialised because a
/// runtime `step_by` blocks vectorisation and format conversion is supposed to
/// be memory-bandwidth-bound (§B.15 scenario 4).
macro_rules! walk {
    ($src:expr, $ss:expr, $dst:expr, $ds:expr, $n:expr,
     $sw:literal, $sty:ty, $dw:literal, $dty:ty, $f:expr) => {{
        let (sc, _) = $src.as_chunks::<$sw>();
        let (dc, _) = $dst.as_chunks_mut::<$dw>();
        let f = $f;
        if $ss == 1 && $ds == 1 {
            for (s, d) in sc.iter().zip(dc.iter_mut()).take($n) {
                *d = <$dty>::to_le_bytes(f(<$sty>::from_le_bytes(*s)));
            }
        } else {
            for (s, d) in sc
                .iter()
                .step_by($ss)
                .zip(dc.iter_mut().step_by($ds))
                .take($n)
            {
                *d = <$dty>::to_le_bytes(f(<$sty>::from_le_bytes(*s)));
            }
        }
    }};
}

/// Convert `n` elements from `src` to `dst`, reading every `src_stride`th
/// element and writing every `dst_stride`th.
///
/// Strides are in **elements**, not bytes. `src` and `dst` must already be
/// offset to their first element.
#[allow(
    clippy::too_many_lines,
    reason = "thirty-six explicit arms is the point: every pair is visible"
)]
pub fn convert_elems(
    se: Elem,
    src: &[u8],
    src_stride: usize,
    de: Elem,
    dst: &mut [u8],
    dst_stride: usize,
    n: usize,
) {
    use Elem::{F32, F64, S16, S32, S64, U8};
    let (ss, ds) = (src_stride.max(1), dst_stride.max(1));
    match (se, de) {
        // identity
        (U8, U8) => walk!(src, ss, dst, ds, n, 1, u8, 1, u8, |x| x),
        (S16, S16) => walk!(src, ss, dst, ds, n, 2, i16, 2, i16, |x| x),
        (S32, S32) => walk!(src, ss, dst, ds, n, 4, i32, 4, i32, |x| x),
        (S64, S64) => walk!(src, ss, dst, ds, n, 8, i64, 8, i64, |x| x),
        (F32, F32) => walk!(src, ss, dst, ds, n, 4, f32, 4, f32, |x| x),
        (F64, F64) => walk!(src, ss, dst, ds, n, 8, f64, 8, f64, |x| x),
        // integer -> integer
        (U8, S16) => walk!(src, ss, dst, ds, n, 1, u8, 2, i16, elem::u8_to_i16),
        (U8, S32) => walk!(src, ss, dst, ds, n, 1, u8, 4, i32, elem::u8_to_i32),
        (U8, S64) => walk!(src, ss, dst, ds, n, 1, u8, 8, i64, elem::u8_to_i64),
        (S16, U8) => walk!(src, ss, dst, ds, n, 2, i16, 1, u8, elem::i16_to_u8),
        (S16, S32) => walk!(src, ss, dst, ds, n, 2, i16, 4, i32, elem::i16_to_i32),
        (S16, S64) => walk!(src, ss, dst, ds, n, 2, i16, 8, i64, elem::i16_to_i64),
        (S32, U8) => walk!(src, ss, dst, ds, n, 4, i32, 1, u8, elem::i32_to_u8),
        (S32, S16) => walk!(src, ss, dst, ds, n, 4, i32, 2, i16, elem::i32_to_i16),
        (S32, S64) => walk!(src, ss, dst, ds, n, 4, i32, 8, i64, elem::i32_to_i64),
        (S64, U8) => walk!(src, ss, dst, ds, n, 8, i64, 1, u8, elem::i64_to_u8),
        (S64, S16) => walk!(src, ss, dst, ds, n, 8, i64, 2, i16, elem::i64_to_i16),
        (S64, S32) => walk!(src, ss, dst, ds, n, 8, i64, 4, i32, elem::i64_to_i32),
        // integer -> float
        (U8, F32) => walk!(src, ss, dst, ds, n, 1, u8, 4, f32, elem::u8_to_f32),
        (U8, F64) => walk!(src, ss, dst, ds, n, 1, u8, 8, f64, elem::u8_to_f64),
        (S16, F32) => walk!(src, ss, dst, ds, n, 2, i16, 4, f32, elem::i16_to_f32),
        (S16, F64) => walk!(src, ss, dst, ds, n, 2, i16, 8, f64, elem::i16_to_f64),
        (S32, F32) => walk!(src, ss, dst, ds, n, 4, i32, 4, f32, elem::i32_to_f32),
        (S32, F64) => walk!(src, ss, dst, ds, n, 4, i32, 8, f64, elem::i32_to_f64),
        (S64, F32) => walk!(src, ss, dst, ds, n, 8, i64, 4, f32, elem::i64_to_f32),
        (S64, F64) => walk!(src, ss, dst, ds, n, 8, i64, 8, f64, elem::i64_to_f64),
        // float -> integer
        (F32, U8) => walk!(src, ss, dst, ds, n, 4, f32, 1, u8, elem::f32_to_u8),
        (F32, S16) => walk!(src, ss, dst, ds, n, 4, f32, 2, i16, elem::f32_to_i16),
        (F32, S32) => walk!(src, ss, dst, ds, n, 4, f32, 4, i32, elem::f32_to_i32),
        (F32, S64) => walk!(src, ss, dst, ds, n, 4, f32, 8, i64, elem::f32_to_i64),
        (F64, U8) => walk!(src, ss, dst, ds, n, 8, f64, 1, u8, elem::f64_to_u8),
        (F64, S16) => walk!(src, ss, dst, ds, n, 8, f64, 2, i16, elem::f64_to_i16),
        (F64, S32) => walk!(src, ss, dst, ds, n, 8, f64, 4, i32, elem::f64_to_i32),
        (F64, S64) => walk!(src, ss, dst, ds, n, 8, f64, 8, i64, elem::f64_to_i64),
        // float -> float
        (F32, F64) => walk!(src, ss, dst, ds, n, 4, f32, 8, f64, f64::from),
        (F64, F32) => walk!(src, ss, dst, ds, n, 8, f64, 4, f32, |x: f64| x as f32),
    }
}

// ---------------------------------------------------------------------------
// The public conversion entry point
// ---------------------------------------------------------------------------

/// Convert a whole buffer: element type, and packed ↔ planar, in one pass.
///
/// Returns the number of samples per channel written, which is
/// `min(src.samples(), dst.samples())`.
///
/// # Errors
/// [`Error::InvalidData`] if the channel counts differ.
pub fn convert(src: AudioRef<'_>, dst: &mut AudioMut<'_>) -> Result<usize, Error> {
    let channels = src.channels();
    if channels != dst.channels() {
        return Err(Error::InvalidData(
            "convert() does not change the channel count; use Resampler",
        ));
    }
    let n = src.samples().min(dst.samples());
    if n == 0 {
        return Ok(0);
    }
    let (sf, df) = (src.format(), dst.format());
    let (se, de) = (Elem::of(sf), Elem::of(df));

    // Packed -> packed with the same channel count is one flat run: the
    // interleaving is identical on both sides, so there is nothing to permute.
    if !sf.is_planar() && !df.is_planar() {
        let (Some(s), Some(d)) = (src.plane(0), dst.plane_mut(0)) else {
            return Ok(0);
        };
        convert_elems(se, s, 1, de, d, 1, n * channels as usize);
        return Ok(n);
    }

    let sw = se.bytes();
    let dw = de.bytes();
    for ch in 0..channels as usize {
        let (s_plane, s_off, s_stride) = if sf.is_planar() {
            (ch, 0, 1)
        } else {
            (0, ch * sw, channels as usize)
        };
        let (d_plane, d_off, d_stride) = if df.is_planar() {
            (ch, 0, 1)
        } else {
            (0, ch * dw, channels as usize)
        };
        let Some(s) = src.plane(s_plane).and_then(|p| p.get(s_off..)) else {
            return Err(Error::InvalidData("source plane missing"));
        };
        let Some(d) = dst.plane_mut(d_plane).and_then(|p| p.get_mut(d_off..)) else {
            return Err(Error::InvalidData("destination plane missing"));
        };
        convert_elems(se, s, s_stride, de, d, d_stride, n);
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Internal-format bridges, used by the Resampler
// ---------------------------------------------------------------------------

/// Read `n` samples per channel out of `src` into planar internal buffers.
///
/// `out` must have one entry per channel; each is *appended* to, so a caller
/// streaming in chunks just keeps calling.
pub(crate) fn read_planes<T: Internal>(
    src: AudioRef<'_>,
    out: &mut [Vec<T>],
    n: usize,
) -> Result<(), Error> {
    let channels = src.channels() as usize;
    if out.len() != channels {
        return Err(Error::InvalidData("plane count does not match channels"));
    }
    let fmt = src.format();
    let e = Elem::of(fmt);
    let w = e.bytes();
    for (ch, plane) in out.iter_mut().enumerate() {
        let (src_plane, off, stride) = if fmt.is_planar() {
            (ch, 0, 1)
        } else {
            (0, ch * w, channels)
        };
        let Some(bytes) = src.plane(src_plane).and_then(|p| p.get(off..)) else {
            return Err(Error::InvalidData("source plane missing"));
        };
        read_run::<T>(e, bytes, stride, n, plane);
    }
    Ok(())
}

fn read_run<T: Internal>(e: Elem, src: &[u8], stride: usize, n: usize, out: &mut Vec<T>) {
    macro_rules! pull {
        ($w:literal, $ty:ty, $f:expr) => {{
            let (sc, _) = src.as_chunks::<$w>();
            let f = $f;
            if stride == 1 {
                out.extend(sc.iter().take(n).map(|c| f(<$ty>::from_le_bytes(*c))));
            } else {
                out.extend(
                    sc.iter()
                        .step_by(stride)
                        .take(n)
                        .map(|c| f(<$ty>::from_le_bytes(*c))),
                );
            }
        }};
    }
    match e {
        Elem::U8 => pull!(1, u8, T::of_u8),
        Elem::S16 => pull!(2, i16, T::of_i16),
        Elem::S32 => pull!(4, i32, T::of_i32),
        Elem::S64 => pull!(8, i64, T::of_i64),
        Elem::F32 => pull!(4, f32, T::of_f32),
        Elem::F64 => pull!(8, f64, T::of_f64),
    }
}

/// Write `n` samples per channel from planar internal buffers into `dst`,
/// starting at sample offset `dst_off`.
pub(crate) fn write_planes<T: Internal>(
    planes: &[Vec<T>],
    src_off: usize,
    dst: &mut AudioMut<'_>,
    dst_off: usize,
    n: usize,
) -> Result<(), Error> {
    let channels = dst.channels() as usize;
    if planes.len() != channels {
        return Err(Error::InvalidData("plane count does not match channels"));
    }
    let fmt = dst.format();
    let e = Elem::of(fmt);
    let w = e.bytes();
    for (ch, plane) in planes.iter().enumerate() {
        let (dst_plane, off, stride) = if fmt.is_planar() {
            (ch, dst_off * w, 1)
        } else {
            (0, (dst_off * channels + ch) * w, channels)
        };
        let Some(src) = plane.get(src_off..src_off + n) else {
            return Err(Error::InvalidData("internal plane too short"));
        };
        let Some(bytes) = dst.plane_mut(dst_plane).and_then(|p| p.get_mut(off..)) else {
            return Err(Error::InvalidData("destination plane missing"));
        };
        write_run::<T>(e, src, bytes, stride);
    }
    Ok(())
}

fn write_run<T: Internal>(e: Elem, src: &[T], dst: &mut [u8], stride: usize) {
    macro_rules! push {
        ($w:literal, $ty:ty, $f:expr) => {{
            let (dc, _) = dst.as_chunks_mut::<$w>();
            let f = $f;
            if stride == 1 {
                for (s, d) in src.iter().zip(dc.iter_mut()) {
                    *d = <$ty>::to_le_bytes(f(*s));
                }
            } else {
                for (s, d) in src.iter().zip(dc.iter_mut().step_by(stride)) {
                    *d = <$ty>::to_le_bytes(f(*s));
                }
            }
        }};
    }
    match e {
        Elem::U8 => push!(1, u8, T::into_u8),
        Elem::S16 => push!(2, i16, T::into_i16),
        Elem::S32 => push!(4, i32, T::into_i32),
        Elem::S64 => push!(8, i64, T::into_i64),
        Elem::F32 => push!(4, f32, T::into_f32),
        Elem::F64 => push!(8, f64, T::into_f64),
    }
}
