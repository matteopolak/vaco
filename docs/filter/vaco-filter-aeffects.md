# vaco-filter-aeffects

T3 audio effects and modulation filters (FT-4.13d, GitHub issue #484), plan
`16-filters.md` §4.3's `vaco-filter-aeffects` row. Twenty-two filters:
`aecho`, `adelay`, `compensationdelay`, `chorus`, `flanger`, `aphaser`,
`tremolo`, `vibrato`, `apulsator`, `crystalizer`, `aexciter`, `deesser`,
`dialoguenhance`, `crossfeed`, `stereotools`, `stereowiden`, `extrastereo`,
`earwax`, `haas`, `virtualbass`, `dcshift`, `atempo`, plus `axcorrelate`
(not in this plan row, but pre-existing in this crate and left alone).

## Rename history

This crate was originally built (FT-4.13b, GitHub #482) under the name
`vaco-filter-achannel`, with only the seven channel/mixing filters it had at
the time (`axcorrelate`, `crossfeed`, `earwax`, `extrastereo`, `haas`,
`stereotools`, `stereowiden`). `planning/16-filters.md` §4.3 names the crate
that owns this exact filter family `vaco-filter-aeffects`; the mismatch was
recorded in `planning/FILTER-CRATE-DIVERGENCE.md` and fixed here as a
rename, in a commit kept separate from the fifteen new filters this work
package adds, so the move is reviewable on its own.

## Every plan-row name checked against the reference

`planning/16-filters.md` §4.3's `aeffects` row lists twenty-five names.
Checked directly against `ffmpeg -filters` and `ffmpeg -h filter=<name>`
(ffmpeg 8.1, 2026-08-23) rather than recalled: every one of the twenty-five
exists in the reference, with matching media type (`A->A` for all of them
except `headphone`, which is `N->A` — a dynamic-input filter fed by extra
HRIR streams). The row and the reference agree in both directions; nothing
needed adding to or removing from the row.

Twenty-two of the twenty-five are implemented here. Three are not:

- **`surround`** and **`headphone`** were flagged by this crate's original
  author as disproportionately large for this project's pace — an
  STFT/overlap-add upmix with per-channel spread and twenty `win_func`
  choices, and a full HRTF convolution engine driven by caller-supplied
  impulse-response streams and a non-trivial channel-mapping grammar,
  respectively — each comparable in scope to
  `vaco-filter-audio-eq::superequalizer`. They remain unimplemented for the
  same reason.
- **`hdcd`** decodes a proprietary bit-level scheme: control codes for gain
  adjustment and peak extension are embedded in the low-order bits of the
  audio itself, with a code-detect timer, optional per-channel gain
  matching, and a configurable "valid bits" location (`bits_per_sample`).
  Reverse-engineering that from black-box probing alone, to this project's
  own clean-room standard (D7/D17), is a project on the scale of its own
  work package, not a line item in this one. Left unimplemented rather than
  shipped as a guess.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::AeffectsRegistry`](../../crates/filter/vaco-filter-aeffects/src/registry.rs)
— the same shape `vaco-filter-audio-eq` and `vaco-filter-audio-dynamics`
use. `src/sample.rs` is the same `f64`-domain frame decode/encode those two
crates carry, duplicated rather than shared (see that module's own doc for
why). `axcorrelate` is the one filter with two audio inputs that must be
aligned in time, so it goes through `vaco-filter-framesync`'s
`Synced`/`FrameSyncFilter` adapter instead of `vaco-filter-core::adapt::Simple`.

Six filters (`tremolo`, `vibrato`, `chorus`, `flanger`, `aphaser`,
`apulsator`) drive a parameter from a low-frequency oscillator; all six use
[`vaco-filter-adsp::wave`](../../crates/filter/vaco-filter-adsp/src/wave.rs)'s
wave-table generator and phase-accumulator `Lfo` rather than each
reimplementing sine/triangle/square/sawtooth evaluation. `atempo` uses
[`vaco-filter-adsp::wsola`](../../crates/filter/vaco-filter-adsp/src/wsola.rs),
a time-domain WSOLA tempo-change core. `vaco-filter-adsp` did not exist
before this work package; it is created per plan §4.1's row rather than
putting shared kernels in this crate, and only implements the two kernels
this crate's filters actually call (wave tables and WSOLA) — not the biquad
design, EBU R128 core or partitioned FIR the plan's row also lists, since
those have no caller here yet and `vaco-filter-audio-eq` already owns biquad
design (D19: one definition per concept).

Three filters (`chorus`, `flanger`, `vibrato`) share
`common::InterpDelay`, a linearly-interpolated delay line — the building
block an LFO-modulated delay needs that a plain whole-sample delay line
(`adelay`'s, `aecho`'s) does not.

## How it works: what is measured versus structural

Every module's own doc comment states its evidence in full; this is the
summary.

**Sample-exact against `ffmpeg` 8.1** (probed, D17; never read from source,
D7): `dcshift` (the plain shift+clamp path — `limitergain > 0`'s shape is
structural, see below), `adelay` (impulse position for whole and
fractional per-channel delays, and the `all` flag), `compensationdelay`
(delay position at four different temperatures, matching the standard
`v(T) = 20.05 * sqrt(273.15 + T)` acoustic speed-of-sound formula, plus its
`dry`/`wet` mix), `aecho` (multi-tap gains and the non-recursive/FIR
property, confirmed by the *absence* of a repeat at the feedback-implied
lag), `tremolo` (the full `1 - d/2*(1-cos(wt))` gain curve), `apulsator`
(the full sine curve and `amount` scaling, at `mode=sine, timing=hz,
width=1`), `crystalizer` (the exact `x[n] + i*(x[n]-x[n-1])` differencer,
with and without clipping), `extrastereo`, `stereowiden`, `stereotools`
(all eleven modes), `earwax` (the complete 32-tap FIR), and `deesser` at
its own default `i=0`.

**Exact by construction, not by live comparison** (an algebraic consequence
of this module's own formula — usually because the reference has no
corresponding option value to probe, or rejects one outright):
`aecho`'s zero-decay identity (the reference **rejects** `decays=0`
outright — `decay[0]: 0.000000 is out of allowed range: (0, 1]` — so this
is checked against the formula, not a live run), `flanger`'s all-defaults
identity (the swept delay is `0` at `delay=depth=0`, so the feedback loop's
`width`-mix collapses to identity regardless of `width`'s value),
`aphaser`'s zero-decay pure-gain case, `chorus`'s zero-decays dry-only
case, `vibrato`'s zero-depth identity, `aexciter`'s zero-amount pure-gain
case.

**Sample-exact for the reference's own default options, structural beyond
that:** `haas` (see the original crate's own note on the non-obvious
`left_balance`/`right_balance` routing direction).

**Structural** (a standard DSP technique, implemented directly rather than
reverse-engineered, because the effect has no discrete-impulse signature to
probe, or needs a shared kernel this project does not build until a filter
after this one needs it):

- `chorus`, `flanger`, `aphaser`: LFO-modulated delay lines with feedback
  (`flanger`, `aphaser`) or per-voice mixing (`chorus`). The exact
  modulation curve, LFO start phase and interpolation kernel are not
  reverse-engineered — a continuously-varying delay has no single-impulse
  signature to probe, unlike `aecho`'s discrete taps.
- `vibrato`: modulates the same `InterpDelay` core with a sine LFO. Probing
  an isolated impulse shows genuine phase- and sample-rate-dependent
  movement consistent with a modulated delay, but not enough data points to
  fix the exact depth-to-milliseconds scale without much more sine-sweep
  probing.
- `apulsator`: `width != 1` (probed and found to reshape the curve in a way
  not reproduced exactly) and `timing=bpm`/`ms` (converted to `hz` as
  `bpm/60` and `1000/ms`, not independently verified).
- `aexciter`, `deesser`, `virtualbass`: each needs a band-splitting filter.
  All three use a simple one-pole low/high-pass (the same `OnePole` shape
  `crossfeed` already used in this crate before this work package). Not
  claimed to match the reference's own filter shape or detector — and, as
  of the `vaco-filter-adsp::biquad` consolidation, not for lack of trying a
  better one: with the biquad crate reachable (this crate already depends
  on it for `wave`/`wsola`), a real two-pole Butterworth was substituted
  for the one-pole split in each of the three and measured against
  `ffmpeg` on the same probe inputs the crate already uses elsewhere. It
  did not help:

  | Filter | Probe | One-pole max error | Biquad max error |
  |---|---|---:|---:|
  | `aexciter` (defaults) | crate's 8-sample sequence | 0.73 | 1.04 |
  | `deesser=i=0.5:m=0.5:f=0.5` | crate's 8-sample sequence | 0.657 | 0.657 (< 1e-15 different) |
  | `virtualbass=cutoff=250:strength=3` | 4000-sample 80 Hz sine | 0.57 | 0.95 |

  `aexciter` and `virtualbass` got measurably *worse* with a real biquad —
  the reference's actual internal shape is evidently not "this same
  structure at a higher filter order". `deesser` was unaffected either way,
  because at this probe's amplitude the short-term envelope never crosses
  the fixed `0.15` excess threshold, so `reduction` stays `0` and
  `low + ess` reconstructs `dry` regardless of what produced `low` — the
  gap to the reference there is in the undocumented detector/gain-reduction
  logic, not the filter. All three keep the one-pole design; see each
  module's own doc for the measurement in context.
- `dialoguenhance`: measured to **not** be an identity even at every
  default option (`original=1, enhance=1, voice=2` on a smoothly-varying
  stereo signal reads back close to silence for the first several samples
  and stays far from the input throughout) — a real voice-activity gate,
  not a mix knob, the same shape of trap this project's correctness
  discipline documents for `hqdn3d`'s printed defaults. Implemented instead
  as a plain, always-on mid/side rebalance that is **not** a match for the
  reference at any option value, stated plainly rather than shipped as a
  quiet near-miss.
- `atempo`: WSOLA's window size, search radius and cross-fade shape are not
  tuned against the reference. What *is* checked is the two
  duration-scaling invariants this project's correctness discipline calls
  out by name (`tempo=1.0` exact identity, `tempo=2.0` exactly halves
  duration within one analysis window), verified against
  `vaco-filter-adsp::wsola`'s own tests. `atempo` also buffers the entire
  stream and only produces output at end-of-stream flush, rather than
  streaming incrementally — a real, stated limitation (see `atempo.rs`'s
  module doc), not a hidden one.
- `crossfeed`: `strength = 0` measured exactly (pure `level_in * level_out`
  gain); `strength > 0`'s crossfeed shape is this crate's own design.
- `axcorrelate`: sign/magnitude of the correlation is measured; whether the
  reference demeans its window first is not distinguishable from outside
  the binary.

## How to change it

- New filter in this family: add a module following `dcshift.rs`'s shape
  (single audio in/out, no LFO), `tremolo.rs`'s (single audio in/out, one
  `vaco_filter_adsp::wave::Lfo`), or `axcorrelate.rs`'s (two audio inputs
  via `Synced`); add its name to `registry::NAMES` and the `match` in
  `AeffectsRegistry::create`; add a `[[component]]` to
  `vaco-component.toml`. The fuzz target (`filter_aeffects_options.rs`)
  reads names from `AeffectsRegistry::names()` directly, so it does not
  need updating.
- Changing a default or adding an option: check `ffmpeg -h filter=<name>`
  first, not memory — nearly every module here found at least one thing the
  option table's prose does not say (see the module docs).
- A delay line keyed to an option in milliseconds **must** be pre-filled
  with `delay_samples` zeros before the first real sample arrives (`adelay`,
  `aecho`, `compensationdelay`, `haas`, `stereowiden`). A `VecDeque` that
  merely caps its own length at `delay_samples` returns the wrong
  (too-early) value for every sample before it first fills.
- A delay-in-milliseconds option **must** be clamped before it sizes a
  `VecDeque`/`Vec`, or an absurd option value is an absurd, attacker-sized
  allocation reached before `FramePool`'s own limits get a say — the same
  shape a fuzz target already found in an unrelated crate's frame-size
  options. `haas`'s `left_delay`/`right_delay` (`0..40`) and
  `stereowiden`'s `delay` (`1..100`) clamp to the reference's own declared
  range (`ffmpeg -h filter=<name>`); `adelay`'s `delays` has no such range
  in the reference at all (it is a bare `<string>`), so its
  `MAX_DELAY_MS` constant is a defensive engineering cap, not a
  conformance clamp — document the difference if you add another one.
  `compensationdelay`'s `mm`/`cm`/`m` were the original worked example.
- A new LFO-driven filter should reach for
  `vaco_filter_adsp::wave::{Lfo, WaveShape}` rather than re-deriving
  sine/triangle/square/sawtooth evaluation; a new modulated-delay filter
  should reach for `common::InterpDelay` rather than writing a second
  linear-interpolation delay line.
- A filter that needs proper biquad design should reach for
  `vaco_filter_adsp::biquad` — reachable from this crate already. Do not
  assume a real biquad automatically improves an existing one-pole
  approximation without measuring, though: `aexciter`, `deesser` and
  `virtualbass` all tried it and it did not help (see their own module
  docs and the table above).

## Configuration

No environment variables or feature flags. Behaviour is entirely the
per-filter options above, read at filtergraph-parse time via
`Instantiate::named`, following `vaco-filter-audio-eq::common`'s precedent:
an option this crate does not implement is accepted and silently ignored
rather than rejecting a filtergraph string that sets it (e.g.
`dialoguenhance`'s `voice`, accepted and stored but not applied — see that
module's doc for why).

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-resample`
(the shared `f64` sample domain, `sample.rs`), `vaco-filter-core`
(`Filter`/`FrameFilter`, the `Simple` adapter), `vaco-filter-graph`
(`FilterRegistry`), `vaco-filter-framesync` (`Synced`/`FrameSyncFilter`, for
`axcorrelate`), `vaco-filter-adsp` (wave-table LFOs and WSOLA, new this
work package). No new third-party dependencies.

`earwax`'s two 32-element FIR tables (`DIRECT`, `CROSS`) are declared in
`provenance/vaco-filter-aeffects.toml`, citing the existing
`ffmpeg-filters-probe` blackbox source in `provenance/sources.toml`. No
filter added this work package introduces a `static`/`const` array of 32 or
more elements (`registry::NAMES` has 22 entries), so no new provenance
entries were needed; `cargo xtask provenance-check` confirms this.

## vaco-filter-adsp

Companion crate created this work package,
[`crates/filter/vaco-filter-adsp`](../../crates/filter/vaco-filter-adsp).
Two modules:

- `wave`: `WaveShape` (`Sine`/`Triangle`/`Square`/`SawUp`/`SawDown`) and
  `Lfo`, a phase-accumulator that turns a frequency and sample rate into a
  stream of shape samples scaled to `[min, max]`. Independent oracle: every
  shape must be periodic with period `1.0` and its mean over a full period
  must sit at the midpoint of `[min, max]` — checked directly rather than
  against a second implementation of the same formula.
- `wsola`: `wsola_tempo`, time-domain WSOLA (windowed cross-correlation
  search for the best splice point, then overlap-add). No FFT dependency —
  a phase-vocoder alternative is left for a future caller that specifically
  needs it. Independent oracle: output length must track `input_len /
  tempo` (the defining arithmetic property of the window/hop spacing,
  independent of the correlation search), checked at several ratios
  including the two identities `tempo=1.0` (exact) and `tempo=2.0` (exact
  halving).

Both modules are `#[forbid(unsafe_code)]`, layer 5 (matching every other
filter crate), with no third-party dependencies.
