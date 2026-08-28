# vaco-filter-key

Keying/masking video filters: `premultiply`, `unpremultiply`,
`maskedmerge`, `colorkey`, `colorhold`, `maskedclamp`, `maskedmax`,
`maskedmin`, `maskedthreshold`, `threshold` (10/20).

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-key` row is
20 filters, all verified against `ffmpeg -filters`/`ffmpeg -h
filter=<name>` (8.1) with no discrepancy from the plan in either
direction. Ten are built. `premultiply`/`unpremultiply`/`maskedmerge`
carried over from a prior mis-scoped crate (GitHub issue #476's
`vaco-filter-component`; see that issue for the correction);
`colorkey`/`colorhold`/`maskedclamp`/`maskedmax`/`maskedmin`/
`maskedthreshold`/`threshold` landed in this pass.

**Correction to a prior brief**: something told a previous agent that
`vaco-filter-framesync` should carry `maskedmerge`'s `masked*` siblings.
Measured, it does not: `ffmpeg -h filter=maskedclamp` (and
`maskedmax`/`maskedmin`/`maskedthreshold`/`threshold`) expose no
`eof_action`/`shortest`/`ts_sync_mode` section — the same test
`maskedmerge.rs` already used to justify a lockstep implementation
instead. All six went through `vaco-filter-core`'s new
[`Paired`](../../crates/filter/vaco-filter-core/src/adapt.rs) adapter,
the N-in-1-out strict-lockstep shape built for exactly this case, rather
than through framesync.

**Left for follow-up, stated honestly** (10 filters): `chromakey`/
`chromahold`, `hsvkey`/`hsvhold`, `lumakey`, `backgroundkey`, `despill`,
`premultiply_dynamic`, `maskfun`, `hysteresis`, `floodfill` — see
`vaco_filter_key`'s crate-level doc (`lib.rs`) for the specific probe
that stopped each one, not just "not attempted". Three are worth
repeating here because they turned out to be more than "measure and
implement": `chromakey`/`chromahold` need the reference's internal chroma
upsampling reproduced to be byte-exact (a uniform-colour `yuv420p` frame
produced two different alpha values on pixels sharing one subsampled
chroma sample); `lumakey`'s transparent band is measurably **not**
symmetric around its threshold, ruling out the simple band formula this
crate's other keying filters use; `maskfun` returned the same constant
for every uniform-input probe, meaning it is a neighbourhood/segmentation
operation, not a per-pixel threshold its option names suggest.

## What it is

Two families: alpha-compositing primitives (`premultiply`/
`unpremultiply`, `maskedmerge`, `colorkey`, `colorhold`) and multi-stream
per-pixel arithmetic pickers (`maskedclamp`, `maskedmax`/`maskedmin`,
`maskedthreshold`, `threshold`) — every one of the second group is exact
integer comparison/selection/clamping, no interpolation, and all four
were pinned down with a handful of hand-verifiable probes each.

## How it works

### `maskedmerge`: measured formula

`out = base + (overlay - base) * mask / maxval`, per selected plane
(`planes` bitmask, default all). Confirmed against the reference with a
concrete probe: base `0x64` (100), overlay `0xc8` (200), mask `0x80`
(128) produced `0x96` (150) — exactly `100 + 100*128/255`. Implemented as
a direct three-input `vaco_filter_core::Filter` (lockstep consumption of
one frame per pad; predates `Paired`, which the four new masked-family
filters below use instead of hand-rolling the same loop again).

### `colorkey`/`colorhold`: one measured distance/ramp, two outputs

[`keying::rgb_distance`](../../crates/filter/vaco-filter-key/src/keying.rs)
is `sqrt(sum((p_i - k_i)^2)) / sqrt(3)` over `[0,1]`-normalised RGB —
pinned down by sweeping `colorkey`'s `similarity` against pure red on a
black key and finding the opaque/transparent boundary sitting at exactly
`1/sqrt(3)` to five decimal places. `keying::ramp` is
`clamp((distance - similarity) / blend, 0, 1)`, `blend <= 0` treated as a
hard step; five interior points on a `blend=0.2` sweep matched this
exactly for `colorkey` (which writes the ramp straight to the alpha
channel). That write is an unconditional overwrite, not a multiply
against whatever alpha the pixel already carried — confirmed 2026-08-28
with a source that has non-trivial alpha before `colorkey` runs (an
opaque source cannot tell the two apart); see `src/colorkey.rs`'s doc for
the two-probe measurement.

`colorhold` reuses the identical ramp to blend a pixel toward its own
plain RGB mean (`mean(R,G,B)`, not luma-weighted — confirmed: red against
a black key produces exactly `85 = mean(255,0,0)`) rather than adding
transparency; the *matching* colour range is what stays untouched (a
near-red inside `similarity` of a `red` key reproduces the input
byte-for-byte). **Not byte-exact**: two of `colorhold`'s four interior
blend probes came out one ULP off this crate's `f64` computation of the
documented formula (`colorhold.rs`'s doc has both cases) — shipped as
measured, with the mismatch stated rather than rounded away to hide it.

### The masked-family pickers: each pinned down with 3–7 probes

- `maskedmax(source, f1, f2)` = whichever of `f1`/`f2` is **farther**
  from `source` by absolute difference (ties favour `f1`); `maskedmin`
  picks whichever is **nearer** (same tie-break). Confirmed with `source`
  above, below and between both inputs, and a genuine tie.
- `maskedclamp(base, dark, bright)` = `clamp(base, dark - undershoot,
  bright + overshoot)`. Confirmed in-range, below-range, above-range, and
  with non-zero `undershoot`/`overshoot`.
- `maskedthreshold(source, reference)`, `mode=abs` (the default): `source`
  if `|source - reference| <= threshold`, else `reference`. Confirmed at
  and either side of the boundary. `mode=diff` = `min(source,
  max(reference - threshold, 0))`, recovered 2026-08-28 by sweeping
  `source`'s full `0..=255` range at eight `(reference, threshold)` pairs
  rather than trusting the single ambiguous probe that used to leave this
  "not implemented" — the same sweep-not-sample technique that closed a
  nine-round MPEG-1 investigation and pinned `sobel`'s border rule.
  `mode` also needed the named-string fix (`mode=diff`/`mode=abs`, not
  just the bare integer) `pixelize`/`convolution` already hit for the
  same reason: a plain ranged `i32` option field never consults named
  constants during parsing unless it declares `unit`/`consts` — the
  fix used here is the same `String`-field-plus-hand-parse shape those
  two already used. `vaco-opts` itself does support this centrally via
  `#[derive(OptEnum)]` (confirmed against real consumers in
  `vaco-filter-mm::misc`); see `docs/filter/vaco-filter-geometry.md` for
  this campaign's own fix using it, and this crate's
  own doc history for the tree-wide named-constant survey this
  three-times-repeated bug prompted.
- `threshold(source, threshold, min, max)` = `max` if `source > threshold`
  else `min` (strict `>`, confirmed at the equal-value case landing on
  `min`).

**Multi-input, real end-to-end conformance, 2026-08-28**: this crate is
also this project's first multi-input `filter`-tool conformance target
(`vaco-conformance`'s `filterexec.rs`, which used to build exactly one
source node, now builds one per declared input pad — see that crate's own
doc for the design). All eight cases in
`tests/conformance/filter/vaco-filter-key-multi.toml` agree with real
`ffmpeg 8.1` byte-for-byte, covering every multi-input adapter shape the
crate uses — `maskedmerge` (a hand-rolled `Filter`, 3 pads),
`maskedmax`/`maskedmin`/`maskedclamp`/`maskedthreshold` (`Paired`, 3/3/3/2
pads), `threshold` (`Paired` with an `input_count` override to 4, the
load-bearing case for "the harness reads the filter's own declared
arity" rather than assuming one), and `premultiply` (`Synced` —
`vaco-filter-framesync`, the third and last adapter shape) — on
discriminating, non-flat sources throughout (a mask that ramps across
its own range rather than just the two saturated endpoints, a
`maskedthreshold` case that spans both the unchanged and flattened
regions of `mode=diff` in one pass), confirming the formulas measured
above by hand-probing are correct against a genuinely independent
execution path, not merely self-consistent with the probes that derived
them. `premultiply`'s case also settles a question this doc used to
carry as open: see `premultiply.rs`'s own "Settled, 2026-08-28" for why
`gray8` with no alpha channel turned out to be the *entire* rule, not a
corner of a richer one that a packed alpha format would have exercised
differently.

### `premultiply`/`unpremultiply`: what is measured and what is not

Measured: the reference genuinely instantiates an internal `framesync`
(confirmed via `-loglevel verbose`, which prints "Sync level N" lines)
even though no `eof_action`/`shortest`/`ts_sync_mode` option is exposed —
so this crate uses `vaco_filter_framesync::Synced` with
`FrameSyncOpts::default()`, matching the framework's precedent for that
shape. Also measured: a main input with **no alpha channel is an exact
pixel no-op**, for every alpha value tried on the second input.

**Not conclusively pinned down**: the doc string says "PreMultiply first
stream with first plane of second stream", but a `yuva420p` main input's
output bytes were ambiguous between "premultiplied by its own alpha" and
"premultiplied by the second stream's channel". This crate implements the
conservative, testable reading — multiply by the **main input's own**
alpha channel when it has one — and states this as a documented
simplification in `premultiply.rs`'s own doc, not a confirmed match.
`inplace=1`'s single-input shape (measured to exist) is parsed but not
wired to a dynamic pad count.

## How to change it

- A new keying filter from the row above: measure its `-h` output for a
  framesync surface first. None → follow `maskedclamp.rs`/
  `masked_pick.rs` (`vaco_filter_core::adapt::Paired`, the N-in-1-out
  lockstep shape) for a fixed multi-input filter, or `colorkey.rs` for a
  single-input one. A framesync surface (`hysteresis` has one; the
  `masked*` family does not) → follow `premultiply.rs`
  (`vaco_filter_framesync::FrameSyncFilter`).
- `chromakey`/`chromahold`/`hsvkey`/`hsvhold`/`lumakey`: start by finding
  the actual distance metric via more targeted probing than this pass had
  time for — `keying.rs`'s RGB metric is the wrong starting assumption
  for at least `lumakey` (see `lib.rs`'s scope note).
- Register in `vaco-component.toml`, run `cargo xtask gen-registry`, and
  add the name to `registry.rs`.

## Configuration

No crate-level configuration; each filter's options are its own
`vaco_opts::Options` struct.

## Dependencies

`vaco-core` (also `vaco_core::parse::color` for `colorkey`/`colorhold`'s
`color` option), `vaco-opts`, `vaco-frame`, `vaco-pixfmt`,
`vaco-filter-core` (also its `Paired` adapter), `vaco-filter-graph`,
`vaco-filter-framesync` (for `premultiply`/`unpremultiply`), `smallvec`
(the `Paired`-based filters' input collection).
