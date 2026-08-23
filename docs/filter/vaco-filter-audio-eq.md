# vaco-filter-audio-eq

T2 audio EQ filters (FT-4.8a, GitHub issue #471, one of two children epic
FT-4.8/#56 split into for single-writer ownership — the other is
`vaco-filter-audio-dynamics`/#472): the biquad family (`equalizer`, `bass`,
`lowshelf`, `treble`, `highshelf`, `tiltshelf`, `highpass`, `lowpass`,
`bandpass`, `bandreject`, `allpass`, `biquad`) plus `anequalizer`,
`firequalizer`, `superequalizer`.

## Scope reconciliation

The brief that requested this crate named thirteen filters and omitted
`tiltshelf` and `firequalizer`. Checked directly with `gh issue view 471`
rather than trusting that restatement (the standing practice on this project
after an earlier agent found its own epic named a different grouping than
its brief claimed): the issue names "the biquad family (12 filters from one
file) plus `anequalizer`, `firequalizer`, `superequalizer`" — fifteen names.

Counted against the shipped reference (`ffmpeg -hide_banner -filters`,
`ffmpeg 8.1`, 2026-08-23) rather than recalled: `af_biquads.c` registers
twelve names, confirmed by their shared `AVClass` name strings —
`ffmpeg -h filter=bass` and `-h filter=lowshelf` both print the identical
header `bass/lowshelf AVOptions:`, and `treble`/`highshelf`/`tiltshelf` all
print `treble/high/tiltshelf AVOptions:`. That means `bass` and `lowshelf`
are the *same* registered filter under two names (confirmed further: their
option tables are byte-identical), and `treble`/`highshelf` are too — while
`tiltshelf`, despite sharing the option schema, is a different transfer
function (see below). The twelve are: `equalizer`, `bass`, `lowshelf`,
`treble`, `highshelf`, `tiltshelf`, `highpass`, `lowpass`, `bandpass`,
`bandreject`, `allpass`, `biquad`. All fifteen names are implemented here.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::EqRegistry`](../../crates/filter/vaco-filter-audio-eq/src/registry.rs)
— the same shape `vaco-filter-audio` uses. `src/engine.rs` holds the shared
math; `src/common.rs` holds shared option parsing and the `Biquad`
`FrameFilter` that is the entire body of every filter here except
`tiltshelf`, `anequalizer`, `superequalizer` and `firequalizer` (each of
which needs more than one biquad section, so each writes its own small
`FrameFilter`).

## How it works

### The Audio EQ Cookbook engine (`engine.rs`) — the hard part

`equalizer`, `bass`/`lowshelf`, `treble`/`highshelf`, `highpass`, `lowpass`,
`bandpass`, `bandreject`, `allpass` and the raw `biquad` filter are one IIR
section — `y = b0 x0 + b1 x1 + b2 x2 - a1 y1 - a2 y2` — with different
coefficient formulas. Those formulas are transcribed from Robert
Bristow-Johnson's "Audio EQ Cookbook", the standard citable reference for
RBJ biquads (`provenance/sources.toml`'s `rbj-audio-eq-cookbook` entry),
never from an implementation.

**Why the tests are a real oracle and not a restated formula.** A second
transcription of the same cookbook sentence cannot disagree with the first —
that is `vaco-codec-dsp-idct`'s cautionary tale (`planning/AGENT-CONSTRAINTS.md`):
both its "independent" checks were wrong the same way because both were the
same equation read twice. Instead, `Coeffs::response_db` evaluates
`H(e^{jw}) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)` directly
from the z-transform definition, at `z = e^{jw}` — a route to a number that
has nothing to do with how the coefficients were derived. Every filter's
tests check a property of that response: the `-3 dB` point lands at the
design frequency for a Butterworth `lowpass`/`highpass`, the design-frequency
gain matches the `gain` option for `equalizer`, a shelf's asymptote at DC or
Nyquist matches `gain`, `bandreject`'s notch is deep, `allpass`'s magnitude
is flat everywhere. A coefficient sign error or a wrong `Q`/`BW`/`S` mapping
moves these numbers measurably; two transcriptions of the formula could not
have caught it.

**Numerically verified this way**: `lowpass`, `highpass` (both `-3 dB` at
cutoff and DC/Nyquist asymptotes), `bandpass` (both `csg` states — peak gain
`Q` versus 0 dB), `bandreject` (notch depth and DC/Nyquist unity),
`allpass` (both orders, flat magnitude across the band), `equalizer`
(design-frequency gain for several settings, and the zero-gain identity —
both as a fixed test and as a `proptest` over the full frequency/width
range), `bass`/`lowshelf` and `treble`/`highshelf` (DC/Nyquist asymptote
matches `gain`, zero-gain identity), `tiltshelf` (the DC/Nyquist pivot —
see below). A `proptest` also checks that every one of `lowpass` through
`highshelf` produces finite coefficients across a range of frequencies that
includes 0, negative, and above-Nyquist values, and widths that include 0
and negative — the crate's answer to "does a bad cutoff ever produce NaN".

**Structural, not held to the same bar**: `biquad` (coefficients are
user-supplied — there is no design formula to check response against beyond
finiteness), `poles=1` on `lowpass`/`highpass` (a standard one-pole
exponential-smoothing design, checked only for DC unity and monotonic
roll-off, not a `-3 dB` point — the cookbook has no one-pole case to check
against), `anequalizer`, `superequalizer`, `firequalizer` (see their own
sections below).

### `width_type` — what `h`/`o`/`q`/`s`/`k` mean

Probed via `ffmpeg -h filter=equalizer` (every biquad-family filter shares
this option): `width_type`/`t` takes `h`=Hz, `o`=octave, `q`=Q-factor (the
default), `s`=slope, `k`=kHz, or the reference's numeric encoding `1..=5` in
that same order (`equalizer=f=1000:t=2:w=1` means octave width — probed by
reading the option's `<int>` enum table, which lists `h=1 o=2 q=3 s=4 k=5`).

* `q` uses `width` directly as the cookbook's `Q`.
* `o` uses `width` as the cookbook's `BW` (bandwidth in octaves):
  `alpha = sin(w0) * sinh(ln(2)/2 * BW * w0/sin(w0))`.
* `h`/`k` give an absolute bandwidth in Hz/kHz, converted to `Q = frequency /
  bandwidth_hz` — the conventional bandwidth-to-`Q` relation. Not probed
  against the reference's exact arithmetic; this is the standard
  interpretation, not a measurement.
* `s` (shelf slope) uses the cookbook's `S` formula
  (`alpha = sin(w0)/2 * sqrt((A + 1/A)*(1/S - 1) + 2)`) for `bass`/`lowshelf`/
  `treble`/`highshelf`/`tiltshelf`; any other filter that selects `s` falls
  back to treating `width` as `Q`, since the cookbook defines no shelf slope
  for a non-shelving section (a documented, not probed, choice).

### Measured defaults

All probed via `ffmpeg -h filter=<name>`, 2026-08-23, `ffmpeg 8.1`:

| Filter | `frequency` default | `width`/`width_type` default | `gain` default |
|---|---:|---|---:|
| `equalizer` | 0 Hz | 1 / `q` | 0 dB |
| `highpass`/`lowpass` | 3000 / 500 Hz | 0.707 / `q` (Butterworth) | — |
| `bandpass`/`bandreject` | 3000 Hz | 0.5 / `q` | — |
| `allpass` | 3000 Hz | 0.707 / `q`, `order=2` | — |
| `bass`/`lowshelf` | 100 Hz | 0.5 / `q` | 0 dB |
| `treble`/`highshelf`/`tiltshelf` | 3000 Hz | 0.5 / `q` | 0 dB |
| `biquad` | — (`a0=1`, rest `0`: identity) | — | — |

Every filter also defaults `mix=1`, `channels="all"`, `normalize=false`. All
share `poles=2` (`highpass`/`lowpass`/`bass`/`treble`) and `csg=false`
(`bandpass`), matching the reference.

### `tiltshelf` — a cascade, not a formula

The cookbook has no "tilt" filter, and `tiltshelf` is a different transfer
function from `treble`/`highshelf` despite sharing their option table
(confirmed: probing `ffmpeg -h filter=tiltshelf` shows identical options to
`treble`, but the reference's own class name groups all three together,
which is a hint, not proof — the construction here is standard practice for
a tilt EQ, not a probe of the reference's DSP). Built as a low shelf cutting
`-gain/2` cascaded with a high shelf boosting `+gain/2`, both at the same
`frequency`: each stage crosses 0 dB exactly at `frequency`, so the cascade
sums to `-gain/2` at DC, `+gain/2` at Nyquist, and 0 dB at the pivot — a
genuine tilt. Verified in `engine::tests::tiltshelf_pivots_between_the_two_gains`.

### `anequalizer` — structural

Six options in the reference (`params`, `curves`, `size`, `mgain`, `fscale`,
`colors`); only `params` is implemented, and its per-band grammar (`c<chan>
f=<f> w=<w> g=<g> t=<type>|...`) comes from the reference's own texi manual
— documentation is a specification under D7, but this was not measured
against a running filter, so treat the grammar itself as medium-confidence.
Each band becomes one cookbook peaking section (via `WidthType::Hz`, since
`w` is documented in Hz) on its declared channel, cascaded in declaration
order. `t` (filter type — the reference offers Butterworth/Chebyshev
variants) is accepted and ignored: every band is a peaking section
regardless. `curves`/`size`/`mgain`/`fscale`/`colors` (the video
response-curve output) are accepted and ignored; this crate produces the
audio output only.

### `superequalizer` — an IIR approximation of an FFT-domain filter

The reference's eighteen `<N>b` options are **linear** gain multipliers
(range 0–20, default 1), not dB — the one filter in this crate where that's
true. Converted via `gain_db = 20*log10(gain)`, so the documented default
(`1` on every band) is exactly the cookbook's 0 dB identity — checked in
`superequalizer::tests::default_gains_are_identity`. Band centre frequencies
(65 Hz through 20000 Hz) are copied from each option's help text. The
reference implements this filter with an FFT-domain filter bank; this crate
cascades eighteen cookbook peaking sections instead, at a fixed `Q =
sqrt(2)` chosen for the ~half-octave spacing between adjacent bands. This is
a structural approximation — it will not match the reference's magnitude
response band-for-band, only the flat/identity case at every band's own
centre frequency.

### `firequalizer` — the least-verified filter here

The reference's `gain` option is a full expression grammar
(`gain_interpolate(f)` by default, evaluated against `gain_entry` control
points) that `vaco-expr` would be needed to implement; only the `gain_entry`
control-point path is implemented, reading `entry(freq,gain_db)` or a bare
`freq,gain_db` pair, `;`/`|`-separated. Everything else (`scale`, `wfunc`'s
specific window shapes, `delay`, `accuracy`, `multi`, `zero_phase`,
`min_phase`) is accepted and ignored.

Design method: frequency sampling. The desired linear-gain magnitude curve
is sampled at each of 255 DFT bins (piecewise-linear interpolation between
control points in Hz) and inverse-transformed by direct summation (no FFT
needed for a one-time, at-`configure()` computation; `vaco-tx` was not
needed). **The oracle here is a property of the transform itself, not of
this module's arithmetic**: a flat gain curve's IDFT is a unit impulse
exactly, by the DFT basis's orthogonality — checked in
`firequalizer::tests::flat_gain_curve_is_the_identity`. Applying the kernel
introduces a group delay of `(TAPS-1)/2 = 127` samples, which is the correct
behaviour for a linear-phase FIR (not a bug) but means output is not
sample-for-sample identical to input even at a flat gain curve — only
delayed.

## How to change it

* New biquad-family filter: add a formula function to `engine.rs`, a
  `Design` variant in `common.rs`, and a thin module following
  `lowpass.rs`'s shape. Add the frequency-response test to
  `engine.rs::tests` before anything else — that is the actual
  specification check.
* Changing a default: check `ffmpeg -h filter=<name>` first (`docs/filter/`
  convention: measure, don't recall — six defaults in this project's roadmap
  have been wrong from memory this week alone).
* `common.rs`'s doc explains why filters read options via
  `Instantiate::named` rather than a `vaco_opts::Options`-derived
  `set_from_string`: several reference options (`transform`, `precision`,
  `blocksize`) are accepted but not applied, and a strict `set_from_string`
  would reject a valid filtergraph string that sets one.

## Configuration

No environment variables or feature flags. Behaviour is entirely the
per-filter options above, read at filtergraph-parse time.

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-resample`
(the shared `f64` sample domain, `sample.rs`), `vaco-filter-core` (the
`Filter`/`FrameFilter` traits, the `Simple` adapter), `vaco-filter-graph`
(`FilterRegistry`). No new dependencies were added; `vaco-tx` (FFT/MDCT/DCT)
was considered for `firequalizer`/`superequalizer` but not needed — the
frequency-sampling FIR design and the fixed IIR band cascade are both direct
summations, not full transforms.
