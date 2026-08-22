//! Fixed-point fields and the display matrix.
//!
//! ISO/IEC 14496-12 uses three fixed-point layouts and one 3×3 matrix:
//!
//! | Field | Layout | Where |
//! |---|---|---|
//! | `rate` | 16.16 signed | `mvhd` |
//! | `volume` | 8.8 signed | `mvhd`, `tkhd` |
//! | `width`/`height` | 16.16 unsigned | `tkhd` |
//! | `samplerate` | 16.16 unsigned | `AudioSampleEntry` v0/v1 |
//! | matrix `a b c d u v` | 16.16 | `mvhd`, `tkhd` |
//! | matrix `x y w` | 2.30 | `mvhd`, `tkhd` |
//!
//! Everything here converts into [`Rational`] rather than a float. A 16.16
//! value is exactly `n / 65536`, and a track whose `width` is `160.5` must stay
//! `321/2` all the way to the sample-aspect-ratio derivation — truncating it to
//! `160` silently changes the aspect ratio of the output, which is one of the
//! quieter ways a transcode goes wrong.

use vaco_core::Rational;

/// Denominator of a 16.16 value.
pub const FP16_ONE: i32 = 1 << 16;
/// Denominator of a 2.30 value.
pub const FP30_ONE: i32 = 1 << 30;
/// Denominator of an 8.8 value.
pub const FP8_ONE: i32 = 1 << 8;

/// The identity display matrix, as it appears in a freshly written `mvhd`.
pub const IDENTITY_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// A signed 16.16 value as an exact rational.
#[must_use]
pub fn fp16(raw: u32) -> Rational {
    Rational::new(raw.cast_signed(), FP16_ONE)
}

/// An unsigned 16.16 value as an exact rational.
///
/// Distinct from [`fp16`] because `tkhd.width` and `AudioSampleEntry.samplerate`
/// are unsigned: `0xAC44_0000` is 44 100.0, not a negative width. The numerator
/// does not fit `i32`, so the fraction is **reduced before it is stored**
/// rather than clamped — 44 100 Hz would otherwise saturate to 32 767.99, which
/// is exactly the bug this exists to avoid.
#[must_use]
pub fn fp16u(raw: u32) -> Rational {
    Rational::reduce(i64::from(raw), i64::from(FP16_ONE), i64::from(i32::MAX)).0
}

/// A signed 8.8 value as an exact rational.
#[must_use]
pub fn fp8(raw: u16) -> Rational {
    Rational::new(i32::from(raw.cast_signed()), FP8_ONE)
}

/// A signed 2.30 value as an exact rational.
#[must_use]
pub fn fp30(raw: u32) -> Rational {
    Rational::new(raw.cast_signed(), FP30_ONE)
}

/// The 3×3 transformation matrix of `mvhd` and `tkhd` (§6.2.2).
///
/// Stored as the file stores it — row-major `{a, b, u, c, d, v, x, y, w}` —
/// with the fixed-point interpretation applied on access, because the two
/// column layouts differ and mixing them is the classic rotation bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMatrix {
    /// The nine raw big-endian words, in file order.
    pub raw: [u32; 9],
}

impl Default for DisplayMatrix {
    fn default() -> Self {
        Self {
            raw: IDENTITY_MATRIX,
        }
    }
}

impl DisplayMatrix {
    /// Read nine words from a reader positioned at the matrix.
    #[must_use]
    pub fn parse(r: &mut vaco_bitstream::ByteReader<'_>) -> Self {
        let mut raw = [0u32; 9];
        for slot in &mut raw {
            *slot = r.be32();
        }
        Self { raw }
    }

    /// Whether the matrix is the identity.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.raw == IDENTITY_MATRIX
    }

    /// `a`, `b`, `c`, `d` as exact 16.16 rationals.
    #[must_use]
    pub fn linear(&self) -> [Rational; 4] {
        [
            fp16(self.raw[0]),
            fp16(self.raw[1]),
            fp16(self.raw[3]),
            fp16(self.raw[4]),
        ]
    }

    /// `u`, `v`, `w` as exact 2.30 rationals.
    #[must_use]
    pub fn perspective(&self) -> [Rational; 3] {
        [fp30(self.raw[2]), fp30(self.raw[5]), fp30(self.raw[8])]
    }

    /// `x`, `y` translation, as exact 16.16 rationals.
    #[must_use]
    pub fn translation(&self) -> [Rational; 2] {
        [fp16(self.raw[6]), fp16(self.raw[7])]
    }

    /// Clockwise display rotation in degrees, when the matrix is a pure
    /// rotation by a multiple of 90°.
    ///
    /// `None` for anything else — a shear, a scale, an arbitrary angle. A
    /// caller that wants a best-effort angle should use
    /// [`DisplayMatrix::rotation_degrees_f64`]; this one only answers when the
    /// answer is exact, because "rotate 90°" and "rotate 89.97°" are different
    /// instructions to a filter graph.
    #[must_use]
    pub fn rotation_quadrant(&self) -> Option<u16> {
        /// +1.0 in 16.16.
        const P: u32 = 0x0001_0000;
        /// -1.0 in 16.16.
        const N: u32 = 0xFFFF_0000;
        let [a, b, c, d] = [self.raw[0], self.raw[1], self.raw[3], self.raw[4]];
        match (a, b, c, d) {
            (P, 0, 0, P) => Some(0),
            (0, P, N, 0) => Some(90),
            (N, 0, 0, N) => Some(180),
            (0, N, P, 0) => Some(270),
            _ => None,
        }
    }

    /// Rotation in degrees, derived from `atan2(b, a)`.
    ///
    /// Returned in the range `(-180, 180]`. This is the general form; prefer
    /// [`DisplayMatrix::rotation_quadrant`] where an exact answer is wanted.
    #[must_use]
    pub fn rotation_degrees_f64(&self) -> f64 {
        let a = fp16(self.raw[0]).to_f64();
        let b = fp16(self.raw[1]).to_f64();
        if a == 0.0 && b == 0.0 {
            return 0.0;
        }
        -b.atan2(a).to_degrees()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::field_reassign_with_default,
    reason = "test code; the fixed-point conversions are exact by construction, so float equality is the right assertion"
)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_sixteen_is_exact() {
        assert_eq!(fp16(0x0001_0000), Rational::new(65536, 65536));
        assert_eq!(fp16(0x0001_0000).to_f64(), 1.0);
        // 160.5 must stay 160.5, not become 160.
        assert_eq!(fp16u(160 * 65536 + 32768).to_f64(), 160.5);
        assert_eq!(fp16(0xFFFF_0000).to_f64(), -1.0);
    }

    #[test]
    fn unsigned_sixteen_sixteen_does_not_go_negative() {
        // 0x8000_0000 is 32768.0 unsigned and -32768.0 signed.
        assert!(fp16u(0x8000_0000).to_f64() > 0.0);
        assert!(fp16(0x8000_0000).to_f64() < 0.0);
    }

    #[test]
    fn an_unsigned_rate_above_i32_max_reduces_rather_than_clamping() {
        // 44 100 Hz as `AudioSampleEntry.samplerate`: 44100 << 16 does not fit
        // an i32 numerator, and clamping it produced 32 767.99.
        assert_eq!(fp16u(44_100 << 16).to_f64(), 44_100.0);
        assert_eq!(fp16u(48_000 << 16).to_f64(), 48_000.0);
    }

    #[test]
    fn two_thirty_and_eight_eight() {
        assert_eq!(fp30(0x4000_0000).to_f64(), 1.0);
        assert_eq!(fp8(0x0100).to_f64(), 1.0);
        assert_eq!(fp8(0xFF00).to_f64(), -1.0);
    }

    #[test]
    fn identity_matrix_is_recognised() {
        let m = DisplayMatrix::default();
        assert!(m.is_identity());
        assert_eq!(m.rotation_quadrant(), Some(0));
        assert_eq!(m.rotation_degrees_f64(), 0.0);
    }

    #[test]
    fn quadrant_rotations_are_exact() {
        let mut m = DisplayMatrix::default();
        m.raw = [0, 0x0001_0000, 0, 0xFFFF_0000, 0, 0, 0, 0, 0x4000_0000];
        assert_eq!(m.rotation_quadrant(), Some(90));
        assert!((m.rotation_degrees_f64() - -90.0).abs() < 1e-9);

        m.raw = [0xFFFF_0000, 0, 0, 0, 0xFFFF_0000, 0, 0, 0, 0x4000_0000];
        assert_eq!(m.rotation_quadrant(), Some(180));

        m.raw = [0, 0xFFFF_0000, 0, 0x0001_0000, 0, 0, 0, 0, 0x4000_0000];
        assert_eq!(m.rotation_quadrant(), Some(270));
    }

    #[test]
    fn a_sheared_matrix_has_no_exact_quadrant() {
        let mut m = DisplayMatrix::default();
        m.raw[1] = 0x0000_8000;
        assert_eq!(m.rotation_quadrant(), None);
        assert!(!m.is_identity());
    }

    #[test]
    fn a_zero_matrix_reports_no_rotation_rather_than_a_nan() {
        let m = DisplayMatrix { raw: [0; 9] };
        assert_eq!(m.rotation_degrees_f64(), 0.0);
        assert_eq!(m.rotation_quadrant(), None);
        assert_eq!(m.perspective()[0].to_f64(), 0.0);
        assert_eq!(m.translation()[1].to_f64(), 0.0);
        assert_eq!(m.linear()[2].to_f64(), 0.0);
    }
}
