# `vaco-checkasm` — the SIMD differential harness

## What it is

`vaco-checkasm` verifies that an optimised, runtime-dispatched kernel computes
the same function as its own scalar reference. Vaco has no hand-written
assembly to check (`#![forbid(unsafe_code)]` workspace-wide, D2 — SIMD comes
from `std::simd`/`fearless_simd`/autovectorisation, never inline asm), so
unlike its namesake this crate is not diffing asm against a C reference. It is
checking something structurally identical and just as easy to get wrong: a
`#[inline(always)]` body written once and monomorphised per
[`vaco_simd::Tier`] against the plain-Rust scalar implementation it was
optimised from.

It ships as both a library (`vaco_checkasm`, for a crate's own tests) and a
binary (`vaco-checkasm`, a standalone `verify`/`list`/`bench` CLI over a small
built-in kernel table). Benchmark mode measures real kernel adapters with an
explicitly named counter unit, so elapsed nanoseconds are never presented as
CPU cycles.

## How it works

Three pieces:

- **`Kernel`** (`src/differential.rs`) — implement this once per kernel
  family. It names a deterministic corpus (`cases()`) and how to run the
  scalar and vector sides over one case, flattened to a `Vec<Lane>` so
  multi-output kernels (three interleaved rows, say) still produce one
  comparable sequence.
- **`Differential<K>::run()`** — walks `K::cases()`, runs both sides, and
  compares lane for lane with `K::lanes_match` (default: `PartialEq`,
  overridable for a float kernel that wants `NaN` to match `NaN`). Returns a
  `Report<K>`.
- **`Report<K>`** — `is_clean()`, `total_mismatches()`, and up to 16 full
  `Mismatch`es (the case, the lane index, both values). `Display` prints a
  human-readable form; `assert_clean()` panics with it. A mismatch always
  names the exact input and the exact lane — "case 12: lane 3 diverged —
  scalar=-1 vector=0", never merely "kernel X failed".

`src/edge.rs` is the generator half: `lengths_around` sweeps every vector
width's tail (0, 1, width∓1, and the same at 2× and 3×, for 128/256/512-bit
tiers expressed in whatever element size the kernel uses), `boundaries_u8`/
`boundaries_i16`/`boundaries_i32` hit saturation limits, and
`float_specials_f32` covers signed zeros, both smallest subnormals, and NaN.
Random input finds average-case bugs; a kernel that only gets tested at
mid-range values will pass while its tail and saturation handling are broken
— these generators exist to make that impossible.

### Benchmark mode

`vaco-checkasm bench` measures the scalar and runtime-dispatched adapters for
each selected kernel. On Linux `x86_64` and `aarch64`, it first opens one
persistent, per-thread `perf_event_open` `CPU_CYCLES` counter and emits
`backend = "perf-event"`, `unit = "cycles"` only when every sample is a direct
unmultiplexed PMU count. The counter is reset/enabled before each call batch
and disabled/read after it; those operations are outside the measured closure.

If Linux denies PMU access, the event is unsupported, a pinned counter cannot
run, an ioctl/read fails, or `time_enabled != time_running` reports
multiplexing, bench mode writes a warning and reruns with `std::time::Instant`.
macOS and other targets use that same explicit `backend = "instant"`,
`unit = "ns"` fallback. There is deliberately no conversion from time to
synthetic cycles and no scaling of multiplexed values.

Each variant is warmed up, then calibrated so one timed batch lasts at least
20 microseconds. The no-op has the same `fn(&Case) -> Vec<Lane>` signature and
is calibrated independently; its per-call median is subtracted from the raw
samples. Sampling continues for at least 30 independently timed batches and
then stops when median absolute deviation is within 1%, the configured budget
is exhausted, or the 512-sample guard is reached. Results include raw and
corrected median, MAD, minimum and p95.

Hot measurements reuse the input. Cold measurements sweep a 64 MiB eviction
buffer immediately before each timed batch; the sweep itself is outside the
measurement. A production-sized case normally calibrates to one call, making
that call cold. If a tiny kernel requires batching, only the first call in the
batch is cold, so add a production-sized `Kernel::benchmark_case` override
before interpreting its cold result.

The current `Kernel` contract returns an owned `Vec`, so timings include
adapter dispatch, output allocation and output destruction. JSONL records this
as `scope = "adapter-inclusive"`; they are not claims about one isolated SIMD
instruction. `vaco-simd::ops::select_u8` uses a deterministic 1 MiB benchmark
case specifically to prevent timer overhead from dominating the real work.

```sh
CARGO_INCREMENTAL=0 cargo run -p vaco-checkasm --release -- bench \
  --test 'vaco-simd::ops::select_u8' --bench-cache both \
  --json /private/tmp/checkasm.jsonl

CARGO_INCREMENTAL=0 cargo run -p vaco-checkasm --release -- bench \
  --test 'vaco-simd::ops::select_u8' --bench-cache both \
  --baseline /private/tmp/checkasm.jsonl --fail-under 0.95
```

Schema-2 JSONL rows record a machine class, target OS and architecture alongside
the kernel, variant, cache state, backend and unit. Baselines only match the
complete identity, preventing cross-host comparisons or an `ns` row from being
compared with a `cycles` row. `VACO_CHECKASM_MACHINE` sets a stable runner class
for controlled CI; otherwise it defaults to `<os>-<arch>`. `baseline_ratio` is
stored median divided by current median: values above one are faster and
`--fail-under 0.95` allows at most a 5% slowdown. `reference_ratio` is scalar
median divided by the current variant's median for the same cache state.

`.github/workflows/checkasm-pmu-evidence.yml` is manually dispatched only. It
records the GitHub runner's PMU policy and CPU information, enables unprivileged
hardware events for its ephemeral runner, and fails unless every JSONL row from
the selected production kernel is honestly labelled `perf-event`/`cycles`.
Its artifact is evidence that the Linux backend ran; it is not a CI performance
regression gate on a shared runner.

The external [`scripts/perf-hwcycles.py`](../instruction-count-benchmarking.md)
still collects whole-process counts for Vaco and ffmpeg. It measures a
different scope from checkasm's per-adapter rows, so neither its process totals
nor an `Instant` fallback may be relabelled as per-kernel cycles.

### Why cross-tier coverage is per-machine, not per-run

`Kernel::vector` should route through a `vaco_simd::KernelSet`'s `select()`
table, so the tier actually exercised is whichever one
`vaco_simd::Caps::detect()` resolves to on the machine running the check.
Forcing a *weaker* tier on stronger hardware needs a capability token
fabricated without runtime evidence — every `assume_supported` in
`fearless_simd` is `unsafe`, closed to us by D2 — so one process cannot sweep
every tier. Coverage of SSE2 through AVX-512 and NEON accumulates across the
machines CI actually runs on, not within a single invocation. If a future
consumer genuinely needs same-process multi-tier coverage, that is a new,
separately-reviewed capability, not something to route around this
constraint for.

### The wired-in example (`src/kernels/scale_affine.rs`)

Demonstrates the harness against a real, shipping kernel rather than a toy:
`vaco-scale::fast::ScaleKernels::affine_row`, the colour-matrix row transform
every pixel-format conversion with a matrix change runs. The corpus stays
inside `0..=affine.max` deliberately — that is the kernel's own documented
`fits_i32` precondition, and testing outside it would exercise `i32` overflow
the contract already excludes, not a real divergence. A second test
(`the_fixture_actually_exercises_the_vector_path`) pins that the fixture
actually satisfies `fits_i32`, so a future change that silently pushed every
case onto the scalar fallback path would fail loudly instead of reporting a
green "verified" that verified nothing.

### The vertical-filter specialization (`src/kernels/scale_filter_v.rs`)

`vaco-scale::filter_v_generic_vs_fixed` compares the shipped generic
tap-major vertical filter with the fixed-width output-major implementation for
2, 4, 6, and 8 taps. This is not a SIMD dispatch pair: the existing result
labels retain their schema meanings, so `scalar` denotes generic and `vector`
denotes fixed-width for this adapter. The correctness corpus crosses all four
tap counts, i32-lane tail widths, and one to three output rows; the benchmark
case is an 8-tap 1920×1080 pass.

Both sides allocate an equal-sized output and scratch vector before their row
loops. Allocation and destruction therefore remain part of the existing
`adapter-inclusive` scope, but neither side receives an adapter-allocation
advantage. The fixed side calls `filter_v_fixed::<N>` directly rather than the
production dispatcher, so a fallback cannot turn the comparison into generic
versus generic.

The production functions and grid remain private. `vaco-checkasm` alone enables
`vaco-scale`'s default-off, documentation-hidden `checkasm` adapter feature,
whose opaque case type is the narrow cross-crate bridge to those private
callees.

A 2026-09-04 macOS smoke used the 1920×1080 case, hot cache, at least 30
samples and a 250 ms per-variant budget. The generic (`scalar`) median was
1,745,790.266 ns over 136 samples; the fixed (`vector`) median was 762,873.767
ns over 315 samples, a 2.288× reference ratio. Both JSONL rows were explicitly
`backend=instant unit=ns`. This confirms the fallback and adapter are useful,
but it is not Linux PMU runtime evidence and makes no raw-cycle claim.

## How to change it

- **Add a kernel to the CLI**: implement `Kernel` for a new marker type under
  `src/kernels/`, provide a production-sized `benchmark_case` when the
  correctness corpus is tiny, then add one `Entry` to `ENTRIES` in
  `src/main.rs`.
- **Add a kernel to your own crate's tests without touching this binary**:
  depend on `vaco_checkasm` (the library) and call
  `Differential::<YourKernel>::run().assert_clean()` from a `#[test]`. This is
  how `kernels::scale_affine` itself is tested, and it is the intended path
  for a codec crate (e.g. the H.264 motion-compensation interpolation
  kernels) that wants its own kernels checked without a change to this
  binary or a new dependency edge onto it.
- **Add an edge-case generator**: `src/edge.rs`. Keep it a pure function of a
  size/domain, never seeded — a case that cannot be reproduced from the
  report is not useful when it fails.
- **A ternary/masked-select kernel** (mask, a, b) → out: the `Kernel` trait
  is already generic enough — set `Case` to whatever triple the kernel needs
  and implement `scalar`/`vector` directly. No ternary-specific driver is
  required here; a masked-select *primitive* belongs in `vaco-simd::ops`
  (out of this crate's scope) if the composition itself is missing.

## Configuration

`bench` accepts `--test` and `--function` glob filters, `--bench-cache
hot|cold|both`, `--min-samples`, a per-variant `--budget` in milliseconds,
`--json`, and `--baseline`. `--fail-under R` gates stored-baseline ratios;
`--fail-slower-than-reference` gates vector rows slower than their scalar row.
`VACO_CHECKASM_MACHINE` optionally names a stable machine class in JSONL; it
does not override the separately recorded target OS or architecture. The tool
enables the default-off `vaco-scale/checkasm` dependency feature solely to
reach the opaque vertical-filter adapter described above.

## Dependencies

The portable backend uses the Rust standard library. Linux direct-cycle support
uses the internal `vaco-hw-perf-event` OS-binding crate; its only unsafe surface
is the audited Linux UAPI boundary. The harness also depends on `vaco-core`,
`vaco-simd` (the `KernelSet`/`Caps`/`Tier` vocabulary), and,
for the wired-in examples only, `vaco-scale`, `vaco-color`, `vaco-pixfmt`.
