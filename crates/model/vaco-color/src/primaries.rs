//! [`ColorPrimaries`]: H.273 Table 2, its chromaticity coordinates, and the
//! RGB↔XYZ derivation everything else in this crate is built on.

use crate::ColorPrimaries;

/// The four CIE 1931 `(x, y)` coordinates that define an RGB colour space.
///
/// `y` is never negative and, for every primary set H.273 assigns other than
/// [`ColorPrimaries::Smpte428`], never zero — see [`Self::rgb_to_xyz`] for what
/// happens when it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticity {
    pub red: (f64, f64),
    pub green: (f64, f64),
    pub blue: (f64, f64),
    /// The reference white, at which `R' = G' = B' = 1`.
    pub white: (f64, f64),
}

/// CIE Illuminant D65 as H.273 states it.
///
/// H.273 lists 0.3127 / 0.3290 to four places for every D65-referred primary
/// set. That is the number the specification gives, so it is the number used:
/// the "more precise" 0.312727 / 0.329024 that appears in some derivations is a
/// different white point and would move every derived coefficient.
const D65: (f64, f64) = (0.3127, 0.3290);

/// CIE Illuminant C, used by BT.470 System M and by generic film.
const ILLUMINANT_C: (f64, f64) = (0.310, 0.316);

/// The DCI reference white of SMPTE RP 431-2 — not D65, and deliberately so.
const DCI_WHITE: (f64, f64) = (0.314, 0.351);

impl ColorPrimaries {
    /// Every assigned code point, in ascending order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Bt709,
            Self::Unspecified,
            Self::Bt470m,
            Self::Bt470bg,
            Self::Smpte170m,
            Self::Smpte240m,
            Self::Film,
            Self::Bt2020,
            Self::Smpte428,
            Self::Smpte431,
            Self::Smpte432,
            Self::Ebu3213,
        ]
    }

    /// The H.273 code point.
    #[inline]
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        self as u8
    }

    /// The variant for an H.273 code point, or `None` if the value is reserved
    /// or unassigned.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Bt709,
            2 => Self::Unspecified,
            4 => Self::Bt470m,
            5 => Self::Bt470bg,
            6 => Self::Smpte170m,
            7 => Self::Smpte240m,
            8 => Self::Film,
            9 => Self::Bt2020,
            10 => Self::Smpte428,
            11 => Self::Smpte431,
            12 => Self::Smpte432,
            22 => Self::Ebu3213,
            _ => return None,
        })
    }

    /// The name the reference tool prints in `ffprobe -show_streams`.
    ///
    /// Not the same table as [`Self::from_name`] accepts — see the crate docs.
    /// For this enum the two agree except at code point 22.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bt709 => "bt709",
            // D17: the reference prints "unknown", not "unspecified", for this
            // code point in `-show_streams`, while `chroma_location` prints
            // "unspecified" for its own unspecified value. The asymmetry is the
            // reference's; reproducing it is what D6 requires.
            Self::Unspecified => "unknown",
            Self::Bt470m => "bt470m",
            Self::Bt470bg => "bt470bg",
            Self::Smpte170m => "smpte170m",
            Self::Smpte240m => "smpte240m",
            Self::Film => "film",
            Self::Bt2020 => "bt2020",
            Self::Smpte428 => "smpte428",
            Self::Smpte431 => "smpte431",
            Self::Smpte432 => "smpte432",
            // D17: the reference's *option* table names code point 22
            // `jedec-p22` first and `ebu3213` second, but `-show_streams` prints
            // `ebu3213`. Verified against ffmpeg 8.1 by writing the value into
            // an H.264 VUI and probing it back. Output name and option name are
            // separate tables; do not collapse them.
            Self::Ebu3213 => "ebu3213",
        }
    }

    /// Parse a name as the reference's `-color_primaries` option accepts it.
    ///
    /// Case-sensitive, and every documented alias is accepted. The reference
    /// rejects `BT709`, so we do too.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "bt709" => Self::Bt709,
            "unknown" | "unspecified" => Self::Unspecified,
            "bt470m" => Self::Bt470m,
            "bt470bg" => Self::Bt470bg,
            "smpte170m" => Self::Smpte170m,
            "smpte240m" => Self::Smpte240m,
            "film" => Self::Film,
            "bt2020" => Self::Bt2020,
            "smpte428" | "smpte428_1" => Self::Smpte428,
            "smpte431" => Self::Smpte431,
            "smpte432" => Self::Smpte432,
            // D17: `jedec-p22` is the option table's primary spelling for 22;
            // `ebu3213` is an accepted alias there and the *only* spelling in
            // `-show_streams`. Both parse.
            "jedec-p22" | "ebu3213" => Self::Ebu3213,
            _ => return None,
        })
    }

    /// The CIE 1931 chromaticity coordinates H.273 Table 2 assigns, or `None`
    /// for [`Self::Unspecified`].
    ///
    /// Coordinates are the specification's, to the digits the specification
    /// prints. Where two code points name the same coordinates (170M and 240M,
    /// 431-2 and 432-1 apart from the white point) that is a fact about the
    /// standards, not a copied row.
    #[must_use]
    pub const fn chromaticity(self) -> Option<Chromaticity> {
        Some(match self {
            Self::Unspecified => return None,
            // BT.709-6 / sRGB (IEC 61966-2-1) / SMPTE RP 177.
            Self::Bt709 => Chromaticity {
                red: (0.640, 0.330),
                green: (0.300, 0.600),
                blue: (0.150, 0.060),
                white: D65,
            },
            // BT.470-6 System M, i.e. the 1953 NTSC phosphors, referred to
            // Illuminant C rather than D65.
            Self::Bt470m => Chromaticity {
                red: (0.67, 0.33),
                green: (0.21, 0.71),
                blue: (0.14, 0.08),
                white: ILLUMINANT_C,
            },
            // BT.470-6 System B,G / BT.601-7 625-line / BT.1700 625 PAL/SECAM.
            Self::Bt470bg => Chromaticity {
                red: (0.64, 0.33),
                green: (0.29, 0.60),
                blue: (0.15, 0.06),
                white: D65,
            },
            // BT.601-7 525-line / SMPTE ST 170M / BT.1700 NTSC. SMPTE ST 240M
            // specifies the same coordinates.
            Self::Smpte170m | Self::Smpte240m => Chromaticity {
                red: (0.630, 0.340),
                green: (0.310, 0.595),
                blue: (0.155, 0.070),
                white: D65,
            },
            // Generic film: Illuminant C through Wratten 25 / 58 / 47 filters.
            Self::Film => Chromaticity {
                red: (0.681, 0.319),
                green: (0.243, 0.692),
                blue: (0.145, 0.049),
                white: ILLUMINANT_C,
            },
            // BT.2020-2, also BT.2100-2.
            Self::Bt2020 => Chromaticity {
                red: (0.708, 0.292),
                green: (0.170, 0.797),
                blue: (0.131, 0.046),
                white: D65,
            },
            // SMPTE ST 428-1: the CIE 1931 XYZ axes themselves, with equal-energy
            // white. Two of these coordinates have y = 0, which is why
            // `rgb_to_xyz` special-cases this value instead of solving.
            Self::Smpte428 => Chromaticity {
                red: (1.0, 0.0),
                green: (0.0, 1.0),
                blue: (0.0, 0.0),
                white: (1.0 / 3.0, 1.0 / 3.0),
            },
            // SMPTE RP 431-2 (DCI-P3), DCI white.
            Self::Smpte431 => Chromaticity {
                red: (0.680, 0.320),
                green: (0.265, 0.690),
                blue: (0.150, 0.060),
                white: DCI_WHITE,
            },
            // SMPTE ST 432-1 (Display P3): the 431-2 primaries referred to D65.
            Self::Smpte432 => Chromaticity {
                red: (0.680, 0.320),
                green: (0.265, 0.690),
                blue: (0.150, 0.060),
                white: D65,
            },
            // EBU Tech 3213-E, the JEDEC P22 phosphor set.
            Self::Ebu3213 => Chromaticity {
                red: (0.630, 0.340),
                green: (0.295, 0.605),
                blue: (0.155, 0.077),
                white: D65,
            },
        })
    }

    /// Column-major-free 3×3 mapping linear R, G, B in `0..=1` to CIE XYZ,
    /// normalised so that the reference white maps to `Y = 1`.
    ///
    /// Rows are X, Y, Z; columns are R, G, B. `None` when the primaries are
    /// unspecified or degenerate.
    #[must_use]
    pub fn rgb_to_xyz(self) -> Option<[[f64; 3]; 3]> {
        // ST 428-1's "primaries" are the XYZ axes, so the mapping is the
        // identity by definition. The general solve cannot produce it: two of
        // the coordinates have y = 0 and x/y is undefined there.
        if self == Self::Smpte428 {
            return Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        }
        self.chromaticity()?.rgb_to_xyz()
    }

    /// The inverse of [`Self::rgb_to_xyz`].
    #[must_use]
    pub fn xyz_to_rgb(self) -> Option<[[f64; 3]; 3]> {
        invert3(self.rgb_to_xyz()?)
    }
}

impl Chromaticity {
    /// Linear RGB → CIE XYZ, normalised so the reference white has `Y = 1`.
    ///
    /// # Derivation
    ///
    /// A chromaticity `(x, y)` fixes a colour only up to scale: the XYZ triple
    /// of a primary is `S · (x/y, 1, (1 − x − y)/y)` for some unknown `S`. Write
    /// `M₀` for the 3×3 whose columns are those unit vectors for R, G and B.
    /// Then the matrix we want is `M₀ · diag(Sr, Sg, Sb)`.
    ///
    /// The scales come from the one extra constraint the standard gives: at
    /// `R = G = B = 1` the result must be the reference white, whose XYZ is
    /// `W = (xw/yw, 1, (1 − xw − yw)/yw)` under the same `Y = 1` normalisation.
    /// So `M₀ · S = W`, and `S = M₀⁻¹ · W`.
    ///
    /// The middle row of the result is `(Sr, Sg, Sb)`, because the Y component
    /// of every unit vector is 1. That row *is* `(Kr, Kg, Kb)` — see
    /// [`Self::luma_coefficients`].
    #[must_use]
    pub fn rgb_to_xyz(self) -> Option<[[f64; 3]; 3]> {
        let unit = |(x, y): (f64, f64)| -> Option<[f64; 3]> {
            if y == 0.0 {
                return None;
            }
            Some([x / y, 1.0, (1.0 - x - y) / y])
        };
        let r = unit(self.red)?;
        let g = unit(self.green)?;
        let b = unit(self.blue)?;
        let w = unit(self.white)?;

        // Columns are the primaries, rows are X/Y/Z.
        let m0 = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let [sr, sg, sb] = mul3v(invert3(m0)?, w);

        // Scale each column by its primary's solved coefficient.
        let [[xr, xg, xb], [yr, yg, yb], [zr, zg, zb]] = m0;
        Some([
            [xr * sr, xg * sg, xb * sb],
            [yr * sr, yg * sg, yb * sb],
            [zr * sr, zg * sg, zb * sb],
        ])
    }

    /// `(Kr, Kb)` for these primaries: the relative luminance contributions of
    /// the red and blue primaries at the reference white.
    ///
    /// This is the derivation H.273 prescribes for matrix coefficients 12 and 13
    /// ("chroma-derived"). It is *not* how the fixed matrices are defined:
    /// H.273 Table 4 states literal `Kr`/`Kb` for BT.709, BT.601 and BT.2020,
    /// and those literals — not the values this function returns — are what
    /// every encoder and decoder in the world uses. The two agree only to about
    /// four decimal places, which is a visible difference at 10 bits. See
    /// [`MatrixCoefficients::luma_coefficients`](crate::MatrixCoefficients::luma_coefficients).
    #[must_use]
    pub fn luma_coefficients(self) -> Option<(f64, f64)> {
        let [_, [kr, _kg, kb], _] = self.rgb_to_xyz()?;
        Some((kr, kb))
    }
}

/// `m · v`.
fn mul3v(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let [row0, row1, row2] = m;
    let [v0, v1, v2] = v;
    let dot = |row: [f64; 3]| -> f64 {
        let [c0, c1, c2] = row;
        c0 * v0 + c1 * v1 + c2 * v2
    };
    [dot(row0), dot(row1), dot(row2)]
}

/// Cofactor inverse of a 3×3. `None` when the determinant is zero or not
/// finite, which for a chromaticity matrix means the three primaries are
/// collinear and describe no gamut at all.
pub(crate) fn invert3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let [[a0, a1, a2], [b0, b1, b2], [c0, c1, c2]] = m;

    // Transposed cofactor matrix (the adjugate), so `out[i][j]` is already the
    // inverse's entry once divided by the determinant.
    let i00 = b1 * c2 - b2 * c1;
    let i01 = a2 * c1 - a1 * c2;
    let i02 = a1 * b2 - a2 * b1;
    let i10 = b2 * c0 - b0 * c2;
    let i11 = a0 * c2 - a2 * c0;
    let i12 = a2 * b0 - a0 * b2;
    let i20 = b0 * c1 - b1 * c0;
    let i21 = a1 * c0 - a0 * c1;
    let i22 = a0 * b1 - a1 * b0;

    let det = a0 * i00 + a1 * i10 + a2 * i20;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some([
        [i00 / det, i01 / det, i02 / det],
        [i10 / det, i11 / det, i12 / det],
        [i20 / det, i21 / det, i22 / det],
    ])
}
