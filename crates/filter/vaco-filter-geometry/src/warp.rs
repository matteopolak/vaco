//! A plain 2D projective transform (homography) from four point
//! correspondences — shared machinery for [`crate::perspective`].
//!
//! This is standard, textbook numerical linear algebra (solve an 8x8 linear
//! system for the eight free coefficients of a projective map, then invert a
//! 3x3 matrix when the correspondence direction needs reversing); nothing
//! here is derived from, or specific to, any reference implementation.
//!
//! Indexing throughout this module is into fixed-size arrays (`[_; 3]`,
//! `[_; 4]`, `[_; 8]`) at indices bounded by the same constant or a `0..N`
//! loop over it — provably in range, and checked exhaustively by this
//! module's round-trip test. Routing every access through `.get().unwrap()`
//! would trade a compiler-checked array bound for a denied-elsewhere
//! `unwrap` with no safety gain, so this module opts out of
//! `indexing_slicing` instead of `unwrap_used`.
#![allow(
    clippy::indexing_slicing,
    reason = "fixed-size-array indices bounded by constants, see module doc"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "u/v/x/y/w/h/a/b/c match the standard homography-matrix notation"
)]

/// `x = (a*u + b*v + c) / (g*u + h*v + 1)`, `y = (d*u + e*v + f) / (g*u + h*v + 1)`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Homography {
    coeffs: [f64; 8],
}

impl Homography {
    /// Solve for the homography mapping the canonical rectangle
    /// `(0,0)`, `(w,0)`, `(0,h)`, `(w,h)` to `pts[0..4]` in that order.
    ///
    /// `None` if the four points are degenerate (the linear system is
    /// singular).
    pub(crate) fn from_rect(w: f64, h: f64, pts: [(f64, f64); 4]) -> Option<Self> {
        let uv = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)];
        let mut a = [[0.0_f64; 8]; 8];
        let mut b = [0.0_f64; 8];
        for i in 0..4 {
            let (u, v) = uv[i];
            let (x, y) = pts[i];
            // Row for x: a*u + b*v + c - g*u*x - h*v*x = x
            a[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x];
            b[2 * i] = x;
            // Row for y: d*u + e*v + f - g*u*y - h*v*y = y
            a[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y];
            b[2 * i + 1] = y;
        }
        solve8(a, b).map(|coeffs| Self { coeffs })
    }

    /// Apply the forward map `(u, v) -> (x, y)`.
    pub(crate) fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        let c = &self.coeffs;
        let denom = c[6] * u + c[7] * v + 1.0;
        if denom.abs() < 1e-12 {
            return (f64::NAN, f64::NAN);
        }
        let x = (c[0] * u + c[1] * v + c[2]) / denom;
        let y = (c[3] * u + c[4] * v + c[5]) / denom;
        (x, y)
    }

    /// The inverse map, as its own [`Homography`] (`None` if singular).
    pub(crate) fn invert(&self) -> Option<Self> {
        let c = &self.coeffs;
        // M = [[a,b,c],[d,e,f],[g,h,1]]
        let m = [[c[0], c[1], c[2]], [c[3], c[4], c[5]], [c[6], c[7], 1.0]];
        let inv = invert3(m)?;
        // Normalise so the bottom-right entry is 1, matching this struct's
        // own convention (the third row's constant term).
        let s = inv[2][2];
        if s.abs() < 1e-12 {
            return None;
        }
        Some(Self {
            coeffs: [
                inv[0][0] / s,
                inv[0][1] / s,
                inv[0][2] / s,
                inv[1][0] / s,
                inv[1][1] / s,
                inv[1][2] / s,
                inv[2][0] / s,
                inv[2][1] / s,
            ],
        })
    }
}

fn invert3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    let cof =
        |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    // Adjugate transpose / det.
    Some([
        [
            cof(1, 1, 2, 2) * inv_det,
            -cof(0, 1, 2, 2) * inv_det,
            cof(0, 1, 1, 2) * inv_det,
        ],
        [
            -cof(1, 0, 2, 2) * inv_det,
            cof(0, 0, 2, 2) * inv_det,
            -cof(0, 0, 1, 2) * inv_det,
        ],
        [
            cof(1, 0, 2, 1) * inv_det,
            -cof(0, 0, 2, 1) * inv_det,
            cof(0, 0, 1, 1) * inv_det,
        ],
    ])
}

/// Gaussian elimination with partial pivoting for an 8x8 system.
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let mut pivot = col;
        let mut best = a.get(col)?.get(col)?.abs();
        for row in (col + 1)..8 {
            let v = a.get(row)?.get(col)?.abs();
            if v > best {
                best = v;
                pivot = row;
            }
        }
        if best < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        let pivot_val = *a.get(col)?.get(col)?;
        for row in (col + 1)..8 {
            let factor = *a.get(row)?.get(col)? / pivot_val;
            if factor == 0.0 {
                continue;
            }
            for k in col..8 {
                let sub = factor * *a.get(col)?.get(k)?;
                if let Some(v) = a.get_mut(row).and_then(|r| r.get_mut(k)) {
                    *v -= sub;
                }
            }
            let sub_b = factor * *b.get(col)?;
            if let Some(v) = b.get_mut(row) {
                *v -= sub_b;
            }
        }
    }
    let mut x = [0.0_f64; 8];
    for col in (0..8).rev() {
        let mut sum = *b.get(col)?;
        for k in (col + 1)..8 {
            sum -= *a.get(col)?.get(k)? * *x.get(k)?;
        }
        let diag = *a.get(col)?.get(col)?;
        if diag.abs() < 1e-12 {
            return None;
        }
        *x.get_mut(col)? = sum / diag;
    }
    Some(x)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn identity_rect_maps_corners_to_themselves() {
        let h = Homography::from_rect(4.0, 2.0, [(0.0, 0.0), (4.0, 0.0), (0.0, 2.0), (4.0, 2.0)])
            .unwrap();
        let (x, y) = h.apply(2.0, 1.0);
        assert!((x - 2.0).abs() < 1e-9);
        assert!((y - 1.0).abs() < 1e-9);
    }

    #[test]
    fn inverse_of_identity_is_identity() {
        let h = Homography::from_rect(4.0, 2.0, [(0.0, 0.0), (4.0, 0.0), (0.0, 2.0), (4.0, 2.0)])
            .unwrap();
        let inv = h.invert().unwrap();
        let (x, y) = inv.apply(3.0, 1.5);
        assert!((x - 3.0).abs() < 1e-9);
        assert!((y - 1.5).abs() < 1e-9);
    }

    #[test]
    fn apply_then_invert_round_trips() {
        let h = Homography::from_rect(
            10.0,
            10.0,
            [(1.0, 2.0), (9.0, 0.0), (0.0, 9.0), (10.0, 10.0)],
        )
        .unwrap();
        let inv = h.invert().unwrap();
        for (u, v) in [
            (0.0, 0.0),
            (10.0, 0.0),
            (0.0, 10.0),
            (10.0, 10.0),
            (5.0, 5.0),
        ] {
            let (x, y) = h.apply(u, v);
            let (ru, rv) = inv.apply(x, y);
            assert!((ru - u).abs() < 1e-6, "u round-trip: {ru} vs {u}");
            assert!((rv - v).abs() < 1e-6, "v round-trip: {rv} vs {v}");
        }
    }
}
