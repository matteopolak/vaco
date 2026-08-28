# `vaco-tx` — transforms

Implements `planning/17-scale-resample-tx.md` Part C. Issues SP-C1 (#243), SP-C2
(#244), SP-C3 (#245), SP-C4 (#526), SP-C6 (#246).

---

## 1. What it is

One crate providing every transform the codec layer needs, in three precisions:

| Kind | Meaning | `f32` | `f64` | `i32` |
|---|---|:-:|:-:|:-:|
| `Fft` | complex-to-complex DFT, forward and inverse | ✅ | ✅ | ✅ |
| `Mdct` | modified DCT; forward `N → N/2`, inverse `N/2 → N/2` or `N` | ✅ | ✅ | ✅ |
| `Rdft` | real-to-complex DFT and its inverse | ✅ | ✅ | ✅ |
| `Dct` | DCT-II forward, DCT-III inverse | ✅ | ✅ | ✅ |
| `DctI` | DCT-I, self-inverse up to a scale | ✅ | ✅ | ✅ |
| `DstI` | DST-I, self-inverse up to a scale | ✅ | ✅ | ✅ |

It contains **no codec knowledge**: no windows, no psychoacoustics, no I/O.
Windowing (sine, KBD, Vorbis power-complementary) is codec-specific and belongs
in the codec DSP crate.

Two properties are the reason this crate exists rather than a dependency:

**`Plan::new` is total over transform lengths.** For `TxKind::Fft`, *every*
`len` in `1..=16_777_216` succeeds. A codec asks for the length its bitstream
specifies and gets a plan; it never carries a fallback for "the transform crate
cannot do this size". `Plan::describe()` reports which decomposition rule fired.

**The `i32` path is a specification.** Several codecs define fixed-point
decoding normatively and are conformance-tested against exact output. §5 states
the arithmetic contract precisely enough to implement against; it is pinned by
golden vectors and versioned with the crate.

---

## 2. How it works

### 2.1 The shape of the crate

```
Plan::new(kind, dir, len, scale, flags)
        │
        ├── derived transform  (mdct / rdft / dct / dct1)  ── pre/post processing, O(n)
        │            │
        │            ▼
        └────► Engine   ── the complex FFT, split-complex, forward only
                     │
                     ├── Stockham      mixed radix {2,3,4,5,7,8}
                     ├── Direct        O(n²), small n and fixed-point awkward n
                     ├── PrimeFactor   Good–Thomas over coprime factors
                     ├── Rader         prime p → cyclic convolution of length p−1
                     └── Bluestein     anything else → convolution at next_pow2(2n−1)
```

Every non-FFT transform reduces to a complex FFT plus `O(n)` work. That is what
lets one crate cover the whole surface with one algorithm family — and it means
a bug in the FFT shows up in all six kinds at once, which the tests exploit.

### 2.2 Decomposition, in the order the rules are tried

| # | Condition | Engine | Notes |
|---|---|---|---|
| 1 | `n = 1` | identity | |
| 2 | `n` factors over `{2,3,5,7}` | Stockham mixed radix | the common case; every codec length |
| 3 | `n ≤ 32`, or `i32` and `n ≤ 4096` | direct `O(n²)` | beats Rader for tiny primes; for `i32` it is a **precision** decision (§5.6) |
| 4 | `n = n₁·n₂`, `gcd = 1` | Good–Thomas | no twiddles between the coprime stages |
| 5 | `n` prime | Rader | convolution of length exactly `p−1`, not a padded power of two |
| 6 | anything else | Bluestein | `M = next_pow2(2n−1)`; never recurses |

Rule 6 is the safety net that makes the whole thing total. Rule 4 peels the
smooth part first (`176 = 16·11`, `120 = 8·3·5`), then falls back to splitting
off the first prime power.

### 2.3 Why Stockham, and why that decides several other things

Stockham autosort was chosen over a bit-reversed in-place FFT for three reasons,
all of which this crate needs:

1. **No permutation pass.** Input and output are both in natural order, so there
   is no digit-reversal scatter to write, to vectorise, or to get wrong for a
   mixed-radix length.
2. **Every stage is exactly one radix.** That is what makes the fixed-point
   contract *statable*: "a radix-`r` stage divides by `r`" is a complete account
   of the scaling, and it lands on exactly `1/n` for every decomposition.
3. **The inner loop is already a batch of independent sub-transforms.** Plan 17
   §C.6.2's "vectorise across sub-transforms, not within butterflies" is not a
   transformation applied to this loop — it *is* this loop.

The recurrence, with `n_cur` the remaining size, `s` the number of
sub-transforms already built (starting at 1), `m = n_cur / r`:

```
  a[j]  = src[q + s·(p + j·m)]                        j ∈ [0, r)
  b     = DFT_r(a)
  dst[q + s·(r·p + k)] = b[k] · exp(-2πi·p·k/n_cur)    k ∈ [0, r)
  n_cur ← m,  s ← s·r,  swap src and dst
```

### 2.4 The inverse is not a second implementation

`IDFT(x) = swap(F(swap(x)))`, where `swap` exchanges the real and imaginary
arrays. This follows from `swap(z) = i·conj(z)` and `IDFT(x) = conj(F(conj x))`,
and because the internal layout is split-complex the swap is *passing the two
arrays in the other order* — zero instructions.

So there is one engine, one set of kernels and one set of twiddle tables. The
inverse cannot drift from the forward, because there is nothing to drift.

### 2.5 Split-complex, and the SIMD story

Internally `re` and `im` are separate arrays. A complex multiply is then
`(ar·wr − ai·wi, ar·wi + ai·wr)` — four multiplies, two adds, and **zero
shuffles**, at any lane width on any architecture. This is plan 17 §C.6.1's main
lever and it is what removes the permute problem the research warned about. The
public API takes interleaved `[re, im, …]`; conversion happens once on the way in
and once on the way out, `O(n)` against `O(n log n)`.

The vector loop runs over `q` in chunks of the lane width. Because `q + s·p` is
contiguous, this gives contiguous loads, contiguous stores and a **broadcast**
twiddle — every lane in a vector shares the same `p`. There is no permute
anywhere in `src/simd.rs`.

**There is one butterfly implementation, not two.** Kernels are generic over the
`Lane` trait, which `f32`, `f64`, `i32` and the SIMD vector wrapper all
implement. The vectorised radix-8 butterfly and the scalar one are the same
source, monomorphised twice. What the differential tests actually check is the
load/store indexing — which is where the risk lives.

### 2.6 The stages that are not vectorised — measured

Plan 17 §C.6.3 asks about "the last `log₂(lanes)` stages". **In a Stockham flow
they are the first stages, not the last**, because `s` starts at 1 and multiplies
by the radix. The planner emits the largest radix first, so for every realistic
length and lane count exactly **one** stage — the first, at `s = 1` — falls below
the vector width. For `n = 1024` the radices are `[8, 8, 8, 2]` and `s` runs
`1, 8, 64, 512`.

Measured on Apple M5, `aarch64-apple-darwin`, NEON, 4-lane `f32`, `bench`
profile, median of 100 samples, forward `f32` FFT:

| n | vector | scalar | ratio |
|---:|---:|---:|---:|
| 64 | 169 ns | 205 ns | **1.22×** |
| 256 | 698 ns | 979 ns | **1.40×** |
| 1024 | 2.92 µs | 4.08 µs | **1.40×** |
| 4096 | 12.24 µs | 20.04 µs | **1.64×** |

Both columns are the *same plan*, run twice; `Tx::set_scalar_reference(true)`
forces the scalar kernels, so the ratio isolates the kernels from the
boundary conversion and the plan tables. Reproduce with
`cargo bench -p vaco-tx -- stages`.

The machine these were taken on is shared with other build jobs, so **read the
ratio, not the absolute nanoseconds**: a re-run under load moved both columns by
1.4× and left the ratio at 1.41× to two decimal places. The absolute figures in
§6 were taken on an otherwise-idle machine and reproduce to a few percent there.

**A 1.2–1.6× SIMD win at 4 lanes is well short of 4×, and the un-vectorised
first stage is not the main reason.** At `n = 4096` that stage is one of four,
so if it ran at the vector rate the transform would be at most ~1.25× faster
again. The rest is register pressure in the radix-8 butterfly (eight complex
values is sixteen vectors, half the NEON register file, before temporaries) and
the `O(n)` interleave/deinterleave at the boundary, which is why the ratio is
worst at `n = 64` where that boundary is the largest fraction of the work.

**One experiment worth recording because its result is counterintuitive.**
Preferring radix 4 over radix 8 improves the SIMD *ratio* — 1.82× at `n = 1024`
against 1.40× — while making the transform **slower in absolute terms**: 3.54 µs
against 2.92 µs. The extra stage costs more than the better register behaviour
saves, and the scalar baseline degrades more than the vector path improves. The
ratio is a misleading optimisation target on its own; the crate keeps radix 8.

Unmeasured and open: this is aarch64 only, with one SIMD level. x86 — where the
lane count doubles or quadruples and where the first-stage fraction therefore
grows — has not been measured, and D12's second addendum flags cross-tier
bit-exactness as blocking for exactly that reason.

---

## 3. How to change it

### 3.1 Where things live

| File | Contents |
|---|---|
| `src/fixed.rs` | the normative Q31 primitives. **Changing anything here is a codec-affecting decision.** |
| `src/num.rs` | `Lane` (the arithmetic a butterfly needs), `Arith` (what a plan needs), `TxSample` (public, sealed) |
| `src/butterfly.rs` | radix 2/4/8 straight-line kernels, generic odd-radix kernel, radix constants |
| `src/factor.rs` | factorisation, modular arithmetic, radix selection, `MAX_LEN` |
| `src/engine/` | the five complex-FFT engines and the decomposition selector |
| `src/derived/` | MDCT, RDFT, DCT-IV, DCT-II/III, DCT-I/DST-I |
| `src/simd.rs` | the vector `Lane` impl and the vectorised stage passes |
| `src/plan.rs` | `Plan`, `Tx`, buffer contracts, flags |
| `src/reference.rs` | direct `O(n²)` definitions. Verification only; nothing fast calls it. |

### 3.2 Adding a radix

Three edits, in this order:

1. `butterfly.rs`: a `bfN` function plus a `kernels::kN` wrapper with the shared
   signature `fn(KConst<'_, L::Const>, &mut [L; N], &mut [L; N])`. Odd radices
   can just delegate to `bf_odd::<L, N>` — the tables are generated already.
2. `engine/stockham.rs`: `scalar_pass!(passN, N, kernels::kN);` and a match arm.
3. `simd.rs`: `vector_pass!(vpassN, N, kernels::kN);` and a match arm.
4. `factor.rs`: add it to `KERNEL_RADICES`, `KERNEL_PRIMES` if prime, and emit it
   from `smooth_radices` — **largest radix first**, which is what keeps the
   number of sub-vector-width stages at one.

The gotcha: array lengths in the kernels are concrete literals, never `[L; R]`
with `R` a const parameter. With a const parameter, `re[7]` inside a `match R`
arm is a post-monomorphisation error for `R = 2` even though that arm can never
run. That is why the passes are macro-generated rather than const-generic.

### 3.3 Changing the fixed-point contract

Do not do this casually. If you must:

1. Change `src/fixed.rs` and/or `Lane for i32` in `src/num.rs`.
2. Update §5 of this document — the prose and the code must not drift.
3. Regenerate the golden table:
   `cargo test -p vaco-tx --test golden_i32 -- --ignored --nocapture print_golden`
   and paste the emitted block into `tests/golden_i32_digests.in`. **Review the
   diff; do not paste it blind.**
4. Re-run any codec conformance suite that depends on it. A digest moving means
   a fixed-point decoder's output moved.

### 3.4 Where the performance work is, in priority order

1. **The `s = 1` stage.** Its input is contiguous per leg and its twiddles are a
   linear scan; only the output is a stride-`r` scatter. `store_four_interleaved`
   exists in the substrate for 128-bit vectors, which covers radix 4 exactly.
   Worth ≤ 1.25× at `n = 4096`.
2. **Register pressure in the radix-8 vector butterfly.** Sixteen live vectors
   before temporaries. Restructuring `bf8` to consume its inputs in place is the
   obvious attempt; check the spill count, not the ratio (see the measurement
   report's Rule A).
3. **Batching.** The vector loop processes one vector per iteration. The
   `vaco-simd` measurements say batch until you spill — but with radix 8 we are
   already at the limit, so this is a radix-2/4 optimisation only.
4. **Cache blocking (plan 17 §C.3.2).** Deliberately deferred. The trigger is
   `n = 8192` (Vorbis); `n = 32768` in the benchmark shows the knee — 203 µs for
   32768 against 27.8 µs for 8192 is worse than the `n log n` scaling predicts.
   Nothing shipping needs it yet.

### 3.5 What was deferred, and why

**Conjugate-pair split-radix (plan 17 §C.3.1 rule 1) is not implemented.** The
power-of-two path is radix-8/4/2 Stockham instead. Two reasons, and the second is
the load-bearing one:

* Split-radix's advantage over radix-8 is roughly 5–10% of arithmetic
  operations, on a path where the measurements above say memory layout and
  register pressure dominate — not operation count.
* **Split-radix has no uniform per-stage scaling rule.** Its flow graph decomposes
  `N` into one `N/2` and two `N/4` sub-blocks with *different depths*, so
  "divide by the radix at every stage" has no meaning and the fixed-point
  contract of §5 could not be stated for it. Plan 17 §C.5.2 pins scale-every-stage
  and §C.3.1 pins split-radix, and the two do not compose. Something had to give,
  and the arithmetic contract is the one with a conformance requirement behind it.

The consequence to accept: on a pure power-of-two length we are leaving a single-
digit percentage of arithmetic on the table. If that ever matters, the honest
route is a float-only split-radix path selected explicitly, with the `i32` path
staying on Stockham — and `Decomposition` gaining a variant so `describe()` still
tells the truth.

**`i32` has no SIMD path.** It runs the scalar kernels at every level. This is
deliberate: the contract needs `i64` accumulation and 32×32→64 multiplies, which
x86 below AVX-512 does not have and which would need widening plus recombination;
and the reproducibility guarantee is *stronger* when there is structurally only
one code path. Measured cost: `i32` is 2.4–3.2× slower than `f32` (§6). If a
fixed-point codec ever makes that the bottleneck, the work is a `Lane` impl for a
vector `i32` wrapper — the butterflies need no change at all — plus a golden-vector
run to prove it did not move a single bit.

**No `bitexact` flag (plan 17 §C.7 class B).** The float paths are class C. As it
happens the SIMD and scalar `f32` paths agree *bit for bit* today — same source,
no FMA — and `tests/properties.rs` asserts exact equality. That is stronger than
class C promises and is deliberately **not** part of the contract: it must not
become something a codec depends on. If a codec turns out to need byte-exact
float output, add the flag then, with that codec's conformance vectors in hand.

**`rustfft` as a differential oracle remains unadopted** — see §7.1. It would
strengthen `tests/oracle.rs` above `n ≈ 1024`, where `reference.rs`'s direct
`O(n²)` evaluation stops being trustworthy on its own arithmetic cost.

**Fuzzing**: `fuzz/fuzz_targets/tx_plan.rs` drives `Plan::new` and `Tx::execute`
over `(kind, dir, len, flags, scale)` plus arbitrary input samples, length
capped at 8192 (see the target's own doc for why). It found one real slow
input above that cap before the cap was added: `TxKind::DctI` at
`len = 933439` took several seconds of CPU time, because `DctI`'s inner FFT
runs at `2*(len-1)` and a badly-factoring length there can chain Rader and
Bluestein recursively. `Plan::new`'s totality contract (§2.1) is not violated
— it still returns a valid plan — but the cost at an unfriendly length well
past the crate's own 8192/32768 benchmark range is unmeasured and undefended
against. Worth a real look if a codec ever asks for a `DctI`/`DstI` length in
that range from untrusted input; nothing currently does.

---

## 4. Configuration

There are no environment variables, no features and no build-time knobs. Every
decision is a plan parameter.

### 4.1 `Plan::new(kind, dir, len, scale, flags)`

| Parameter | Meaning |
|---|---|
| `kind` | `Fft`, `Mdct`, `Rdft`, `Dct`, `DctI`, `DstI` |
| `dir` | `Forward` / `Inverse` |
| `len` | the transform length. `1..=2^24`. |
| `scale` | applied to the output. `T::IDENTITY_SCALE` is compared at plan time and the pass is dropped, so the default costs nothing. |
| `flags` | see below |

| Flag | Effect |
|---|---|
| `INPLACE` | permits `Tx::execute_inplace`. Rejected at plan time unless `input_len == output_len`. |
| `UNALIGNED` | accepted and recorded, but this crate never assumes alignment: every access goes through a slice. The flag exists so a caller porting from an aligned-load API does not have to think about it. |
| `FULL_IMDCT` | the inverse MDCT emits all `len` samples instead of the `len/2` unique ones. A fill, not extra transform work. |
| `REAL_TO_REAL` | the complex side of an `Rdft` carries only real parts; `output_len` halves. |
| `REAL_TO_IMAGINARY` | likewise, imaginary parts. Mutually exclusive with the above. |

### 4.2 Buffer contract

| Kind | Direction | `input_len` | `output_len` |
|---|---|---|---|
| `Fft` | either | `2n` (interleaved) | `2n` |
| `Rdft` | forward | `n` | `2(n/2+1)`, or `n/2+1` with R2R/R2I |
| `Rdft` | inverse | `2(n/2+1)` or `n/2+1` | `n` |
| `Mdct` | forward | `n` | `n/2` |
| `Mdct` | inverse | `n/2` | `n/2`, or `n` with `FULL_IMDCT` |
| `Dct` / `DctI` / `DstI` | either | `n` | `n` |

A short buffer produces no output rather than a panic. `clippy::panic`,
`unwrap_used` and `indexing_slicing` are denied workspace-wide, and this crate is
on the path of data derived from untrusted bitstreams.

### 4.3 Domain restrictions

`Mdct` needs `len % 4 == 0`; `DctI` needs `len ≥ 2`. Those are properties of the
transforms, not of the decomposition. Everything else accepts every length —
including odd-length `Rdft`, which runs a full complex transform with a zero
imaginary part rather than declining.

### 4.4 Constants worth knowing

| Constant | Value | Where | Why |
|---|---|---|---|
| `MAX_LEN` | `2^24` | `factor.rs` | a bound on what a bitstream-derived length can make us allocate (D6). Not an algorithmic limit. |
| `DIRECT_MAX_FLOAT` | 32 | `engine/mod.rs` | above this Rader wins |
| `DIRECT_MAX_FIXED` | 4096 | `engine/mod.rs` | a precision threshold, not a speed one (§5.6) |
| `MAX_RADER_DEPTH` | 6 | `engine/mod.rs` | Rader's inner length is `p−1`, which may itself be prime; Bluestein takes over below the cap |

### 4.5 Scaling conventions

| Precision | Forward | Inverse | Round trip |
|---|---|---|---|
| `f32` / `f64` | the mathematical transform | unnormalised inverse | `n·x` |
| `i32` | transform divided by `S` | inverse divided by `S` | `x/n` |

`S` is `n` for `Fft`, `Rdft`, `Mdct` and `Dct`; `2(n−1)` for `DctI` and `2(n+1)`
for `DstI` — the length of the symmetric extension those two run internally.

---

## 5. The `i32` arithmetic contract

**Normative.** `src/fixed.rs` is the executable copy of this section and the two
must not drift. Every value is pinned by `tests/golden_i32.rs`.

### 5.1 Representation

Samples and twiddle factors are **Q31**: a signed 32-bit integer `v` denotes
`v / 2^31`, nominal range `[−1, 1)`. `1.0` is not representable; `fixed::ONE`
(`i32::MAX`, i.e. `1 − 2^-31`) is the saturated encoding and `Plan` treats it as
the identity scale, dropping the scaling pass rather than multiplying by it.

### 5.2 Rounding: round-half-up, everywhere

```rust
fn round_shift(x: i64, s: u32) -> i32 {          // round(x / 2^s)
    clamp_i32(x.saturating_add(1i64 << (s - 1)) >> s)
}

fn round_div(x: i64, d: i64) -> i32 {            // round(x / d), d > 0
    if d & (d - 1) == 0 { return round_shift(x, d.trailing_zeros()); }
    clamp_i32(x.saturating_add(d / 2).div_euclid(d))
}
```

Exact halves go toward `+∞`. Chosen because it is a single add-then-shift on
every architecture and has no data-dependent branch. `round_div` and
`round_shift` agree bit for bit when `d` is a power of two, which is what lets
radix-2/4/8 stages use the cheap form without changing the contract.

### 5.3 Overflow: saturate, never wrap

Both the `i64` accumulation and the narrowing to `i32` saturate. `add`, `sub`,
`neg` and every accumulator operation are saturating. Nothing wraps, nothing
panics, nothing is UB. A wrap would appear as a sign flip, which is the worst
failure mode a fixed-point decoder has;
`tests/golden_i32.rs::saturation_never_wraps` pins this.

### 5.4 Constants

Every twiddle and every algebraic constant enters through

```rust
fn quantise(x: f64) -> i32 {                      // round(x · 2^31), half up, saturating
    let scaled = (x * f64::from(1u32 << 31) + 0.5).floor();
    // then clamped into i32
}
```

computed in `f64` at plan time. The product has at most 32 significant bits and
`f64` carries 53, so the multiply is exact and two plans for the same length hold
bit-identical tables regardless of build profile or architecture. Plan-time
tables that themselves require a transform (Rader's and Bluestein's kernel
spectra) are generated through the **scalar** path deliberately, so a plan does
not depend on the host's SIMD level.

`quantise(1.0) = i32::MAX`, `quantise(-1.0) = i32::MIN`, `quantise(0.5) = 2^30`.

### 5.5 Complex multiply

For `(ar + i·ai)·(wr + i·wi)`, all Q31:

```
  re = round_shift(ar·wr − ai·wi, 31)     with a saturating i64 subtract
  im = round_shift(ar·wi + ai·wr, 31)     with a saturating i64 add
```

The `i64` intermediate is **mandatory**: the product of two Q31 values needs 62
bits and there is no correct way to do it in 32. Four multiplies, one add, one
subtract — deliberately not a three-multiply Karatsuba form, whose different
rounding would put this path on a different side of the closed forms the tests
compare against.

### 5.6 Stage scaling: divide by the radix, every stage

**Each radix-`r` stage divides every input sample by `r`** with `round_div`,
before the butterfly. Three consequences:

* A forward transform produces `DFT(x) / n`, exactly, for every decomposition —
  because `Π r = n`.
* It never overflows.
* It costs `log_r(n) / 2`-ish bits of precision.

This is plan 17 §C.5.2's pinned policy, chosen over block floating point for one
reason: it introduces **no data-dependent shift**. A data-dependent shift is
exactly the kind of thing where a vector variant can diverge from the scalar
reference, and this crate's whole reproducibility claim rests on there being no
such place.

Bounds, so a future change can check itself: after the divide, `|a| ≤ 2^31/r`.
The widest partial sum in the odd-radix kernel is `≈ 2^62.9` at `r = 7` — inside
`i64` — and every operation saturates regardless.

### 5.7 Butterfly accumulation

Small-radix kernels accumulate in `i64` (`Lane::Acc`) and narrow once, with
`round_shift(acc, 31)`. Promoting a sample into the accumulator is
`(x as i64) << 31`, which is exact, so a sum of un-multiplied samples narrows
back with no rounding at all.

### 5.8 The `scale` parameter

Applied to the output as a Q31 multiply, `round_shift(x·s, 31)`. Skipped
entirely when `scale == fixed::ONE`, which is compared once at plan time.

### 5.9 Precision, measured

Forward `f32`-equivalent SNR of the `i32` FFT against an `f64` direct evaluation:

| n | SNR | effective bits |
|---:|---:|---:|
| 64 | 152.5 dB | ~25 |
| 128 | 149.9 dB | ~25 |
| 256 | 148.8 dB | ~25 |
| 512 | 143.0 dB | ~24 |
| 1024 | 140.9 dB | ~23 |
| 2048 | 138.9 dB | ~23 |

Comfortably above the 16- and 24-bit output the codecs produce.
`tests/golden_i32.rs` asserts floors ~6 dB below these, so a real regression
fails before it becomes audible while ordinary noise does not.

**Precision of the awkward lengths.** Rader and Bluestein normalise through a
convolution, and the normalisation is where the precision goes: the intermediate
sits at roughly `2^31·n/M²`, so a Bluestein length loses on the order of
`log₂(M²/n)` bits. That is why `DIRECT_MAX_FIXED` is 4096 rather than 32 — a
direct `O(n²)` DFT rounds **once per output** and is by far the most accurate
fixed-point option at any length where it is affordable. No shipping codec asks
for a fixed-point transform at a length that reaches Bluestein; if one ever does,
this is the number to check first.

### 5.10 What is guaranteed

Bit-identical output across architecture, lane width, build profile and
optimisation level, for every kind and every size. This is **structural**, not
tested-for: the `i32` path has no SIMD variant and no data-dependent control flow
anywhere.

---

## 6. Benchmarks

`cargo bench -p vaco-tx`. Apple M5, `aarch64-apple-darwin`, NEON, rustc 1.97.1,
`bench` profile, median of 100 samples.

| Scenario | n | `f32` | `i32` | `i32` / `f32` |
|---|---:|---:|---:|---:|
| complex FFT | 64 | 166 ns | 375 ns | 2.3× |
| | 128 | 349 ns | 906 ns | 2.6× |
| | 256 | 703 ns | 1.92 µs | 2.7× |
| | 512 | 1.43 µs | 4.08 µs | 2.9× |
| | 1024 | 2.94 µs | 9.31 µs | 3.2× |
| | 4096 | 12.4 µs | — | |
| | 8192 | 27.8 µs | — | |
| | 32768 | 203 µs | — | |
| complex FFT, Opus/CELT (2·3·5) | 120 | 344 ns | | |
| | 240 | 687 ns | | |
| | 480 | 1.41 µs | | |
| | 960 | 2.85 µs | | |
| MDCT | 256 (AC-3) | 281 ns | 562 ns | 2.0× |
| | 512 (AC-3) | 594 ns | 1.29 µs | 2.2× |
| | 960 (Opus) | 1.14 µs | | |
| | 2048 (AAC LC) | 2.35 µs | 5.62 µs | 2.4× |
| IMDCT | 256 | 302 ns | | |
| | 2048 | 2.37 µs | | |
| RDFT | 512 | 1.06 µs | | |
| | 2048 | 4.33 µs | | |
| DCT-II | 32 | 84 ns | | |
| | 512 | 1.33 µs | | |
| `f64` FFT | 1024 | 4.42 µs | | |

Awkward lengths — the cost of totality, not of anything a codec asks for:

| n | rule | time |
|---:|---|---:|
| 97 | Rader | 646 ns |
| 121 | Bluestein (M = 256) | 1.52 µs |
| 143 = 11·13 | Good–Thomas | 2.04 µs |
| 251 | Rader | 2.50 µs |
| 1021 | Rader | 29.7 µs |
| 2809 = 53² | Bluestein (M = 8192) | 59.0 µs |

Cold `Plan::new`: 1.6 µs at 256, 4.5 µs at 960, 8.3 µs at 2048; 2.5 µs for the
prime 97 and 26 µs for the prime 1021 — the primes are dominated by building the
inner convolution's kernel spectrum, which is itself a transform.

`f64` at 1024 is 1.5× `f32`, which is what a scalar path against a 4-lane
vector path should look like: `f64` has no vector kernel (see §3.5).

---

## 7. Dependencies

| Dependency | Why |
|---|---|
| `vaco-core` | the `Error` taxonomy |
| `vaco-simd` | the D11 adapter over `fearless_simd`; `Caps` and `dispatch_kernel!` |
| `bitflags` | `TxFlags` |
| `proptest` (dev) | randomised round-trip and linearity |
| `divan` (dev) | benchmarks |

**No `num-complex`, and no complex type at all.** The public API passes
interleaved `[re, im, …]` slices; the internal representation is split-complex
and needs none.

### 7.1 Build-or-buy (D10)

`rustfft` and `realfft` both clear D10's hard gates and both fail on model fit.
The decisive facts, from plan 17 §C.10.2, all confirmed in the course of writing
this:

1. **Fixed point is structurally excluded.** `rustfft` is generic over
   `FftNum: Float`. This is not a missing feature; it is a type-level exclusion.
   The `i32` paths are ours regardless.
2. **Half the surface is missing.** MDCT is the transform audio codecs actually
   use and neither crate has it. `rustdct` covers DCT-II/III but not MDCT.
3. **The layout mismatch is real.** `rustfft` works in interleaved `Complex<T>`;
   our MDCT pre/post-rotation is written in split-complex and would convert twice
   per transform for no benefit.

**`rustfft` as a dev-only differential oracle for the float paths is a good
idea and was not adopted, because it is not in `[workspace.dependencies]`** and
D10 makes every adoption a reviewed decision. Requested; see the SP-C report.
In its place, `src/reference.rs` provides direct `O(n²)` definitions in `f64`,
which is a *stronger* oracle below `n ≈ 1024` (it is obviously correct by
inspection rather than correct by another team's argument) and unusable above it.
That gap is exactly where `rustfft` would earn its keep.

---

## 8. Tests

| File | What it establishes |
|---|---|
| `src/butterfly.rs` (unit) | every radix kernel against a direct DFT |
| `src/fixed.rs`, `src/num.rs` (unit) | the contract's primitives: rounding, saturation, quantisation |
| `src/factor.rs` (unit) | factorisation against a sieve, primitive roots generate the group, radix selection |
| `tests/oracle.rs` | every kind, both directions, against `reference` — 70 lengths covering all six decomposition rules |
| `tests/properties.rs` | round-trip, linearity, Parseval, DC/impulse/tone, shift theorem, **SIMD vs scalar bit-exact**, `Plan::new` totality, decomposition-rule selection |
| `tests/golden_i32.rs` | the contract: digests, literal small vectors, determinism, saturation, SNR floors |
| `fuzz/fuzz_targets/tx_plan.rs` | `Plan::new`/`Tx::execute` over arbitrary kind, direction, length (≤ 8192) and flags |

The tone test earns its place: a flipped twiddle sign is the single most common
FFT bug and it shows up there as energy in bin `n−k` instead of bin `k`.
Round-trip alone would not catch it.
