# `vaco-simd`

Layer 0 (foundation). Depends on `fearless_simd` and `vaco-core`. Dev: `proptest`.

## What it is

The project's SIMD substrate and kernel-dispatch layer, and the **D11 adapter** over
`fearless_simd`: that crate is reachable from here and from nowhere else in the workspace, and
kernels are written against our own `KernelSet` abstraction so the substrate stays swappable.

It exists to resolve a conflict. D2 forbids `unsafe` everywhere outside `vaco-hw-*`. Runtime ISA
dispatch — one binary that detects AVX2 / AVX-512 / NEON at startup and uses it — needs
`#[target_feature]`, whose calls are `unsafe` because the caller cannot prove the CPU has the
feature. `fearless_simd` supplies that proof as a *value*: a zero-sized capability token that can
only be obtained from a checked runtime detection. Holding the token is the proof, so the intrinsic
call is safe at every call site and `#![forbid(unsafe_code)]` survives across the entire DSP layer
(D12).

## How it works

Four things are exported.

| Item | Role |
|---|---|
| `Tier` | *Which* instruction set we resolved to. A plain `Ord` enum. Selects a kernel table; cannot call a kernel. |
| `Caps` | The capability **proof**. A `Copy` newtype over the substrate's `Level`. The only thing `dispatch_kernel!` accepts. |
| `KernelSet` | A table of `fn` pointers for one DSP area, built once per `Tier`. |
| `ops` | Every operation the substrate lacks, composed from ones it has, under our own names. |

`Tier` and `Caps` are separate on purpose: a `Tier` is a label you can store, compare and print, and
a `Caps` carries the token that makes an intrinsic call safe. You cannot synthesise the second from
the first, and that is the whole safety argument.

### Dispatch

`dispatch_kernel!(caps, simd => body)` wraps the substrate's `dispatch!`. Verified against v0.7.0:
the expansion is a `match` over the level that binds a token and calls `Simd::vectorize(token, ||
body)`, a safe trait method. **Nothing in the expansion is `unsafe`.**

The substrate's other entry point, `kernel!`, *does* expand `unsafe { … }` into the calling crate
(`kernel_macros.rs:226`) and is closed to us. Any operation the substrate does not expose must be
composed; we cannot reach through to the intrinsic.

`dispatch_kernel!` expands to `$crate::__substrate::dispatch!(…)`, going through a `#[doc(hidden)]`
re-export rather than naming `fearless_simd`. That is what lets a consumer crate use the macro
**without listing `fearless_simd` in its own `Cargo.toml`**, which is what keeps the D11 boundary
CI-checkable: the substrate appears in exactly one manifest under `crates/`.

Measured overhead: **0.00–0.23 ns per dispatch**, indistinguishable from a plain `fn` pointer.

### Authoring a kernel

`src/example.rs` is a complete worked example (`yuv420p → rgb24`, one row) written to be copied. The
shape is always:

1. **A scalar reference** — always compiled, definitionally correct, and *also the tail handler*, so
   there is no second edge implementation that could disagree with the body.
2. **One `#[inline(always)]` body generic over `S: Lanes`**, monomorphised once per level.
3. **A dispatching wrapper** built with `dispatch_kernel!`.
4. **A `KernelSet`** holding the `fn` pointers, resolved once in the consumer's constructor, so the
   indirect call is paid per row and never per pixel.
5. **A proptest against the scalar reference.** A kernel without one does not merge.

`#[inline(always)]` on step 2 is a **correctness-of-codegen requirement**, not a tuning knob: it is
how the dispatched level's target-feature context reaches the body. A kernel that fails to inline is
compiled at the ambient baseline — still correct, silently slow, and invisible to every correctness
test. The crate therefore turns off `clippy::inline_always` once at the root, with that reason.

### The two authoring rules the measurements produced

Both are worth more than any composition in `ops`, and both are invisible to correctness tests.

**Rule A — batch, until you spill.** LLVM unrolls an iterator loop 4x and does not unroll a
`chunks_exact` loop at all. Processing four vectors per iteration took `rounded_avg_u8` from 1.55x to
1.00x. But batching the 8-tap FIR to two output vectors made it *worse* (1.12x → 1.36x) because one
stack spill became six. Check the spill count, not just the ratio.

**Rule B — never carry a single accumulator.** A loop-carried vector accumulator is a chain of
dependent adds with nothing filling the latency. LLVM splits a scalar reduction into eight
accumulators automatically and will not do that to a hand-written loop. One accumulator measured
**3.90x**; four measured **0.99x**.

### `ops` — two mirrored namespaces

`ops::*` holds the **scalar references**: one lane, obviously correct, written with `std`'s own
primitives where one exists. `ops::simd::*` holds the **vector compositions under the same names**.
`ops::rounded_avg_u8` and `ops::simd::rounded_avg_u8` compute the same function, on one lane and on
N.

Every pair is proved equal by `tests/ops_agree.rs` — proptest for coverage, plus a deterministic edge
sweep from `testing` that walks every length from 0 to 193 and hits `0`/`MAX`, alternating patterns
and every bit position.

Plan 11 §5.4 rule 5 applies: anything in the gap table is called through `ops`, never open-coded. One
composition, one place to fix, one place to *delete* when the substrate grows the operation.

### Masked-lane select

`select_u8`/`select_i16`/`select_i32` (and their `simd::` siblings) are the one entry in `ops` that is
not a composition: `fearless_simd` already provides `Select` generically over `SimdBase::Mask` at
every lane width, so each is a one-line pass-through, named here so a kernel body reaches it under
this crate's own vocabulary instead of importing `fearless_simd::Select` directly. `select_u8` was
`#127`'s own spike; `select_i16`/`select_i32` closed `#619` — `vaco-codec-dsp-deblock`'s own recorded
blocker, which needed the widths its per-sample filter decisions actually compare at (`i16`/`i32`
sample differences against `α`/`β`/`tC0`), not just the `u8` output width `select_u8` alone covers.

The masks themselves need no composition either: `SimdBase::simd_lt`/`simd_le`/`simd_ge`/`simd_gt`/
`simd_eq` are already generic over native width (unlike the substrate's per-concrete-fixed-width
comparison methods reachable through the `Simd` level token, e.g. `simd_lt_i16x8` — the same
duplication `pack_u8_from_i16x8` hits). A kernel compares `S::i16s` values with `a.simd_lt(b)` and
feeds the resulting `S::mask16s` straight to `ops::simd::select_i16`.

Measured (`docs/core/simd-adoption-measurements.md`, Group 7): `select_u8` ≈0.10x scalar,
`select_i16` ≈0.19x, `select_i32` ≈0.43x — all three beat the branchy scalar loop cleanly, 5/5
independent runs, essentially zero variance. This is a real win on its own, not only an unblocker.

### Testing

`testing` is a normal (not `cfg(test)`) module so every kernel crate gets the harness for free, and
so `vaco-checkasm` and the per-crate proptests exercise the same corpus. `check_binary_u8` and
`check_unary_u8` sweep the edge corpus for binary/unary lane-wise ops; `check_ternary_u8` is the same
shape for a masked-lane-select-shaped `(mask, a, b) -> out` op — `select_u8`'s own edge-corpus test
used to hand-roll this sweep inline before `check_ternary_u8` existed. `assert_lanes_eq` reports the
first differing lane. `assert_close` exists for float kernels only — **integer kernels must be
bit-identical**, and any use of a tolerance must state and justify it.

Only `u8` gets an exhaustive-style edge-corpus driver (`edge_patterns` is byte-oriented, so it is the
practical width to enumerate). `select_i16`/`select_i32` are proptested instead
(`tests/ops_agree.rs`) — enumerating anything close to `i16`'s or `i32`'s value space is not
practical, and proptest's shrinking gives a minimal failing case if one ever turns up. Pinned unit
tests cover the extremes (`0`, `!0`, and the non-canonical-but-nonzero `MIN`) at full native-vector
width on top of the random sweep.

## How to change it

* **Adding a composed operation.** Add the scalar reference to `ops`, the vector form to
  `ops::simd` under the same name, and a proptest pair in `tests/ops_agree.rs`. Add an elementwise
  row to the `elementwise1!`/`elementwise2!` list if it fits that shape. Then benchmark it in
  `benches/adoption.rs` against a scalar loop — several "gaps" turn out to cost nothing, and you only
  find that out by measuring.
* **Deleting one.** When the substrate grows an operation natively, the composition in `ops::simd`
  is the only body that changes. Nothing downstream moves.
* **Adding a `Tier`.** `Tier` is `#[non_exhaustive]`. Add the variant, extend `Caps::tier()`'s
  cfg-gated ladder, and extend `Tier::name`. `KernelSet::for_tier` implementations must return a
  complete table for the new tier — falling back to scalar is fine and is the point of the trait's
  contract.
* **Swapping the substrate.** Rewrite this crate. `fearless_simd` is named in `lib.rs`
  (`__substrate`, `Lanes`, `Caps`) and inside `ops::simd`, and nowhere else in the workspace. What
  the adapter does *not* insulate you from is kernel bodies elsewhere: `Lanes` is a `pub use` of the
  substrate's `Simd` trait, not a newtype, so kernels call substrate methods directly. That is
  deliberate — wrapping 1,453 generated methods in newtypes would cost more than the swap it insures
  against, and a wrapper that failed to inline would silently produce non-vectorised code. **The real
  blast radius is one crate of thought plus a mechanical rename pass.** Plan 11 §5.3 states this
  precisely; do not oversell it.

### Gotchas

* **`#[inline(always)]` is load-bearing.** See above. If you must not inline a large body, use the
  substrate's `Lanes::vectorize(lanes, || …)` to re-establish the target-feature context.
* **`dispatch_kernel!` runs its body in a closure.** `?` and early `return` do not escape it. Return
  a value from the macro, or use `ControlFlow`.
* **The body is compiled once per level.** Keep it to a single call to an `#[inline(always)]`
  generic function; anything larger multiplies build time and binary size by the level count.
* **There is no `i16 → u8` saturating narrow.** `SimdNarrow` gives `i16 → i8` and `u16 → u8`, not the
  `packuswb`/`sqxtun` every pixel kernel ends with. Use `ops::simd::pack_u8_from_i16`, which costs a
  `max(0)` and a bitcast on top.
* **`S::i16s` and `i16x8<S>` do not unify generically.** They are the same type on NEON and SSE and
  different on AVX2, so a helper that names both an input and its narrowed output needs two versions.
  `pack_u8_from_i16` / `pack_u8_from_i16x8` are duplicated for exactly this reason.
* **`Level::Fallback` does not exist on aarch64** unless the substrate's `force_support_fallback`
  feature is on. That is fine and we do not enable it: `Tier::Scalar` means *our* scalar reference
  implementations, not the substrate's fallback backend. The two are unrelated.
* **`abs` on `MIN` passes the value through** (`max(MIN, -MIN)` is `MIN`, since the negation wraps).
  The scalar references are written to match the vector, not the other way round, and a test pins it.
  Callers who cannot accept that must range-limit first.

## Configuration

`VACO_TIER` is an optional process environment variable for differential
testing. It caps dispatch at a tier the running CPU already proved it supports;
it can never enable instructions the CPU lacks. Accepted spellings are
`sse2`, `sse4.2`, `avx2`, `avx512`, and `neon`; unknown, cross-architecture,
and unavailable values retain automatic detection. `scalar` is reserved as the
scalar-reference label, but is not a dispatch override because the shipped
substrate does not compile its fallback backend on x86-64 or aarch64. The
value is read once, on the first `Caps::detect()` call, so set it before
starting the process.

Two build knobs exist outside the crate:

* `RUSTFLAGS="--cfg disable_dispatch_avx512"` (also `_avx2`, `_sse4_2`, `_sse2`) prunes a level from
  multiversioning. It affects binary size only; the token types and `Caps::tier()` are unaffected.
  CI builds `vvmpeg` both with all dispatch levels and with the x86-64 baseline
  arm, rejecting an all-level binary larger than 2.5x the baseline build.
* `-C target-cpu=native` raises the *baseline*, which `Caps::baseline()` reports. `Caps::detect()`
  is unaffected and remains the right call for shipped code.

`Caps::detect()` is cheap to call repeatedly — the substrate caches its `cpuid` probe in a
`LazyLock` on x86, and the level is a compile-time constant elsewhere — but it should still be
stored, not re-derived per row.

## Startup and size measurement

On 2026-09-05, a release `vvmpeg` built from `b16885d5` was compared with
the same archive plus this change on an Apple Silicon host (macOS 26.6.2,
Rust 1.99.0-nightly). Both builds used `CARGO_INCREMENTAL=0`, an isolated
target directory, and one build job. A 398-byte WAV made by the `ffmpeg 9.0.1`
binary was opened before an intentionally unknown output extension, so each
Vaco invocation reaches the registry extension scan before returning its
format error. Twelve A/B/reference rounds alternated their order; each sample
contained 50 launches.

| Binary | Wall time / launch | CPU time / launch |
| --- | ---: | ---: |
| baseline Vaco | 11.417 ms | 6.450 ms |
| candidate Vaco | 11.683 ms | 6.400 ms |
| `ffmpeg` reference | 46.100 ms | 27.933 ms |

The Vaco wall-time difference is within the noise of such a short process;
the 0.8% CPU reduction is not presented as a reliable whole-process startup
win. The point of the registry change is instead structural and local: its
static-table scans no longer allocate a temporary filename or MIME `String`,
and muxer matching builds one borrowed `ProbeData` for the scan rather than
one per row. Both release binaries were 12,627,920 bytes on this ARM build;
the all-level and x86-dispatch-pruned flag builds were also equal because this
target has only the NEON arm. The CI budget is the x86-64 enforcement point.

Instruments' named `CPU Counters` template was available and recorded the
candidate, but the sub-millisecond target produced no counter interval, so no
instruction or cycle value is claimed. The App Launch trace is retained as a
secondary launch trace; grouped `/usr/bin/time -lp` values above provide the
reported wall and CPU measurements.

## Dependencies

| Crate | Why | Gate assessment |
|---|---|---|
| `fearless_simd` 0.7 | The SIMD substrate (D12). | Pass on all three D10 gates: zero dependencies, Apache-2.0 OR MIT, Linebender, MSRV 1.89. It contains `unsafe` internally (~116 occurrences, mostly in generated backends) — our guarantee is "no unsafe in our code", not "no unsafe in the process". |
| `vaco-core` | Foundation vocabulary types. | Workspace-internal. |
| `proptest` (dev) | The differential oracle for every composed op and kernel (D6). | Workspace-declared. |

The benchmark deliberately uses **no** framework: `benches/adoption.rs` is `harness = false` and
times paired A/B implementations by hand, because what the adoption checklist needs is the *ratio*
between a composition and its native counterpart, in one process on one core, as a min-of-N.

## See also

* `docs/core/simd-adoption-measurements.md` — what the substrate actually costs, on real hardware,
  with the disassembly to back it. Read it before assuming a gap composition is free, and before
  writing a kernel.
* `planning/00-decisions.md` — D2 (forbid unsafe), D11 (adapter boundary), D12 and its addendum.
* `planning/11-foundations.md` §5.3–5.7 — the fork decision, the authoring model, the gap table.
* `planning/12-performance.md` §11 — the adoption checklist this crate executed.
