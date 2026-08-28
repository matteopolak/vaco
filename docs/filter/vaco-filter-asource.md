# vaco-filter-asource

Audio test-signal and FIR-coefficient generator sources (FT-4.13a, GitHub
#481): plan 16 §4.3's `vaco-filter-asource` row.

## What it is

Six source filters, registered here: `sine`, `anoisesrc`, `aevalsrc`,
`afdelaysrc`, `sinc`, `hilbert`.

Two names in the plan's row are **not** registered here:

- `anullsrc`, `anullsink` — already shipped by `vaco-filter-plumbing`
  (FT-4.3, GitHub #467).

Two more are not implemented at all: `afirsrc`, `afireqsrc`. Both need the
frequency-sampling FIR design method (interpolate a frequency response onto
a bin grid, inverse-FFT via `vaco-tx`, window, and for `afireqsrc` its
named EQ presets, which are undocumented gain-table constants this project
cannot recover without reading source). That is real signal-processing
surface area this crate did not have time to implement and verify
correctly within this pass — see "How to change it" below for what
finishing it would take.

## How it works

One module per filter, each exposing `pub const DESC: FilterDesc` and a
crate-private `create`, dispatched by `registry::AsourceRegistry`. Every
filter is a `SourceFilter` wrapped in `vaco_filter_core::adapt::Sourced`.

`rng.rs` holds one `SplitMix64` generator (Vigna, public domain), shared by
`anoisesrc`'s `seed` option — the third copy of this exact utility in the
`vaco-filter-*` tree (`vaco-filter-temporal` and `vaco-filter-source` each
have their own), all for the identical reason: none of them reproduce the
reference's own RNG bit stream, so reproducibility rather than
bit-identity is the contract.

`window.rs` holds a shared `WinFunc` enum and six closed-form window
implementations (`rect`, `bartlett`, `hann`, `hamming`, `blackman`, `sine`)
used by `hilbert`. `sinc` and `afdelaysrc` do **not** use this module —
measured directly, neither exposes a `win_func` option at all today,
despite the module's own header once implying otherwise. The other
fifteen documented `win_func` values (`welch`, `bhann`, `flattop`,
`bharris`, `bnuttall`, `nuttall`, `lanczos`, `gauss`, `tukey`, `dolph`,
`cauchy`, `parzen`, `poisson`, `bohman`, `kaiser`) used to be silently
computed as one of the six real formulas (accepted, wrong, no error);
`hilbert::create` now rejects each by name instead — see
`window.rs::ensure_implemented`'s own doc.

`aevalsrc` reuses `vaco_expr` directly (the same expression engine every
other filter's expression options go through), binding `t` (time in
seconds) and `n` (absolute sample index) — no second expression parser.

## Per-generator exactness

| Filter | Status | Independent check |
|---|---|---|
| `sine` | **Not bit-exact — and this was the important negative result of this pass.** The amplitude (`4095`, not `32767`) and general shape are measured and correct. A `floor(4095*sin(...))` per-sample formula matched an initial 10-sample probe at 8/10 points, which is precisely the "8 of 9 measured points" false-confirmation trap `AGENT-CONSTRAINTS.md` warns about. Extending the check to 2000 samples falsifies it: 51% of samples disagree. The error distribution (symmetric, `stdev=0.436`, values beyond `±1.2`) is consistent with a **dithered quantiser** in the reference, which no closed-form formula reproduces without its exact RNG. Ships as the plain formula anyway (right amplitude, frequency and phase continuity; ~half of samples off by one LSB), documented honestly rather than claimed exact. |
| `anoisesrc` | `color=white`'s distribution (uniform, not Gaussian — matched via measured standard deviation `0.578 ≈ 1/√3`) is measured and matched. Every colour (`pink`/`brown`/`blue`/`violet`/`velvet`) uses a published construction on top of this crate's own RNG. **Algorithmically faithful; bit-exact for none** (RNG divergence). |
| `aevalsrc` | **Exact.** `t`/`n` variable bindings measured directly against the reference; evaluation goes through the same `vaco_expr` engine every other filter's expressions use, so its fidelity is inherited. One simplification: `ld`/`st` register state resets every sample rather than persisting across the stream. |
| `afdelaysrc` | **Exact near the peak** (matched an unwindowed `sinc(n-delay)` to within 0.2% at the two peak taps of a measured `delay=2.5` kernel), **increasingly approximate away from it** (ratio drops to ~0.76 by the 6th tap, meaning the reference does apply *some* taper this crate does not reproduce). A first attempt used a Blackman window centred on the whole array and was wrong by more than an order of magnitude at the peak — falsified by the same data before being shipped. This crate's `taps=0` auto-length heuristic is its own choice, not the reference's (measured: `delay=0` gives 1 tap, `delay=2.5` gives 21 — no simple formula from two points was attempted). |
| `sinc` | The Kaiser beta-from-attenuation formula is Kaiser's own published 1974 equations, exact. The windowed-sinc construction is textbook FIR design. **Not calibrated**: the reference's auto-taps formula (this crate uses its own fixed default) and its exact `phase` (linear/minimum-phase blend) handling — this crate always produces a linear-phase kernel. |
| `hilbert` | **Exact** for the default `win_func=blackman`: the ideal-Hilbert-times-Blackman formula matches a measured `taps=11` reference kernel's zero/antisymmetric structure exactly, and its two non-trivial tap ratios to within manual-verification precision. `rect`/`bartlett`/`hann`/`hamming`/`sine` also run their own real formula. The other 15 documented `win_func` values are a named "not implemented" error (previously a silent substitution — see `window.rs`). |

## What is not implemented

**`afirsrc`, `afireqsrc`.** Both need: (1) interpolating `frequency`/
`magnitude`/`phase` (or, for `afireqsrc`, `bands`/`gains`) onto a bin grid
matched to `vaco-tx`'s FFT size conventions, (2) enforcing conjugate
symmetry so the inverse transform is real-valued, (3) the circular shift
that turns a zero-phase frequency-sampled response into a causal linear-
phase FIR, and (4) for `afireqsrc`'s named presets (`bass`, `jazz`,
`rock`, …), gain-table constants that are not published anywhere this
project can cite — only the `flat`/`custom` path (all-zero or
user-supplied gains) would even be well-defined without reading source.
That is a full FIR-design subsystem, not a small addition, and this pass
did not have time to build and verify it.

## How to change it

- **`afirsrc`/`afireqsrc`**: start from `sinc.rs`'s Kaiser/lowpass
  machinery for the windowing half, and reuse `vaco-tx`'s `Plan`/`Tx` for
  the inverse transform — do not hand-roll a second FFT.
- **`sine`'s dither**: if the reference's RNG is ever identified (it would
  need to be read from source, which D7 forbids here, or found published
  elsewhere), the fix is a per-sample dither offset before `floor`, not a
  different rounding rule — plain rounding rules were already ruled out by
  the 2000-sample measurement in `sine.rs`'s doc comment.
- **`afdelaysrc`'s taper**: re-probe at several `delay` values with a
  wider sample range and fit the decay-ratio curve (measured here as
  ~1.0, 1.0, 0.91, 0.84, 0.76 for the first few taps past the peak at
  `delay=2.5`) against candidate window families before picking one —
  the Blackman-centred-on-array guess this crate tried first was wrong by
  an order of magnitude, so verify against data before committing.

## Configuration

No environment variables or external configuration. Every filter's options
mirror `ffmpeg -h filter=<name>` exactly (names, aliases, defaults).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-expr` (`aevalsrc`'s evaluator),
`vaco-frame`/`vaco-sampfmt`/`vaco-chlayout` (audio frame allocation),
`vaco-filter-core`, `vaco-filter-graph`. No `vaco-tx` dependency —
`afirsrc`/`afireqsrc`, the two filters that would have needed it, are not
implemented; `afdelaysrc`/`sinc`/`hilbert` all compute their FIR kernels in
closed form, with no transform required.
