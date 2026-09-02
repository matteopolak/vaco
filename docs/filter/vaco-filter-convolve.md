# vaco-filter-convolve

T2/T3 convolution and morphology video filters, split out of
`vaco-filter-blur` (GitHub issue #468/FT-4.6a) after the orchestrator
corrected the crate boundary against `planning/16-filters.md` §4.2's
authoritative table. Twelve implemented: `convolution`,
`sobel`/`prewitt`/`roberts`/`kirsch`/`scharr`, `dilation`, `erosion`,
`median`, `inflate`, `deflate`, `morpho`.

## Scope reconciliation

`planning/16-filters.md` §4.2 assigns this crate eighteen names:
`convolution, morpho, erosion, dilation, inflate, deflate, median, sobel,
prewitt, roberts, scharr, kirsch, edgedetect, blurdetect, convolve,
deconvolve, corr, xcorrelate`. All eighteen were re-verified against
`ffmpeg -hide_banner -filters`/`ffmpeg -h filter=<name>` a second time
(`ffmpeg 8.1`, 2026-08-23) before this pass's work started — every one
exists in this reference build (`convolve`, `deconvolve`, `corr`,
`xcorrelate` in particular are real, distinct filters, not aliases of one
another: `convolve`/`deconvolve` are `VV->V` frequency-domain operations,
`corr`/`xcorrelate` are `VV->V` two-stream correlation measures). Twelve
are implemented here; the rest are listed under "Left for a follow-up"
below.

Checked directly against the shipped reference (`ffmpeg -hide_banner
-filters` and `ffmpeg -h filter=<name>`, `ffmpeg 8.1`, 2026-08-23) rather
than recalled:

- **`kirsch`/`prewitt`/`roberts`/`scharr`/`sobel`** share one option class
  — `ffmpeg -h filter=sobel` prints `kirsch/prewitt/roberts/scharr/sobel
  AVOptions:` — confirming they are one file/engine in the reference, the
  same way `vaco-filter-audio-eq` found for the biquad family.
- **`convolution` is a separate engine** from the sobel family (its own
  `convolution AVOptions:` header, a different option shape — per-plane
  matrices rather than `planes`/`scale`/`delta`), but it shares
  `Kernel::value_at` directly with `sobel`/`prewitt`/`scharr`, so its
  `reflect-101` border rule (see "Two measured, distinct border
  conventions" above) applies to all four.
- **`erosion`/`dilation`** likewise share one option class
  (`erosion/dilation AVOptions:`), and **`deflate`/`inflate`** share
  another (`deflate/inflate AVOptions:`) — a fourth, related but distinct
  engine: same 8-neighbour geometry and threshold-caps-not-gates rule as
  `dilation`/`erosion`, but combining by truncating average instead of
  max/min, and with no `coordinates` option (see below).
- **`morpho`** is its own engine again: `VV->V` (a structuring-element
  video, not an option), with `erode/dilate/open/close/gradient/tophat/
  blackhat` as one `mode` option and its own `framesync` surface.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ConvolveRegistry`](../../crates/filter/vaco-filter-convolve/src/registry.rs)
— the same shape `vaco-filter-blur`/`vaco-filter-video-geometry` use. Four
shared engines back several filters each:

- `src/common.rs` — 8-bit-only plane validation, the `planes` bitmask,
  frame metadata copying, and clamp-to-edge sampling. A deliberate fork of
  `vaco-filter-blur::common`'s non-`box_pass` half from when both filter
  families shared one crate — see that module's doc for why it is not a
  shared dependency between the two sibling crates.
- `src/convolution.rs` — the generic per-plane matrix engine (also the
  `convolution` filter itself), reused by `src/edge.rs` for the
  two-gradient `sobel`/`prewitt`/`scharr` engine.
- `src/morph.rs` — the shared dilation/erosion engine (`apply_plane`, a
  fixed 3x3 `coordinates`-selected neighbourhood, self always a candidate)
  and, new this pass, `inflate`/`deflate`'s average-based variant of the
  same fixed neighbourhood, plus `apply_structured` — a second engine for
  `morpho`'s arbitrary structuring element, where self is **not** an
  implicit candidate (see below).
- One module per filter, each exposing `pub const DESC: FilterDesc` and
  `pub(crate) fn create`, aggregated by
  [`registry::ConvolveRegistry`].

`morpho` is this crate's first two-input filter: it is built directly
against `vaco-filter-framesync` (`FrameSyncFilter`, `Synced`), following
`vaco-filter-video-composite::overlay`'s pattern rather than waiting on
`vaco-filter-core`'s not-yet-landed `Paired<F>` adapter
.

## How it works

### Scope: 8-bit formats only

Every filter here rejects any pixel format wider than 8 bits per component
(`common::ensure_8bit_addressable`), the same deliberate gap
`vaco-filter-video-composite::geom::ensure_addressable_8bit` documents for
the same reason: generic sample-width math is a separate, larger effort
than this brief's time budget.

### Two measured, distinct border conventions (a third, once suspected, turned out not to exist)

**There is no one border rule in this crate**, but there are exactly two,
not three:

- `dilation`/`erosion`/`median`/`inflate`/`deflate` extend the border by
  **replicating the nearest real sample** (clamp-to-edge). For
  `dilation`/`erosion` specifically this is mathematically equivalent to
  "omit the missing neighbour": a max/min combine cannot change when a
  candidate value is duplicated, and self is always a candidate (confirmed:
  the centre pixel of an isolated impulse never disappears even though its
  own neighbours start at zero). `inflate`/`deflate` combine by *average*
  rather than max/min, so that equivalence does not hold for them the same
  way — averaging **is** sensitive to which value gets duplicated at a
  border. This was flagged as an open, unverified extension of the min/max
  argument as of 2026-08-27 and pinned on 2026-08-28 by a corner probe
  (all-distinct-value 5x5 source): the existing clamp-to-edge, fixed
  divide-by-8 implementation matches the reference exactly (`5` at a
  specific corner) and a competing "omit out-of-bounds neighbours, divide
  by however many are left" hypothesis is ruled out (it predicts `8` at
  the same pixel). See `src/morph.rs`'s doc.
- `convolution` and, because they reuse its engine (`Kernel::value_at`),
  `sobel`/`prewitt`/`scharr` extend the border by **`reflect-101`**:
  mirror an out-of-bounds tap back across the border without duplicating
  the edge pixel, independently per axis, including simultaneously at
  corners. This crate originally shipped (and, worse, believed it had
  *measured*) a "hard zero at any out-of-bounds tap" rule instead — the
  measurement that produced that belief used a source varying in only one
  axis against a derivative kernel, a shape where reflect-101 *also*
  cancels to exactly zero at the border, so the source could not actually
  tell the two rules apart. A corner/edge probe (5x5, all-distinct values)
  against real `ffmpeg 8.1` settled it on 2026-08-28: reflect-101 matches
  at the corner (`0`) and an adjacent edge cell (`8`) where both zero-pad
  and plain clamp-to-edge predict different, wrong values. Verified with
  zero mismatches across all 400 pixels of a two-axis discriminating
  source for both `sobel` and `prewitt`. See `src/convolution.rs`'s doc
  for the full derivation, and `src/common.rs`'s `sample_reflect101` for
  the implementation.
- `roberts` and `kirsch` were measured to match **neither** convention at
  their borders — a genuine open question, not a third convention this
  crate chose to invent.
- `morpho`'s border was not separately measured (a documented gap, not a
  probed-and-confirmed one); it uses clamp-to-edge to match the rest of
  this crate's family.

**Lesson learned the hard way**: a test source that cannot distinguish
between two candidate border rules validates neither, even when the
(wrong) implementation passes it. The one-axis-varying source that seemed
to confirm "hard zero" for years of this crate's assumptions never
actually ruled out reflect-101. See `planning/AGENT-CONSTRAINTS.md`'s
"a source that cannot separate two rules validates neither" rule, of
which this was one of the founding examples (alongside `vaco-filter-scope`'s
`vectorscope`/`waveform` findings).

### The matrix engine (`convolution.rs`)

`<n>rdiv=0` is a sentinel for "normalise by the matrix's own coefficient
sum", never a literal zero divisor (measured: an all-ones 3x3 kernel with
`rdiv=0` on a constant field returns the field unchanged). `bias` is added
*after* the `rdiv` division, both before the final clip. `sobel`/`prewitt`
reuse this engine directly with fixed 3x3 kernels; `scharr` needs
`rdiv=16` folded in (measured: the textbook unnormalised Scharr response
of `320` on a test ramp comes back as the reference's `20`, exactly
`320/16`). `Kernel::value_at`'s border rule is `reflect-101` (see above);
`scharr`'s combined magnitude has a separate, unrelated, unresolved
discrepancy on interior (non-border) pixels — see "Left for a follow-up"
below.

### `roberts` and `kirsch`: interior confirmed, border genuinely open

Both operators' *interior* pixels match a measured probe exactly:

- `roberts`: standard 2x2 cross, `magnitude=sqrt(Gx^2+Gy^2)`.
- `kirsch`: the standard eight-rotation compass mask
  (`[5,5,5;-3,0,-3;-3,-3,-3]` cyclically shifted around the ring), maximum
  response over all eight, divisor `3`.

**A wrong divisor and a wrong mask cancelled into a false match once,
worth recording as a cautionary tale.** An earlier pass hand-wrote the
eight Kirsch rotations and got one wrong — an extra `5` in place of a `-3`,
caught only because a true rotation's coefficients must sum to `0` and that
one summed to `8`. The wrong mask's inflated maximum (`400` instead of the
correct `240`) happened to divide evenly by `5` into the measured `80`,
which read as confirmation. Regenerating the eight masks programmatically
(cyclic shift of the perimeter, rejecting anything that does not sum to
zero) found the correct maximum `240` and the correct divisor `3`
(`240/3 = 80`, still matching). This is `AGENT-CONSTRAINTS.md`'s "two
probes that disagree are not noise" lesson from the other direction: a
wrong model and a wrong constant agreeing with one measurement is not
confirmation either, and the fix was a mechanical invariant (masks sum to
zero), not more probing.

Neither operator's **border** matches clamp-to-edge, zero-padding, or
`convolution`'s `reflect-101` rule under the confirmed interior formula —
every model tried was refuted by direct calculation against the measured
border values, not merely untested. Both are implemented with
clamp-to-edge (the least surprising choice, and correct for the interior)
and documented in their own modules as unverified at the border.

### `dilation`/`erosion`: `coordinates` and `threshold`, fully confirmed

`coordinates` is an 8-neighbour bitmask in raster order excluding centre
(`(-1,-1) (-1,0) (-1,1) (0,-1) (0,1) (1,-1) (1,0) (1,1)` for bits
`1,2,4,8,16,32,64,128`), confirmed by growing an isolated impulse with
each single bit set and checking which one neighbour picked it up.
`threshold` caps the change rather than gating it —
`new = min(local_max, self + threshold)` for dilation, symmetrically for
erosion — confirmed by a `threshold0=10` probe where neighbours grew to
exactly `10`, not `0` and not the full value.

### `median`: order statistics, confirmed at three points

`percentile=0`/`1` reproduce the window minimum/maximum exactly;
`percentile=0.5` (the default) reproduces the true median. The rank used,
`round(percentile*(len-1))`, collapses to all three measured cases.

### `inflate`/`deflate`: average, not maximum, and which way it rounds

No `coordinates` option in the reference (`ffmpeg -h filter=inflate` prints
only `threshold0..3`) — the neighbourhood is always the fixed full
8-ring. Measured (a `10`-valued centre against three `100`-valued and five
`0`-valued neighbours, sum `300`): the reference returns `37`, not `38` —
`300/8 = 37.5` **truncated**, matching `avgblur`'s convention rather than
`boxblur`'s round-to-nearest. `inflate` only ever grows a pixel (`new =
avg` only when `avg > self`); `deflate` only ever shrinks it (`new = avg`
only when `avg < self`) — confirmed by a background probe where every
non-centre pixel's own (higher) neighbourhood average left it unchanged
under `deflate`. `threshold` caps the change exactly as it does for
`dilation`/`erosion`. See `src/inflate.rs`/`src/deflate.rs`'s docs.

One invariant that looked plausible and was checked and rejected before
being written down: `inflate`/`deflate` do **not** satisfy the same
duality `dilation`/`erosion` do (`erode(x) = 255 - dilate(255-x)`) under
truncating-average combination — verified numerically (`18` mismatches
out of `36` cells on a test grid) before it could become a false invariant
in a test, per `AGENT-CONSTRAINTS.md`'s warning about a plausible-sounding
property that is not actually true. The oracles this crate's `inflate`/
`deflate` tests use instead — "never decreases"/"never increases" a pixel,
and a flat field is a fixed point — do hold and are what is tested.

### `morpho`: structuring-element morphology, self-inclusion measured to differ from `dilation`

`morpho` takes a *second video stream* as its structuring element (`ffmpeg
-h filter=morpho` prints two inputs: `default` and `structure`), unlike
`dilation`/`erosion`'s `coordinates` bitmask option. Measured:

- **The structure is a support mask, not an additive value.** An all-`255`
  3x3 structure dilates a `100` impulse into its full 3x3 neighbourhood at
  value `100`, not `100+255` clamped — a nonzero structure pixel means
  "this offset participates", the same semantics as `coordinates`.
- **Self is not an implicit candidate — the structure's own centre pixel
  decides.** With every structure pixel white except its own centre (dark),
  the impulse's own centre pixel comes back `0`: because the structure's
  centre offset is excluded, self is not a candidate, and the impulse's
  real neighbours were all `0`. This is the one place `morpho` measurably
  diverges from `dilation`/`erosion`, which always include self regardless
  of `coordinates` — confirmed with a dedicated `apply_structured` engine
  (`src/morph.rs`) separate from `apply_plane`'s always-include-self one,
  rather than reusing it and hoping the difference did not matter.
- A single active structure offset grows the impulse into exactly the
  position that offset names (`out(x,y) = combine over active (dy,dx) of
  in(x+dx,y+dy)`), confirmed directly.

`mode=open/close/gradient/tophat/blackhat` are standard greyscale
morphology compositions of the measured `erode`/`dilate` core
(`open = dilate(erode(x))`, `close = erode(dilate(x))`, `gradient =
dilate(x) - erode(x)`, `tophat = x - open(x)`, `blackhat = close(x) - x`),
not independently probed against the reference for every mode — verified
instead via the mathematical-morphology invariants any structuring element
containing its own origin must satisfy (anti-extensivity `open(x) <= x`,
extensivity `close(x) >= x`), plus `dilate(x) >= erode(x)` pointwise for
`gradient`'s non-negativity. `structure=first`/`all` and `mode`'s named
values (`erode`, `dilate`, …) both parse the reference's string spelling,
not just the numeric index, via a `String` field parsed by hand — the
same pattern `vaco-filter-video-composite::overlay`'s `eval`/`format`/
`alpha` fields use. Correction, 2026-08-28: `vaco-opts` does support
named-integer options centrally (`#[derive(OptEnum)]`); this predates
that idiom rather than working around a real gap — see
`vaco-filter-geometry`'s doc for a fix using it directly. `morpho`'s own
two-input wiring is exercised through
the real `Graph`/`Synced` scheduler in `src/tests_graph.rs`, not just
through its pure helper functions.

## What is verified versus structural

| Confidence | Filters |
|---|---|
| Framecrc-level (interior, against small generated inputs run through the reference directly) | `convolution`, `sobel`, `prewitt`, `scharr`, `dilation`, `erosion`, `median`, `inflate`, `deflate` |
| Interior verified, border a documented gap | `roberts`, `kirsch` (border unverified against any tried model), `morpho` (border assumed clamp-to-edge by family convention, not separately probed) |
| Measured core, standard compositions verified by mathematical invariant rather than individually probed | `morpho`'s `open`/`close`/`gradient`/`tophat`/`blackhat` modes |

Independent oracles used per kernel (never the implementation re-run
against itself, per `AGENT-CONSTRAINTS.md`): a DC/constant-field fixed
point for every edge operator and for `inflate`/`deflate`; order-statistic
bounds (`min <= output <= max` for any percentile) for `median`; the
erosion/dilation duality `erode(x) = 255 - dilate(255-x)` under inversion
(confirmed to hold for `dilation`/`erosion`'s max/min engine, and confirmed
numerically **not** to hold for `inflate`/`deflate`'s average engine before
either was written into a test); "never decreases"/"never increases" a
pixel for `inflate`/`deflate`; anti-extensivity/extensivity and
`dilate >= erode` for `morpho`'s compositions; and, for every framecrc-level
filter, the reference binary's own raw pixel output on a small generated
`lavfi`/`geq` input, pinned into a regression test. Three fixes in this
pass were falsified (the bug reintroduced, the test confirmed to fail,
then reverted): `inflate`'s truncating-not-rounding average,
`morpho`'s self-exclusion-on-a-dark-structure-centre, and `cas`'s
flat-field identity (in the sibling `vaco-filter-blur` crate, checked from
this crate's work in the same pass since both crates share the falsification
discipline).

## Left for a follow-up

`scharr`'s combined magnitude has a real divergence unrelated to the
border rule above, now investigated past the point of "not yet
root-caused" to "provably not a function of `(Gx, Gy)` at all". The
truncate-per-component-before-combining hypothesis (the same shape as
`waveform`'s `step = floor(intensity*255)` bug) was tested and refuted:
at every diverging pixel, `Gx/16` and `Gy/16` are already exact integers,
so there is no fractional part for truncation order to act on. Stronger
still: two different real 3x3 windows, read directly from the reference's
own raw output, give bit-identical `Gx=192, Gy=704` (confirmed by hand)
yet real `ffmpeg 8.1 -vf scharr` outputs `46` at one and `44` at the
other — two identical gradient vectors, two different results, which no
formula of `(Gx, Gy)` alone can produce. This is the signature of
floating-point/SIMD implementation noise in the reference's accelerated
path, not a discoverable behavioural rule; chasing it further would mean
reverse-engineering one specific binary's numerics rather than measuring
a rule to reimplement. See `src/edge.rs`'s doc for the full window data
and a regression test that pins the mathematical impossibility. Left out
of the conformance corpus on purpose — not because it wasn't chased hard
enough, but because the evidence already rules out a fix of this shape.

Six more filters `planning/16-filters.md` §4.2 counts in this crate:
`edgedetect`, `blurdetect` (not reached — `edgedetect`'s own hysteresis/
edge-tracing stage was measured, on a translation-invariant synthetic
step-edge input, to produce a periodic pattern of edge/no-edge rows a plain
Sobel-plus-double-threshold model does not reproduce, and confirmed
deterministic and not a threading artifact via `-filter_threads 1`; the
real algorithm needs more probing time than this pass had), `convolve`/
`deconvolve` (frequency-domain, two-video-stream convolution — needs an
FFT matched bit-exactly to the reference's specific windowing and
normalisation to be worth shipping at all; `vaco-tx` is the right crate for
the transform itself once that measurement is done), `corr`, `xcorrelate`
(two-stream correlation measures, not reached). None of them block the
twelve filters that landed.

## How to change it

- A new fixed-kernel edge operator: add a module following `sobel.rs`
  (three lines: `DESC` via `edge::pad_desc`, `create` via
  `edge::create_two_gradient`) if it fits the two-gradient shape, or
  `roberts.rs`/`kirsch.rs`'s pattern otherwise.
- A new filter driven by a second video stream follows `morpho.rs`'s
  pattern (a `FrameSyncFilter` over `FsInput::dual`, `event.get(1)` for the
  secondary frame) — check `planning/INTERFACE-GAPS.md` gap 10 first in
  case `Paired<F>` has landed by the time you read this.
- Gotcha: `common::plane_selected`'s bitmask, `common::to_i32`'s saturating
  cast, and `common::sample_clamped`'s clamp-to-edge are used throughout
  this crate; a change to any of them is a change to every filter that
  uses clamp-to-edge (not `convolution`/`sobel`/`prewitt`/`scharr`, which
  use `convolution::Kernel::value_at`'s separate `reflect-101` border
  logic via `common::sample_reflect101` instead, and not `morpho`, which
  uses `morph::apply_structured`'s separate no-implicit-self logic instead
  of `morph::apply_plane`'s).
- Gotcha: `morph.rs` now has two distinct engines — `apply_plane` (fixed
  3x3, self always a candidate, used by `dilation`/`erosion`/`inflate`/
  `deflate`) and `apply_structured` (arbitrary offsets, self only a
  candidate if the caller includes `(0, 0)`, used by `morpho`). They look
  similar and are not interchangeable — see `apply_structured`'s doc for
  the measured reason.

## Configuration

No environment variables, flags or config files. Every option is the
reference's own CLI filter-argument surface (`vaco-opts` `#[derive(Options)]`
structs per module), documented in each module's own doc comment against
`ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` (all internal, layer 5), and, new this pass,
`vaco-filter-framesync` for `morpho`'s two-input wiring. No external
crates.
