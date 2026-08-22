//! Directly evaluated transform definitions in `f64`.
//!
//! **Verification only.** Nothing in the crate's fast paths calls this module;
//! it exists so tests, benchmarks and downstream conformance work have an oracle
//! that is obviously correct by inspection rather than correct by argument.
//!
//! Every function is a literal transcription of the transform's defining sum, at
//! `O(n²)`, with the angle reduced by an exact integer modulus so the oracle
//! stays accurate at the sizes it is used for. If one of these disagrees with a
//! [`crate::Plan`], the plan is wrong.

#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "an O(n²) reference: every loop is bounded by a length taken from the input slice itself, and n/2 is the definition's own bin count"
)]
mod imp {
    use core::f64::consts::{PI, TAU};

    /// Complex DFT. `inverse` selects `exp(+2πi·nk/N)` and does **not**
    /// normalise, matching the crate's float convention.
    #[must_use]
    pub fn dft(re: &[f64], im: &[f64], inverse: bool) -> (Vec<f64>, Vec<f64>) {
        let n = re.len().min(im.len());
        let sign = if inverse { 1.0 } else { -1.0 };
        let mut out_r = vec![0.0; n];
        let mut out_i = vec![0.0; n];
        for k in 0..n {
            let (mut sr, mut si) = (0.0, 0.0);
            for j in 0..n {
                let theta = sign * TAU * ((j * k) % n) as f64 / n as f64;
                let (s, c) = theta.sin_cos();
                sr += re[j] * c - im[j] * s;
                si += re[j] * s + im[j] * c;
            }
            out_r[k] = sr;
            out_i[k] = si;
        }
        (out_r, out_i)
    }

    /// Real-input DFT, returning the `n/2 + 1` unique bins.
    #[must_use]
    pub fn rdft(x: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let n = x.len();
        let (r, i) = dft(x, &vec![0.0; n], false);
        let bins = n / 2 + 1;
        (r.into_iter().take(bins).collect(), i.into_iter().take(bins).collect())
    }

    /// Forward MDCT: `n` samples to `n/2` coefficients.
    #[must_use]
    pub fn mdct(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        let half = n / 2;
        (0..half)
            .map(|k| {
                (0..n)
                    .map(|j| {
                        let a = TAU / n as f64
                            * (j as f64 + 0.5 + n as f64 / 4.0)
                            * (k as f64 + 0.5);
                        x[j] * a.cos()
                    })
                    .sum()
            })
            .collect()
    }

    /// Inverse MDCT: `n/2` coefficients to all `n` samples.
    #[must_use]
    pub fn imdct(coeffs: &[f64]) -> Vec<f64> {
        let half = coeffs.len();
        let n = half * 2;
        (0..n)
            .map(|j| {
                (0..half)
                    .map(|k| {
                        let a = TAU / n as f64
                            * (j as f64 + 0.5 + n as f64 / 4.0)
                            * (k as f64 + 0.5);
                        coeffs[k] * a.cos()
                    })
                    .sum()
            })
            .collect()
    }

    /// DCT-II.
    #[must_use]
    pub fn dct2(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        (0..n)
            .map(|k| {
                (0..n)
                    .map(|j| x[j] * (PI * (2 * j + 1) as f64 * k as f64 / (2.0 * n as f64)).cos())
                    .sum()
            })
            .collect()
    }

    /// DCT-III, with the halved DC term of the standard definition.
    #[must_use]
    pub fn dct3(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        (0..n)
            .map(|k| {
                x[0] / 2.0
                    + (1..n)
                        .map(|j| {
                            x[j] * (PI * (2 * k + 1) as f64 * j as f64 / (2.0 * n as f64)).cos()
                        })
                        .sum::<f64>()
            })
            .collect()
    }

    /// DCT-I, with the halved endpoints of the standard definition.
    #[must_use]
    pub fn dct1(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        let m = (n - 1) as f64;
        (0..n)
            .map(|k| {
                let last = if k.is_multiple_of(2) { x[n - 1] } else { -x[n - 1] };
                let ends = f64::midpoint(x[0], last);
                ends + (1..n - 1)
                    .map(|j| x[j] * (PI * j as f64 * k as f64 / m).cos())
                    .sum::<f64>()
            })
            .collect()
    }

    /// DST-I.
    #[must_use]
    pub fn dst1(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        let m = (n + 1) as f64;
        (0..n)
            .map(|k| {
                (0..n)
                    .map(|j| x[j] * (PI * (j + 1) as f64 * (k + 1) as f64 / m).sin())
                    .sum()
            })
            .collect()
    }
}

pub use imp::{dct1, dct2, dct3, dft, dst1, imdct, mdct, rdft};
