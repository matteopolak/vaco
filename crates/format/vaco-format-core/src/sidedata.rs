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

impl DisplayTransform {
    /// The exact matrix `ffmpeg 9.0.1` itself computes for this transform —
    /// the same eight values [`dihedral_transform_from_matrix`] decomposes,
    /// run in reverse. Used to build a real `tkhd`-shaped matrix for a
    /// `-display_rotation`/`-display_hflip`/`-display_vflip` override that
    /// a caller wants to *preserve* into an output container rather than
    /// apply to pixels (a `-c copy` remux, or `-noautorotate`) — see
    /// `StreamSpec::display_matrix`'s own doc.
    #[must_use]
    pub const fn to_matrix(self) -> [i32; 9] {
        const P: i32 = 1 << 16;
        const N: i32 = -(1 << 16);
        const W: i32 = 1 << 30;
        let (a, b, c, d) = match self {
            Self::Hflip => (N, 0, 0, P),
            Self::Vflip => (P, 0, 0, N),
            Self::Rotate180 => (N, 0, 0, N),
            Self::TransposeClock => (0, P, N, 0),
            Self::TransposeCclock => (0, N, P, 0),
            Self::TransposeClockFlip => (0, N, N, 0),
            Self::TransposeCclockFlip => (0, P, P, 0),
        };
        [a, b, 0, c, d, 0, 0, 0, W]
    }
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

/// One of the eight rigid transforms a display matrix or `-display_rotation`/
/// `-display_hflip`/`-display_vflip` can express: the four multiples of 90°,
/// each with or without a reflection. Named after
/// `vaco-filter-video-geometry`'s own `transpose` directions plus
/// `hflip`/`vflip`, because that is exactly the filter chain each variant
/// becomes — see that crate's `transpose` module doc for what each direction
/// computes on real pixels.
///
/// Deliberately **not** a general rotation-by-any-angle: real devices only
/// ever write one of these eight (0/90/180/270, optionally mirrored), and the
/// reference's own handling of anything else is a best-effort, interpolated
/// `rotate`-style filter with no fixed output geometry (measured: `ffmpeg
/// 9.0.1` prints `Odd rotation angle.` to stderr and still alters pixel
/// values for a 45° matrix). Reproducing that is out of scope; a matrix that
/// is not exactly one of these eight is reported as `None` by both
/// constructors below, and a caller should leave the frame unrotated rather
/// than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayTransform {
    Hflip,
    Vflip,
    /// `hflip` then `vflip` (order does not matter — both are their own
    /// inverse and commute), the pixel effect of a 180° rotation.
    Rotate180,
    /// `transpose=dir=clock` — 90° clockwise, no reflection.
    TransposeClock,
    /// `transpose=dir=cclock` — 90° counter-clockwise, no reflection.
    TransposeCclock,
    /// `transpose=dir=clock_flip`.
    TransposeClockFlip,
    /// `transpose=dir=cclock_flip` (`vaco-filter-video-geometry::transpose`'s
    /// own default direction — a plain matrix transpose).
    TransposeCclockFlip,
}

/// Decompose a display matrix into one of the eight [`DisplayTransform`]s, or
/// `None` if it is the identity (nothing to do) or is not exactly one of
/// them (an arbitrary angle — see [`DisplayTransform`]'s own doc for why
/// that is left alone rather than approximated).
///
/// **Measured against real `ffmpeg 9.0.1`**, not derived: every one of the
/// eight matrices below is `ffprobe`'s own `displaymatrix` side-data dump for
/// a real `-display_rotation`/`-display_hflip`/`-display_vflip` combination
/// remuxed with `-c copy` (so the printed matrix is exactly what the
/// reference itself computes) — sixteen combinations were run, and every one
/// produced one of these eight matrices.
///
/// **This table's rotation-only entries (`TransposeClock`/`TransposeCclock`)
/// were wrong once, and the bug survived this crate's own unit tests.** The
/// first version derived them by composing two independently-measured facts
/// — [`display_rotation`]'s clockwise-angle convention for a matrix already
/// in a container, and the CLI's own documented counter-clockwise
/// convention for `-display_rotation` — on the theory that one is the
/// other's negation. That composition swapped `TransposeClock` and
/// `TransposeCclock`, and a unit test written from the same theory naturally
/// agreed with itself. It was only caught by a full pipeline test: decoding
/// a real, asymmetric H.264 frame through `vaco-codec-h264`, rotating it
/// through this function and `vaco-filter-video-geometry::transpose`, and
/// comparing the actual output bytes against real `ffmpeg`'s own decode of
/// the identical file — which disagreed on every rotated pixel. Every
/// matrix below is now instead read directly off `ffprobe`'s raw
/// `displaymatrix` dump for the plain (no-flip) `-display_rotation 90` and
/// `-display_rotation -90` cases specifically, closing the gap the
/// composition papered over. The four reflection entries were never wrong:
/// their matrices came from a real `ffprobe` dump from the start, because a
/// reflection has no clockwise/counter-clockwise convention to get
/// backwards.
#[must_use]
pub fn dihedral_transform_from_matrix(matrix: &[i32; 9]) -> Option<DisplayTransform> {
    const P: i32 = 1 << 16;
    const N: i32 = -(1 << 16);
    if is_identity_matrix(matrix) {
        return None;
    }
    let (a, b, c, d) = (matrix[0], matrix[1], matrix[3], matrix[4]);
    match (a, b, c, d) {
        (N, 0, 0, P) => Some(DisplayTransform::Hflip),
        (P, 0, 0, N) => Some(DisplayTransform::Vflip),
        (N, 0, 0, N) => Some(DisplayTransform::Rotate180),
        (0, N, P, 0) => Some(DisplayTransform::TransposeCclock),
        (0, P, N, 0) => Some(DisplayTransform::TransposeClock),
        (0, N, N, 0) => Some(DisplayTransform::TransposeClockFlip),
        (0, P, P, 0) => Some(DisplayTransform::TransposeCclockFlip),
        _ => None,
    }
}

/// The [`DisplayTransform`] `-display_rotation <degrees>` (counter-clockwise,
/// the CLI option's own documented convention — measured `ffmpeg 9.0.1 -h
/// full`) plus `-display_hflip`/`-display_vflip` (each applied *after* the
/// rotation, per the same documented text) resolve to, or `None` for the
/// identity or an angle that is not a multiple of 90° within rounding
/// (`epsilon` degrees — see [`DisplayTransform`]'s own doc for why this
/// workspace does not attempt anything else).
///
/// See [`dihedral_transform_from_matrix`]'s doc for how this table was
/// measured: sixteen real `ffmpeg` runs (four angles × the four
/// `hflip`/`vflip` combinations), read back pixel-for-pixel.
#[must_use]
pub fn dihedral_transform_from_angle_and_flips(
    degrees_ccw: f64,
    hflip: bool,
    vflip: bool,
) -> Option<DisplayTransform> {
    use DisplayTransform::{
        Hflip, Rotate180, TransposeClock, TransposeCclock, TransposeClockFlip, TransposeCclockFlip, Vflip,
    };
    const EPSILON: f64 = 0.01;
    let normalised = degrees_ccw.rem_euclid(360.0);
    let quadrant = (normalised / 90.0).round();
    if (normalised - quadrant * 90.0).abs() > EPSILON {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "quadrant is one of 0.0/1.0/2.0/3.0/4.0 by construction above"
    )]
    let quadrant = (quadrant as i32).rem_euclid(4);
    let transform = match (quadrant, hflip, vflip) {
        (0, false, false) => return None,
        (0, true, false) => Hflip,
        (0, false, true) => Vflip,
        (0, true, true) => Rotate180,
        (1, false, false) => TransposeCclock,
        (1, true, false) => TransposeClockFlip,
        (1, false, true) => TransposeCclockFlip,
        (1, true, true) => TransposeClock,
        (2, false, false) => Rotate180,
        (2, true, false) => Vflip,
        (2, false, true) => Hflip,
        (2, true, true) => return None,
        (3, false, false) => TransposeClock,
        (3, true, false) => TransposeCclockFlip,
        (3, false, true) => TransposeClockFlip,
        (3, true, true) => TransposeCclock,
        _ => unreachable!("quadrant is 0..=3 by construction above"),
    };
    Some(transform)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "the measured values are exact in binary")]
mod tests {
    use super::*;

    const P: i32 = 1 << 16;
    const N: i32 = -(1 << 16);

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

    /// Every row is a real `ffmpeg 9.0.1` run: `ffmpeg -display_rotation
    /// <deg> [-display_hflip] [-display_vflip] -i <4x6 test pattern> -c copy
    /// -f mp4 out.mp4`, then `ffprobe -show_streams out.mp4`'s own printed
    /// `displaymatrix`. See `dihedral_transform_from_matrix`'s own doc.
    #[test]
    fn matches_every_measured_display_rotation_matrix() {
        use DisplayTransform::{
            Hflip, Rotate180, TransposeClock, TransposeCclock, TransposeClockFlip,
            TransposeCclockFlip, Vflip,
        };
        const P: i32 = 65536;
        const N: i32 = -65536;
        let cases: &[([i32; 9], Option<DisplayTransform>)] = &[
            ([P, 0, 0, 0, P, 0, 0, 0, 1 << 30], None), // identity
            ([N, 0, 0, 0, P, 0, 0, 0, 1 << 30], Some(Hflip)),
            ([P, 0, 0, 0, N, 0, 0, 0, 1 << 30], Some(Vflip)),
            ([N, 0, 0, 0, N, 0, 0, 0, 1 << 30], Some(Rotate180)),
            ([0, N, 0, P, 0, 0, 0, 0, 1 << 30], Some(TransposeCclock)),
            ([0, P, 0, N, 0, 0, 0, 0, 1 << 30], Some(TransposeClock)),
            ([0, N, 0, N, 0, 0, 0, 0, 1 << 30], Some(TransposeClockFlip)),
            ([0, P, 0, P, 0, 0, 0, 0, 1 << 30], Some(TransposeCclockFlip)),
        ];
        for (matrix, want) in cases {
            assert_eq!(
                dihedral_transform_from_matrix(matrix),
                *want,
                "matrix {matrix:?}"
            );
        }
    }

    /// `to_matrix` is the reverse of `dihedral_transform_from_matrix`'s own
    /// table -- checked against the *same* real, `ffprobe`-measured literal
    /// matrices that test pins, not merely round-tripped through this
    /// crate's own two functions. A round trip alone would not have caught
    /// the `TransposeClock`/`TransposeCclock` swap this file's own history
    /// records: swapping both functions' tables the same way cancels out
    /// under a round trip and still agrees with itself.
    #[test]
    fn to_matrix_matches_the_same_measured_literals() {
        use DisplayTransform::{
            Hflip, Rotate180, TransposeClock, TransposeCclock, TransposeClockFlip, TransposeCclockFlip, Vflip,
        };
        let cases: &[(DisplayTransform, [i32; 9])] = &[
            (Hflip, [N, 0, 0, 0, P, 0, 0, 0, 1 << 30]),
            (Vflip, [P, 0, 0, 0, N, 0, 0, 0, 1 << 30]),
            (Rotate180, [N, 0, 0, 0, N, 0, 0, 0, 1 << 30]),
            (TransposeClock, [0, P, 0, N, 0, 0, 0, 0, 1 << 30]),
            (TransposeCclock, [0, N, 0, P, 0, 0, 0, 0, 1 << 30]),
            (TransposeClockFlip, [0, N, 0, N, 0, 0, 0, 0, 1 << 30]),
            (TransposeCclockFlip, [0, P, 0, P, 0, 0, 0, 0, 1 << 30]),
        ];
        for (transform, want) in cases {
            assert_eq!(transform.to_matrix(), *want, "{transform:?}");
            // And the round trip, as a second, weaker check.
            assert_eq!(dihedral_transform_from_matrix(want), Some(*transform));
        }
    }

    #[test]
    fn rejects_a_matrix_that_is_not_exactly_one_of_the_eight() {
        // The real 45-degree matrix ffmpeg 9.0.1 computes and warns about
        // ("Odd rotation angle.") rather than a hand-picked non-example.
        let odd = [46_341, 46_341, 0, -46_341, 46_341, 0, 0, 0, 1 << 30];
        assert_eq!(dihedral_transform_from_matrix(&odd), None);
    }

    /// All sixteen `-display_rotation <deg> [-display_hflip] [-display_vflip]`
    /// combinations actually run against real `ffmpeg 9.0.1`, read back
    /// pixel-for-pixel from a 4x6 asymmetric test pattern. See
    /// `dihedral_transform_from_angle_and_flips`'s own doc.
    #[test]
    fn matches_every_measured_display_rotation_cli_combination() {
        use DisplayTransform::{
            Hflip, Rotate180, TransposeClock, TransposeCclock, TransposeClockFlip,
            TransposeCclockFlip, Vflip,
        };
        let cases: &[(f64, bool, bool, Option<DisplayTransform>)] = &[
            (0.0, false, false, None),
            (0.0, true, false, Some(Hflip)),
            (0.0, false, true, Some(Vflip)),
            (0.0, true, true, Some(Rotate180)),
            (90.0, false, false, Some(TransposeCclock)),
            (90.0, true, false, Some(TransposeClockFlip)),
            (90.0, false, true, Some(TransposeCclockFlip)),
            (90.0, true, true, Some(TransposeClock)),
            (-90.0, false, false, Some(TransposeClock)),
            (-90.0, true, false, Some(TransposeCclockFlip)),
            (-90.0, false, true, Some(TransposeClockFlip)),
            (-90.0, true, true, Some(TransposeCclock)),
            (180.0, false, false, Some(Rotate180)),
            (180.0, true, false, Some(Vflip)),
            (180.0, false, true, Some(Hflip)),
            (180.0, true, true, None),
            // 270 must agree with -90 (same physical angle, mod 360).
            (270.0, false, false, Some(TransposeClock)),
        ];
        for (deg, hflip, vflip, want) in cases {
            assert_eq!(
                dihedral_transform_from_angle_and_flips(*deg, *hflip, *vflip),
                *want,
                "deg={deg} hflip={hflip} vflip={vflip}"
            );
        }
    }

    #[test]
    fn rejects_a_non_multiple_of_90() {
        assert_eq!(
            dihedral_transform_from_angle_and_flips(45.0, false, false),
            None
        );
        assert_eq!(
            dihedral_transform_from_angle_and_flips(1.0, false, false),
            None
        );
    }

    #[test]
    fn the_side_data_name_is_what_the_reference_prints() {
        assert_eq!(
            StreamSideData::DisplayMatrix([0; 9]).name(),
            "Display Matrix"
        );
    }
}
