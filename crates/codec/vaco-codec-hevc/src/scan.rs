//! Coefficient scan order generation, ITU-T H.265 §6.5.3/§6.5.4.
//!
//! Generated algorithmically rather than transcribed as static tables, from
//! the same generator process the HM reference decoder uses
//! (`TComRom.cpp::ScanGenerator`, BSD-3, Tier A — see the crate doc): a
//! generator with (line, column) state advances one way for up-right
//! diagonal scan, another for horizontal, another for vertical. Deriving the
//! table this way is checkable against a small property no transcription
//! error can hide from (`tests::every_scan_is_a_permutation` below), which a
//! hand-copied table cannot offer the same way a VLC's prefix-free property
//! does.

/// One of the three coefficient scan orders a transform block can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanOrder {
    /// Up-right diagonal, §6.5.3 — the only order used above 8x8, and the
    /// only one used for chroma at any size.
    Diag,
    /// §6.5.4 with `sPos`'s row/column roles as printed (horizontal-first).
    Horiz,
    /// §6.5.4 with row/column swapped relative to [`ScanOrder::Horiz`].
    Vert,
}

/// Generate the scan order for a `size x size` block: `out[scanPos]` is the
/// `(x, y)` raster position visited at `scanPos`.
///
/// `size` must be a power of two; any other value returns an empty vector
/// rather than panicking (this is an internal derivation, never fed
/// attacker-controlled sizes directly — callers only ever pass 1, 2, 4 or 8,
/// the transform/group sizes HEVC defines).
#[must_use]
pub(crate) fn generate(size: usize, order: ScanOrder) -> Vec<(u8, u8)> {
    if size == 0 || !size.is_power_of_two() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let (mut line, mut col) = (0usize, 0usize);
    for _ in 0..size.saturating_mul(size) {
        out.push((col_u8(col), col_u8(line)));
        advance(order, size, &mut line, &mut col);
    }
    out
}

/// The scan `residual_coding()` actually walks for a `size x size` transform
/// block, HM's `SCAN_GROUPED_4x4`: 4x4 sub-blocks visited in `order`
/// (`sub_pos0`, `sub_pos1`, ...), and *within* each sub-block, its own 16
/// positions visited in `order` again.
///
/// This is **not** the same sequence [`generate`] produces for `size > 4`.
/// A plain diagonal (or horizontal/vertical) scan run once at the full block
/// size interleaves positions from different 4x4 sub-blocks along a shared
/// anti-diagonal — e.g. `size=8`'s `x + y == 4` anti-diagonal contains both
/// `(4, 0)` (sub-block `(1, 0)`) and `(1, 3)` (sub-block `(0, 0)`) — which is
/// exactly wrong for `residual_coding()`: every syntax element in the
/// significance/greater-than/remaining loop is either a sub-block-level
/// construct (`coded_sub_block_flag`, `patternSigCtx`) or depends on a scan
/// position landing at a sub-block boundary (the forced-significant special
/// case, the `last_scan_pos >> 4` subset derivation) — all of which silently
/// desynchronise if the flat coefficient order does not respect sub-block
/// grouping. `size <= 4` has exactly one sub-block, so it degenerates to
/// [`generate`] exactly.
#[must_use]
pub(crate) fn generate_grouped(size: usize, order: ScanOrder) -> Vec<(u8, u8)> {
    if size <= 4 {
        return generate(size, order);
    }
    let groups = size >> 2;
    let group_scan = generate(groups, order);
    let local_scan = generate(4, order);
    let mut out = Vec::new();
    for &(gx, gy) in &group_scan {
        for &(lx, ly) in &local_scan {
            out.push((gx * 4 + lx, gy * 4 + ly));
        }
    }
    out
}

fn col_u8(v: usize) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

/// Advance the generator's `(line, column)` state by one step, matching
/// `ScanGenerator::GetNextIndex`'s post-advance exactly (that function
/// returns the *current* position, then updates state for next time — this
/// function is only the update half, called after the caller has recorded
/// the current position).
fn advance(order: ScanOrder, size: usize, line: &mut usize, col: &mut usize) {
    match order {
        ScanOrder::Diag => {
            if *col == size - 1 || *line == 0 {
                *line += *col + 1;
                *col = 0;
                if *line >= size {
                    *col += *line - (size - 1);
                    *line = size - 1;
                }
            } else {
                *col += 1;
                *line -= 1;
            }
        }
        ScanOrder::Horiz => {
            if *col == size - 1 {
                *line += 1;
                *col = 0;
            } else {
                *col += 1;
            }
        }
        ScanOrder::Vert => {
            if *line == size - 1 {
                *col += 1;
                *line = 0;
            } else {
                *line += 1;
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test fixtures index/slice small fixed vectors; an out-of-range access here is itself a test failure"
)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_scan_is_a_permutation() {
        for size in [1usize, 2, 4, 8, 16, 32] {
            for order in [ScanOrder::Diag, ScanOrder::Horiz, ScanOrder::Vert] {
                let scan = generate(size, order);
                assert_eq!(scan.len(), size * size, "size={size} order={order:?}");
                let set: HashSet<_> = scan.iter().copied().collect();
                assert_eq!(set.len(), scan.len(), "size={size} order={order:?} has duplicates");
                for &(x, y) in &scan {
                    assert!((x as usize) < size && (y as usize) < size);
                }
            }
        }
    }

    #[test]
    fn diag_4x4_matches_the_known_up_right_order() {
        // ITU-T H.265 Figure 6-11 / the well-known HEVC 4x4 diagonal scan,
        // as (x, y) raster pairs in scan order.
        let scan = generate(4, ScanOrder::Diag);
        let expected = [
            (0, 0),
            (0, 1),
            (1, 0),
            (0, 2),
            (1, 1),
            (2, 0),
            (0, 3),
            (1, 2),
            (2, 1),
            (3, 0),
            (1, 3),
            (2, 2),
            (3, 1),
            (2, 3),
            (3, 2),
            (3, 3),
        ];
        assert_eq!(scan, expected);
    }

    #[test]
    fn diag_starts_at_dc_and_ends_at_the_far_corner() {
        for size in [4usize, 8, 16, 32] {
            let scan = generate(size, ScanOrder::Diag);
            assert_eq!(scan.first(), Some(&(0, 0)));
            let last = (col_u8(size - 1), col_u8(size - 1));
            assert_eq!(scan.last(), Some(&last));
        }
    }

    #[test]
    fn horiz_is_row_major_and_vert_is_column_major() {
        let h = generate(4, ScanOrder::Horiz);
        assert_eq!(h[..4], [(0, 0), (1, 0), (2, 0), (3, 0)]);
        let v = generate(4, ScanOrder::Vert);
        assert_eq!(v[..4], [(0, 0), (0, 1), (0, 2), (0, 3)]);
    }

    #[test]
    fn grouped_scan_is_a_permutation_and_completes_each_sub_block_before_the_next() {
        for size in [8usize, 16, 32] {
            for order in [ScanOrder::Diag, ScanOrder::Horiz, ScanOrder::Vert] {
                let scan = generate_grouped(size, order);
                assert_eq!(scan.len(), size * size);
                let set: HashSet<_> = scan.iter().copied().collect();
                assert_eq!(set.len(), scan.len(), "size={size} order={order:?} has duplicates");
                // Every run of 16 consecutive scan positions stays within one
                // 4x4 sub-block (the property a flat full-size scan breaks).
                for chunk in scan.chunks(16) {
                    let (gx, gy) = (chunk[0].0 >> 2, chunk[0].1 >> 2);
                    for &(x, y) in chunk {
                        assert_eq!((x >> 2, y >> 2), (gx, gy), "size={size} order={order:?} chunk crosses a sub-block");
                    }
                }
            }
        }
    }

    #[test]
    fn grouped_scan_matches_plain_scan_at_size_4() {
        for order in [ScanOrder::Diag, ScanOrder::Horiz, ScanOrder::Vert] {
            assert_eq!(generate_grouped(4, order), generate(4, order));
        }
    }
}
