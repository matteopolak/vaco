//! [`TransferCharacteristic`]: H.273 Table 3, and the transfer functions
//! themselves.
//!
//! # Direction, and why it needs saying
//!
//! H.273 writes every row of Table 3 as `V = f(L)` — signal from light — but the
//! quantity `L` means is not the same for every row. For the camera curves
//! (BT.709, BT.601, BT.2020, HLG) it is *scene* linear light `Lc`; for the
//! display-referred ones (PQ, ST 428-1) it is *display* linear light `Lo`. This
//! module follows the specification's own direction and calls it
//! [`TransferCharacteristic::encode`], with [`TransferCharacteristic::decode`]
//! as its exact inverse. The argument is normalised so that `1.0` is the
//! reference peak of whichever quantity that row is written in: 10000 cd/m² for
//! PQ, 48/52.37 of the ST 428-1 reference, and nominal peak white elsewhere.
//!
//! Signal values outside `0..=1` are meaningful for the extended-gamut rows (11
//! and 12) and are handled per the specification. Elsewhere the functions
//! extrapolate rather than clamp — clamping is the caller's decision, made once,
//! where it knows the target's headroom.

use crate::TransferCharacteristic as Tc;

impl Tc {
    /// Every assigned code point, in ascending order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Bt709,
            Self::Unspecified,
            Self::Gamma22,
            Self::Gamma28,
            Self::Smpte170m,
            Self::Smpte240m,
            Self::Linear,
            Self::Log100,
            Self::Log316,
            Self::Iec61966_2_4,
            Self::Bt1361e,
            Self::Iec61966_2_1,
            Self::Bt2020_10,
            Self::Bt2020_12,
            Self::Smpte2084,
            Self::Smpte428,
            Self::AribStdB67,
        ]
    }

    /// The H.273 code point.
    #[inline]
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// The variant for an H.273 code point, or `None` if reserved.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Bt709,
            2 => Self::Unspecified,
            4 => Self::Gamma22,
            5 => Self::Gamma28,
            6 => Self::Smpte170m,
            7 => Self::Smpte240m,
            8 => Self::Linear,
            9 => Self::Log100,
            10 => Self::Log316,
            11 => Self::Iec61966_2_4,
            12 => Self::Bt1361e,
            13 => Self::Iec61966_2_1,
            14 => Self::Bt2020_10,
            15 => Self::Bt2020_12,
            16 => Self::Smpte2084,
            17 => Self::Smpte428,
            18 => Self::AribStdB67,
            _ => return None,
        })
    }

    /// The name the reference tool prints in `ffprobe -show_streams`.
    ///
    /// # D17: two rows disagree with the option table
    ///
    /// Code points 4 and 5 are spelled `gamma22` and `gamma28` on the command
    /// line but print as `bt470m` and `bt470bg`. Verified against ffmpeg 8.1:
    /// `-color_trc bt470m` is rejected, `-color_trc gamma22` is accepted, and a
    /// stream carrying 4 probes back as `color_transfer=bt470m`. The output name
    /// and the option name are separate tables in the reference and must stay
    /// separate here — collapsing them breaks either the CLI or `-show_streams`,
    /// and D6 requires both.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bt709 => "bt709",
            // D17: prints "unknown", like the other three colour properties and
            // unlike `chroma_location`, which prints "unspecified".
            Self::Unspecified => "unknown",
            // D17: option name `gamma22`. See the note on this function.
            Self::Gamma22 => "bt470m",
            // D17: option name `gamma28`.
            Self::Gamma28 => "bt470bg",
            Self::Smpte170m => "smpte170m",
            Self::Smpte240m => "smpte240m",
            Self::Linear => "linear",
            Self::Log100 => "log100",
            Self::Log316 => "log316",
            Self::Iec61966_2_4 => "iec61966-2-4",
            Self::Bt1361e => "bt1361e",
            Self::Iec61966_2_1 => "iec61966-2-1",
            Self::Bt2020_10 => "bt2020-10",
            Self::Bt2020_12 => "bt2020-12",
            Self::Smpte2084 => "smpte2084",
            Self::Smpte428 => "smpte428",
            Self::AribStdB67 => "arib-std-b67",
        }
    }

    /// Parse a name as the reference's `-color_trc` option accepts it.
    ///
    /// Case-sensitive. Note that [`Self::name`]'s output is **not** always
    /// accepted here: `bt470m` and `bt470bg` are rejected by the reference for
    /// this option and are rejected here too.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "bt709" => Self::Bt709,
            "unknown" | "unspecified" => Self::Unspecified,
            "gamma22" => Self::Gamma22,
            "gamma28" => Self::Gamma28,
            "smpte170m" => Self::Smpte170m,
            "smpte240m" => Self::Smpte240m,
            "linear" => Self::Linear,
            "log100" | "log" => Self::Log100,
            "log316" | "log_sqrt" => Self::Log316,
            "iec61966-2-4" | "iec61966_2_4" => Self::Iec61966_2_4,
            "bt1361e" | "bt1361" => Self::Bt1361e,
            "iec61966-2-1" | "iec61966_2_1" => Self::Iec61966_2_1,
            "bt2020-10" | "bt2020_10bit" => Self::Bt2020_10,
            "bt2020-12" | "bt2020_12bit" => Self::Bt2020_12,
            "smpte2084" => Self::Smpte2084,
            "smpte428" | "smpte428_1" => Self::Smpte428,
            "arib-std-b67" => Self::AribStdB67,
            _ => return None,
        })
    }

    /// Whether this curve is one of the two high-dynamic-range systems of
    /// BT.2100-2.
    ///
    /// Useful as a tone-mapping trigger; deliberately narrow, because the
    /// extended-gamut SDR curves (11, 12) are not HDR and treating them as such
    /// crushes them.
    #[must_use]
    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::Smpte2084 | Self::AribStdB67)
    }

    /// Linear light → nonlinear signal, exactly as H.273 Table 3 writes it.
    ///
    /// `None` for [`Self::Unspecified`], which names no function.
    #[must_use]
    pub fn encode(self, l: f64) -> Option<f64> {
        // Nothing below is meaningful for a NaN input and every branch would
        // silently take its `else` arm, so reject it once here instead.
        if l.is_nan() {
            return Some(f64::NAN);
        }
        Some(match self {
            Self::Unspecified => return None,
            // H.273 rows 1, 6, 14: one function, three code points. The
            // difference between them is the bit depth the *quantisation* uses,
            // not the curve.
            Self::Bt709 | Self::Smpte170m | Self::Bt2020_10 => bt709_oetf(l),
            Self::Gamma22 => powf_signed(l, 1.0 / 2.2),
            Self::Gamma28 => powf_signed(l, 1.0 / 2.8),
            // SMPTE ST 240M: the same shape as BT.709 with its own break point.
            Self::Smpte240m => {
                if l < SMPTE240M_BETA {
                    4.0 * l
                } else {
                    SMPTE240M_ALPHA * l.powf(0.45) - (SMPTE240M_ALPHA - 1.0)
                }
            }
            Self::Linear => l,
            // Logarithmic over 100:1. Everything at or below the floor maps to
            // zero — the curve has no value there to invert.
            Self::Log100 => {
                if l < 0.01 {
                    0.0
                } else {
                    1.0 + l.log10() / 2.0
                }
            }
            // Logarithmic over 100·√10 : 1. The floor is 10^-2.5.
            Self::Log316 => {
                if l < LOG316_FLOOR {
                    0.0
                } else {
                    1.0 + l.log10() / 2.5
                }
            }
            // IEC 61966-2-4 (xvYCC): BT.709's curve, reflected through the
            // origin so negative light is representable.
            Self::Iec61966_2_4 => {
                if l >= BT709_BETA {
                    BT709_ALPHA * l.powf(0.45) - (BT709_ALPHA - 1.0)
                } else if l > -BT709_BETA {
                    4.5 * l
                } else {
                    -(BT709_ALPHA * (-l).powf(0.45) - (BT709_ALPHA - 1.0))
                }
            }
            // BT.1361 extended colour gamut. Asymmetric: the negative lobe is
            // compressed by 4 in both axes, and the positive lobe extends to
            // 1.33 rather than 1.0.
            Self::Bt1361e => {
                if l >= BT709_BETA {
                    BT709_ALPHA * l.powf(0.45) - (BT709_ALPHA - 1.0)
                } else if l >= -0.0045 {
                    4.5 * l
                } else {
                    -(BT709_ALPHA * (-4.0 * l).powf(0.45) - (BT709_ALPHA - 1.0)) / 4.0
                }
            }
            // IEC 61966-2-1, i.e. sRGB / sYCC.
            Self::Iec61966_2_1 => {
                if l < SRGB_BETA {
                    12.92 * l
                } else {
                    SRGB_ALPHA * l.powf(1.0 / 2.4) - (SRGB_ALPHA - 1.0)
                }
            }
            // BT.2020 12-bit: the same shape as BT.709 to more digits. The
            // extra precision is the point of the row; do not fold it into
            // BT.709.
            Self::Bt2020_12 => {
                if l < BT2020_12_BETA {
                    4.5 * l
                } else {
                    BT2020_12_ALPHA * l.powf(0.45) - (BT2020_12_ALPHA - 1.0)
                }
            }
            // SMPTE ST 2084 (PQ), display-referred, 1.0 = 10000 cd/m².
            Self::Smpte2084 => {
                let y = l.max(0.0).powf(PQ_N);
                ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M)
            }
            // SMPTE ST 428-1 (D-Cinema), display-referred.
            Self::Smpte428 => powf_signed(48.0 * l / 52.37, 1.0 / 2.6),
            // ARIB STD-B67 (HLG), scene-referred.
            Self::AribStdB67 => {
                if l <= 1.0 / 12.0 {
                    (3.0 * l.max(0.0)).sqrt()
                } else {
                    HLG_A * (12.0 * l - HLG_B).ln() + HLG_C
                }
            }
        })
    }

    /// Nonlinear signal → linear light: the exact inverse of [`Self::encode`].
    ///
    /// Not an inverse where the forward function is not injective:
    /// [`Self::Log100`] and [`Self::Log316`] map their whole floor region to
    /// zero, and this returns that floor.
    #[must_use]
    pub fn decode(self, v: f64) -> Option<f64> {
        if v.is_nan() {
            return Some(f64::NAN);
        }
        Some(match self {
            Self::Unspecified => return None,
            Self::Bt709 | Self::Smpte170m | Self::Bt2020_10 => bt709_eotf(v),
            Self::Gamma22 => powf_signed(v, 2.2),
            Self::Gamma28 => powf_signed(v, 2.8),
            Self::Smpte240m => {
                if v < 4.0 * SMPTE240M_BETA {
                    v / 4.0
                } else {
                    ((v + SMPTE240M_ALPHA - 1.0) / SMPTE240M_ALPHA).powf(1.0 / 0.45)
                }
            }
            Self::Linear => v,
            Self::Log100 => {
                if v <= 0.0 {
                    0.01
                } else {
                    10.0_f64.powf((v - 1.0) * 2.0)
                }
            }
            Self::Log316 => {
                if v <= 0.0 {
                    LOG316_FLOOR
                } else {
                    10.0_f64.powf((v - 1.0) * 2.5)
                }
            }
            Self::Iec61966_2_4 => {
                let knee = 4.5 * BT709_BETA;
                if v >= knee {
                    ((v + BT709_ALPHA - 1.0) / BT709_ALPHA).powf(1.0 / 0.45)
                } else if v > -knee {
                    v / 4.5
                } else {
                    -(((-v) + BT709_ALPHA - 1.0) / BT709_ALPHA).powf(1.0 / 0.45)
                }
            }
            Self::Bt1361e => {
                if v >= 4.5 * BT709_BETA {
                    ((v + BT709_ALPHA - 1.0) / BT709_ALPHA).powf(1.0 / 0.45)
                } else if v >= 4.5 * -0.0045 {
                    v / 4.5
                } else {
                    -((-4.0 * v + BT709_ALPHA - 1.0) / BT709_ALPHA).powf(1.0 / 0.45) / 4.0
                }
            }
            Self::Iec61966_2_1 => {
                if v < 12.92 * SRGB_BETA {
                    v / 12.92
                } else {
                    ((v + SRGB_ALPHA - 1.0) / SRGB_ALPHA).powf(2.4)
                }
            }
            Self::Bt2020_12 => {
                if v < 4.5 * BT2020_12_BETA {
                    v / 4.5
                } else {
                    ((v + BT2020_12_ALPHA - 1.0) / BT2020_12_ALPHA).powf(1.0 / 0.45)
                }
            }
            Self::Smpte2084 => {
                let e = v.max(0.0).powf(1.0 / PQ_M);
                let num = (e - PQ_C1).max(0.0);
                let den = PQ_C2 - PQ_C3 * e;
                if den == 0.0 {
                    f64::INFINITY
                } else {
                    (num / den).powf(1.0 / PQ_N)
                }
            }
            Self::Smpte428 => powf_signed(v, 2.6) * 52.37 / 48.0,
            Self::AribStdB67 => {
                if v <= 0.5 {
                    v * v / 3.0
                } else {
                    (((v - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
                }
            }
        })
    }
}

// ---------------------------------------------------------------- constants
//
// Every number below is read straight out of the specification row it belongs
// to. They are written as the spec writes them (or as the exact rational the
// spec gives), never pre-combined, so that each one can be checked against the
// document by eye.

/// BT.709-6 / BT.601-7 / BT.2020-2 10-bit: `V = α·L^0.45 − (α−1)` above `β`.
const BT709_ALPHA: f64 = 1.099;
const BT709_BETA: f64 = 0.018;

/// BT.2020-2 12-bit states the same curve to more digits.
const BT2020_12_ALPHA: f64 = 1.0993;
const BT2020_12_BETA: f64 = 0.0181;

/// SMPTE ST 240M: `V = 1.1115·L^0.45 − 0.1115` above 0.0228, `4·L` below.
const SMPTE240M_ALPHA: f64 = 1.1115;
const SMPTE240M_BETA: f64 = 0.0228;

/// IEC 61966-2-1 (sRGB): `V = 1.055·L^(1/2.4) − 0.055` above 0.0031308.
const SRGB_ALPHA: f64 = 1.055;
const SRGB_BETA: f64 = 0.003_130_8;

/// H.273 row 10 writes the floor as 0.0031622777; it is exactly `10^(-2.5)`.
const LOG316_FLOOR: f64 = 0.003_162_277_660_168_379_3;

// SMPTE ST 2084 (PQ). The specification defines these as exact rationals over
// powers of two, so they are written that way rather than as decimals: every one
// of them is exactly representable in binary floating point.
/// `2610 / 16384`.
const PQ_N: f64 = 2610.0 / 16384.0;
/// `(2523 / 4096) · 128`.
const PQ_M: f64 = 2523.0 / 4096.0 * 128.0;
/// `3424 / 4096`, which is also `c3 − c2 + 1`.
const PQ_C1: f64 = 3424.0 / 4096.0;
/// `(2413 / 4096) · 32`.
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
/// `(2392 / 4096) · 32`.
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

// ARIB STD-B67 (HLG). `b` and `c` are given by the specification both as
// decimals and as expressions in `a`; the expressions are used because they are
// what make the two segments meet exactly at L = 1/12.
const HLG_A: f64 = 0.178_832_77;
/// `1 − 4a`, printed by the specification as 0.28466892.
const HLG_B: f64 = 1.0 - 4.0 * HLG_A;
/// `0.5 − a·ln(4a)`, printed by the specification as 0.55991073. Not a `const`
/// expression because `ln` is not const, so it is stated to the digits the
/// specification prints and checked against the expression in the tests.
const HLG_C: f64 = 0.559_910_73;

/// BT.709's OETF, shared by code points 1, 6 and 14.
fn bt709_oetf(l: f64) -> f64 {
    if l < BT709_BETA {
        4.5 * l
    } else {
        BT709_ALPHA * l.powf(0.45) - (BT709_ALPHA - 1.0)
    }
}

/// The inverse of [`bt709_oetf`].
fn bt709_eotf(v: f64) -> f64 {
    if v < 4.5 * BT709_BETA {
        v / 4.5
    } else {
        ((v + BT709_ALPHA - 1.0) / BT709_ALPHA).powf(1.0 / 0.45)
    }
}

/// `x^e`, reflected through the origin for negative `x`.
///
/// `f64::powf` returns NaN for a negative base and a fractional exponent, which
/// would turn a slightly-negative sample — routine after a chroma upsample — into
/// a NaN that poisons the rest of the frame. The pure-power rows of Table 3 are
/// defined on non-negative light only, so the odd extension is our choice; it is
/// the one that keeps the function monotonic and its inverse exact.
fn powf_signed(x: f64, e: f64) -> f64 {
    if x < 0.0 { -(-x).powf(e) } else { x.powf(e) }
}
