# `vaco-codec-dsp-mecmp` — motion-estimation comparison functions

---

## 1. What it is

D-12 (#144): the four numeric costs an encoder's motion search evaluates on
every candidate offset it tries — SAD, SSD, mean-corrected variance, and
SATD. `vaco-codec-dsp-me` (D-13, #260, motion-estimation *search patterns*)
is the direct caller: it decides *where* to look, this crate decides *how
good* a candidate is.

None of these numbers is normative — there is no bitstream syntax element
any of them is ever written into, so there is nothing to be bit-exact
against. The correctness bar is that the scalar and SIMD-dispatched paths
compute the *same function*, which `vaco-checkasm`'s differential tests
(`crates/tool/vaco-checkasm/src/kernels/mecmp.rs`) verify directly.

| Function | Formula | Used for |
|---|---|---|
| `sad` | Σ &#124;cur − ref&#124; | the inner-loop cost for every D-13 search pattern |
| `ssd` | Σ (cur − ref)² | breaking SAD ties; feeds `variance` |
| `variance` | Σ(cur−ref)² − (Σ(cur−ref))²/N | SAD/SSD alone cannot tell "genuinely closer" from "same shape, uniform DC offset"; this subtracts exactly that bias |
| `satd` | Σ &#124;Hadamard(cur − ref)&#124; | mode/reference-frame decisions, evaluated once or twice per block rather than hundreds of times |

## 2. How it works

### 2.1 `Plane` — the shared, panic-free borrow

`plane::Plane<'a>` is a strided view (`data`, `stride`, `width`, `height`)
over caller-owned pixel data, with two operations: `row(y)` (returns a
possibly-short or empty slice rather than panicking on out-of-range `y`,
overflowing arithmetic, or a truncated backing buffer) and `sub(x, y, w, h)`
(a bounds-checked sub-block view, `None` on any overflow or out-of-bounds
read). Every comparison function reads through these two calls only — no
crate here indexes a slice directly. This matters because a motion search
evaluates hundreds of caller-computed offsets per block, many legitimately
close to a frame edge, and none of them should be able to panic the
encoder.

### 2.2 SAD/SSD/variance — vectorised; SATD — scalar only

`sad`, `ssd` and `sum_and_sse` (which `variance` is built from) each have a
`#[inline(always)]` body generic over `vaco_simd::Lanes`, monomorphised once
per `vaco_simd::Tier` by `dispatch_kernel!` — the same shape as
`vaco-scale::fast::affine_row`. The pipeline for a chunk of pixels is:
`abs_diff_u8` (or a widen-then-subtract for the signed difference
`variance` needs) → `widen_u8_i16` → widen again to `i32` → accumulate in
**two** `i32` vector accumulators, not one (a single loop-carried vector
accumulator is a dependency chain with nothing to fill the latency —
`vaco-simd`'s own measured "Rule B"). Whatever tail of a row does not fill a
whole SIMD chunk falls back to the scalar reference, so there is exactly
one definition of what a short or misaligned row means.

`satd` has no vector variant. A 4×4 Hadamard transform is a butterfly
network with an in-lane transpose, not a lanewise reduction, and this crate
does not yet have a tested transpose primitive to build one on —
`MecmpKernels::for_tier` returns the same scalar `satd` fn for every tier,
which is the "not yet vectorised" fallback `KernelSet::for_tier`'s own
contract names. SATD is the mode-decision metric (evaluated once or twice
per block), not the inner search loop, so this is a deliberately low-value
gap to leave for later rather than something blocking D-13/D-14.

`satd4x4`'s Hadamard butterfly (`hadamard4`) and the row/column transpose
around it are written with array-pattern destructuring
(`let [a0, a1, a2, a3] = v`) rather than indexing, to satisfy the
project-wide `indexing_slicing` deny without an `#[allow]`.

### 2.3 Why `KernelSet` fields are plain `fn` pointers, not closures

`MecmpKernels` mirrors `vaco-scale::fast::ScaleKernels`: a table of
same-signature `fn` pointers resolved once (`for_tier`/`select`/
`reference`), so the per-block cost of picking scalar vs. vector is one
indirect call amortised over a whole search, not a branch per pixel.

## 3. How to change it

- **A new comparison function**: add a scalar reference (always correct,
  doubles as the SIMD path's tail handler), decide whether it is hot enough
  to justify a vector body (SAD/SSD/variance are; SATD was judged not to be
  yet — see §2.2), wire both into `MecmpKernels`, and add a `Kernel` impl
  under `vaco-checkasm`'s `kernels::mecmp` (not in this crate — see the
  layering note below).
- **A vector body for `satd`**: needs an in-lane 4-element transpose. Check
  whether `vaco_simd::ops` already has one before writing it here (D19); if
  not, it likely belongs in `vaco_simd::ops` itself rather than duplicated
  per caller, per that crate's own "one composition, one place" rule — this
  crate does not own `vaco-simd` and cannot add it there directly.
- **Mismatched or malformed `(cur, refp)` pairs** (different declared
  sizes, a stride shorter than a row) are defined behaviour: every function
  computes over `overlap()`, the elementwise-minimum width/height, and
  `Plane::row`/`sub` degrade to shorter/empty results rather than panicking.
  Do not add a bounds assertion that would turn a mismatch into a panic —
  that is the exact failure mode `Plane`'s doc exists to prevent.

### Why the `vaco-checkasm` wiring lives in `vaco-checkasm`, not here

`cargo xtask layer-check` enforces that dependency edges point only
downward. `vaco-checkasm` is the top layer (10); this crate is layer 3. A
dev-dependency from here on `vaco-checkasm` — the first attempt — is
therefore a layering violation regardless of it being test-only. The fix,
matching `kernels::fir_mc`'s and `kernels::scale_affine`'s existing
precedent for `vaco-codec-dsp-mc` and `vaco-scale`: `vaco-checkasm` depends
on this crate instead, and the `Kernel` impls live under
`crates/tool/vaco-checkasm/src/kernels/mecmp.rs`.

## 4. Configuration

None — no env vars or feature flags.

## 5. Dependencies

`vaco-core`, `vaco-simd` (the `Lanes`/`Caps`/`Tier`/`KernelSet`/`ops`
vocabulary — no direct `fearless_simd` dependency; see `vaco-simd`'s own
doc for the D11 boundary this crate stays inside of). No `provenance/`
table: the Hadamard-transform butterfly and the widen/accumulate pipeline
are textbook DSP and original composition respectively, not transcribed
from any specification.
