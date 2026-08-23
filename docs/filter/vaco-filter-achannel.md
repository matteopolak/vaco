# vaco-filter-achannel

T3 audio channel, layout and mixing filters (FT-4.13b, GitHub issue #482):
`axcorrelate`, `crossfeed`, `earwax`, `extrastereo`, `haas`, `stereotools`,
`stereowiden`.

## Scope reconciliation

The brief that requested this crate named a channel/layout/mixing family
drawn loosely from `amerge`, `amix`, `asplit`-adjacent work, `channelmap`,
`channelsplit`, `join`, `pan`, `surround`, `stereotools`, `stereowiden`,
`haas`, `crossfeed`, `earwax`, `axcorrelate`, `headphone`, `sofalizer`, and
said explicitly not to trust that list in either direction.

Checked directly against `crates/filter/*/vaco-component.toml` (D19: register
nothing already registered) rather than trusting the brief's restatement:
`vaco-filter-audio` already registers `amerge`, `amix`, `channelmap`,
`channelsplit`, `join` and `pan`; `vaco-filter-plumbing` already registers
`asplit`. Seven of the brief's fifteen names are already owned elsewhere, so
this crate does not touch them.

Counted against the shipped reference (`ffmpeg -hide_banner -filters`,
ffmpeg 8.1, 2026-08-23) rather than recalled, one more member turned up that
the brief's list missed: `extrastereo` ("Increase difference between stereo
audio channels") is exactly this family and is implemented here.
`amultiply`, `ainterleave` and `acrossfade` also take multiple audio inputs
but combine them in *time* (ring-modulate, temporally interleave,
cross-fade), not in channel layout or mixing, so they were left out as a
different family.

Three names are not implemented:

- **`sofalizer`** does not exist in the reference binary this project
  measures against — `ffmpeg -h filter=sofalizer` prints `Unknown filter`,
  because the local build lacks `--enable-libmysofa`. There is nothing to be
  sample-exact against (D17), so it cannot be measured, let alone
  implemented, from this vantage point.
- **`headphone`** needs a full HRTF convolution engine driven by
  caller-supplied impulse-response streams (`N->A`, a `map` grammar for
  which extra input drives which output channel) — anticipated as a likely
  skip in this work package's own brief, confirmed after reading the option
  table.
- **`surround`** is an STFT/overlap-add upmix filter bank with per-channel
  spread parameters and twenty `win_func` choices — comparable in scope to
  `vaco-filter-audio-eq::superequalizer`, and disproportionate to this work
  package's pace target. Flagged here rather than silently dropped.

Seven implemented, plus seven already registered by sibling crates, plus
three skipped, is fourteen — matching GitHub #482's own "~14" estimate once
the overlap with `vaco-filter-audio`/`vaco-filter-plumbing` is subtracted.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::AchannelRegistry`](../../crates/filter/vaco-filter-achannel/src/registry.rs)
— the same shape `vaco-filter-audio-eq` and `vaco-filter-audio-dynamics` use.
`src/sample.rs` is the same `f64`-domain frame decode/encode those two crates
carry, duplicated rather than shared (see that module's own doc for why).
`axcorrelate` is the one filter with two audio inputs that must be aligned in
time, so it goes through `vaco-filter-framesync`'s `Synced`/`FrameSyncFilter`
adapter instead of `vaco-filter-core::adapt::Simple` — the same reason
`vaco-filter-audio-dynamics::sidechaincompress` does.

## How it works: what is measured versus structural

Every module's own doc comment states its evidence in full; this is the
summary.

**Sample-exact against `ffmpeg` 8.1** (measured by probing the binary, D17 —
never by reading its source, D7):

- `extrastereo`: the full mid/side formula, both with and without clipping.
- `stereowiden`: the full drymix/crossfeed/feedback formula, confirmed as a
  single-tap cross-delay (not a recirculating feedback loop, despite the
  option's name) by checking for further taps out to lag 2150 and finding
  none.
- `stereotools`: all eleven `mode` values (a plain mid/side matrix with *no*
  extra `0.5` beyond the one already in `mid`/`side`), plus `level_in`,
  `level_out`, `balance_in`, `balance_out`, `mutel`, `muter`, `phasel`,
  `phaser`.
- `earwax`: the complete 32-tap FIR (both the own-channel and cross-channel
  sequences), recovered by feeding a full-scale impulse into `ffmpeg -af
  earwax` at 44100 Hz and reading the exact `/128` fixed-point coefficients
  back out of the output — three different impulse amplitudes agreed on the
  same integer taps with zero rounding noise. At other sample rates the
  reference's response is much longer and not causal-looking, the signature
  of internal resampling around a fixed 44100 Hz design; this implementation
  applies the measured taps unconditionally regardless of the input's actual
  rate, which is exact at 44100 Hz and a structural approximation elsewhere.

**Sample-exact for the reference's own default options, structural beyond
that:**

- `haas`: the full three-branch structure (an undelayed centre term plus two
  delayed/gain/phase branches panned by `{left,right}_balance`) reproduces
  the reference's default-option impulse response exactly, including the
  non-obvious discovery that `left_balance = -1` routes its branch entirely
  into the **right** output and `right_balance = 1` routes its branch
  entirely into the **left** — the opposite of what the option names
  suggest. `middle_source` values other than the default `mid`,
  `middle_phase`, and whether `side_gain` scales only the centre term (the
  default probe cannot distinguish that from "no gain applied", since
  `side_gain` defaults to `1` either way) are a structural extension of the
  measured shape, not independently verified.

**Structural (a plausible, option-table-consistent implementation, not
verified against the reference's exact output):**

- `crossfeed`: `strength = 0` measured exactly as pure `level_in * level_out`
  gain with no cross-mix at all. The `strength > 0` crossfeed shape (a
  one-pole low-pass of the opposite channel, gated by `strength`, tuned by
  `range`/`slope`) is this crate's own design — isolating the reference's
  true transfer function would need per-frequency sine-sweep probing beyond
  this work package's budget.
- `axcorrelate`: the sign and magnitude of a normalised cross-correlation
  (identical signals -> +1, inverted -> -1, uncorrelated -> ~0) is measured;
  whether the reference demeans its sliding window before the ratio is not
  distinguishable from outside the binary, since every probe signal used was
  already (approximately) zero-mean. This implementation uses the raw
  (non-demeaned) form.

## How to change it

- New filter in this family: add a module following `extrastereo.rs`'s
  shape (single audio in/out) or `axcorrelate.rs`'s (two audio inputs via
  `Synced`), add its name to `registry::NAMES` and the `match` in
  `AchannelRegistry::create`, and add a `[[component]]` to
  `vaco-component.toml`.
- Changing a default or adding an option: check `ffmpeg -h filter=<name>`
  first, not memory — this crate's own `haas`/`stereowiden`/`earwax`
  measurements each turned up something the option table's prose does not
  say (see the module docs).
- A delay line keyed to an option in milliseconds (`haas`'s `Branch`,
  `stereowiden`'s `hist_l`/`hist_r`) **must** be pre-filled with
  `delay_samples` zeros before the first real sample arrives. A `VecDeque`
  that merely caps its own length at `delay_samples` returns the wrong
  (too-early) value for every sample before it first fills — this was a real
  bug caught during development by an end-to-end test that drove the actual
  delay line rather than a hand-supplied "already delayed" value (see
  `haas::tests::matches_measured_default_impulse_response` and
  `stereowiden::tests::end_to_end_delay_line_matches_measured_lags`).

## Configuration

No environment variables or feature flags. Behaviour is entirely the
per-filter options above, read at filtergraph-parse time via
`Instantiate::named`, following `vaco-filter-audio-eq::common`'s precedent:
an option this crate does not implement (e.g. `stereotools`'s `slev`,
`mlev`, `mpan`, `base`, `delay`, `sclevel`, `phase`, `bmode_in`, `bmode_out`,
`softclip`) is accepted and silently ignored rather than rejecting a
filtergraph string that sets it.

## Dependencies

`vaco-core`, `vaco-frame`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-resample`
(the shared `f64` sample domain, `sample.rs`), `vaco-filter-core`
(`Filter`/`FrameFilter`, the `Simple` adapter), `vaco-filter-graph`
(`FilterRegistry`), `vaco-filter-framesync` (`Synced`/`FrameSyncFilter`, for
`axcorrelate`). No new third-party dependencies.

`earwax`'s two 32-element FIR tables (`DIRECT`, `CROSS`) are declared in
`provenance/vaco-filter-achannel.toml`, citing the existing
`ffmpeg-filters-probe` blackbox source in `provenance/sources.toml`.
