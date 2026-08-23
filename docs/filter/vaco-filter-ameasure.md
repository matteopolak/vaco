# vaco-filter-ameasure

T3 audio analysis and measurement filters (FT-4.13c, GitHub issue #483,
origin plan `16-filters.md` §4.3/§8.4): `ashowinfo`, `aspectralstats`,
`apsnr`, `asdr`, `asisdr`, `drmeter`, `ebur128`, `replaygain`,
`aphasemeter`, `aderivative`, `aintegral` — eleven filters.

## Scope reconciliation

GitHub #483's own suggested membership list said plainly not to trust it,
so it was checked against `ffmpeg -hide_banner -filters` / `ffmpeg -h
filter=<name>` (ffmpeg 8.1, 2026-08-23) and against plan 16 §4.3's
`vaco-filter-aanalysis` row — the table row this work package actually
originates from — rather than accepted as written. It was wrong in four
ways:

1. **`showfreqs`/`showspectrum`/`showvolume`/`showwaves` do not belong
   here.** They are `A->V` visualisers (audio in, video out). Plan 16 §4.3
   gives the whole visualiser family — those four plus `showspectrumpic`,
   `showcqt`, `showcwt`, `showspatial`, `showwavespic`, `avectorscope`,
   `a3dscope`, `abitscope`, `ahistogram`, `spectrumsynth` — its own crate,
   `vaco-filter-avvis`, a separate T3 work package.
2. **There is no "`adrc`-adjacent analysis" filter.** `adrc` ("Audio
   Spectral Dynamic Range Controller") *modifies* the signal; plan 16 §4.3
   places it under `vaco-filter-adynamics` (i.e. `vaco-filter-audio-
   dynamics`'s territory), not `vaco-filter-aanalysis`.
3. **There is no "`aformat`-adjacent probing" filter.** `aformat` is a T1
   format-conversion filter, already registered by `vaco-filter-audio`.
   Nothing in the reference's measurement family is adjacent to it.
4. **`axcorrelate` is real and does belong in this family — but is already
   taken.** Plan 16 §4.3 places `axcorrelate` under `vaco-filter-
   aanalysis` (this crate's row), not under any channel/mixing grouping.
   But `vaco-filter-achannel` (FT-4.13b, GitHub #482 — a different work
   package, apparently handed the same filter by its own brief) landed
   first and already registers it. `cargo xtask dup-check` — and, before
   that, `gen-registry` itself, which refuses to emit two rows for one
   name — is what surfaced this. This crate does not re-register
   `axcorrelate`; a fully independent implementation and its tests
   (Pearson-correlation fixed points, an O(1)-per-sample sliding window)
   were written and then deleted rather than shipped as unregistered dead
   code once the collision was found. This is a genuine two-issue overlap
   for the orchestrator to note, not a mistake either agent's brief could
   have caught by reading only its own issue.

Plan 16 §4.3's `vaco-filter-aanalysis` row names fourteen filters in total:
`astats`, `aspectralstats`, `ebur128`, `drmeter`, `silencedetect`,
`replaygain`, `apsnr`, `asdr`, `asisdr`, `axcorrelate`, `aderivative`,
`aintegral`, `ashowinfo`, `aphasemeter`. Three are excluded from this
crate's registration: `astats` and `silencedetect` because
`vaco-filter-audio-dynamics` already registers both (its own scope drifted
to include them — see that crate's doc), and `axcorrelate` per point 4
above. The remaining eleven are implemented and registered here.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::AmeasureRegistry`](../../crates/filter/vaco-filter-ameasure/src/registry.rs)
— the same shape every sibling audio crate uses. Two shared pieces sit
underneath:

- [`kweight`](../../crates/filter/vaco-filter-ameasure/src/kweight.rs) — the
  ITU-R BS.1770-4 K-weighting filter design (a high-shelf and a high-pass,
  recomputed from `(f0, Q, gain)` at the link's actual sample rate).
- [`loudness`](../../crates/filter/vaco-filter-ameasure/src/loudness.rs) —
  the gated BS.1770-4 loudness scanner built on `kweight`: 100 ms
  sub-blocks, the 400 ms/75%-overlap window and two-stage gate for
  Integrated Loudness, the 3 s window and percentile method for Loudness
  Range. `ebur128` and `replaygain` both use this one scanner (D19) rather
  than each measuring loudness its own way.

`apsnr`, `asdr` and `asisdr` are the two-input filters here; they go
through `vaco-filter-framesync`'s `Synced`/`FrameSyncFilter` adapter
instead of `vaco-filter-core`'s `Simple`, the same way
`vaco-filter-audio-dynamics::sidechaincompress` does. They share one
accumulator, [`common::PairStats`](../../crates/filter/vaco-filter-ameasure/src/common.rs)
(`sum_ref_sq`/`sum_est_sq`/`sum_diff_sq`/`sum_cross`/`count`), each
filter reducing it with a different closed-form formula.

## How it works

### The oracle per filter (why the tests are real checks, not restatements)

Per `planning/AGENT-CONSTRAINTS.md`'s "an oracle you wrote shares your
misreading" — two transcriptions of the same equation cannot disagree, so
every filter here is checked against something structurally different
from its own implementation:

| Filter | Oracle |
|---|---|
| `aderivative`/`aintegral` | A round-trip property (`aderivative(aintegral(x)) == x`) and a closed-form fixed point (a ramp's derivative is its constant step), computed by hand in the test, not by calling the filter twice. |
| `apsnr` | `PSNR = 10*log10(peak^2/MSE)`, hand-computed on two-sample inputs (`[1,0]` vs `[0,0]` gives exactly `10*log10(2) ≈ 3.01 dB`). |
| `asdr` | Plain SDR (`10*log10(‖ref‖²/‖ref-est‖²)`), and specifically that it is scale-*variant*: a doubled estimate still reads distortion. |
| `asisdr` | Scale-invariant SDR (Le Roux et al. 2019, `provenance/sources.toml`'s `leroux-2019-sisdr`): the same doubled-estimate case `asdr` penalises reads as `+infinity` here — the contrast between the two filters' tests is what proves two different formulas were implemented, not one formula copied twice. |
| `aspectralstats` | [`aspectralstats::engine::measures`](../../crates/filter/vaco-filter-ameasure/src/aspectralstats/engine.rs) is a pure function of `(magnitude, frequency)` pairs, checked against synthetic spectra with known shapes (a single-bin spectrum's centroid is that bin's frequency and its spread is exactly zero; a flat spectrum has flatness `== 1`; a spectrum symmetric about its centroid has zero skewness) — never against a second FFT run. |
| `drmeter` | The published TT Dynamic Range Meter algorithm's own fixed point (a full-scale sine reads `DR == 0` exactly) and its defining property (at *equal* sustained loudness, a higher peak-to-RMS crest factor reads a higher DR). |
| `ebur128`/`replaygain` | A calibrated loudness reference tone: an amplitude derived independently from the closed-form loudness map (`-0.691 + 10*log10(mean square)`, not from a second gating loop) must read close to -23 LUFS, and digital silence must be fully gated away by the -70 LUFS absolute gate. |
| `aphasemeter` | Pearson-correlation fixed points: identical channels correlate at exactly `1.0`, exact opposites at exactly `-1.0`, and channels constructed to be exactly orthogonal (not merely uncorrelated by chance) at exactly `0.0`. |
| `ashowinfo` | Structural only — see "What is not verified" below. |

### K-weighting and the loudness gate

ITU-R BS.1770-4's K-weighting curve is specified as a high shelf
(models head diffraction, ~+4 dB above ~1.7 kHz) cascaded with a
high-pass (models the outer/middle ear's low-frequency rolloff, corner
~38 Hz). `kweight.rs` builds both stages from `vaco_filter_adsp::biquad`'s
standard Robert Bristow-Johnson "Audio EQ Cookbook" formulas at the
`(f0, Q, gain)` working point the spec gives, **recomputed at the link's
actual sample rate** rather than hard-coded as the reference's own printed
48 kHz coefficient table. That is what lets this filter skip an internal
resample step and still be correct at 44.1 kHz, 96 kHz, or anything else.

`kweight.rs` used to carry its own copy of these two formulas (`Coeffs`,
`BiquadState`, `high_shelf`, `high_pass`), written before
`vaco-filter-adsp::biquad` existed as a shared home for them (D19: one
biquad design, not five). It now depends on that crate and calls
`highshelf`/`highpass` directly. Auditing the old copy before deleting it
found one real divergence worth recording: its zero-`a0` fallback replaced
`a0` with `1.0` and left the numerator untouched, rather than returning the
identity section the shared `Coeffs::normalise` returns. Not reachable at
any real sample rate for this module's fixed design points (`SHELF_F0` ≈
1682 Hz, `HP_F0` ≈ 38 Hz), so not a live bug — but a second, quieter answer
to the same "what if `a0` is zero" question, exactly the kind of thing D19
exists to surface before it does matter.

`loudness.rs`'s gate is the two-stage BS.1770-4 algorithm: split into
400 ms blocks (100 ms apart, i.e. 75% overlap); stage 1 discards blocks
below -70 LUFS absolute; stage 2 computes the mean of what remains, maps
it to a loudness, subtracts 10 LU, and discards blocks below *that*;
Integrated Loudness is the loudness of the mean of what is left. Loudness
Range repeats the shape at a 3 s window (EBU Tech 3342): gate at -70 LUFS
absolute and -20 LU relative, then report the 95th-minus-10th percentile
spread of what survives.

### What is not implemented

- **True peak** (BS.1770-4 Annex 2's 4x-oversampled peak). `ebur128` and
  `replaygain` report the plain sample peak instead — the same documented
  simplification `vaco-filter-audio-dynamics::loudnorm` already makes, for
  the same reason (no oversampling filter exists in this codebase yet).
- **Video output.** `ebur128`'s meter graphic and `aphasemeter`'s phase
  scope are both accepted-but-ignored options; only the audio-domain
  measurement each exists to drive is implemented. Same shape as
  `vaco-filter-audio-eq::anequalizer`'s undone response-curve video.
- **`ashowinfo`'s checksum algorithm.** The reference prints
  `checksum:<hex>` and `plane_checksums: [ <hex> ... ]`; the field *names*
  are reproduced (D7: interface names are freely reusable) but the
  algorithm is not — it is unmeasured, and D7 forbids reading the
  reference's source to find out. This crate logs an FNV-1a hash instead:
  same shape, a different, honestly-different number.
- **`aspectralstats`'s window functions.** The reference has 21
  (`win_func`); only `hann` (the default) is implemented, and any other
  value silently falls back to it.
- **`axcorrelate`'s `algo` option.** Not applicable — this crate does not
  register `axcorrelate` at all (see "Scope reconciliation" above).

## How to change it

- Adding a filter: one new `src/<name>.rs` with `pub const DESC` and
  `pub(crate) fn create`, wired into `registry::NAMES` and
  `AmeasureRegistry::create`'s match, plus a `[[component]]` table in
  `vaco-component.toml` — then `cargo xtask gen-registry`. Check the other
  three `vaco-filter-audio*` crates' fragments first; the dup-check gate
  (and `gen-registry` itself) will refuse a second registration of a name
  another crate already owns.
- Changing the loudness gate's constants (`ABSOLUTE_GATE_LUFS`,
  `RELATIVE_GATE_OFFSET_LU`, `LRA_RELATIVE_GATE_OFFSET_LU` in
  `loudness.rs`): these are BS.1770-4/Tech 3342 constants, not tuning
  knobs — changing them changes what standard is being implemented.
- Adding a spectral measure to `aspectralstats`: add a field to
  `engine::Measures` and a computation in `engine::measures`, following
  the existing pattern of "closed form over `(mag, freqs)`, checked
  against a synthetic spectrum with a known shape".

## Configuration

No crate-level feature flags or environment variables. Per-filter options
are read directly off the filtergraph argument string
(`Instantiate::named`), the same non-strict convention every sibling audio
crate uses — an option this crate does not implement is silently accepted
rather than rejected. See each filter module's doc comment for its
specific option table, defaults and gaps (measured against `ffmpeg -h
filter=<name>`, ffmpeg 8.1, 2026-08-23).

## Dependencies

- `vaco-filter-core` (`Filter`, `Simple`, `FilterDesc`, `Pad`), `vaco-filter-graph`
  (`FilterRegistry`, `Instantiate`) — the same as every filter crate.
- `vaco-filter-framesync` (`Synced`, `FrameSyncFilter`) for `apsnr`/`asdr`/`asisdr`.
- `vaco-tx` (`Plan`/`Tx`, the workspace's FFT/MDCT/DCT crate) for
  `aspectralstats`'s window FFT.
- `vaco-resample` (`convert`, `AudioRef`/`AudioMut`) for the shared
  f64-domain sample decode/encode every filter here uses.
- `vaco-filter-adsp` (`biquad`: `Coeffs`, `State`, `WidthType`, `highshelf`,
  `highpass`) for `kweight`'s K-weighting cascade — new dependency, added
  when `kweight`'s own duplicate cookbook formulas were replaced with this
  shared one.
- `vaco-chlayout` for `ebur128`/`replaygain`'s per-channel BS.1770 weight
  lookup (front/centre `1.0`, surround/side/back `~1.41`, LFE `0`).
