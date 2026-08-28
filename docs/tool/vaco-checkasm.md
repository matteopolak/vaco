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
binary (`vaco-checkasm`, a standalone `verify`/`list` CLI over a small
built-in kernel table).

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

## How to change it

- **Add a kernel to the CLI**: implement `Kernel` for a new marker type under
  `src/kernels/`, then add one `Entry` to `ENTRIES` in `src/main.rs`.
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

None — no env vars or feature flags. `vaco-checkasm verify` and
`vaco-checkasm list` are the only CLI surface.

## Dependencies

`vaco-core`, `vaco-simd` (the `KernelSet`/`Caps`/`Tier` vocabulary), and,
for the wired-in example only, `vaco-scale`, `vaco-color`, `vaco-pixfmt`.
