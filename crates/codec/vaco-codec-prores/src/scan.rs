//! Inverse slice and block scanning — RDD 36 SS7.2.

/// Progressive block scan pattern, Figure 4. `PROGRESSIVE_SCAN[v * 8 + u]` is
/// the scanned-array index of coefficient `(v, u)` — i.e. `scan[v][u]` in the
/// spec's own notation, used as `QF[v][u] = QFS[scan[v][u]]`.
#[rustfmt::skip]
pub(crate) const PROGRESSIVE_SCAN: [usize; 64] = [
     0,  1,  4,  5, 16, 17, 21, 22,
     2,  3,  6,  7, 18, 20, 23, 28,
     8,  9, 12, 13, 19, 24, 27, 29,
    10, 11, 14, 15, 25, 26, 30, 31,
    32, 33, 37, 38, 45, 46, 53, 54,
    34, 36, 39, 44, 47, 52, 55, 60,
    35, 40, 43, 48, 51, 56, 59, 61,
    41, 42, 49, 50, 57, 58, 62, 63,
];

/// Interlaced (field-picture) block scan pattern, Figure 5.
#[rustfmt::skip]
pub(crate) const INTERLACED_SCAN: [usize; 64] = [
     0,  2,  8, 10, 32, 34, 35, 41,
     1,  3,  9, 11, 33, 36, 40, 42,
     4,  6, 12, 14, 37, 39, 43, 49,
     5,  7, 13, 15, 38, 44, 48, 50,
    16, 18, 19, 25, 45, 47, 51, 57,
    17, 20, 24, 26, 46, 52, 56, 58,
    21, 23, 27, 30, 53, 55, 59, 62,
    22, 28, 29, 31, 54, 60, 61, 63,
];

/// Gather one macroblock/block's scanned quantized DCT coefficient array
/// `QFS[]` out of a color component's flat `scannedCoeffs[]` array, inverting
/// the slice-scan formula of SS7.2.1:
/// `QFSm,b[n] = scannedCoeffs[nB * sliceSizeInMb * n + nB * m + b]`.
pub(crate) fn gather_block_scanned(
    scanned: &[i32],
    n_b: usize,
    slice_size_in_mb: usize,
    m: usize,
    b: usize,
) -> [i32; 64] {
    let mut qfs = [0i32; 64];
    let stride = n_b.saturating_mul(slice_size_in_mb);
    let base = n_b.saturating_mul(m).saturating_add(b);
    for (n, slot) in qfs.iter_mut().enumerate() {
        let idx = stride.saturating_mul(n).saturating_add(base);
        *slot = scanned.get(idx).copied().unwrap_or(0);
    }
    qfs
}

/// Invert the block scan: `QF[v][u] = QFS[scan[v][u]]`, returned row-major
/// (`out[v * 8 + u]`).
pub(crate) fn inverse_block_scan(qfs: &[i32; 64], scan_table: &[usize; 64]) -> [i32; 64] {
    let mut qf = [0i32; 64];
    for (dst, &src) in qf.iter_mut().zip(scan_table.iter()) {
        *dst = qfs.get(src).copied().unwrap_or(0);
    }
    qf
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn is_permutation_of_0_63(table: &[usize; 64]) -> bool {
        let set: HashSet<usize> = table.iter().copied().collect();
        set.len() == 64 && table.iter().all(|&v| v < 64)
    }

    #[test]
    fn progressive_scan_is_a_permutation() {
        assert!(is_permutation_of_0_63(&PROGRESSIVE_SCAN));
    }

    #[test]
    fn interlaced_scan_is_a_permutation() {
        assert!(is_permutation_of_0_63(&INTERLACED_SCAN));
    }

    #[test]
    fn scan_position_zero_is_dc() {
        // Both patterns' (v=0, u=0) entry must be scanned index 0 (the DC
        // coefficient always leads the scanned array, SS7.2.1 Figure 3).
        assert_eq!(PROGRESSIVE_SCAN[0], 0);
        assert_eq!(INTERLACED_SCAN[0], 0);
    }

    #[test]
    fn gather_and_inverse_scan_round_trip_identity_scan() {
        // With a trivial identity scan and a single-macroblock, single-block
        // slice, gather+inverse should just be the identity on a 64-length
        // input.
        let scanned: Vec<i32> = (0..64).collect();
        let qfs = gather_block_scanned(&scanned, 1, 1, 0, 0);
        assert_eq!(qfs.to_vec(), scanned);
        let identity: [usize; 64] = std::array::from_fn(|i| i);
        let qf = inverse_block_scan(&qfs, &identity);
        assert_eq!(qf.to_vec(), scanned);
    }
}
