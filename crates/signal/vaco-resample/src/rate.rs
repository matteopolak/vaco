//! Polyphase rate conversion.
//!
//! # Exact-rational phase arithmetic
//!
//! `44100 → 48000` is `147/160`. The position of output `k` is `k·q/p` input
//! samples, and it is carried as an integer pair — never a float accumulator.
//!
//! ```text
//! frac += q % p;  idx += q / p;
//! if frac >= p { frac -= p; idx += 1; }
//! ```
//!
//! This cannot drift over any stream length. A single-precision float
//! accumulator loses a sample after roughly `2^24` outputs — about five minutes
//! at 48 kHz — which is a real reported class of bug in naive resamplers.
//!
//! When the reduced denominator `p` is at most [`MAX_EXACT_PHASES`] the bank has
//! exactly `p` phases, so `phase(k) = (k·q) mod p` is not just drift-free but
//! *exact*: there is no phase quantisation error at all. Above that the bank
//! falls back to `2^phase_shift` phases and the phase index is the floor of the
//! exact fraction — still no accumulator, still no drift, just a bounded
//! quantisation of the sub-sample offset.
//!
//! # Edge handling is mirroring, not zero-priming
//!
//! Plan 17 §B.5.4 prescribes prepending and appending `centre` zeros. The
//! reference does something better and it is directly observable: feed constant
//! `1.0` and the output is flat `1.0` from the very first sample, with no
//! fade-in. Probing with impulses at input 0, 1, 2 and 5 identifies the rule
//! exactly, and the two ends are **not** symmetric:
//!
//! ```text
//! head:  x[-k]      = x[k]          whole-sample mirror about 0
//! tail:  x[N-1+k]   = x[N-k]        half-sample mirror about N - 1/2
//! ```
//!
//! Impulse at input 1, output 1 (phase 1) measures `0.4295627`, which is
//! `h[1][16] + h[1][14] = 0.6339873 − 0.2044246` to the last digit — the
//! mirrored copy at input −1 contributing through tap 14. The half-sample tail
//! was found the same way; a whole-sample tail mirror is off by 0.5 and
//! measurably wrong.
//!
//! Mirroring depends only on the stream, never on how it was chunked, so it
//! costs nothing in chunk-invariance.

#![allow(
    clippy::integer_division,
    reason = "p, q, phase and tap counts are all non-zero by construction"
)]

use vaco_core::Error;
use vaco_limits::Budget;

use crate::convert::Internal;
use crate::design::{Bank, DesignParams, Window, build_bank};

/// Above this reduced denominator the exact-rational bank is too large and the
/// engine falls back to `2^phase_shift` phases.
///
/// Every rate pair anyone actually uses is far below it: `44100 ↔ 48000`
/// reduces to `160/147`.
pub const MAX_EXACT_PHASES: u64 = 4096;

/// Largest rate ratio, in either direction, that this engine will build.
///
/// # Why a ratio needs its own bound
///
/// Found by the fuzzer, as a slow unit rather than a crash: `8 Hz -> 335872 Hz`
/// with `filter_size = 8192` took **47.7 seconds** for eight input samples.
///
/// Nothing that already existed catches it. The reduced ratio is `41984/1`, so
/// the phase count is 1 and the coefficient bank is 32 KB — the allocation
/// budget sees a perfectly ordinary request. The cost is entirely in the
/// *output* count: eight input samples become 335 872 output samples, and each
/// one is a full 8192-tap convolution. 2.75 billion multiply-accumulates, from
/// a header that names two integers.
///
/// This is a denial-of-service surface for anything decoding untrusted media,
/// and it is a bound rather than a faster loop that fixes it. The bound belongs
/// here rather than in `vaco-limits`, because "ratio of two sample rates" is
/// domain knowledge that crate should not have to carry — what `vaco-limits`
/// supplies is the mechanism (`check_sample_rate`, `consume_fuel`) and this is
/// the domain constant that uses it.
///
/// 1024 is far above anything real: `2822400 / 8000` is 352.8, and that is the
/// widest pair the permissive limits admit at all.
pub const MAX_RATE_RATIO: u64 = 1024;

/// Settings for one rate conversion.
#[derive(Clone, Copy, Debug)]
pub struct RateParams {
    pub in_rate: u32,
    pub out_rate: u32,
    pub filter_size: usize,
    /// `1 << phase_shift` phases when the exact-rational bank is not used.
    pub phase_shift: u32,
    pub cutoff: f64,
    pub window: Window,
    pub kaiser_beta: f64,
    pub exact_rational: bool,
    /// Interpolate between adjacent phases by the residual fraction.
    pub linear_interp: bool,
}

impl Default for RateParams {
    fn default() -> Self {
        Self {
            in_rate: 48000,
            out_rate: 48000,
            filter_size: 32,
            phase_shift: 10,
            // MEASURED: the reference's default. See `design`.
            cutoff: 0.97,
            window: Window::Kaiser,
            kaiser_beta: 9.0,
            exact_rational: true,
            linear_interp: false,
        }
    }
}

/// A stateful polyphase rate converter over `channels` independent channels.
#[derive(Debug)]
pub struct RateConvert<T: Internal> {
    bank: Bank<T>,
    /// Reduced ratio: advance `q/p` input samples per output sample.
    p: u64,
    q: u64,
    /// Phases in the bank. Equal to `p` in the exact-rational case.
    phases: u64,
    exact: bool,
    linear_interp: bool,

    channels: usize,
    /// Sliding window per channel, holding absolute indices `[base, consumed)`.
    win: Vec<Vec<T>>,
    /// The first `centre + 1` samples of the stream, kept for the head mirror.
    head: Vec<Vec<T>>,
    scratch: Vec<T>,
    base: u64,
    consumed: u64,
    idx: u64,
    frac: u64,
    produced: u64,
    draining: bool,

    /// Lifetime processing-cost fuel: see [`RateConvert::new`]'s doc on why
    /// this exists alongside [`MAX_RATE_RATIO`]/[`MAX_TAPS`].
    work_fuel_spent: u64,
    work_fuel_cap: u64,
}

impl<T: Internal> RateConvert<T> {
    /// # Errors
    /// [`Error::InvalidData`] for a zero rate or channel count;
    /// [`Error::LimitExceeded`] if the coefficient bank exceeds the budget.
    pub fn new(params: &RateParams, channels: usize, budget: &mut Budget) -> Result<Self, Error> {
        if params.in_rate == 0 || params.out_rate == 0 {
            return Err(Error::InvalidData("sample rate must be non-zero"));
        }
        if channels == 0 {
            return Err(Error::InvalidData("channel count must be non-zero"));
        }
        if !(0.0..=1.0).contains(&params.cutoff) || params.cutoff <= 0.0 {
            return Err(Error::InvalidData("cutoff must be in (0, 1]"));
        }
        if params.filter_size == 0 {
            return Err(Error::InvalidData("filter_size must be non-zero"));
        }
        let g = gcd(u64::from(params.in_rate), u64::from(params.out_rate));
        let p = u64::from(params.out_rate) / g;
        let q = u64::from(params.in_rate) / g;
        let ratio = if p >= q { p / q.max(1) } else { q / p.max(1) };
        if ratio > MAX_RATE_RATIO {
            return Err(Error::LimitExceeded {
                limit: "resample rate ratio",
                requested: ratio,
                cap: MAX_RATE_RATIO,
            });
        }

        let exact = params.exact_rational && p <= MAX_EXACT_PHASES;
        let phases = if exact {
            p
        } else {
            1_u64 << params.phase_shift.min(24)
        };
        let factor = DesignParams::factor(params.in_rate, params.out_rate, params.cutoff);
        let design = DesignParams {
            phases: usize::try_from(phases)
                .map_err(|_| Error::InvalidData("phase count overflows"))?,
            filter_size: params.filter_size,
            factor,
            window: params.window,
            kaiser_beta: params.kaiser_beta,
        };
        let bank = build_bank::<T>(&design, budget)?;
        let taps = bank.taps;
        Ok(Self {
            bank,
            p,
            q,
            phases,
            exact,
            linear_interp: params.linear_interp,
            channels,
            win: vec![Vec::new(); channels],
            head: vec![Vec::new(); channels],
            scratch: vec![T::ZERO; taps],
            base: 0,
            consumed: 0,
            idx: 0,
            frac: 0,
            produced: 0,
            draining: false,
            work_fuel_spent: 0,
            // MEASURED, by fuzzing: `MAX_RATE_RATIO`, `MAX_STRETCH` and
            // `MAX_TAPS` each bound one factor of the per-output-sample cost
            // (`taps` multiply-adds per channel) but none bounds their
            // *product* against how many output samples get produced. A
            // fuzz input with filter_size=23301, a ~625x upsample ratio and
            // 25 channels — every one of those individually legal — cost
            // 15-23 seconds of CPU from 51 input samples, because
            // taps × channels × output_samples came to ~1.9e10
            // multiply-adds. `process`/`flush` charge this budget's `fuel`
            // per output sample actually produced (`emit`, below), which is
            // the same mechanism `design::build_bank` already uses for the
            // bank-construction cost — applied here to the cost of *using*
            // the bank instead. It is a fresh counter rather than continuing
            // to draw on `budget` itself, because `budget` is not retained
            // past construction and a streaming `process`/`flush` call has
            // no `&mut Budget` to charge against.
            work_fuel_cap: budget.limits().fuel,
        })
    }

    /// Filter length in input samples.
    #[must_use]
    pub const fn taps(&self) -> usize {
        self.bank.taps
    }

    /// Group delay in input samples.
    #[must_use]
    pub const fn centre(&self) -> usize {
        self.bank.centre
    }

    #[must_use]
    pub const fn phases(&self) -> u64 {
        self.phases
    }

    /// `true` when the bank has exactly `out_rate/gcd` phases, so the phase
    /// index carries no quantisation error at all.
    #[must_use]
    pub const fn is_exact_rational(&self) -> bool {
        self.exact
    }

    /// Input samples held internally but not yet turned into output.
    #[must_use]
    pub const fn delay_in_samples(&self) -> u64 {
        self.consumed.saturating_sub(self.idx)
    }

    /// Total output samples a stream of `in_samples` produces, from the start.
    ///
    /// The count is `ceil(in · p / q)`, which the reference reproduces exactly:
    /// 100 input samples at `44100 → 48000` give 109, 1000 give 1089, and 44100
    /// give exactly 48000.
    #[must_use]
    pub fn total_out_samples(&self, in_samples: u64) -> u64 {
        let n = u128::from(in_samples) * u128::from(self.p);
        let q = u128::from(self.q);
        let out = n.div_ceil(q);
        u64::try_from(out).unwrap_or(u64::MAX)
    }

    /// Upper bound on the samples one more `process` call of `in_samples` can
    /// emit, given the current state.
    #[must_use]
    pub fn out_samples(&self, in_samples: u64) -> u64 {
        self.total_out_samples(self.consumed.saturating_add(in_samples))
            .saturating_sub(self.produced)
    }

    pub fn reset(&mut self) {
        for w in &mut self.win {
            w.clear();
        }
        for h in &mut self.head {
            h.clear();
        }
        self.base = 0;
        self.consumed = 0;
        self.idx = 0;
        self.frac = 0;
        self.produced = 0;
        self.draining = false;
    }

    /// Feed `n` samples per channel and append whatever output is ready.
    ///
    /// # Errors
    /// [`Error::InvalidData`] on a channel-count mismatch, or if called after
    /// [`RateConvert::flush`].
    pub fn process(
        &mut self,
        input: &[Vec<T>],
        in_off: usize,
        n: usize,
        out: &mut [Vec<T>],
    ) -> Result<usize, Error> {
        if self.draining {
            return Err(Error::InvalidData("rate converter is draining"));
        }
        if input.len() != self.channels || out.len() != self.channels {
            return Err(Error::InvalidData("rate converter channel mismatch"));
        }
        let keep = self.bank.centre + 1;
        for (ch, plane) in input.iter().enumerate() {
            let Some(src) = plane.get(in_off..in_off + n) else {
                return Err(Error::InvalidData("input plane too short"));
            };
            let Some(w) = self.win.get_mut(ch) else {
                return Err(Error::InvalidData("missing window"));
            };
            w.extend_from_slice(src);
            let Some(h) = self.head.get_mut(ch) else {
                return Err(Error::InvalidData("missing head"));
            };
            if h.len() < keep {
                let take = (keep - h.len()).min(src.len());
                h.extend_from_slice(src.get(..take).unwrap_or_default());
            }
        }
        self.consumed = self.consumed.saturating_add(n as u64);
        // Streaming: an output is ready once its whole window is inside the
        // samples we have. The head mirror only ever reaches index `centre`, so
        // this one condition covers both edges.
        let limit = self.consumed + self.bank.centre as u64;
        let taps = self.bank.taps as u64;
        let mut count = 0usize;
        while self.idx + taps <= limit {
            self.emit(out)?;
            count += 1;
        }
        self.trim();
        Ok(count)
    }

    /// Drain: emit every remaining output using the tail mirror.
    ///
    /// # Errors
    /// [`Error::InvalidData`] on a channel-count mismatch.
    pub fn flush(&mut self, out: &mut [Vec<T>]) -> Result<usize, Error> {
        if out.len() != self.channels {
            return Err(Error::InvalidData("rate converter channel mismatch"));
        }
        self.draining = true;
        let mut count = 0usize;
        while self.idx < self.consumed {
            self.emit(out)?;
            count += 1;
        }
        Ok(count)
    }

    fn advance(&mut self) {
        self.frac += self.q % self.p;
        self.idx += self.q / self.p;
        if self.frac >= self.p {
            self.frac -= self.p;
            self.idx += 1;
        }
        self.produced = self.produced.saturating_add(1);
    }

    /// The bank row for the current fractional position, plus the residual used
    /// by `linear_interp`.
    fn phase_index(&self) -> (usize, f64) {
        if self.exact {
            (usize::try_from(self.frac).unwrap_or(0), 0.0)
        } else {
            let scaled = u128::from(self.frac) * u128::from(self.phases);
            let p = u128::from(self.p);
            let ph = scaled / p;
            let rem = scaled % p;
            let residual = if p == 0 {
                0.0
            } else {
                (rem as f64) / (p as f64)
            };
            (usize::try_from(ph).unwrap_or(0), residual)
        }
    }

    fn emit(&mut self, out: &mut [Vec<T>]) -> Result<(), Error> {
        let taps = self.bank.taps;
        // One dot product per channel, `taps` multiply-adds each: see the
        // doc on `work_fuel_cap` in `RateConvert::new` for why this is
        // charged per sample actually produced rather than bounded any other
        // way.
        let cost = (taps as u64).saturating_mul(self.channels as u64);
        self.work_fuel_spent = self.work_fuel_spent.saturating_add(cost);
        if self.work_fuel_spent > self.work_fuel_cap {
            return Err(Error::LimitExceeded {
                limit: "resample processing fuel",
                requested: self.work_fuel_spent,
                cap: self.work_fuel_cap,
            });
        }
        let start = signed(self.idx) - signed_len(self.bank.centre);
        let (phase, residual) = self.phase_index();
        let phase = phase.min(self.bank.phases.saturating_sub(1));
        let next = (phase + 1).min(self.bank.phases.saturating_sub(1));
        let interp = self.linear_interp && residual > 0.0 && next != phase;
        let geom = Geometry {
            base: signed(self.base),
            consumed: signed(self.consumed),
            draining: self.draining,
            start,
            taps,
        };

        for ch in 0..self.channels {
            let value = {
                let coeffs = self
                    .bank
                    .phase(phase)
                    .ok_or(Error::InvalidData("phase out of range"))?;
                let samples = Self::window(
                    self.win.get(ch),
                    self.head.get(ch),
                    &mut self.scratch,
                    &geom,
                )?;
                let acc = T::dot(samples, coeffs);
                if interp {
                    let next_coeffs = self
                        .bank
                        .phase(next)
                        .ok_or(Error::InvalidData("phase out of range"))?;
                    let acc_next = T::dot(samples, next_coeffs);
                    acc.add(acc_next.sub(acc).mul(T::from_f64(residual)))
                } else {
                    acc
                }
            };
            let Some(dst) = out.get_mut(ch) else {
                return Err(Error::InvalidData("missing output plane"));
            };
            dst.push(value);
        }
        self.advance();
        Ok(())
    }

    /// A contiguous `taps`-long view of the input window at `start`.
    ///
    /// Borrows the sliding window directly when the whole window is inside it —
    /// the steady-state case, and the one that matters for throughput — and
    /// gathers into `scratch` with mirroring only at the two ends of the stream.
    fn window<'a>(
        win: Option<&'a Vec<T>>,
        head: Option<&Vec<T>>,
        scratch: &'a mut [T],
        geom: &Geometry,
    ) -> Result<&'a [T], Error> {
        let win = win.ok_or(Error::InvalidData("missing window"))?;
        let taps = geom.taps;
        let end = geom.start.saturating_add(signed_len(taps));
        if geom.start >= geom.base && end <= geom.consumed {
            let off = usize::try_from(geom.start - geom.base).unwrap_or(usize::MAX);
            return win
                .get(off..off.saturating_add(taps))
                .ok_or(Error::InvalidData("window out of range"));
        }
        let head = head.ok_or(Error::InvalidData("missing head"))?;
        let n = geom.consumed;
        for (k, slot) in scratch.iter_mut().enumerate().take(taps) {
            let mut i = geom.start.saturating_add(signed_len(k));
            // MEASURED: whole-sample mirror at the head, half-sample at the tail.
            if i < 0 {
                i = -i;
            }
            if geom.draining && i >= n {
                i = 2 * n - 1 - i;
            }
            let i = i.clamp(0, n.saturating_sub(1).max(0));
            *slot = if i < geom.base {
                head.get(usize::try_from(i).unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(T::ZERO)
            } else {
                win.get(usize::try_from(i - geom.base).unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(T::ZERO)
            };
        }
        scratch
            .get(..taps)
            .ok_or(Error::InvalidData("scratch too short"))
    }

    fn trim(&mut self) {
        let taps = self.bank.taps as u64;
        let left = self.idx.saturating_sub(self.bank.centre as u64);
        // Keep enough behind the write head for the tail mirror at flush.
        let keep_from = left.min(self.consumed.saturating_sub(2 * taps));
        if keep_from <= self.base {
            return;
        }
        let drop = (keep_from - self.base) as usize;
        for w in &mut self.win {
            if drop < w.len() {
                w.drain(..drop);
            } else {
                w.clear();
            }
        }
        self.base = keep_from;
    }
}

/// Where one output sample's window sits, in absolute stream indices.
///
/// Bundled because the window helper needs five of them and a five-argument
/// private helper is worse than a struct.
struct Geometry {
    base: i64,
    consumed: i64,
    draining: bool,
    start: i64,
    taps: usize,
}

/// Stream positions are bounded by the number of samples ever fed, so they are
/// far below `i64::MAX`; saturating rather than wrapping keeps that true even
/// under an adversarial input.
fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn signed_len(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    if a == 0 { 1 } else { a }
}

// ---------------------------------------------------------------------------
// The convolution kernel
// ---------------------------------------------------------------------------

/// Dot products, in the shapes the benchmark compares.
///
/// `benches/resample.rs` measures all three side by side at 32, 50 and 256 taps
/// in `f32` and `f64`. Plan 12's PF-0.1 amendment is explicit that a confident
/// assumption here has measured backwards twice, so the naive form is the
/// baseline and the others have to earn their place.
///
/// # It measured backwards a third time
///
/// PF-0.0's authoring rule is "never carry a single accumulator; use four",
/// worth up to 4x. Against the naive single-accumulator loop, on Apple M5,
/// min-of-100 over two agreeing runs:
///
/// | | 32 taps | 50 taps | 256 taps |
/// |---|---|---|---|
/// | `dot4`, `f32` | **1.55x** | **1.33x** | 0.62x |
/// | `dot8`, `f32` | 0.52x | 0.49x | 0.19x |
/// | `dot4`, `f64` | **1.68x** | **1.16x** | 0.77x |
/// | `dot8`, `f64` | 0.77x | 0.68x | 0.29x |
///
/// **Four accumulators are slower than one** at exactly the tap counts a
/// resampler uses — 32 is the default and 50 is what a 3:2 downsample stretches
/// it to. Eight are faster than either, everywhere.
///
/// The reason is that the rule is about *lane width*, not about the number four.
/// Four `f32` lanes is precisely one NEON register, so `as_chunks::<4>` pins the
/// loop to one vector per iteration and takes away the unrolling LLVM performs
/// on a plain `iter().zip()` reduction by itself. Eight gives it two registers
/// and it wins. The naive form is not naive: it is the shape the compiler is
/// best at.
///
/// [`Internal::dot`](crate::convert::Internal::dot) therefore uses [`dot8`].
pub mod kernel {
    use crate::convert::Internal;

    /// One accumulator, sequential order. The definition, and the oracle.
    pub fn dot_naive<T: Internal>(x: &[T], h: &[T]) -> T {
        let mut acc = T::ZERO;
        for (a, b) in x.iter().zip(h) {
            acc = acc.add(a.mul(*b));
        }
        acc
    }

    /// Four independent accumulators.
    ///
    /// A single loop-carried accumulator is a chain of dependent adds with
    /// nothing to fill the FMA latency; PF-0.0 measured 3.90x against 0.99x for
    /// exactly this shape. Four is what `T = 32` and a 4- or 8-wide vector want.
    pub fn dot4<T: Internal>(x: &[T], h: &[T]) -> T {
        let (xc, xr) = x.as_chunks::<4>();
        let (hc, hr) = h.as_chunks::<4>();
        let mut a = [T::ZERO; 4];
        for (xs, hs) in xc.iter().zip(hc) {
            for ((acc, xv), hv) in a.iter_mut().zip(xs).zip(hs) {
                *acc = acc.add(xv.mul(*hv));
            }
        }
        let mut s = T::ZERO;
        for v in a {
            s = s.add(v);
        }
        for (p, q) in xr.iter().zip(hr) {
            s = s.add(p.mul(*q));
        }
        s
    }

    /// Eight accumulators, for the wide-vector tiers.
    pub fn dot8<T: Internal>(x: &[T], h: &[T]) -> T {
        let (xc, xr) = x.as_chunks::<8>();
        let (hc, hr) = h.as_chunks::<8>();
        let mut a = [T::ZERO; 8];
        for (xs, hs) in xc.iter().zip(hc) {
            for ((acc, xv), hv) in a.iter_mut().zip(xs).zip(hs) {
                *acc = acc.add(xv.mul(*hv));
            }
        }
        let mut s = T::ZERO;
        for v in a {
            s = s.add(v);
        }
        for (p, q) in xr.iter().zip(hr) {
            s = s.add(p.mul(*q));
        }
        s
    }
}
