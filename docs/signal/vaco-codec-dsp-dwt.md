# `vaco-codec-dsp-dwt` — discrete wavelet transform

---

## 1. What it is

Wavelet transform primitives for two unrelated wavelet families that
happen to share the same separable-2D lifting structure:

| Module | Filter | Domain | Bit-exact? |
|---|---|---|---|
| [`vc2`](../../crates/signal/vaco-codec-dsp-dwt/src/vc2.rs) | VC-2/Dirac's seven `wavelet_index` filters (Deslauriers-Dubuc 9/7 and 13/7, LeGall 5/3, Haar x2, Fidelity, integer Daubechies 9/7) | `i32` | **Yes — exact round trip, checked by property test** |
| [`cdf97`](../../crates/signal/vaco-codec-dsp-dwt/src/cdf97.rs) | JPEG 2000's irreversible CDF 9/7 | `f64` | No — genuinely floating-point; round trip is bounded, not exact (see `MAX_ABS_ROUND_TRIP_ERROR`) |
| [`lift`](../../crates/signal/vaco-codec-dsp-dwt/src/lift.rs) | — (shared 1D engine `vc2` builds on) | `i32` | n/a — the primitive both filters' exactness argument rests on |

Nothing in this crate is wired into a decoder yet: it is DSP primitives
only, per issue #261 (D-15), which asked for the transform family itself
— a future VC-2/Dirac or JPEG 2000 codec crate is the consumer.

## 2. How it works

### 2.1 `lift` — one 1D engine, four step types, exact by construction

A lifting step modifies one parity class of an array (even or odd
indices) from a weighted sum of the *other*, untouched parity class.
`StepKind::{Type1,Type2,Type3,Type4}` cover every combination of "which
parity" and "add or subtract"; `LiftStep` pairs a kind with its taps,
tap-index offset and post-sum right shift. Because a step never reads the
parity it writes, undoing it is exact and mechanical: swap `Type1`↔`Type2`
or `Type3`↔`Type4` (same taps, same read positions, `+=` becomes `-=`) and
run the *whole* step sequence in reverse order. `run_synthesis` runs a
filter's own step sequence forward; `run_analysis` is that same mechanism
applied automatically — callers never hand-write an inverse.

Out-of-range reads clamp into the correct-parity range
(`clamp_read_pos`), not into an arbitrary sample — clamping to the wrong
parity would make a step read a position its own inverse pass has already
overwritten, and break the "never reads the parity it writes" precondition
this module's exactness argument depends on.

### 2.2 `vc2` — the seven `wavelet_index` filters and the 2D driver

Each `WaveletKind` names a `&'static [LiftStep]` table and a `filter_shift`
(`Table 15.1`–`15.7`'s own values), transcribed from the Dirac
Specification v2.2.3 (`dirac-specification-2.2.3` in
`provenance/sources.toml`) — see §4 for the one transcription this crate
is not fully confident in.

`dwt_2d`/`idwt_2d` implement the multi-level 2D transform in this crate's
own **packed single-buffer layout**: one `width x height` array, with
each level's `LL` subband nested recursively in the top-left corner —
deliberately *not* Dirac's own per-subband-array bitstream storage
(`coeff_data[n][LL/HL/LH/HH]`), which is a bitstream-syntax detail outside
this crate's scope. The math is identical either way; only the memory
layout differs.

The one property that is easy to get backwards and silently wrong: a
separable 2D transform's inverse must undo its row and column passes in
**reverse order**, not the same order. `idwt_2d` (synthesis) filters
columns then rows; `dwt_2d` (analysis) filters rows then columns. Filtering
both directions in the same axis order compiles, looks reasonable, and
does not round-trip — this was a real bug caught by this crate's own
round-trip property test during development, not a hypothetical.

### 2.3 `cdf97` — JPEG 2000's irreversible transform

`forward_1d`/`inverse_1d` run the standard predict/update/predict/update
lifting cascade (`ALPHA`/`BETA`/`GAMMA`/`DELTA`) followed by the `1/K`,`K`
low/high-pass scale. Because this is genuinely floating point, round trip
is bounded, not exact — `MAX_ABS_ROUND_TRIP_ERROR` states the empirically
measured bound (see the module doc for the exact measurement domain), per
this crate's bit-exactness policy of never accepting "close enough"
without a stated number. `dwt_2d`/`idwt_2d` share `vc2`'s packed-buffer
layout and row/column-order discipline, with no integer shift step (the
irreversible path stays in floating point end to end).

**Provenance is weaker here than for `vc2`** — see `cdf97.rs`'s own module
doc and §4 below before relying on this for a JPEG 2000-conformance use
case.

## 3. How to change it

- **Adding an eighth VC-2-style filter or a variant table**: add a
  `&'static [LiftStep]` next to the existing seven in `vc2.rs`, a
  `WaveletKind` variant, and a `filter_shift` arm — `dwt_2d`/`idwt_2d`
  need no changes, since they only ever call through `WaveletKind::steps`/
  `filter_shift`.
- **Suspect the row/column order first** if a new filter round-trips at
  1D (`lift`'s own tests) but not at 2D — re-read §2.2's note before
  re-checking the filter's own taps.
- **Never hand-write an inverse `LiftStep` sequence.** `run_analysis`
  derives it mechanically from `run_synthesis`'s own table via
  `StepKind::inverse`; a hand-written inverse is a second, divergeable copy
  of the same information.
- **Extending to 3-level-plus multi-resolution use**: `levels` is already
  general (`check_shape` verifies both dimensions divide `2^levels`
  evenly); no code change needed to go past today's tested `1..=3`.

## 4. Configuration

None — every function is a pure transform over an in-memory `&mut [i32]`/
`&mut [f64]` buffer plus a `Budget` for its scratch allocation; there is
no persistent state, feature flag, or environment variable.

**Known transcription uncertainty (flagged on issue #261's closing
comment, not silently accepted):** Dirac Specification v2.2.3 Table 15.6
(the "Fidelity" filter, `wavelet_index == 5`) rendered with **both** of
its lifting steps numbered "1." in the fetched PDF. This crate implements
Type 3 (the 8-tap `[-2,10,-25,81,81,-25,10,-2]` step) before Type 2 (the
8-tap `[-8,21,-46,161,161,-46,21,-8]` step) — the reverse order from every
other table in this crate, which are all even-then-odd. That is this
crate's best reading of an ambiguous render, not a confirmed transcription
— see `provenance/sources.toml`'s `dirac-specification-2.2.3` entry.
`Fidelity`'s own round trip is still exact regardless (the property test
covers it like every other filter), because `lift`'s inversion mechanism
does not depend on which order is the "correct" one per the standard —
only on running the two steps back in the same order they were run
forward. What is unverified is whether this crate's `Fidelity` matches
what a real VC-2 encoder/decoder produces, not whether it round-trips.

## 5. Dependencies

- **`vaco-core`** — `Error`/`Result`.
- **`vaco-limits`** — `Budget::alloc` for the one scratch buffer each 2D
  transform needs (never `Vec::with_capacity`, project-wide policy).
- **`proptest`** (dev-only) — the exact-round-trip property test over
  random input, random `WaveletKind`, and a random point in the
  `levels`/`width`/`height` domain (`vc2::tests::every_filter_round_trips_random_input_exactly`),
  and `cdf97`'s bounded-tolerance property test.

No `vaco-simd`: every transform operates on whatever `width`/`height` a
caller supplies, known only at runtime, and no profile yet exists showing
this crate on a hot path — see `vaco-codec-dsp-idct`'s doc for why "no
profile, no SIMD" is this project's default rather than an oversight here.
