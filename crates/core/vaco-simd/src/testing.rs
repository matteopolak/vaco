//! The differential harness: our clean-room `checkasm` equivalent.
//!
//! Lives here rather than in a test file so every kernel crate gets it for free,
//! and so `vaco-checkasm` (tools) and the per-crate proptests exercise exactly
//! the same corpus.
//!
//! Two halves:
//!
//! * [`edge_patterns`] and [`EDGE_LENGTHS`] — the deterministic sweep. Random
//!   input finds average-case bugs; the interesting SIMD bugs live at
//!   saturation boundaries, at `0`/`MAX`, at single bit positions, and in the
//!   loop tail. This sweep hits all four every time, with no seed to reproduce.
//! * [`check_binary_u8`] and friends — the drivers that run a vector op and its
//!   scalar reference over that sweep and demand bit-identical output.
//!
//! **Integer kernels must be bit-identical**, not close. `assert_close` exists
//! only for float kernels, and any use of it must state its tolerance and why.

#![allow(
    clippy::panic,
    clippy::indexing_slicing,
    reason = "this module's job is to fail loudly and precisely when a kernel diverges"
)]

/// Lengths the sweep runs at: every length from 0 to `3 * 64 + 1`.
///
/// Covers zero, sub-vector, exactly-one-vector, and multi-vector-plus-tail at
/// every native width the substrate has (16, 32 and 64 bytes).
pub const EDGE_LENGTHS: core::ops::RangeInclusive<usize> = 0..=(3 * 64 + 1);

/// The byte patterns the sweep runs, for a given length.
///
/// All-zero, all-`MAX`, alternating `00`/`FF`, alternating `FF`/`00`, a single
/// set bit walked through every bit position, and a counting ramp.
#[must_use]
pub fn edge_patterns(len: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    out.push(vec![0x00; len]);
    out.push(vec![0xFF; len]);
    out.push(
        (0..len)
            .map(|i| if i % 2 == 0 { 0x00 } else { 0xFF })
            .collect(),
    );
    out.push(
        (0..len)
            .map(|i| if i % 2 == 0 { 0xFF } else { 0x00 })
            .collect(),
    );
    out.push((0..len).map(|i| (i & 0xFF) as u8).collect());
    out.push(
        (0..len)
            .map(|i| 0xFFu8.wrapping_sub((i & 0xFF) as u8))
            .collect(),
    );
    for bit in 0..8u32 {
        out.push(vec![1u8 << bit; len]);
        out.push(
            (0..len)
                .map(|i| if i % 3 == 0 { 1u8 << bit } else { 0 })
                .collect(),
        );
    }
    out
}

/// Assert two integer kernels produced identical output, reporting the first
/// differing lane rather than dumping both buffers.
///
/// # Panics
///
/// Panics on any difference in length or content. That is the point.
pub fn assert_lanes_eq<T>(actual: &[T], expected: &[T], context: &str)
where
    T: PartialEq + core::fmt::Debug,
{
    assert_eq!(
        actual.len(),
        expected.len(),
        "{context}: length {} != reference length {}",
        actual.len(),
        expected.len()
    );
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a == e,
            "{context}: lane {i} diverged: simd {a:?} != scalar {e:?}"
        );
    }
}

/// Float-kernel comparison. **Integer kernels must not use this.**
///
/// # Panics
///
/// Panics if any lane differs by more than `tol`.
pub fn assert_close(actual: &[u8], expected: &[u8], tol: u8) {
    assert_eq!(actual.len(), expected.len(), "length mismatch");
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.abs_diff(*e) <= tol,
            "lane {i}: simd {a} vs scalar {e} exceeds tolerance {tol}"
        );
    }
}

/// Run a binary lane-wise `u8` kernel over the full edge sweep and demand
/// bit-identical agreement with its scalar reference.
///
/// `vector` receives two equal-length inputs and writes an equal-length output;
/// `scalar` is the one-lane oracle.
///
/// # Panics
///
/// Panics with the failing length, pattern index and lane on any divergence.
pub fn check_binary_u8<V, F>(name: &str, vector: V, scalar: F)
where
    V: Fn(&[u8], &[u8]) -> Vec<u8>,
    F: Fn(u8, u8) -> u8,
{
    for len in EDGE_LENGTHS {
        let pats = edge_patterns(len);
        let n = pats.len();
        // Three pairings rather than the full n^2 square: self, reversed, and
        // next. That still crosses every pattern with a zero, a MAX, an
        // alternating and a walking-bit partner, at a twentieth of the cost.
        for i in 0..n {
            for j in [i, n - 1 - i, (i + 1) % n] {
                let (a, b) = (&pats[i], &pats[j]);
                let got = vector(a, b);
                let want: Vec<u8> = a.iter().zip(b).map(|(&x, &y)| scalar(x, y)).collect();
                assert_lanes_eq(&got, &want, &format!("{name} len={len} pat=({i},{j})"));
            }
        }
    }
}

/// The unary form of [`check_binary_u8`].
///
/// # Panics
///
/// Panics on any divergence.
pub fn check_unary_u8<V, F>(name: &str, vector: V, scalar: F)
where
    V: Fn(&[u8]) -> Vec<u8>,
    F: Fn(u8) -> u8,
{
    for len in EDGE_LENGTHS {
        for (i, a) in edge_patterns(len).iter().enumerate() {
            let got = vector(a);
            let want: Vec<u8> = a.iter().map(|&x| scalar(x)).collect();
            assert_lanes_eq(&got, &want, &format!("{name} len={len} pat={i}"));
        }
    }
}

/// The ternary form of [`check_binary_u8`]/[`check_unary_u8`]: a
/// masked-lane-select shape — `(mask, a, b) -> out` — run over the full edge
/// sweep and demanded bit-identical against its scalar reference.
///
/// This is the driver `vaco-codec-dsp-deblock`'s own module doc named as
/// missing (a "ternary check driver in testing"): before this existed,
/// `select_u8`'s own edge-corpus test hand-rolled this exact sweep inline
/// rather than reaching for a shared driver the way every binary and unary
/// op already could.
///
/// `mask`, `a` and `b` are all drawn from [`edge_patterns`] for the same
/// `len`, crossed three ways rather than [`check_binary_u8`]'s two so a
/// select kernel is exercised against a genuinely mixed mask (not only the
/// all-zero/all-`FF` extremes) crossed with two independently-varying
/// operands.
///
/// This driver is `u8`-shaped because [`edge_patterns`] is: it is the
/// **narrow**-width half of a select primitive's own test story (the wide
/// `i16`/`i32` widths are proptested instead — enumerating anything close to
/// their value space is not practical the way it is for a byte). See
/// `tests/ops_agree.rs`'s `select_u8`/`select_i16`/`select_i32` tests for
/// both halves side by side.
///
/// # Panics
///
/// Panics with the failing length, pattern index and lane on any divergence.
pub fn check_ternary_u8<V, F>(name: &str, vector: V, scalar: F)
where
    V: Fn(&[u8], &[u8], &[u8]) -> Vec<u8>,
    F: Fn(u8, u8, u8) -> u8,
{
    for len in EDGE_LENGTHS {
        let pats = edge_patterns(len);
        let n = pats.len();
        for i in 0..n {
            for j in [i, n - 1 - i, (i + 1) % n] {
                for k in [n - 1 - i, (i + 2) % n] {
                    let (Some(mask), Some(a), Some(b)) =
                        (pats.get(i), pats.get(j), pats.get(k))
                    else {
                        // `i`, `j` and `k` are all constructed from `0..n`
                        // modulo/subtraction arithmetic above, so this is
                        // unreachable; the `else` exists only so the lookup
                        // never needs an `unwrap` or indexing.
                        continue;
                    };
                    let got = vector(mask, a, b);
                    let want: Vec<u8> = mask
                        .iter()
                        .zip(a)
                        .zip(b)
                        .map(|((&m, &x), &y)| scalar(m, x, y))
                        .collect();
                    assert_lanes_eq(&got, &want, &format!("{name} len={len} pat=({i},{j},{k})"));
                }
            }
        }
    }
}
