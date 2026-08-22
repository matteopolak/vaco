//! Stream-scoped side data — what describes the *stream* rather than any one
//! packet.
//!
//! Plan 18 §1.1 names this `StreamSideData` and lists nine eventual members
//! (display matrix, stereo3d, spherical, mastering display, content light,
//! ICC profile, ambient viewing environment, CPB properties, `ReplayGain`).
//! Only the display matrix has a producer today, so only the display matrix
//! is modelled: an enum with one variant is honest about what exists, and
//! `#[non_exhaustive]` says the list is expected to grow.
//!
//! # Why this is side data and not a `Stream` field
//!
//! A `display_matrix: Option<[i32; 9]>` field would work, and would be one
//! line shorter. It is the wrong shape for three reasons:
//!
//! 1. **The reference prints it as a list.** `ffprobe` emits a repeated
//!    `[SIDE_DATA]` sub-section per stream, and `vaco-textformat` already has
//!    `STREAM_SIDE_DATA_LIST`/`STREAM_SIDE_DATA` section ids for it. A field
//!    per kind cannot produce a list whose length varies.
//! 2. **The next eight members are not fields either.** Adding one field per
//!    side-data kind puts eight mostly-`None` `Option`s on every stream in the
//!    workspace, including the streams of every container that can never
//!    produce one.
//! 3. **Its meaning is container-independent.** The matrix is the same object
//!    whether it arrived in an ISOBMFF `tkhd`, a Matroska `Projection`, or an
//!    H.264 display-orientation SEI, so it belongs in a shared vocabulary
//!    rather than in a field named after the box that carried it.

/// One piece of stream-scoped side data.
///
/// Deliberately **not** `#[non_exhaustive]`. Everything that consumes this is
/// in this workspace, and `non_exhaustive` would force a catch-all arm into
/// `vaco-probe`'s printer — turning "a new side-data kind is unprinted" from a
/// compile error into a silently missing `[SIDE_DATA]` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSideData {
    /// The 3×3 display transformation matrix, row-major.
    ///
    /// Stored in the *file's* fixed-point encoding, unconverted: entries 0, 1,
    /// 3, 4, 6 and 7 are 16.16 and entries 2, 5 and 8 are 2.30, exactly as
    /// ISO/IEC 14496-12 §6.2.2 defines the `tkhd` matrix. Keeping the raw form
    /// matters because the reference prints the integers themselves, so any
    /// normalisation here would have to be undone to print.
    DisplayMatrix([i32; 9]),
}

impl StreamSideData {
    /// The name the reference prints in `side_data_type`.
    ///
    /// An interface fact (D9), read off `ffprobe 8.1`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DisplayMatrix(_) => "Display Matrix",
        }
    }
}

/// Clockwise rotation, in degrees, implied by a display matrix.
///
/// **Measured against `ffprobe 8.1`**, by byte-patching one `tkhd` matrix in a
/// `.mov` and reading `side_data_list[0].rotation` back:
///
/// | matrix `[a, b, …]` | reference |
/// |---|---|
/// | `[0, 65536, 0, -65536, 0, 0, …]` | `-90` |
/// | `[-65536, 0, 0, 0, -65536, 0, …]` | `-180` |
/// | `[0, -65536, 0, 65536, 0, 0, …]` | `90` |
/// | `[56755, 32768, 0, -32768, 56755, 0, …]` | `-30` |
/// | `[-2147483648, 2147483647, 0, 0, 0, 0, …]` | `-135` |
/// | all zero | `0` |
///
/// The rule those five points pin down is
/// `-atan2(b / hypot(b, d), a / hypot(a, c))`, i.e. the two *columns* are
/// normalised to unit length before the angle is taken. A naive
/// `-atan2(b, a)` agrees on every pure rotation and disagrees the moment the
/// two axes are scaled differently: `[65536, 66000, 0, 0, 65536, 0, …]`
/// measures `-35`, where `-atan2(b, a)` predicts `-45`. That file is the one
/// that separates the two rules, and a corpus of pure rotations cannot.
///
/// A zero column is treated as unit length, which is what makes the all-zero
/// matrix report `0` instead of a NaN.
#[must_use]
pub fn display_rotation(matrix: &[i32; 9]) -> f64 {
    let get = |i: usize| matrix.get(i).copied().unwrap_or(0);
    let (a, b) = (f64::from(get(0)), f64::from(get(1)));
    let (c, d) = (f64::from(get(3)), f64::from(get(4)));
    let scale_a = non_zero(a.hypot(c));
    let scale_b = non_zero(b.hypot(d));
    -(b / scale_b).atan2(a / scale_a).to_degrees()
}

/// A zero scale would make the normalisation a division by zero; the reference
/// reports `0` for an all-zero matrix, which is what unit scale produces.
fn non_zero(v: f64) -> f64 {
    if v == 0.0 { 1.0 } else { v }
}

/// Whether a matrix is the identity, and therefore not worth carrying.
///
/// The reference emits **no** `[SIDE_DATA]` block for an identity `tkhd`
/// matrix, which every unrotated file written by `ffmpeg` has. Verified on
/// `prog.mp4`, whose matrix is `[65536, 0, 0, 0, 65536, 0, 0, 0, 1073741824]`
/// and which reports no side data at all.
#[must_use]
pub fn is_identity_matrix(matrix: &[i32; 9]) -> bool {
    const ONE_16_16: i32 = 1 << 16;
    const ONE_2_30: i32 = 1 << 30;
    const IDENTITY: [i32; 9] = [ONE_16_16, 0, 0, 0, ONE_16_16, 0, 0, 0, ONE_2_30];
    *matrix == IDENTITY
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "the measured values are exact in binary")]
mod tests {
    use super::*;

    /// Every row is one `ffprobe 8.1` observation; see [`display_rotation`].
    #[test]
    fn rotation_matches_every_measured_matrix() {
        let cases: &[([i32; 9], i64)] = &[
            ([0, 65536, 0, -65536, 0, 0, 7_864_320, 0, 1 << 30], -90),
            (
                [-65536, 0, 0, 0, -65536, 0, 10_485_760, 7_864_320, 1 << 30],
                -180,
            ),
            ([0, -65536, 0, 65536, 0, 0, 0, 10_485_760, 1 << 30], 90),
            ([56755, 32768, 0, -32768, 56755, 0, 0, 0, 1 << 30], -30),
            ([-65536, 0, 0, 0, 65536, 0, 10_485_760, 0, 1 << 30], -180),
            ([i32::MIN, i32::MAX, 0, 0, 0, 0, 0, 0, 1 << 30], -135),
            ([0; 9], 0),
            // The discriminating case: unequal axis scales. `-atan2(b, a)`
            // would say -45 here; the reference says -35.
            ([65536, 66000, 0, 0, 65536, 0, 0, 0, 1 << 30], -35),
            ([6_553_600, 1, 0, 0, 65536, 0, 0, 0, 1 << 30], 0),
        ];
        for &(m, want) in cases {
            let got = display_rotation(&m).trunc() as i64;
            assert_eq!(got, want, "matrix {m:?} -> {}", display_rotation(&m));
        }
    }

    #[test]
    fn the_identity_is_the_only_identity() {
        assert!(is_identity_matrix(&[
            65536,
            0,
            0,
            0,
            65536,
            0,
            0,
            0,
            1 << 30
        ]));
        assert!(!is_identity_matrix(&[0; 9]));
        assert!(!is_identity_matrix(&[
            0,
            65536,
            0,
            -65536,
            0,
            0,
            7_864_320,
            0,
            1 << 30
        ]));
    }

    #[test]
    fn the_side_data_name_is_what_the_reference_prints() {
        assert_eq!(
            StreamSideData::DisplayMatrix([0; 9]).name(),
            "Display Matrix"
        );
    }
}
