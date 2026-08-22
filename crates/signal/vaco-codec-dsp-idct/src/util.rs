//! Shared, size-generic `NxN` array plumbing used by both [`crate::h264`] and
//! [`crate::hevc`].
//!
//! `clippy::indexing_slicing` is denied workspace-wide (plan 13 §2.2.1), so
//! every access here goes through `.get()`/`.get_mut()` or array
//! pattern-destructuring rather than the `[]` operator — both are provably
//! panic-free without a `#[allow]`. This is the one place that plumbing is
//! written, per D19: `h264` and `hevc` each supply their own 1-D transform but
//! share the row/column bookkeeping around it.

/// Build an `N x N` row-major matrix from a flat slice.
///
/// Short input is zero-padded; this only matters for malformed/adversarial
/// callers, since every real call site passes a slice of exactly `N*N`.
#[must_use]
pub(crate) fn from_flat<const N: usize>(flat: &[i32]) -> [[i32; N]; N] {
    core::array::from_fn(|r| {
        // `r < N` and `N` is always one of {2, 4, 8, 16, 32} here, so `r * N`
        // is far inside `i32`/`usize` range — no overflow.
        let start = r * N;
        core::array::from_fn(|c| flat.get(start + c).copied().unwrap_or(0))
    })
}

/// Scatter an `N x N` row-major matrix into a flat output slice.
///
/// A short `out` silently drops the tail rather than panicking; every real
/// call site passes a slice of exactly `N*N`.
pub(crate) fn to_flat<const N: usize>(m: &[[i32; N]; N], out: &mut [i32]) {
    for (r, row) in m.iter().enumerate() {
        let start = r * N;
        for (c, v) in row.iter().enumerate() {
            if let Some(slot) = out.get_mut(start + c) {
                *slot = *v;
            }
        }
    }
}

/// Matrix transpose. Used to turn "transform every row" into "transform every
/// column" without writing the column-gather logic twice.
#[must_use]
pub(crate) fn transpose<const N: usize>(m: &[[i32; N]; N]) -> [[i32; N]; N] {
    core::array::from_fn(|i| {
        core::array::from_fn(|j| m.get(j).and_then(|row| row.get(i)).copied().unwrap_or(0))
    })
}

/// Apply a 1-D transform to every row of an `N x N` matrix.
#[must_use]
pub(crate) fn map_rows<const N: usize>(
    m: [[i32; N]; N],
    f: impl Fn([i32; N]) -> [i32; N],
) -> [[i32; N]; N] {
    m.map(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_flat_then_to_flat_round_trips() {
        let flat: [i32; 16] = core::array::from_fn(|i| i32::try_from(i).unwrap_or(0));
        let m = from_flat::<4>(&flat);
        let mut out = [0i32; 16];
        to_flat(&m, &mut out);
        assert_eq!(flat, out);
    }

    #[test]
    fn transpose_is_involutive() {
        let flat: [i32; 16] = core::array::from_fn(|i| i32::try_from(i).unwrap_or(0) - 8);
        let m = from_flat::<4>(&flat);
        assert_eq!(transpose(&transpose(&m)), m);
    }

    #[test]
    fn short_input_is_zero_padded_not_panicking() {
        let m = from_flat::<4>(&[1, 2, 3]);
        assert_eq!(m.first().copied(), Some([1, 2, 3, 0]));
    }
}
