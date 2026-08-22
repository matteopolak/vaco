//! Complex-to-complex FFT engines, in split-complex form.
//!
//! Every engine computes the **forward** DFT `X[k] = Σ_n x[n]·exp(-2πi·n·k/N)`
//! on separate `re`/`im` buffers. There is no inverse engine: the inverse is
//! `swap ∘ forward ∘ swap`, i.e. calling `exec(im, re)` instead of
//! `exec(re, im)`. That identity is exact — `swap(z) = i·conj(z)` and
//! `IDFT(x) = conj(F(conj(x)))` — so it costs nothing, halves the kernel count,
//! and guarantees the inverse is bit-for-bit the mirror of the forward path
//! rather than a second implementation that could drift.
//!
//! For `i32`, "forward DFT" means `DFT(x)/N`: each radix-`r` stage divides by
//! `r` (see [`crate::num::Lane::STAGE_SCALED`]).
//!
//! # How a length is decomposed
//!
//! [`Engine::build`] applies these rules in order, and the last one always
//! succeeds — which is what makes `Plan::new` total:
//!
//! | # | Condition | Engine |
//! |---|---|---|
//! | 1 | `n = 1` | [`Engine::Identity`] |
//! | 2 | `n` factors over `{2,3,5,7}` | [`stockham`] mixed radix |
//! | 3 | `n` small, or `i32` and `n` moderate | [`direct`] `O(n²)` DFT |
//! | 4 | `n = n₁·n₂` with `gcd(n₁,n₂) = 1` | [`pfa`] Good–Thomas |
//! | 5 | `n` prime | [`rader`] |
//! | 6 | anything else | [`bluestein`] |
//!
//! Rule 3 exists for two different reasons at once. For tiny primes a direct
//! DFT genuinely beats Rader. For `i32` it is a *precision* decision: a direct
//! DFT rounds once per output, where Bluestein rounds through two length-`M`
//! transforms and loses roughly `log₂(M²/n)` bits. See
//! `docs/signal/vaco-tx.md`, "Precision of the awkward lengths".

pub(crate) mod bluestein;
pub(crate) mod conv;
pub(crate) mod direct;
pub(crate) mod pfa;
pub(crate) mod rader;
pub(crate) mod stockham;

use crate::factor;
use crate::num::Arith;
use vaco_simd::Caps;

/// Longest length handled by a direct `O(n²)` DFT in the float precisions.
///
/// Above this Rader wins; below it, the direct form is both faster and more
/// accurate than setting up a convolution.
const DIRECT_MAX_FLOAT: usize = 32;

/// Longest length handled by a direct `O(n²)` DFT in `i32`.
///
/// Much larger than [`DIRECT_MAX_FLOAT`] because the trade is different: a
/// direct DFT is the only awkward-length path whose fixed-point precision is
/// good. 4096² multiply-accumulates at plan-selection time is slow but bounded,
/// and no shipping codec asks for a fixed-point transform at a length that
/// reaches here.
const DIRECT_MAX_FIXED: usize = 4096;

/// Recursion cap on Rader (whose inner length is `p-1`, which may itself be
/// prime). Beyond it, Bluestein — which never recurses — takes over.
const MAX_RADER_DEPTH: u32 = 6;

/// Execution context: the capability token, plus the switch the differential
/// tests use.
///
/// `scalar` exists so a test can run the *same* plan twice — once through the
/// vector kernels, once through the scalar ones — and compare. Without it the
/// only way to reach the scalar path on a machine with SIMD would be to build a
/// second plan, and then a mismatch could be a table difference rather than a
/// kernel difference.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ctx {
    pub caps: Caps,
    pub scalar: bool,
}

impl Ctx {
    pub(crate) fn detect() -> Self {
        Self {
            caps: Caps::detect(),
            scalar: false,
        }
    }

    /// Scalar kernels only. Also used for **plan-time** table generation, so a
    /// plan's precomputed transforms are identical on every machine regardless
    /// of the SIMD level the host happens to have.
    pub(crate) fn scalar_only() -> Self {
        Self {
            caps: Caps::detect(),
            scalar: true,
        }
    }
}

pub(crate) fn copy_prefix<T: Copy>(dst: &mut [T], src: &[T], n: usize) {
    if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
        d.copy_from_slice(s);
    }
}

/// Multiply every element by `num / den`.
///
/// Floats do it directly. Fixed point cannot, because `num/den` is routinely
/// larger than the Q31 range: the ratio is split into a Q31 factor below 1 and a
/// power-of-two shift, so there is exactly one rounding and the intermediate
/// stays inside `i32`.
pub(crate) fn scale_ratio<T: Arith>(x: &mut [T], num: u64, den: u64) {
    debug_assert!(den > 0);
    if T::STAGE_SCALED {
        let ratio = num as f64 / den as f64;
        let mut sh = 0u32;
        while ratio > (1u64 << sh) as f64 && sh < 40 {
            sh += 1;
        }
        let q = T::from_f64(ratio / (1u64 << sh) as f64);
        let shift = 1u32 << sh;
        for v in x.iter_mut() {
            *v = T::mul_int(T::mul_c(*v, q), shift);
        }
    } else {
        let f = T::from_f64(num as f64 / den as f64);
        for v in x.iter_mut() {
            *v = T::mul_c(*v, f);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Engine<T: Arith> {
    /// `n = 1`: the DFT is the identity.
    Identity,
    Stockham(stockham::Stockham<T>),
    Direct(direct::Direct<T>),
    PrimeFactor(Box<pfa::PrimeFactor<T>>),
    Rader(Box<rader::Rader<T>>),
    Bluestein(Box<bluestein::Bluestein<T>>),
}

impl<T: Arith> Engine<T> {
    /// Build an engine for `n`. Total for every `n ≥ 1`.
    pub(crate) fn new(n: usize) -> Self {
        Self::build(n, 0)
    }

    fn build(n: usize, depth: u32) -> Self {
        if n <= 1 {
            return Self::Identity;
        }
        if let Some(radices) = factor::smooth_radices(n) {
            return Self::Stockham(stockham::Stockham::new(n, &radices));
        }
        let direct_max = if T::STAGE_SCALED {
            DIRECT_MAX_FIXED
        } else {
            DIRECT_MAX_FLOAT
        };
        if n <= direct_max {
            return Self::Direct(direct::Direct::new(n));
        }

        // Good–Thomas: peel the part we have kernels for, or the first prime
        // power, whichever gives a genuine coprime split.
        let smooth = factor::smooth_part(n);
        let powers = factor::factorise(n);
        let first_power = powers
            .first()
            .filter(|_| powers.len() >= 2)
            .map(|&(p, e)| p.pow(e));
        for split in [Some(smooth).filter(|&s| s > 1 && s < n), first_power]
            .into_iter()
            .flatten()
        {
            if let Some(pf) = pfa::PrimeFactor::new(n, split, depth) {
                return Self::PrimeFactor(Box::new(pf));
            }
        }

        if let Some(r) = Some(n)
            .filter(|&n| depth < MAX_RADER_DEPTH && factor::is_prime(n))
            .and_then(|n| rader::Rader::new(n, depth))
        {
            return Self::Rader(Box::new(r));
        }
        Self::Bluestein(Box::new(bluestein::Bluestein::new(n)))
    }

    pub(crate) fn scratch_len(&self) -> usize {
        match self {
            Self::Identity => 0,
            Self::Stockham(s) => s.scratch_len(),
            Self::Direct(d) => d.scratch_len(),
            Self::PrimeFactor(p) => p.scratch_len(),
            Self::Rader(r) => r.scratch_len(),
            Self::Bluestein(b) => b.scratch_len(),
        }
    }

    /// Forward DFT in place on `re`/`im`. Pass `(im, re)` for the inverse.
    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T], ctx: Ctx) {
        match self {
            Self::Identity => {}
            Self::Stockham(s) => s.exec(re, im, scratch, ctx),
            Self::Direct(d) => d.exec(re, im, scratch),
            Self::PrimeFactor(p) => p.exec(re, im, scratch, ctx),
            Self::Rader(r) => r.exec(re, im, scratch, ctx),
            Self::Bluestein(b) => b.exec(re, im, scratch, ctx),
        }
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        use crate::Decomposition as D;
        match self {
            Self::Identity => D::Identity,
            Self::Stockham(s) => D::MixedRadix {
                radices: s.radices(),
            },
            Self::Direct(d) => D::Direct { n: d.len() },
            Self::PrimeFactor(p) => p.describe(),
            Self::Rader(r) => r.describe(),
            Self::Bluestein(b) => b.describe(),
        }
    }
}
