# vaco-filter-audio-eq

T2 audio EQ filters (FT-4.8a, GitHub issue #471, one of two children epic
FT-4.8/#56 split into for single-writer ownership — the other is
`vaco-filter-adynamics`/#472): the biquad family (`equalizer`, `bass`,
`lowshelf`, `treble`, `highshelf`, `tiltshelf`, `highpass`, `lowpass`,
`bandpass`, `bandreject`, `allpass`, `biquad`) plus `anequalizer`,
`firequalizer`, `superequalizer`.

Plus `aemphasis` and `atilt` (FT-4.13e, GitHub #485, closing epic #58) — see
"The FT-4.13e additions" below.

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
— the same shape `vaco-filter-audio` uses. The Audio EQ Cookbook math itself
— `Coeffs`, `State`, `WidthType`, and every formula function — lives in
[`vaco_filter_adsp::biquad`](../../crates/filter/vaco-filter-adsp/src/biquad.rs),
not in this crate (see below for why); `src/common.rs` holds shared option
parsing and the `Biquad` `FrameFilter` that is the entire body of every
filter here except `tiltshelf`, `anequalizer`, `superequalizer` and
`firequalizer` (each of which needs more than one biquad section, so each
writes its own small `FrameFilter`).

## How it works

### The Audio EQ Cookbook engine (`vaco-filter-adsp::biquad`) — the hard part

`equalizer`, `bass`/`lowshelf`, `treble`/`highshelf`, `highpass`, `lowpass`,
`bandpass`, `bandreject`, `allpass` and the raw `biquad` filter are one IIR
section — `y = b0 x0 + b1 x1 + b2 x2 - a1 y1 - a2 y2` — with different
coefficient formulas. Those formulas are transcribed from Robert
Bristow-Johnson's "Audio EQ Cookbook", the standard citable reference for
RBJ biquads (`provenance/sources.toml`'s `rbj-audio-eq-cookbook` entry),
never from an implementation.

**This math used to live in this crate, as a `pub(crate)` module named
`engine`.** It moved to `vaco-filter-adsp::biquad` (D19) once three other
crates — `vaco-filter-aeffects`, `vaco-filter-aanalysis` and
`vaco-filter-adynamics` — turned out to need the same two-pole design
and, finding this crate's copy crate-private and therefore unreachable,
each either wrote a fallback (`aeffects`'s one-pole approximations in
`aexciter`/`deesser`/`virtualbass`) or duplicated the cookbook formulas
outright (`ameasure::kweight`, `audio-dynamics::mcompand`). This crate now
depends on `vaco-filter-adsp` like the other three; `Coeffs::normalise`'s
NaN-safety fallback and `Coeffs::response_db`'s independent z-transform
oracle (below) are unchanged by the move — same code, same tests, new
address.

**Why the tests are a real oracle and not a restated formula.** A second
transcription of the same cookbook sentence cannot disagree with the first —
that is `vaco-codec-dsp-idct`'s cautionary tale:
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
genuine tilt. Verified in `vaco-filter-adsp`'s `biquad::tests::tiltshelf_pivots_between_the_two_gains`.

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

### The FT-4.13e additions

**`atilt`** is not the same filter as `tiltshelf` despite the similar
name and description — its own option set (`freq`/`slope`/`width`/`order`/
`level`, confirmed via `ffmpeg -h filter=atilt`) has no `width_type`, no
`mix`, and an `order` parameter (2 to 30) `tiltshelf` does not have at all.
`order` is the tell that this is a variable-order filter, not one shelf
pair, and there is no public description of how the reference maps
`order`/`slope`/`width` onto a specific cascade — probing cannot recover an
undocumented internal structure. Built instead from the one tilt
construction this crate already has and can verify
(`vaco_filter_adsp::biquad::tilt`), cascaded `(order/2).max(1)` times with
`slope` mapped linearly to a `slope*24 dB` total swing split evenly across
stages — a real, standard filter-design technique (cascading identical
shelving sections to steepen a transition), not a reproduction of the
reference's specific realisation. See `atilt.rs`'s module doc.

**`aemphasis`** measures direction and rough shape correctly (`reproduction`
cuts highs, `production` boosts them, confirmed via a sine sweep against the
reference) but not the exact curve. `50fm`/`75fm`/`50kf`/`75kf`/`cd` use
[`vaco_filter_adsp::biquad::lowpass_one_pole`] at the standard published FM
broadcast/CD time constants (50/75 µs); `production` is that filter's exact
digital inverse (a two-tap FIR), verified by the real property that
cascading `reproduction` then `production` is the identity
(`tests::production_exactly_inverts_reproduction`). `riaa` simplifies the
true three-time-constant curve to its single dominant corner (318 µs);
`col`/`emi`/`bsi` (historical 78 rpm curves) use an explicitly-flagged
**unverified placeholder** time constant — no published values for these
three were confidently available within this pass, and this is called out
rather than shipped as if measured. See `aemphasis.rs`'s module doc for the
full measurement.

## How to change it

* New biquad-family filter: add a formula function to `vaco-filter-adsp`'s `src/biquad.rs`, a
  `Design` variant in `common.rs`, and a thin module following
  `lowpass.rs`'s shape. Add the frequency-response test to
  its `tests` module before anything else — that is the actual
  specification check.
* Changing a default: check `ffmpeg -h filter=<name>` first (`docs/filter/`
  convention: measure, don't recall — six defaults in this project's roadmap
  have been wrong from memory this week alone).
* `common.rs`'s doc explains why filters read options via
  `Instantiate::named` rather than a `vaco_opts::Options`-derived
  `set_from_string`: several reference options (`transform`, `precision`,
  `blocksize`) are accepted but not applied, and a strict `set_from_string`
  would reject a valid filtergraph string that sets one.
  `common::ensure_known_options` (probed against `ffmpeg -h
  filter=<name>`, 2026-08-28) still accepts every one of those, but
  rejects an option name the reference does not document at all — a typo
  used to run silently with the implemented options' defaults and no
  error.

## Configuration

No environment variables or feature flags. Behaviour is entirely the
per-filter options above, read at filtergraph-parse time.

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-resample`
(the shared `f64` sample domain, `sample.rs`), `vaco-filter-core` (the
`Filter`/`FrameFilter` traits, the `Simple` adapter), `vaco-filter-graph`
(`FilterRegistry`), `vaco-filter-adsp` (`biquad`: the cookbook coefficient
math this crate used to carry itself — new dependency, added when `engine`
moved there). `vaco-tx` (FFT/MDCT/DCT)
was considered for `firequalizer`/`superequalizer` but not needed — the
frequency-sampling FIR design and the fixed IIR band cascade are both direct
summations, not full transforms.

## Issues

Also closes its share of GitHub #485 (FT-4.13e, closing epic #58):
`aemphasis` and `atilt` (both structural — see "The FT-4.13e additions"
above for exactly what is and is not measured).
