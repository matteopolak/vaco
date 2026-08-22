//! [`MatrixCoefficients`]: H.273 Table 4, and the R'G'B'↔Y'`CbCr` matrices it
//! defines.

use crate::{ColorPrimaries, MatrixCoefficients as Mc};

impl Mc {
    /// Every assigned code point, in ascending order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Identity,
            Self::Bt709,
            Self::Unspecified,
            Self::Fcc,
            Self::Bt470bg,
            Self::Smpte170m,
            Self::Smpte240m,
            Self::YCgCo,
            Self::Bt2020Ncl,
            Self::Bt2020Cl,
            Self::Smpte2085,
            Self::ChromaDerivedNcl,
            Self::ChromaDerivedCl,
            Self::Ictcp,
            Self::IptC2,
            Self::YCgCoRe,
            Self::YCgCoRo,
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
            0 => Self::Identity,
            1 => Self::Bt709,
            2 => Self::Unspecified,
            4 => Self::Fcc,
            5 => Self::Bt470bg,
            6 => Self::Smpte170m,
            7 => Self::Smpte240m,
            8 => Self::YCgCo,
            9 => Self::Bt2020Ncl,
            10 => Self::Bt2020Cl,
            11 => Self::Smpte2085,
            12 => Self::ChromaDerivedNcl,
            13 => Self::ChromaDerivedCl,
            14 => Self::Ictcp,
            15 => Self::IptC2,
            16 => Self::YCgCoRe,
            17 => Self::YCgCoRo,
            _ => return None,
        })
    }

    /// The name the reference tool prints as `color_space` in
    /// `ffprobe -show_streams`.
    ///
    /// # D17: code point 0 disagrees with the option table
    ///
    /// `-colorspace rgb` is how the identity matrix is selected on the command
    /// line, but a stream carrying 0 probes back as `color_space=gbr`.
    /// `-colorspace gbr` is rejected. Verified against ffmpeg 8.1. The two
    /// tables stay separate for the same reason as in
    /// [`TransferCharacteristic::name`](crate::TransferCharacteristic::name).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            // D17: option name `rgb`. `gbr` names the component order the
            // identity matrix actually produces — Y'CbCr slots carry G, B, R —
            // which is why the two tables disagree here.
            Self::Identity => "gbr",
            Self::Bt709 => "bt709",
            // D17: prints "unknown", not "unspecified".
            Self::Unspecified => "unknown",
            Self::Fcc => "fcc",
            Self::Bt470bg => "bt470bg",
            Self::Smpte170m => "smpte170m",
            Self::Smpte240m => "smpte240m",
            Self::YCgCo => "ycgco",
            Self::Bt2020Ncl => "bt2020nc",
            Self::Bt2020Cl => "bt2020c",
            Self::Smpte2085 => "smpte2085",
            Self::ChromaDerivedNcl => "chroma-derived-nc",
            Self::ChromaDerivedCl => "chroma-derived-c",
            Self::Ictcp => "ictcp",
            Self::IptC2 => "ipt-c2",
            Self::YCgCoRe => "ycgco-re",
            Self::YCgCoRo => "ycgco-ro",
        }
    }

    /// Parse a name as the reference's `-colorspace` option accepts it.
    ///
    /// Case-sensitive. `gbr` is *not* accepted, matching the reference — see
    /// [`Self::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            // D17: the option spelling is `rgb`; the printed spelling is `gbr`.
            "rgb" => Self::Identity,
            "bt709" => Self::Bt709,
            "unknown" | "unspecified" => Self::Unspecified,
            "fcc" => Self::Fcc,
            "bt470bg" => Self::Bt470bg,
            "smpte170m" => Self::Smpte170m,
            "smpte240m" => Self::Smpte240m,
            "ycgco" | "ycocg" => Self::YCgCo,
            "bt2020nc" | "bt2020_ncl" => Self::Bt2020Ncl,
            "bt2020c" | "bt2020_cl" => Self::Bt2020Cl,
            "smpte2085" => Self::Smpte2085,
            "chroma-derived-nc" => Self::ChromaDerivedNcl,
            "chroma-derived-c" => Self::ChromaDerivedCl,
            "ictcp" => Self::Ictcp,
            "ipt-c2" => Self::IptC2,
            "ycgco-re" => Self::YCgCoRe,
            "ycgco-ro" => Self::YCgCoRo,
            _ => return None,
        })
    }

    /// Whether luma is formed from *linear* light rather than from the
    /// gamma-encoded components.
    ///
    /// Constant-luminance systems have no R'G'B'→Y'`CbCr` matrix at all: `Y'c` is
    /// the transfer function applied to a linear-light luminance, and the two
    /// chroma channels use different scale factors above and below zero. That
    /// is why [`Self::rgb_to_ycbcr`] returns `None` for them even though
    /// [`Self::luma_coefficients`] returns a value.
    #[must_use]
    pub const fn is_constant_luminance(self) -> bool {
        matches!(self, Self::Bt2020Cl | Self::ChromaDerivedCl)
    }

    /// The luma coefficients (Kr, Kb) this matrix implies.
    ///
    /// Derived from the primaries where H.273 says to; returns `None` for
    /// matrices that are not a simple luma projection (`Identity`, `Ictcp`).
    ///
    /// # Where the numbers come from
    ///
    /// H.273 Table 4 states `Kr` and `Kb` as literal decimals for code points 1,
    /// 4, 5, 6, 7, 9 and 10, and those literals are returned verbatim. They are
    /// *close to* but not equal to what
    /// [`Chromaticity::luma_coefficients`](crate::Chromaticity::luma_coefficients)
    /// derives from the matching primaries — BT.709 derives 0.212639, and the
    /// standard says 0.2126 — and the standard's rounded value is the one every
    /// implementation uses, so substituting the derived one would put us a
    /// least-significant bit or two away from every other decoder. The
    /// derivation is exercised in the tests as a cross-check on the literals,
    /// which is the right use for it.
    ///
    /// The only code points whose coefficients H.273 tells you to *compute* are
    /// 12 and 13, "chroma-derived", and they need the colour primaries. This
    /// method has no access to them and returns `None`; use
    /// [`Self::luma_coefficients_with`].
    #[must_use]
    pub fn luma_coefficients(self) -> Option<(f64, f64)> {
        self.luma_coefficients_with(ColorPrimaries::Unspecified)
    }

    /// [`Self::luma_coefficients`], resolving the chroma-derived code points
    /// (12 and 13) against `primaries`.
    ///
    /// For every other code point `primaries` is ignored.
    #[must_use]
    pub fn luma_coefficients_with(self, primaries: ColorPrimaries) -> Option<(f64, f64)> {
        Some(match self {
            // No luma projection exists. Identity is a permutation of the RGB
            // components; ICtCp and IPT-C2 form their luma-like channel from
            // LMS after a nonlinearity, not from R'G'B'; ST 2085's `Y'D'zD'x` is
            // defined on X'Y'Z'.
            Self::Identity | Self::Unspecified | Self::Smpte2085 | Self::Ictcp | Self::IptC2 => {
                return None;
            }
            // H.273 Table 4 literals. See the note on `luma_coefficients`.
            Self::Bt709 => (0.2126, 0.0722),
            Self::Fcc => (0.30, 0.11),
            Self::Bt470bg | Self::Smpte170m => (0.299, 0.114),
            Self::Smpte240m => (0.212, 0.087),
            Self::Bt2020Ncl | Self::Bt2020Cl => (0.2627, 0.0593),
            // YCgCo and the two reversible YCgCo-R variants all compute
            // Y = (R + 2G + B) / 4, so their luma projection is Kr = Kb = 1/4.
            // Their *chroma* axes are not Cb/Cr, so this value must not be fed
            // into the generic matrix builder — `rgb_to_ycbcr` knows that.
            Self::YCgCo | Self::YCgCoRe | Self::YCgCoRo => (0.25, 0.25),
            // The one case H.273 defines as a computation rather than a
            // constant.
            Self::ChromaDerivedNcl | Self::ChromaDerivedCl => {
                primaries.chromaticity()?.luma_coefficients()?
            }
        })
    }

    /// R'G'B' → Y'`CbCr` as a 3×3, rows `[Y', Cb, Cr]`, columns `[R', G', B']`.
    ///
    /// Inputs are in `0..=1`; `Y'` comes out in `0..=1` and `Cb`/`Cr` in
    /// `-0.5..=0.5`. Quantising to code values is [`ColorRange`](crate::ColorRange)'s
    /// job, and it is a separate step because the offset differs between luma
    /// and chroma.
    ///
    /// `None` for the code points that are not a linear matrix on R'G'B':
    /// unspecified, the constant-luminance pair (see
    /// [`Self::is_constant_luminance`]), ST 2085, `ICtCp`, `IPT-C2`, the reversible
    /// YCgCo-R variants (integer lifting, and they widen the chroma channels by
    /// a bit), and chroma-derived without primaries — for which
    /// [`Self::rgb_to_ycbcr_with`] applies.
    #[must_use]
    pub fn rgb_to_ycbcr(self) -> Option<[[f64; 3]; 3]> {
        self.rgb_to_ycbcr_with(ColorPrimaries::Unspecified)
    }

    /// [`Self::rgb_to_ycbcr`], resolving the chroma-derived code points against
    /// `primaries`.
    #[must_use]
    pub fn rgb_to_ycbcr_with(self, primaries: ColorPrimaries) -> Option<[[f64; 3]; 3]> {
        match self {
            // H.273: the identity matrix, "typically for GBR". The Y' slot
            // carries G, Cb carries B and Cr carries R — a permutation, not a
            // colour transform. All three channels then quantise like luma, not
            // like chroma.
            Self::Identity => Some([[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]),
            // H.273 code point 8:
            //   Y  = 0.5·G + 0.25·(R + B)
            //   Cg = 0.5·G − 0.25·(R + B)
            //   Co = 0.5·(R − B)
            Self::YCgCo => Some([[0.25, 0.5, 0.25], [-0.25, 0.5, -0.25], [0.5, 0.0, -0.5]]),
            _ if self.is_constant_luminance() => None,
            Self::YCgCoRe | Self::YCgCoRo => None,
            _ => {
                let (kr, kb) = self.luma_coefficients_with(primaries)?;
                ycbcr_matrix(kr, kb)
            }
        }
    }

    /// The inverse of [`Self::rgb_to_ycbcr`].
    #[must_use]
    pub fn ycbcr_to_rgb(self) -> Option<[[f64; 3]; 3]> {
        self.ycbcr_to_rgb_with(ColorPrimaries::Unspecified)
    }

    /// The inverse of [`Self::rgb_to_ycbcr_with`].
    #[must_use]
    pub fn ycbcr_to_rgb_with(self, primaries: ColorPrimaries) -> Option<[[f64; 3]; 3]> {
        match self {
            // Inverse permutation of the identity case above.
            Self::Identity => Some([[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
            // R = Y − Cg + Co; G = Y + Cg; B = Y − Cg − Co.
            Self::YCgCo => Some([[1.0, -1.0, 1.0], [1.0, 1.0, 0.0], [1.0, -1.0, -1.0]]),
            _ if self.is_constant_luminance() => None,
            Self::YCgCoRe | Self::YCgCoRo => None,
            _ => {
                let (kr, kb) = self.luma_coefficients_with(primaries)?;
                inverse_ycbcr_matrix(kr, kb)
            }
        }
    }
}

/// The non-constant-luminance R'G'B' → Y'`CbCr` matrix for a `(Kr, Kb)` pair.
///
/// # Derivation
///
/// H.273 §8.3 defines the transform as three scalar equations:
///
/// ```text
///   Y'  = Kr·R' + Kg·G' + Kb·B'          where Kg = 1 − Kr − Kb
///   Cb  = (B' − Y') / (2·(1 − Kb))
///   Cr  = (R' − Y') / (2·(1 − Kr))
/// ```
///
/// Substituting `Y'` into the second and third and collecting terms gives the
/// matrix directly. For `Cb`:
///
/// ```text
///   (B' − Kr·R' − Kg·G' − Kb·B') / (2(1−Kb))
///     = (−Kr·R' − Kg·G' + (1−Kb)·B') / (2(1−Kb))
///     = −Kr/(2(1−Kb))·R'  −Kg/(2(1−Kb))·G'  + ½·B'
/// ```
///
/// and symmetrically for `Cr`. The `½` in the corner is what makes pure blue sit
/// at `Cb = +0.5` and pure red at `Cr = +0.5`, which is the property the whole
/// scaling is chosen for.
///
/// `None` when `Kb = 1` or `Kr = 1` (a divide by zero) or when `Kg = 0`, none of
/// which any real primary set produces — the guard exists because
/// chroma-derived coefficients come from bitstream-supplied primaries.
fn ycbcr_matrix(kr: f64, kb: f64) -> Option<[[f64; 3]; 3]> {
    let kg = 1.0 - kr - kb;
    let (cb_den, cr_den) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    if cb_den == 0.0 || cr_den == 0.0 || kg == 0.0 {
        return None;
    }
    Some([
        [kr, kg, kb],
        [-kr / cb_den, -kg / cb_den, 0.5],
        [0.5, -kg / cr_den, -kb / cr_den],
    ])
}

/// The inverse of [`ycbcr_matrix`], written out rather than inverted
/// numerically.
///
/// # Derivation
///
/// From `Cr = (R' − Y')/(2(1−Kr))` and `Cb = (B' − Y')/(2(1−Kb))`:
///
/// ```text
///   R' = Y' + 2(1−Kr)·Cr
///   B' = Y' + 2(1−Kb)·Cb
/// ```
///
/// and `G'` follows from the luma equation:
///
/// ```text
///   G' = (Y' − Kr·R' − Kb·B') / Kg
///      = Y' − (2·Kb·(1−Kb)/Kg)·Cb − (2·Kr·(1−Kr)/Kg)·Cr
/// ```
///
/// Writing it out keeps the four exact zeros exact — a numeric inverse leaves
/// them at ~1e-17, and a kernel that multiplies by "almost zero" pays for it on
/// every pixel.
fn inverse_ycbcr_matrix(kr: f64, kb: f64) -> Option<[[f64; 3]; 3]> {
    let kg = 1.0 - kr - kb;
    if kg == 0.0 {
        return None;
    }
    Some([
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ])
}
