# `vaco-codec-dsp-mc` — separable FIR motion compensation

---

## 1. What it is

The generic engine behind every block-based codec's sub-pixel motion
compensation: a separable FIR filter, const-generic over tap count, plus the
border replication ("edge emulation") a motion vector needs whenever it
reaches past the visible picture. D-08 (#23) names this the largest
single SIMD payoff in the codebase; D-08a is the scope this crate covers —
the engine and scalar reference every consumer builds against, not yet the
full per-tier SIMD matrix (D-08b, #259, see §3).

| Module | Covers |
|---|---|
| [`fir`](../../crates/signal/vaco-codec-dsp-mc/src/fir.rs) | `TapSet<N>`, the scalar reference, the dispatched vector kernel, and the two-pass separable composition |
| [`edge`](../../crates/signal/vaco-codec-dsp-mc/src/edge.rs) | Border-replicated block extraction for an out-of-picture motion vector |
| [`h264`](../../crates/signal/vaco-codec-dsp-mc/src/h264.rs) | The picture-scoped `H264McKernels` table for qpel luma, batched chroma, and explicit/implicit weighted prediction |

**Scope boundary**, the same shape as `vaco-codec-dsp-idct`'s: a codec crate
decides its own motion vector, sub-pel position and which [`fir::TapSet`]
that position uses; this crate only runs the resulting FIR once the taps and
the (possibly border-extended) source samples are known.

## 2. How it works

### 2.1 One pass: `TapSet<N>`, `fir_row_scalar`, `fir_row`

`TapSet<N>` is `{ coeffs: [i16; N], shift: u32 }`. `fir_row_scalar` is the
complete oracle: bias (`1 << (shift - 1)`), accumulate, shift, clip to `u8`.
`fir_row` is the dispatched vector implementation, monomorphised per tap
count via the same `N`, checked bit-identical against the scalar reference
across every vector-width tail (`tests` in `fir.rs`, plus
`vaco-checkasm`'s `kernels::fir_mc`).

**The tap-loop structure is a measured choice, not the first one tried.**
`vaco-simd`'s own `benches/adoption.rs` Group 4 already ran this exact
question for an 8-tap filter: "reload and re-widen the source once per tap,
one output vector per iteration" measured **1.12x** against a scalar
auto-vectorised baseline; batching two output vectors per iteration measured
**1.36x** (worse — the extra accumulators spill past the register file);
hoisting the widen and reaching neighbouring taps via `slide` measured
**1.64x** (worse still). `fir_row_body` uses the first, winning shape. Do not
"improve" it toward `slide` or manual batching without re-running that
benchmark on the actual tap count in question — both alternatives measured
backwards here already, and D17 says measure again rather than recall.

### 2.2 `taps` — two verified, spec-cited tap sets

`taps::BILINEAR` (`[1, 1]`, shift 1) and `taps::H264_LUMA_HALFPEL`
(`[1, -5, 20, 20, -5, 1]`, shift 5, `Vaco-Spec-Ref: itu-t-h264-202108
§8.4.2.2.1`). Both are checked against **properties the filter definition
itself guarantees**, not a second transcription of the same numbers — the
lesson `vaco-codec-dsp-idct`'s HEVC transpose bug and this project's CAVLC
table bug both teach: a coefficient can be prefix-free, correctly lengthed,
or independently retyped and still be wrong.

- **DC invariance**: every shipped tap set sums to exactly `1 << shift`, so a
  constant input plane must produce that same constant back out
  (`dc_input_passes_through_*_unchanged`).
- **Impulse response**: a single nonzero sample must read back the
  coefficients themselves, in order — this is what would catch a
  *transposed pair* of equal-magnitude coefficients that the DC check alone
  cannot see (`impulse_response_matches_the_coefficients_directly`).

A consumer adding its own tap set (HEVC/VP9/AV1's per-sub-pel-position
tables) should add both checks alongside the new `TapSet` constant, not just
one.

### 2.3 Two passes: why `separable_2d` does not clip in the middle

A real 2-D interpolation position (H.264 §8.4.2.2.1's "j"-type positions, for
one) runs a horizontal pass and a vertical pass, and the specification keeps
the horizontal pass **unrounded and unclipped** — only the vertical pass's
output is rounded, shifted and clipped to `u8`. Clipping between the two
passes would compound rounding error into a *structured* bias (every
position gets it, not a scattered ±1), which this project's shipping bar
treats as a real defect, not noise.

`tap_sum`/`fir_pass_i32` are the raw, unrounded building block (`i32`
throughout — no realistic tap count or coefficient can overflow it);
`tap_sum_i32`/`separable_2d` compose two passes with the *caller* supplying
the vertical pass's combined rounding bias and shift. This crate does not
guess at one codec's intermediate-precision convention on another's behalf —
see `fir::tests::separable_2d_dc_input_passes_through_unchanged` for the
shape a caller wires up (horizontal `shift = 0`, vertical `shift =` the sum
of both passes' natural shifts).

### 2.4 `edge::extend_edges` — border replication

Plain scalar border-clamp copy: for each destination pixel, clamp the
requested source coordinate to `0..width`/`0..height` and copy. This is
**setup**, not the hot loop — called once per block, not once per tap — so
there is no SIMD path here and none is planned; `vaco-codec-dsp-mc`'s SIMD
payoff is entirely in the FIR taps.

## 3. How to change it

- **A new tap count**: pick `N`, write the `TapSet<N>` constant, add the two
  property tests from §2.2. `fir_row_scalar`/`fir_row` need no change — both
  are already generic over `N`.
- **A new codec's real coefficient tables** (HEVC/VP9/AV1 each need several,
  one per sub-pel position): that is each consuming codec crate's own
  `vaco-component.toml`-adjacent module, not this crate. Verify every table
  the same way §2.2 does before trusting it — the DC/impulse checks are
  cheap and this project has shipped a wrong-but-plausible table before
  (H.264 CAVLC, a different crate, same lesson).
- **Tier specialisations and their gate**: `fir_row` covers every shipped tap
  set, while `h264::H264McKernels` has fixed-width luma/chroma/weight bodies
  monomorphised for every supported tier. `vaco-checkasm` crosses both tap
  sets, every native vector tail, every chroma eighth-pel position, and every
  tier the current CPU genuinely supports. `Caps::capped_at` can select a
  weaker supported tier without fabricating a capability token. Keep new tap
  sets and new H.264 table entries in that matrix.
- **H.264's settled decoder contract** is `h264::H264McKernels`. The decoder
  resolves it once per picture. Luma submits a complete partition window,
  chroma gathers a macroblock's sixteen 2x2 jobs and submits one batch per
  reference list, and weighting submits a whole strided block. Keep the
  scheduling and edge extension in `vaco-codec-h264`; keep only arithmetic
  over caller-owned buffers here. This boundary prevents per-pixel dispatch
  and leaves one stable place for D-08b's tier-specific kernels.

`benches/h264.rs` measures the resolved scalar and selected tables with the
same function-pointer boundary; `benches/fir.rs` covers both tap sets at block
and full-row sizes. Treat their wall-timer output as a local diagnostic, not a
portable cycle count. Publication measurements must follow
`planning/PERF-BASELINE.md`: interleaved A/B order, at least ten rounds, CPU
seconds beside wall time, and a same-session reference run. General bi-weights
whose proven intermediate bound exceeds `i16` retain the scalar fallback;
ordinary explicit, implicit, and unweighted-average cases use the vector
body. On platforms without a trustworthy PMU counter, report that limitation
instead of synthesising cycles or instruction counts.

## 4. Configuration

None. Every function is a pure transform over caller-owned buffers — no
env vars or feature flags. `H264McKernels::select()` uses `vaco-simd`'s runtime
tier selection. Tests and controlled benchmarks may use `Caps::capped_at` to
exercise any weaker tier the CPU actually supports. Length mismatches (a `dst` shorter than `src`
implies, an `extend_edges` block smaller than declared) degrade to writing a
shorter prefix rather than panicking, matching `vaco-codec-dsp-idct`'s own
truncate-don't-panic convention for buffers whose sizes ultimately trace
back to bitstream-signalled values.

## 5. Dependencies

- **`vaco-core`** — workspace conventions only; no codec-specific type from
  it is used.
- **`vaco-simd`** — `Lanes`, `Caps`, `dispatch_kernel!`, and
  `ops::simd::{wmla_u8_i16, pack_u8_from_i16}` for the dispatched tap loop.
  `fearless_simd` itself is never named here, per the D11 boundary: every
  SIMD type/trait comes through `vaco_simd::prelude`.
- **`proptest`, `divan`** (dev-only) — the tail-length sweep and
  `benches/fir.rs`'s scalar-vs-dispatched measurements (16px block width and
  a 1920px row; the dispatched path measurably wins at the row width — see
  the bench's own output — and is roughly break-even at one 16px block,
  which is overhead rather than a defect: a real caller processes whole rows
  or whole planes, not one block at a time).
