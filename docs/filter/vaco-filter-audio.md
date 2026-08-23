# vaco-filter-audio

T1 audio filters (FT-4.2, GitHub issue #466): `aresample`, `aformat`, `volume`,
`amix`, `amerge`, `channelmap`, `channelsplit`, `join`, `pan`, `asetnsamples`,
`asetrate`.

## What it is

Eleven of the "48" T1 filters `planning/16-filters.md` §5.3 lists as required
for `vaco` to be a usable transcoder at all — the audio-core set. Each is a
module (`src/<name>.rs`) exposing a `pub const DESC: FilterDesc` (the
`-filters`/`-h filter=` listing descriptor) and a crate-private
`fn create(&Instantiate) -> Result<Instance, String>`.
[`registry::AudioRegistry`](../../crates/filter/vaco-filter-audio/src/registry.rs)
implements `vaco_filter_graph::registry::FilterRegistry` by dispatching a
parsed filter name to the matching module's `create`.

## How it works

### The shared numeric domain (`sample.rs`)

`volume`, `amix`, `amerge`, `channelmap`, `join` and `pan` all need to read
and combine sample values across the twelve `SampleFmt` variants a link might
negotiate. Rather than hand-roll conversions, `sample::decode`/`sample::encode`
round-trip a frame's planes through `vaco_resample::convert::convert` into
planar `f64` and back — reusing the one place in the tree that has already
measured the reference's exact rounding behaviour (D17) for every format
pair, at the cost of an extra copy.

**A real bug this caught**: `sample::encode`'s packed-format branch originally
computed the output channel count as the *plane count* (always 1 for an
interleaved format) rather than the layout's actual channel count. Every mix
into a packed format (`s16`, `flt`, ...) silently consumed its input and
produced nothing, because `convert::convert` failed on the channel-count
mismatch and `mix()`'s `.ok()?` swallowed the error. Found by the `amix`
integration tests below, not by inspection — see git history on `sample.rs`
for the fix and `amix.rs`'s test module for the regression tests.

### `amix` — uneven input endings

Measured against the reference (`ffmpeg -f lavfi -i sine=... -f lavfi -i
sine=... -filter_complex amix=... -f null -`, commands in `amix.rs`'s module
doc): `duration=longest` (default) keeps mixing with whichever inputs have
not drained, `duration=shortest` ends the instant *any* input drains, and
`duration=first` tracks input 0 specifically regardless of which input is
actually longer. All three are exercised by graph-level integration tests in
`amix.rs`.

Implemented as one rule: each step, `quota` (samples produced) is the minimum
of `available[i]` over a duration-dependent *candidate set*; a drained input
simply leaves the candidate set (contributing silence, not padding), which is
what makes `longest` fall out without a special case. `dropout_transition` is
parsed but **not** applied as a smooth ramp — the normalisation factor changes
instantly rather than crossfading, unlike the reference.

### `pan` — does `vaco-expr` fit?

Yes, via a trick: `pan`'s grammar (`OUT=[gain*]IN[[+-][gain*]IN...]`) is
strictly linear, so each `OUTSPEC` is parsed as a `vaco_expr::Expr` bound to
one variable per input channel (`c0..cN`, resolved only in `configure` once
the real input channel count is negotiated) and evaluated once per input
channel at its unit basis vector (that channel = 1.0, every other = 0.0).
Homogeneity means the result *is* that channel's linear coefficient, which
reconstructs the whole mixing matrix without a second parser. Verified by a
channel-swap integration test (`pan=stereo|c0=c1|c1=c0`) in `pan.rs`.

Named channel references (`FL`, `FR`, ...) are **not** implemented on either
side of `=` — only the numeric `cN` form and `LAYOUT` itself (a full
`ChannelLayout::from_name` parse, so `stereo`, `5.1`, `4c`, ... all work).

### `asetnsamples` — why it does not use `vaco_filter_core::adapt::AudioFilter`

That adapter's `SampleFifo` is documented as frame-granular, not
sample-granular: it refuses to cut a block mid-frame. `asetnsamples` exists
specifically to do that (re-block into an exact `nb_out_samples`), so this
filter keeps its own per-channel `f64` accumulator via `sample::decode` and
implements `FrameFilter` directly. Any other T2 audio filter needing an exact
frame size (FFT-domain filters built on `vaco-tx`) will hit the same wall.

### Sample-accurate plumbing versus approximated plumbing

- **Exact / measured**: `aresample`'s rate and format target selection,
  `amix`'s three `duration` modes, `asetnsamples`'s exact re-blocking with
  optional zero-padding, `pan`'s coefficient extraction, `asetrate`'s
  metadata-only rate change.
- **Structurally present, not measured against the reference**: `amerge`'s
  `layout_mode` (the reference's own docs do not specify what the three modes
  actually compute — see `amerge.rs`), `channelmap`'s `IN`/`OUT` name
  resolution (index form only, not channel names), `join`'s default
  sequential channel mapping, `volume`'s `precision`/`replaygain*` options
  (arithmetic is always `f64`; ReplayGain side data has no representation in
  `vaco_frame::FrameSideData` yet to read from).

## How to change it

- Add a filter: create `src/<name>.rs` following an existing module's shape
  (`DESC`, an `Opts` struct if it takes options, a filter type, `create`),
  declare `mod <name>;` in `lib.rs`, add a `[[component]]` entry to
  `vaco-component.toml`, wire the name into `registry.rs`'s `NAMES` and
  `create` match, then run `cargo xtask gen-registry`.
- Keep new option/filter/state types `pub(crate)`, not `pub`: `cargo xtask
  dup-check` flags a `pub struct`/`pub enum` name that appears in two
  *different* crates, and with ~35 T1/T2 audio filters converging on names
  like `Options`/`State`, `pub(crate)` is what keeps this crate off that
  ledger without an allowlist row per filter.
- To change the numeric domain (e.g. add a fast path that skips the `f64`
  round-trip for same-format channel-only operations), start in `sample.rs`;
  every filter that mixes or remaps channels depends on it.

## Configuration

Each filter's options are declared with `#[derive(vaco_opts::Options)]` and
parsed via `Options::set_from_string(args, "=", ":")` against the raw
`k=v:k2=v2` text the filtergraph parser hands to `Instantiate::args` — except
`pan`, whose entire grammar has no top-level `:` and is read from `args`
directly as one blob (see `pan::create`'s doc comment). Defaults and ranges
were captured with `LC_ALL=C ffmpeg -h filter=<name>` against ffmpeg 8.1; see
each module's doc comment for exactly which of the reference's options are
implemented versus omitted.

`SampleFmt`/`ChannelLayout`-typed options (`aresample`'s `out_sample_fmt`/
`out_chlayout`, `amerge`/`channelmap`/`channelsplit`/`join`'s
`channel_layout`) are declared as `String` fields and parsed by hand in
`create`/`configure`, not as typed `#[opt]` fields — see Dependencies below.

## Dependencies

- `vaco-filter-core` — `Filter`, the `Simple`/`FrameFilter` adapter, format
  negotiation (`FormatSet`, `NodeFormats`, `Tie`).
- `vaco-filter-graph` — the `FilterRegistry`/`Instantiate`/`Instance`
  construction contract; `pads::audio(n)` for dynamic pad counts.
- `vaco-resample` — `Resampler` (`aresample`'s actual engine) and
  `convert::convert` (`sample.rs`'s numeric domain).
- `vaco-opts` — `#[derive(Options)]` option parsing.
- `vaco-expr` — `pan`'s and `volume`'s expression evaluation.
- `vaco-chlayout`, `vaco-sampfmt`, `vaco-frame` — the audio data model.

**A gap found, not fixed here** (out of this crate's scope): neither
`vaco-sampfmt::SampleFmt` nor `vaco-chlayout::ChannelLayout` implements
`vaco_opts::OptValue`. `vaco-resample`'s own `ResampleOptions` documents the
same gap and works around it the same way this crate does (raw `String`
fields, parsed by hand) — see that crate's `opts.rs` module doc.

## Issues

Closes GitHub #466 (FT-4.2). All eleven filters are present and exercised on
their documented common path; see "Sample-accurate plumbing versus
approximated plumbing" above for exactly what is measured versus structural.
