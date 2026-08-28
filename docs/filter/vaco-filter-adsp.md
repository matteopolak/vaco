# vaco-filter-adsp

Shared audio DSP kernels used by more than one `vaco-filter-a*` crate. Plan
`16-filters.md` §4.1 places this crate here: a kernel needed by two or more
audio filter crates belongs in one shared place, not one copy per crate
that happens to need it first (D19).

## What it is

Three modules, added incrementally as real callers needed them rather than
speculatively:

* `wave` — LFO wave-table generation (`WaveShape`, a phase-accumulator
  walker). Added at FT-4.13d for `vaco-filter-aeffects`'s modulation
  filters (`tremolo`, `vibrato`, `chorus`, `flanger`, `aphaser`,
  `apulsator`).
* `wsola` — a time-domain WSOLA (windowed cross-correlation search plus
  overlap-add) tempo-change core. Added at FT-4.13d for `atempo`.
* `biquad` — RBJ Audio EQ Cookbook biquad coefficient design: `Coeffs`
  (normalised second-order-section coefficients), `State` (Direct Form I
  per-channel filter memory), `WidthType`, and one formula function per
  filter shape (`lowpass`, `highpass`, `bandpass`, `bandreject`, `allpass`,
  `peaking`, `lowshelf`, `highshelf`, the one-pole variants, `tilt`).

## How it works

### `biquad` — one definition instead of five

This module did not exist until this crate's second pass. `vaco-filter-aeq`
(now `vaco-filter-aeq`) had a `pub(crate)` `engine` module with the same
math, on the theory that only the EQ family needed it. That theory turned
out to be wrong: three other crates needed a two-pole IIR section and, each
finding the EQ crate's version unreachable across a crate boundary, wrote
their own instead of asking for it to move:

* `vaco-filter-aeffects` shipped one-pole approximations in `aexciter`,
  `deesser` and `virtualbass` — documented in those modules as "no
  cross-crate biquad access".
* `vaco-filter-ameasure::kweight` (BS.1770-4 K-weighting) duplicated the
  cookbook high-shelf and high-pass formulas outright, with its own
  `Coeffs`/`BiquadState` types.
* `vaco-filter-adynamics::mcompand` duplicated the cookbook Butterworth
  low-pass/high-pass formula for its crossover splitter, with its own
  `Biquad2` type.

All four now depend on this module instead. `vaco-filter-aeq` is the
before-and-after case: its `engine.rs` is gone, its filters call
`vaco_filter_adsp::biquad` directly, and its own doc
(`docs/filter/vaco-filter-aeq.md`) explains the move from its side.

**Two guarantees this module exists to make load-bearing everywhere, not
just in one crate:**

1. `Coeffs::normalise` falls back to `Coeffs::identity()` when `a0` is zero
   or non-finite, or when any resulting coefficient is non-finite after
   dividing through by `a0`. No cutoff/width/gain combination any caller
   passes can put a `NaN` into a sample stream. Pinned by
   `biquad::tests::zero_hz_cutoff_does_not_produce_nan`,
   `zero_width_does_not_produce_nan`, and the `coefficients_are_always_finite`
   proptest.
2. `Coeffs::response_db` evaluates `H(e^{jw}) = (b0 + b1 z^-1 + b2 z^-2) /
   (1 + a1 z^-1 + a2 z^-2)` directly from the z-transform definition, not by
   re-running the difference equation — a route to the frequency response
   that is genuinely independent of how the coefficients were derived, so a
   sign error or a wrong `Q`/`BW`/`S` mapping shows up as a wrong `-3 dB`
   point rather than silently agreeing with itself (see
   `planning/AGENT-CONSTRAINTS.md`'s HEVC IDCT cautionary tale for why that
   distinction matters). It is a plain `pub fn`, not `#[cfg(test)]`,
   specifically so every downstream crate's *own* tests get the same
   oracle rather than re-deriving one — gating it to this crate's test
   builds would have reproduced the exact unreachability problem the move
   was meant to fix.

Both guarantees, and the tests that pin them, moved unchanged from
`vaco-filter-aeq::engine`.

### What the four call sites disagreed about

Auditing the duplicates before deleting them found one genuine behavioural
difference, not just four copies of the same formula:

* `vaco-filter-adynamics::mcompand`'s old `Biquad2::build` special-cased
  a crossover frequency at/below DC or at/above Nyquist, substituting an
  explicit identity (lowpass) or zero (highpass) section. This module's
  `lowpass`/`highpass` do **not** do that — their contract is "coefficients
  stay finite" (via `normalise`), not "physically sensible outside
  `(0, Nyquist)`"; fed an out-of-range `f0` they still compute a real
  (if physically odd) filter from the raw trigonometric formulas. Reproducing
  `mcompand`'s prior exact behaviour meant keeping that guard in
  `mcompand.rs` itself (`crossover_lowpass`/`crossover_highpass`), calling
  into this module only inside the valid range — see that crate's doc.
* `vaco-filter-ameasure::kweight`'s old `normalize` used a different,
  looser zero-`a0` fallback: it replaced `a0` with `1.0` and left the
  numerator coefficients as computed, rather than returning the identity
  section. For the fixed BS.1770-4 design points that module uses this
  branch is not reachable at any real sample rate, so it was not a live
  bug, but it was a real, silent divergence between two "the same
  fallback" implementations — the kind D19 exists to catch before it
  matters.
* `vaco-filter-aeq`'s own two-layer fallback in `Coeffs::normalise`
  (an early `a0 == 0 / non-finite` check, then a final all-fields-finite
  check) turned out to be defense in depth rather than two independent
  guards: for every currently tested degenerate input — 0 Hz, negative,
  above-Nyquist frequencies, zero width, and gains up to ±900 dB — either
  check alone is sufficient, because a zero/non-finite `a0` and a
  non-finite output coefficient occur together at these scales. The second
  check only starts doing independent work at gain magnitudes far past any
  documented option range (approximately where `A = 10^(gain/40)` itself
  approaches `f64::MAX`). Both checks were kept; this is recorded because a
  future editor should not assume the first check alone is equivalent.

## How to change it

* Add a new formula function to `biquad.rs` next to the existing ones;
  give it a frequency-response test using `response_db` before anything
  else — that is the actual specification check, not "it compiles" or "a
  second reading of the same formula agrees" (see the module's doc for why
  the latter is not evidence).
* Do not add a kernel here speculatively. `biquad` was added because three
  real callers needed it and were duplicating it; the plan row's remaining
  two kernels (an EBU R128 loudness core, partitioned FIR convolution) have
  no caller in this crate yet and are deliberately not stubbed in.
* If you need a biquad-shaped filter in a new crate, depend on this crate
  and call the formula functions directly — do not write a fifth copy.
  `cargo xtask dup-check` will catch a *named* duplicate (e.g. another
  `pub struct Coeffs`) but cannot catch the same math under a different
  name; that needs a person who reads this doc first.

## Configuration

No environment variables or feature flags. Pure functions of their
arguments (sample rate, design frequency, width, gain).

## Dependencies

None beyond `std` (`biquad`, `wave`) — `#![forbid(unsafe_code)]`, no
`unsafe`, no external crates. `wsola` likewise has no crate dependencies
(it is deliberately time-domain, not FFT-based — see its module doc for
why `vaco-tx` was not pulled in). `proptest` is a dev-dependency only.
