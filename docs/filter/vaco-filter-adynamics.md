# vaco-filter-adynamics

T2 audio dynamics filters (FT-4.8b, GitHub issue #472, the other of two
children FT-4.8/#56 split into for single-writer ownership — the sibling is
`vaco-filter-aeq`/#471): the compressor/limiter/gate/expander/sidechain
family plus loudness normalisation and measurement.

## Scope reconciliation

GitHub #472's own text (checked with `gh issue view 472` rather than trusted
from the brief that requested this crate, per this project's practice after
an earlier agent found its epic named a different grouping than its brief
claimed) reads: "Dynamics: compressor/limiter/gate/expander/sidechain family
plus `loudnorm` and `dynaudnorm`." Counted against `ffmpeg -filters`
(2026-08-23) rather than recalled, that maps to **nine** filters:
`acompressor`, `alimiter`, `agate`, `compand` and `mcompand` (the "expander"
family — `compand`'s own reference description is literally "Compress or
expand audio dynamic range"), `sidechaincompress`, `sidechaingate`,
`loudnorm`, `dynaudnorm`.

The brief that requested this crate additionally named `speechnorm`,
`volumedetect`, `astats`, `silencedetect`, `silenceremove` — five
measurement/silence filters #472's own text does not mention at all. All
fourteen (the nine plus the five) are implemented here; the honest
accounting is that five of them are outside what the tracking issue itself
asked for, done because the brief asked for them and they are all genuinely
useful audio-level tooling, not because #472 requested them.

## What is verified versus structural

There is no single citable specification for a feed-forward dynamics
processor the way the Audio EQ Cookbook is one for a biquad
(`vaco-filter-aeq`'s "hard part"). `engine.rs`'s gain-computer curve
uses Giannoulis, Massberg & Reiss, "Digital Dynamic Range Compressor Design
— A Tutorial and Analysis" (J. Audio Eng. Soc., 2012) for its soft-knee
quadratic — a real, citable, independent formula — but the envelope
follower and the overall processor shape are standard DSP practice, not a
port of anything.

**Numerically verified** (real properties of the gain curve, computed from
first principles rather than a re-transcription): `ratio=1` is the gain
identity at every level; a level strictly below threshold produces zero
gain change (downward mode) and strictly above threshold produces zero
change (upward mode); a `proptest` checks `gain_db` stays finite across the
full documented range of `threshold`/`ratio`/`knee`, including negative and
zero `ratio`/`knee` values a filtergraph could actually send. `compand`'s
transfer curve is checked to be the identity when its control points are
the identity, and to fall back to identity cleanly when the `points` string
is empty or malformed. `mcompand`'s crossover biquads are checked to stay
finite across cutoffs from 0 Hz through above-Nyquist. `speechnorm`'s gain
never leaves its `[1/compression, expansion]` bounds.

**Structural** (implemented, exercised on the common path, not held to a
numeric oracle): `alimiter` (no real lookahead — see its module doc),
`sidechaincompress`/`sidechaingate` (the two-input wiring through
`vaco-filter-framesync` is exercised by `registry`'s creation test but not
by a running two-stream graph), `dynaudnorm`/`loudnorm` (causal EMA
approximations of algorithms the reference runs non-causally or with true
K-weighting — see their module docs for exactly what is and is not
implemented), `astats`/`volumedetect`/`silencedetect` (a subset of the
reference's measured parameters, logged via `tracing::info!` rather than
`av_log` or `stats_file`), `silenceremove` (single-period start/stop
trimming only).

## How it works

### `engine.rs` — the envelope follower and gain-computer curve

[`Envelope`] is a one-pole follower with independent attack/release time
constants (`1 - exp(-1/(time_s * sample_rate))`, the standard RC-to-coefficient
mapping). [`Curve`] is the static compressor/expander/gate curve described
above. `common::Dynamics` composes these into the whole body of
`acompressor`/`agate` (driven by the input itself) and
`sidechaincompress`/`sidechaingate` (driven by a second input, via
`vaco-filter-framesync`'s `Synced`/`FrameSyncFilter` — see
`sidechaincompress.rs`).

### Measured defaults

Probed via `ffmpeg -h filter=<name>`, 2026-08-23:

| Filter | threshold | ratio | attack/release (ms) | knee | makeup |
|---|---:|---:|---:|---:|---:|
| `acompressor`/`sidechaincompress` | 0.125 (linear) | 2 | 20/250 | 2.82843 | 1 |
| `agate`/`sidechaingate` | 0.125 | 2 | 20/250 | 2.82843 | 1 |

`agate`'s `range` (max attenuation) defaults to `0.06125`; `acompressor` has
no `range` (no floor on attenuation). `alimiter`: `limit=1`, `attack=5ms`,
`release=50ms`. `compand`: `points="-70/-70|-60/-20|1/0"`,
`decays="0.8"`. `dynaudnorm`: `framelen=500ms`, `gausssize=31`,
`peak=0.95`, `maxgain=10`. `loudnorm`: `I=-24`, `TP=-2`, `LRA=7`.
`speechnorm`: `peak=0.95`, `expansion=2`, `compression=2`.
`silencedetect`: `noise=0.001` (linear), `duration=2s`.

`acompressor`'s class name printed by `ffmpeg -h filter=acompressor` is
literally `acompressor/sidechaincompress` and `agate`'s is
`agate/sidechaingate` (probed 2026-08-23) — confirming, the same way
`vaco-filter-aeq` confirmed `bass`/`lowshelf`, that these are one
registered processor under two names in the reference, which is why this
crate shares one `Dynamics`/`Curve` engine between each pair rather than
writing sidechain variants from scratch.

### `mcompand` — crossover filters, now shared via `vaco-filter-adsp`

`mcompand.rs` used to implement its own tiny second-order Butterworth
low-pass/high-pass pair (`Biquad2`), on the reasoning that a dozen lines of
the same cookbook formula was cheaper than a cross-crate coupling between
the two FT-4.8 children. `vaco-filter-adsp::biquad` now exists as the
shared home that reasoning was arguing against (D19: the crate's size is
not the test, whether the concept is shared is), so `crossover_lowpass`/
`crossover_highpass` in `mcompand.rs` call `vaco_filter_adsp::biquad::{lowpass,
highpass}` at `Q = 1/sqrt(2)` (Butterworth; still fixed — a crossover has no
user-facing `Q`/`width` option) instead of recomputing the formula locally.

One piece stayed local: a crossover frequency at/below DC or at/above
Nyquist is substituted with the identity (lowpass) or zero (highpass)
section *before* calling into `vaco-filter-adsp`, because that crate's
`lowpass`/`highpass` guarantee only "coefficients stay finite" outside
`(0, Nyquist)`, not "physically sensible" — reproducing this filter's prior
exact behaviour at those edges required keeping the guard here. See
`vaco-filter-adsp`'s own doc for the other three duplicate-formula sites
this same move touched, and what auditing them turned up.

### `silenceremove` — single-period, window-granular

Silence is judged per fixed-size `window` (default 20 ms), not per sample.
`start_periods=1`/`stop_periods=1` (by far the most common invocation) is
implemented as a three-state machine (`Start` → drop until non-silent;
`Passthrough` → emit normally; `Buffering` → a silent run that might be the
tail, held back until either non-silence arrives — flush it, it was not the
tail — or end of stream confirms it was, and only the last `stop_silence`
seconds of it are kept). Anything beyond one period per end is treated as
one period, a documented gap rather than a probed equivalence.

### The astats/volumedetect/silencedetect logging convention

None of these filters' actual contract is their audio output (which they
pass through unchanged) — it is the text they print. This crate has no
`av_log` equivalent, so all three log through `tracing::info!` instead,
under `target: "vaco_filter_adynamics::<name>"`. The exact line format
approximates the reference's (`max_volume: <N> dB`, `silence_start: <t>`,
etc.) but has not been probed byte-for-byte against `ffmpeg`'s output.

## How to change it

* A new compressor-family filter: reuse `common::Dynamics` — see
  `acompressor.rs`/`agate.rs`'s `build()` functions, which are also what
  `sidechaincompress.rs`/`sidechaingate.rs` call to avoid duplicating option
  parsing.
* Changing the gain-computer curve: `engine.rs::Curve::gain_db` is the one
  place it lives; keep `tests::ratio_one_is_identity` and
  `tests::below_threshold_is_untouched_downward` passing — they are the
  actual specification check, not a re-statement of the formula.
* `common.rs`'s doc explains the same `Instantiate::named`-over-strict-
  `Options` choice `vaco-filter-aeq::common` makes, for the same
  reason (several reference options are accepted but not applied).

## Configuration

No environment variables or feature flags. `astats`/`volumedetect`/
`silencedetect` write to the `tracing` subscriber the embedding application
installs; with none installed, the events are simply dropped.

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-resample`
(the shared `f64` sample domain, `sample.rs`, identical in approach to
`vaco-filter-aeq::sample`), `vaco-filter-core` (`Filter`/`FrameFilter`/
`AudioFilter`, the `Simple`/`Blocked` adapters), `vaco-filter-graph`
(`FilterRegistry`), `vaco-filter-framesync` (`Synced`/`FrameSyncFilter`, for
`sidechaincompress`/`sidechaingate`'s two inputs), `tracing` (the
`astats`/`volumedetect`/`silencedetect` logging surface), `vaco-filter-adsp`
(`biquad`: `Coeffs`, `State`, `WidthType`, `lowpass`, `highpass`) for
`mcompand`'s crossover filters — new dependency, added when `mcompand`'s own
duplicate `Biquad2` was replaced with this shared type.
