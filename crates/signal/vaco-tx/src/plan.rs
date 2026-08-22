//! The public `Plan` / `Tx` split, and the buffer contracts.

use std::sync::Arc;

use vaco_core::{Error, Result};

use crate::derived::{dct::Dct, dct1::SymTx, mdct::Mdct, rdft::Rdft};
use crate::engine::{Ctx, Engine};
use crate::factor::MAX_LEN;
use crate::num::{Arith, TxSample};

/// Which transform to compute.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub enum TxKind {
    /// Complex-to-complex DFT.
    Fft,
    /// Modified DCT. Forward takes `len` reals to `len/2`; inverse takes
    /// `len/2` to `len/2`, or to `len` with [`TxFlags::FULL_IMDCT`].
    Mdct,
    /// Real-to-complex DFT and its inverse.
    Rdft,
    /// DCT-II forward, DCT-III inverse.
    Dct,
    /// DCT-I. Self-inverse up to a scale.
    DctI,
    /// DST-I. Self-inverse up to a scale.
    DstI,
}

impl TxKind {
    /// A stable lowercase name for diagnostics and benchmark reporting.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fft => "fft",
            Self::Mdct => "mdct",
            Self::Rdft => "rdft",
            Self::Dct => "dct",
            Self::DctI => "dct1",
            Self::DstI => "dst1",
        }
    }
}

impl core::fmt::Display for TxKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Direction {
    Forward,
    Inverse,
}

impl Direction {
    #[must_use]
    pub const fn is_inverse(self) -> bool {
        matches!(self, Self::Inverse)
    }
}

bitflags::bitflags! {
    /// Options that change a plan's buffer contract or algorithm choice.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TxFlags: u32 {
        /// Permit [`Tx::execute_inplace`]. Only valid when the input and output
        /// lengths match, which the plan checks at construction.
        const INPLACE           = 1 << 0;
        /// The caller's buffers carry no alignment guarantee.
        ///
        /// Accepted and recorded, but this crate never assumes alignment: every
        /// load goes through a slice, and the SIMD paths use the substrate's
        /// slice constructors, which do not require it. The flag exists so a
        /// caller porting from an aligned-load API does not have to think about
        /// whether we need it.
        const UNALIGNED         = 1 << 1;
        /// The inverse MDCT emits all `len` samples instead of the `len/2`
        /// unique ones.
        const FULL_IMDCT        = 1 << 2;
        /// Real input, real output: the complex side of an [`TxKind::Rdft`]
        /// carries only real parts.
        const REAL_TO_REAL      = 1 << 3;
        /// Real input, imaginary output: the complex side of an
        /// [`TxKind::Rdft`] carries only imaginary parts.
        const REAL_TO_IMAGINARY = 1 << 4;
    }
}

/// How a transform length was decomposed. Returned by [`Plan::describe`].
///
/// Printing this answers "why is this size slow?" without a profiler, which is
/// the entire reason it is public.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decomposition {
    /// Length 1.
    Identity,
    /// Directly evaluated `O(n²)` DFT.
    Direct { n: usize },
    /// Mixed-radix Stockham, stage radices in execution order.
    MixedRadix { radices: Vec<u32> },
    /// Good–Thomas over two coprime factors.
    PrimeFactor {
        factors: [usize; 2],
        sub: Vec<Decomposition>,
    },
    /// Rader: a length-`p` DFT as a length-`p-1` cyclic convolution.
    Rader { p: usize, inner: Box<Decomposition> },
    /// Bluestein: a convolution of length `m`, always a power of two.
    Bluestein { m: usize, inner: Box<Decomposition> },
}

/// Everything a plan decided, for tests, logging and `-v debug`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlanDescription {
    pub kind: TxKind,
    pub direction: Direction,
    pub len: usize,
    pub precision: &'static str,
    pub flags: TxFlags,
    /// The complex FFT decomposition underneath this transform.
    pub decomposition: Decomposition,
    pub input_len: usize,
    pub output_len: usize,
    pub scratch_len: usize,
}

#[derive(Debug, Clone)]
enum Inner<T: Arith> {
    Fft(Engine<T>),
    Rdft(Rdft<T>),
    Mdct(Mdct<T>),
    Dct(Dct<T>),
    Sym(SymTx<T>),
}

/// Immutable, shareable setup: twiddles, permutation tables, the chosen
/// algorithm.
///
/// `Send + Sync` and cheap to `Arc`. Build one per
/// `(kind, direction, len, scale, flags)` and hand each worker thread its own
/// [`Tx`].
#[derive(Debug)]
pub struct Plan<T: TxSample> {
    kind: TxKind,
    dir: Direction,
    len: usize,
    scale: T::Scale,
    flags: TxFlags,
    input_len: usize,
    output_len: usize,
    scratch_len: usize,
    inner: Inner<T>,
}

impl<T: TxSample> Plan<T> {
    /// Build a plan.
    ///
    /// # Totality
    ///
    /// For [`TxKind::Fft`] this succeeds for **every** `len` in
    /// `1..=16_777_216`: powers of two go through split-free mixed radix,
    /// smooth lengths through the same, coprime composites through
    /// Good–Thomas, primes through Rader, and everything else through
    /// Bluestein. A codec never has to discover that its bitstream's transform
    /// length is unsupported.
    ///
    /// The other kinds add only the domain restrictions inherent to the
    /// transform itself: [`TxKind::Mdct`] needs `len.is_multiple_of(4)`,
    /// [`TxKind::DctI`] needs `len ≥ 2`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a zero or out-of-domain length,
    /// [`Error::LimitExceeded`] above `2^24`, and [`Error::Unsupported`] for a
    /// flag combination this kind cannot honour.
    #[allow(
        clippy::integer_division,
        reason = "`len/2` is the MDCT's own coefficient count and the RDFT's own bin count — the truncation is the definition, and the kinds that use it validate the parity of `len` themselves"
    )]
    pub fn new(
        kind: TxKind,
        dir: Direction,
        len: usize,
        scale: T::Scale,
        flags: TxFlags,
    ) -> Result<Arc<Self>> {
        if len == 0 {
            return Err(Error::InvalidData("transform length must be non-zero"));
        }
        if len > MAX_LEN {
            return Err(Error::LimitExceeded {
                limit: "tx length",
                requested: len as u64,
                cap: MAX_LEN as u64,
            });
        }
        if flags.contains(TxFlags::REAL_TO_REAL | TxFlags::REAL_TO_IMAGINARY) {
            return Err(Error::Unsupported(
                "REAL_TO_REAL and REAL_TO_IMAGINARY are mutually exclusive",
            ));
        }

        let half = len / 2;
        let (inner, input_len, output_len) = match kind {
            TxKind::Fft => (Inner::Fft(Engine::new(len)), 2 * len, 2 * len),
            TxKind::Rdft => {
                let r = Rdft::new(len).ok_or(Error::InvalidData("rdft length must be positive"))?;
                let bins = r.bins();
                let complex_len = if flags
                    .intersects(TxFlags::REAL_TO_REAL | TxFlags::REAL_TO_IMAGINARY)
                {
                    bins
                } else {
                    2 * bins
                };
                let (i, o) = if dir.is_inverse() {
                    (complex_len, len)
                } else {
                    (len, complex_len)
                };
                (Inner::Rdft(r), i, o)
            }
            TxKind::Mdct => {
                let m = Mdct::new(len)
                    .ok_or(Error::InvalidData("mdct length must be a multiple of 4"))?;
                let (i, o) = if dir.is_inverse() {
                    (
                        half,
                        if flags.contains(TxFlags::FULL_IMDCT) {
                            len
                        } else {
                            half
                        },
                    )
                } else {
                    (len, half)
                };
                (Inner::Mdct(m), i, o)
            }
            TxKind::Dct => {
                let d = Dct::new(len).ok_or(Error::InvalidData("dct length must be positive"))?;
                (Inner::Dct(d), len, len)
            }
            TxKind::DctI | TxKind::DstI => {
                let sine = matches!(kind, TxKind::DstI);
                let s = SymTx::new(len, sine)
                    .ok_or(Error::InvalidData("dct-I length must be at least 2"))?;
                (Inner::Sym(s), len, len)
            }
        };

        if flags.contains(TxFlags::INPLACE) && input_len != output_len {
            return Err(Error::Unsupported(
                "INPLACE needs matching input and output lengths",
            ));
        }

        let engine_scratch = match &inner {
            Inner::Fft(e) => e.scratch_len(),
            Inner::Rdft(r) => r.scratch_len(),
            Inner::Mdct(m) => m.scratch_len(),
            Inner::Dct(d) => d.scratch_len(),
            Inner::Sym(s) => s.scratch_len(),
        };
        // The split-complex staging area the interleaved public buffers are
        // converted through, plus whatever the engine itself needs.
        let staging = match kind {
            TxKind::Fft => 2 * len,
            TxKind::Rdft => 2 * (len / 2 + 2),
            _ => 0,
        };
        let scratch_len = staging + engine_scratch;

        Ok(Arc::new(Self {
            kind,
            dir,
            len,
            scale,
            flags,
            input_len,
            output_len,
            scratch_len,
            inner,
        }))
    }

    /// A complex FFT of `len` points.
    ///
    /// # Errors
    /// As [`Plan::new`].
    pub fn fft(len: usize, inverse: bool) -> Result<Arc<Self>> {
        Self::new(
            TxKind::Fft,
            if inverse {
                Direction::Inverse
            } else {
                Direction::Forward
            },
            len,
            T::IDENTITY_SCALE,
            TxFlags::empty(),
        )
    }

    /// An MDCT or IMDCT of `len` time-domain samples.
    ///
    /// # Errors
    /// As [`Plan::new`].
    pub fn mdct(len: usize, inverse: bool, scale: T::Scale) -> Result<Arc<Self>> {
        Self::new(
            TxKind::Mdct,
            if inverse {
                Direction::Inverse
            } else {
                Direction::Forward
            },
            len,
            scale,
            TxFlags::empty(),
        )
    }

    /// A real-input DFT of `len` samples, or its inverse.
    ///
    /// # Errors
    /// As [`Plan::new`].
    pub fn rdft(len: usize, inverse: bool) -> Result<Arc<Self>> {
        Self::new(
            TxKind::Rdft,
            if inverse {
                Direction::Inverse
            } else {
                Direction::Forward
            },
            len,
            T::IDENTITY_SCALE,
            TxFlags::empty(),
        )
    }

    #[must_use]
    pub const fn kind(&self) -> TxKind {
        self.kind
    }
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.dir
    }
    #[must_use]
    pub const fn flags(&self) -> TxFlags {
        self.flags
    }
    /// The transform length this plan was built for.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }
    /// Never zero — [`Plan::new`] rejects a length of 0.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }
    /// Elements the input buffer must hold.
    #[must_use]
    pub const fn input_len(&self) -> usize {
        self.input_len
    }
    /// Elements the output buffer must hold.
    #[must_use]
    pub const fn output_len(&self) -> usize {
        self.output_len
    }
    /// Scratch elements an executor needs.
    #[must_use]
    pub const fn scratch_len(&self) -> usize {
        self.scratch_len
    }

    /// The chosen decomposition and buffer contract.
    #[must_use]
    pub fn describe(&self) -> PlanDescription {
        let decomposition = match &self.inner {
            Inner::Fft(e) => e.describe(),
            Inner::Rdft(r) => r.describe(),
            Inner::Mdct(m) => m.describe(),
            Inner::Dct(d) => d.describe(),
            Inner::Sym(s) => s.describe(),
        };
        PlanDescription {
            kind: self.kind,
            direction: self.dir,
            len: self.len,
            precision: T::precision_name(),
            flags: self.flags,
            decomposition,
            input_len: self.input_len,
            output_len: self.output_len,
            scratch_len: self.scratch_len,
        }
    }
}

/// A plan bound to its own scratch buffer: what a decoder holds.
///
/// # Why this is not the same type as [`Plan`]
///
/// Twiddle tables are read-only and should be shared across threads; scratch is
/// mutable and must not be. Splitting them lets a frame-threaded decoder build
/// one plan and hand each worker its own `Tx` — a pattern a single fused type
/// makes awkward.
///
/// `execute` takes `&mut self` even though the transform is mathematically
/// pure, because scratch is mutated. That is honest about the aliasing and
/// avoids interior mutability.
#[derive(Debug)]
pub struct Tx<T: TxSample> {
    plan: Arc<Plan<T>>,
    scratch: Vec<T>,
    inplace: Vec<T>,
    ctx: Ctx,
}

impl<T: TxSample> Tx<T> {
    #[must_use]
    pub fn new(plan: Arc<Plan<T>>) -> Self {
        let scratch = vec![T::ZERO; plan.scratch_len];
        let inplace = if plan.flags.contains(TxFlags::INPLACE) {
            vec![T::ZERO; plan.input_len]
        } else {
            Vec::new()
        };
        Self {
            plan,
            scratch,
            inplace,
            ctx: Ctx::detect(),
        }
    }

    #[must_use]
    pub fn plan(&self) -> &Arc<Plan<T>> {
        &self.plan
    }

    /// Force the scalar kernels, bypassing every SIMD path.
    ///
    /// **A test hook, not public API.** It exists so the differential suite can
    /// run one plan twice — vector then scalar — and require bit-identical
    /// output. Building two plans instead would confound a kernel difference
    /// with a table difference, which is exactly the distinction the test needs
    /// to make.
    #[doc(hidden)]
    pub fn set_scalar_reference(&mut self, on: bool) {
        self.ctx.scalar = on;
    }

    /// Out-of-place execution.
    ///
    /// `output` must hold at least [`Plan::output_len`] elements and `input` at
    /// least [`Plan::input_len`]. A short buffer produces no output rather than
    /// a panic — this crate is on the path of codec data derived from untrusted
    /// bitstreams, and `clippy::panic` is denied workspace-wide.
    pub fn execute(&mut self, output: &mut [T], input: &[T]) {
        let p = &self.plan;
        if input.len() < p.input_len || output.len() < p.output_len {
            debug_assert!(
                false,
                "execute: need {}/{} elements, got {}/{}",
                p.input_len,
                p.output_len,
                input.len(),
                output.len()
            );
            return;
        }
        run(p, input, output, &mut self.scratch, self.ctx);
        if p.scale != T::IDENTITY_SCALE {
            for v in output.iter_mut().take(p.output_len) {
                *v = T::apply_scale(*v, p.scale);
            }
        }
    }

    /// In-place execution. Requires the plan to carry [`TxFlags::INPLACE`].
    ///
    /// Implemented as a copy into a private buffer followed by an out-of-place
    /// run: `O(n)` against `O(n log n)`, and it keeps every kernel free of
    /// aliasing reasoning.
    pub fn execute_inplace(&mut self, buf: &mut [T]) {
        let p = &self.plan;
        if !p.flags.contains(TxFlags::INPLACE) {
            debug_assert!(false, "execute_inplace on a plan without TxFlags::INPLACE");
            return;
        }
        let n = p.input_len;
        if buf.len() < n || self.inplace.len() < n {
            debug_assert!(false, "execute_inplace: buffer shorter than {n}");
            return;
        }
        if let (Some(dst), Some(src)) = (self.inplace.get_mut(..n), buf.get(..n)) {
            dst.copy_from_slice(src);
        }
        let p = Arc::clone(&self.plan);
        run(&p, &self.inplace, buf, &mut self.scratch, self.ctx);
        if p.scale != T::IDENTITY_SCALE {
            for v in buf.iter_mut().take(p.output_len) {
                *v = T::apply_scale(*v, p.scale);
            }
        }
    }
}

/// Deinterleave `[re, im, re, im, …]` into split-complex, negating the
/// imaginary part when asked.
fn deinterleave<T: Arith>(src: &[T], re: &mut [T], im: &mut [T], n: usize) {
    for (i, pair) in src.chunks_exact(2).take(n).enumerate() {
        if let (Some(a), Some(b), Some(dr), Some(di)) =
            (pair.first(), pair.get(1), re.get_mut(i), im.get_mut(i))
        {
            *dr = *a;
            *di = *b;
        }
    }
}

fn interleave<T: Arith>(re: &[T], im: &[T], dst: &mut [T], n: usize) {
    for (i, pair) in dst.chunks_exact_mut(2).take(n).enumerate() {
        if let (Some(a), Some(b)) = (re.get(i), im.get(i)) {
            if let Some(s) = pair.first_mut() {
                *s = *a;
            }
            if let Some(s) = pair.get_mut(1) {
                *s = *b;
            }
        }
    }
}

#[allow(
    clippy::integer_division,
    reason = "every divisor here is 2, and the lengths were validated by Plan::new"
)]
fn run<T: TxSample>(p: &Plan<T>, input: &[T], output: &mut [T], scratch: &mut [T], ctx: Ctx) {
    match &p.inner {
        Inner::Fft(engine) => {
            let n = p.len;
            let (wr, rest) = scratch.split_at_mut(n);
            let (wi, sub) = rest.split_at_mut(n);
            deinterleave(input, wr, wi, n);
            if p.dir.is_inverse() {
                engine.exec(wi, wr, sub, ctx);
            } else {
                engine.exec(wr, wi, sub, ctx);
            }
            interleave(wr, wi, output, n);
        }
        Inner::Rdft(r) => {
            let bins = r.bins();
            let (br, rest) = scratch.split_at_mut(p.len / 2 + 2);
            let (bi, sub) = rest.split_at_mut(p.len / 2 + 2);
            let only_re = p.flags.contains(TxFlags::REAL_TO_REAL);
            let only_im = p.flags.contains(TxFlags::REAL_TO_IMAGINARY);
            if p.dir.is_inverse() {
                if only_re || only_im {
                    for i in 0..bins {
                        let v = input.get(i).copied().unwrap_or(T::ZERO);
                        if let (Some(a), Some(b)) = (br.get_mut(i), bi.get_mut(i)) {
                            *a = if only_re { v } else { T::ZERO };
                            *b = if only_im { v } else { T::ZERO };
                        }
                    }
                } else {
                    deinterleave(input, br, bi, bins);
                }
                r.inverse_split(br, bi, output, sub, ctx);
            } else {
                r.forward_split(input, br, bi, sub, ctx);
                if only_re || only_im {
                    let src = if only_re { &*br } else { &*bi };
                    for (o, v) in output.iter_mut().zip(src.iter()).take(bins) {
                        *o = *v;
                    }
                } else {
                    interleave(br, bi, output, bins);
                }
            }
        }
        Inner::Mdct(m) => {
            if p.dir.is_inverse() {
                m.inverse(
                    input,
                    output,
                    scratch,
                    ctx,
                    p.flags.contains(TxFlags::FULL_IMDCT),
                );
            } else {
                m.forward(input, output, scratch, ctx);
            }
        }
        Inner::Dct(d) => d.exec(input, output, scratch, ctx, p.dir.is_inverse()),
        Inner::Sym(s) => s.exec(input, output, scratch, ctx, p.dir.is_inverse()),
    }
}
