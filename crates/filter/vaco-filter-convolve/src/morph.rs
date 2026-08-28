//! Shared engine for `dilation` and `erosion` — one option class in the
//! reference (`ffmpeg -h filter=dilation` prints "erosion/dilation
//! `AVOptions`"), and, measured below, one formula with the sign of a
//! `min`/`max` flipped.
//!
//! Options: `coordinates` (`0..=255`, default `255`), `threshold0..3`
//! (`0..=65535`, default `65535`, one per plane).
//!
//! # Measured: `coordinates` is an 8-neighbour bitmask, raster order
//!
//! ```text
//! ffmpeg -f lavfi -i "color=black:s=5x5,format=gray8,geq=lum='if(eq(X,2)*eq(Y,2),100,0)'" \
//!   -vf "dilation=coordinates=1" -f rawvideo -pix_fmt gray8 -frames:v 1 - | xxd
//! ```
//!
//! (and the same with `2,4,8,16,32,64,128`) each grow the impulse into
//! exactly one neighbour, at the position consistent with bit `k` naming
//! offset `k` in raster order over the 3x3 neighbourhood *excluding
//! centre*: `(-1,-1) (-1,0) (-1,1) (0,-1) (0,1) (1,-1) (1,0) (1,1)` for bits
//! `1,2,4,8,16,32,64,128`. Default `255` sets all eight, the usual 3x3
//! dilation/erosion.
//!
//! # Measured: `threshold` caps the change, it does not gate it
//!
//! ```text
//! ffmpeg ... -vf "dilation=threshold0=10" ...
//! ```
//!
//! on the same impulse: neighbours of the `100` pixel become `10`, not `0`
//! and not `100` — `new = min(local_max, self + threshold)` for dilation
//! (symmetrically `max(local_min, self - threshold)` for erosion). The
//! centre pixel's own local maximum always includes itself (its neighbours
//! are `0`, yet it stays `100`), confirming self is always a candidate in
//! addition to whichever neighbours `coordinates` selects.
//!
//! # Border
//!
//! For `Dilate`/`Erode`: not separately modelled. Clamp-to-edge and "omit
//! the missing neighbour" give identical results for a `min`/`max`
//! combine, since a value can never change a running max/min by being
//! repeated — self is already a candidate (see above), so a corner's
//! clamped neighbours duplicate values already in the candidate set.
//! Confirmed against a corner-impulse probe matching this module's
//! [`crate::dilation`] tests.
//!
//! For `InflateAvg`/`DeflateAvg`: that argument does **not** carry over —
//! averaging is not immune to duplicated border values the way min/max is
//! (clamping a neighbour in *does* change an average). This needed its
//! own, separate measurement rather than inheriting the min/max reasoning.
//! Correction/pin, 2026-08-28 (same campaign, and same discriminating-
//! source discipline, as [`crate::edge`]'s `sobel` finding): a corner probe
//! with all-distinct values (`value = 1 + 10*row + col`, `5x5`) against
//! real `ffmpeg 8.1 -vf inflate` gives `5` at the corner. `apply_plane`'s
//! actual rule — clamp-to-edge via [`common::sample_clamped`] for each of
//! the fixed 8 offsets, **always** dividing by `8` (not by however many
//! offsets were actually in-bounds) — predicts exactly `5`. A competing
//! "omit the out-of-bounds offsets and divide by the count that's left"
//! hypothesis predicts `8` at the same pixel and is ruled out. So the
//! existing, already-shipped implementation is confirmed correct for
//! `InflateAvg`/`DeflateAvg` too, on a source that can actually tell the
//! two hypotheses apart — this was previously assumed by extension from
//! the min/max case rather than independently measured.

use crate::common;

/// Offsets `coordinates` bit `k` (`0..=7`) selects, in the reference's
/// raster order over the 3x3 neighbourhood excluding centre.
const OFFSETS: [(i32, i32); 8] = [
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Dilate,
    Erode,
    /// `inflate`: grow towards the local average of the fixed 8-neighbourhood
    /// when that average exceeds self. See [`crate::inflate`]'s doc for the
    /// probe that pinned this down (distinct from `Dilate`, which grows
    /// towards the local *maximum*, not the average).
    InflateAvg,
    /// `deflate`: the `InflateAvg` dual, shrinking towards the local average
    /// when it is below self. See [`crate::deflate`]'s doc.
    DeflateAvg,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MorphParams {
    pub coordinates: i32,
    pub threshold: i32,
}

/// One dilation/erosion/inflate/deflate pass over a whole plane.
///
/// `Dilate`/`Erode` combine the selected 8-neighbourhood by max/min;
/// `InflateAvg`/`DeflateAvg` combine it by **truncating average** instead
/// (measured — see [`crate::inflate`]'s doc: a `37.5` average probe returns
/// `37`, not `38`, ruling out round-to-nearest) and always consider the
/// fixed full 8-neighbourhood (`inflate`/`deflate` have no `coordinates`
/// option in the reference), never a `coordinates`-selected subset.
#[allow(
    clippy::integer_division,
    reason = "computing an average: the division is the whole point, not a \
              precision bug"
)]
pub(crate) fn apply_plane(
    rows: &[&[u8]],
    w: i32,
    h: i32,
    op: Op,
    params: MorphParams,
) -> Vec<Vec<u8>> {
    let selected: Vec<(i32, i32)> = match op {
        Op::Dilate | Op::Erode => OFFSETS
            .iter()
            .enumerate()
            .filter(|(bit, _)| (params.coordinates >> bit) & 1 != 0)
            .map(|(_, &o)| o)
            .collect(),
        Op::InflateAvg | Op::DeflateAvg => OFFSETS.to_vec(),
    };
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let self_v = i32::from(common::sample_clamped(rows, x, y, w, h));
            let neighbours = || {
                selected
                    .iter()
                    .map(|&(dy, dx)| i32::from(common::sample_clamped(rows, x + dx, y + dy, w, h)))
            };
            let value = match op {
                Op::Dilate => {
                    let extreme = neighbours().fold(self_v, i32::max);
                    extreme.min(self_v.saturating_add(params.threshold))
                }
                Op::Erode => {
                    let extreme = neighbours().fold(self_v, i32::min);
                    extreme.max(self_v.saturating_sub(params.threshold))
                }
                Op::InflateAvg => {
                    let avg = neighbours().sum::<i32>() / 8;
                    if avg > self_v {
                        avg.min(self_v.saturating_add(params.threshold))
                    } else {
                        self_v
                    }
                }
                Op::DeflateAvg => {
                    let avg = neighbours().sum::<i32>() / 8;
                    if avg < self_v {
                        avg.max(self_v.saturating_sub(params.threshold))
                    } else {
                        self_v
                    }
                }
            };
            row.push(u8::try_from(value.clamp(0, 255)).unwrap_or(0));
        }
        out.push(row);
    }
    out
}

/// Greyscale erode/dilate against an arbitrary structuring element, for
/// [`crate::morpho`]. Unlike [`apply_plane`]'s fixed 3x3 `coordinates` mask,
/// **self is not an implicit candidate** — only the offsets `offsets` names
/// (which may or may not include `(0, 0)`) are combined. Measured (see
/// `morpho`'s doc): a structuring element whose own centre pixel is dark
/// (excluded) makes the output pixel forget its own input value entirely,
/// which `dilation`/`erosion`'s always-include-self rule never does — a
/// real difference between the two engines, not a simplification of one.
///
/// No `threshold` cap either: `morpho` has no such option in the reference.
#[must_use]
pub(crate) fn apply_structured(
    rows: &[&[u8]],
    w: i32,
    h: i32,
    op: Op,
    offsets: &[(i32, i32)],
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let mut acc: Option<i32> = None;
            for &(dy, dx) in offsets {
                let v = i32::from(common::sample_clamped(rows, x + dx, y + dy, w, h));
                acc = Some(match (acc, op) {
                    (None, _) => v,
                    (Some(a), Op::Dilate | Op::InflateAvg) => a.max(v),
                    (Some(a), Op::Erode | Op::DeflateAvg) => a.min(v),
                });
            }
            // No active offset at all: the reference has no defined answer
            // for an all-dark structuring element; fall back to the input
            // pixel rather than fabricating a 0, which would look like a
            // real (and wrong) measured value.
            let value = acc.unwrap_or_else(|| i32::from(common::sample_clamped(rows, x, y, w, h)));
            row.push(u8::try_from(value.clamp(0, 255)).unwrap_or(0));
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn impulse(size: usize, cx: usize, cy: usize, value: u8) -> Vec<Vec<u8>> {
        let mut img = vec![vec![0u8; size]; size];
        if let Some(row) = img.get_mut(cy)
            && let Some(px) = row.get_mut(cx)
        {
            *px = value;
        }
        img
    }

    /// Pinned against the reference probe in this module's doc.
    #[test]
    fn default_dilation_grows_to_all_eight_neighbours() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = apply_plane(
            &rows,
            5,
            5,
            Op::Dilate,
            MorphParams {
                coordinates: 255,
                threshold: 65535,
            },
        );
        assert_eq!(out[2][2], 100);
        for (dy, dx) in [
            (-1i32, -1i32),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ] {
            let y = (2 + dy) as usize;
            let x = (2 + dx) as usize;
            assert_eq!(out[y][x], 100, "({x},{y})");
        }
        assert_eq!(out[0][0], 0);
    }

    /// Pinned against the reference probe in this module's doc: each single
    /// bit grows into exactly one raster-order offset.
    #[test]
    fn coordinates_bit_selects_the_documented_offset() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        for (bit, &(dy, dx)) in OFFSETS.iter().enumerate() {
            let out = apply_plane(
                &rows,
                5,
                5,
                Op::Dilate,
                MorphParams {
                    coordinates: 1 << bit,
                    threshold: 65535,
                },
            );
            // A pixel P grows from the centre exactly when P + offset ==
            // centre (P reads the centre as its selected neighbour), i.e.
            // P == centre - offset.
            let y = usize::try_from(2 - dy).unwrap();
            let x = usize::try_from(2 - dx).unwrap();
            assert_eq!(out[y][x], 100, "bit {bit} -> offset ({dx},{dy})");
        }
    }

    /// Pinned against the reference probe in this module's doc: threshold
    /// caps growth rather than gating it, and self is always a candidate.
    #[test]
    fn threshold_caps_the_change() {
        let img = impulse(5, 2, 2, 100);
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let out = apply_plane(
            &rows,
            5,
            5,
            Op::Dilate,
            MorphParams {
                coordinates: 255,
                threshold: 10,
            },
        );
        assert_eq!(out[2][2], 100);
        assert_eq!(out[1][2], 10);
        assert_eq!(out[2][1], 10);
    }

    /// Independent oracle: erosion and dilation are duals under
    /// inversion — `erode(x) = 255 - dilate(255 - x)` — a property of
    /// min/max under negation, not a re-derivation of either kernel.
    #[test]
    fn erosion_and_dilation_are_duals_under_inversion() {
        let img: Vec<Vec<u8>> = (0..5)
            .map(|y| (0..5).map(|x| ((x * 37 + y * 53) % 251) as u8).collect())
            .collect();
        let inverted: Vec<Vec<u8>> = img
            .iter()
            .map(|r| r.iter().map(|&v| 255 - v).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let inv_rows: Vec<&[u8]> = inverted.iter().map(Vec::as_slice).collect();
        let params = MorphParams {
            coordinates: 255,
            threshold: 65535,
        };
        let eroded = apply_plane(&rows, 5, 5, Op::Erode, params);
        let dilated_inverted = apply_plane(&inv_rows, 5, 5, Op::Dilate, params);
        for y in 0..5 {
            for x in 0..5 {
                assert_eq!(eroded[y][x], 255 - dilated_inverted[y][x], "({x},{y})");
            }
        }
    }

    /// Pinned against real `ffmpeg 8.1 -vf inflate`/`deflate`/`dilation`/
    /// `erosion` on an all-distinct-values corner probe (see this module's
    /// doc): confirms clamp-to-edge with a *fixed* divide-by-8 for the
    /// averaging ops, ruling out an "omit the out-of-bounds offsets and
    /// divide by however many are left" alternative, which predicts a
    /// different value (`8`, not `5`) at this exact corner.
    #[test]
    fn corner_probe_matches_the_reference_for_all_four_ops() {
        let img: Vec<Vec<u8>> = (0..5)
            .map(|y| (0..5).map(|x| (1 + 10 * y + x) as u8).collect())
            .collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let default_params = MorphParams {
            coordinates: 255,
            threshold: 65535,
        };
        let dilated = apply_plane(&rows, 5, 5, Op::Dilate, default_params);
        let eroded = apply_plane(&rows, 5, 5, Op::Erode, default_params);
        let inflated = apply_plane(&rows, 5, 5, Op::InflateAvg, default_params);
        let deflated = apply_plane(&rows, 5, 5, Op::DeflateAvg, default_params);
        assert_eq!(dilated[0][0], 12);
        assert_eq!(eroded[0][0], 1);
        assert_eq!(inflated[0][0], 5);
        assert_eq!(deflated[0][0], 1);
    }
}
