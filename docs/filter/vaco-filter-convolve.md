# vaco-filter-convolve

T2/T3 convolution and morphology video filters, split out of
`vaco-filter-blur` (GitHub issue #468/FT-4.6a) after the orchestrator
corrected the crate boundary against `planning/16-filters.md` §4.2's
authoritative table. Nine implemented: `convolution`,
`sobel`/`prewitt`/`roberts`/`kirsch`/`scharr`, `dilation`, `erosion`,
`median`.

## Scope reconciliation

`planning/16-filters.md` §4.2 assigns this crate eighteen names:
`convolution, morpho, erosion, dilation, inflate, deflate, median, sobel,
prewitt, roberts, scharr, kirsch, edgedetect, blurdetect, convolve,
deconvolve, corr, xcorrelate`. Nine are implemented here; the rest are
listed under "Left for a follow-up" below.

Checked directly against the shipped reference (`ffmpeg -hide_banner
-filters` and `ffmpeg -h filter=<name>`, `ffmpeg 8.1`, 2026-08-23) rather
than recalled:

- **`kirsch`/`prewitt`/`roberts`/`scharr`/`sobel`** share one option class
  — `ffmpeg -h filter=sobel` prints `kirsch/prewitt/roberts/scharr/sobel
  AVOptions:` — confirming they are one file/engine in the reference, the
  same way `vaco-filter-audio-eq` found for the biquad family.
- **`convolution` is a separate engine** from the sobel family (its own
  `convolution AVOptions:` header, a different option shape — per-plane
  matrices rather than `planes`/`scale`/`delta`), but its measured
  zero-border rule is identical to `sobel`/`prewitt`/`scharr`'s, which is
  strong evidence the fast-path border handling is shared even though the
  option class is not.
- **`erosion`/`dilation`** likewise share one option class
  (`erosion/dilation AVOptions:`).

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ConvolveRegistry`](../../crates/filter/vaco-filter-convolve/src/registry.rs)
— the same shape `vaco-filter-blur`/`vaco-filter-video-geometry` use. Three
shared engines back several filters each:

- `src/common.rs` — 8-bit-only plane validation, the `planes` bitmask,
  frame metadata copying, and clamp-to-edge sampling. A deliberate fork of
  `vaco-filter-blur::common`'s non-`box_pass` half from when both filter
  families shared one crate — see that module's doc for why it is not a
  shared dependency between the two sibling crates.
- `src/convolution.rs` — the generic per-plane matrix engine (also the
  `convolution` filter itself), reused by `src/edge.rs` for the
  two-gradient `sobel`/`prewitt`/`scharr` engine.
- `src/morph.rs` — the shared dilation/erosion engine.

## How it works

### Scope: 8-bit formats only

Every filter here rejects any pixel format wider than 8 bits per component
(`common::ensure_8bit_addressable`), the same deliberate gap
`vaco-filter-video-composite::geom::ensure_addressable_8bit` documents for
the same reason: generic sample-width math is a separate, larger effort
than this brief's time budget.

### Two measured, incompatible border conventions

The single most important finding this crate made, because it applies to
almost every filter in it: **there is no one border rule.**

- `dilation`/`erosion`/`median` extend the border by **replicating the
  nearest real sample** (clamp-to-edge). For `dilation`/`erosion`
  specifically this is mathematically equivalent to "omit the missing
  neighbour": a max/min combine cannot change when a candidate value is
  duplicated, and self is always a candidate (confirmed: the centre pixel
  of an isolated impulse never disappears even though its own neighbours
  start at zero).
- `convolution` and, because they reuse its engine, `sobel`/`prewitt`/`scharr`
  instead force a **hard zero** at any pixel whose kernel would read outside
  the frame — not a computed value using replicated or zero-padded taps, an
  outright `0`. Measured: a 3x3 Sobel-shaped kernel run through the generic
  `convolution` filter gives `0` at the border where the clamp model
  predicts `40`. See `src/convolution.rs`'s doc.
- `roberts` and `kirsch` were measured to match **neither** rule at their
  borders — a genuine open question, not a third convention this crate
  chose to invent.

### The matrix engine (`convolution.rs`)

`<n>rdiv=0` is a sentinel for "normalise by the matrix's own coefficient
sum", never a literal zero divisor (measured: an all-ones 3x3 kernel with
`rdiv=0` on a constant field returns the field unchanged). `bias` is added
*after* the `rdiv` division, both before the final clip. `sobel`/`prewitt`
reuse this engine directly with fixed 3x3 kernels; `scharr` needs
`rdiv=16` folded in (measured: the textbook unnormalised Scharr response
of `320` on a test ramp comes back as the reference's `20`, exactly
`320/16`).

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
`convolution`'s "force zero" rule under the confirmed interior formula —
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

## What is verified versus structural

| Confidence | Filters |
|---|---|
| Framecrc-level (interior, against small generated inputs run through the reference directly) | `convolution`, `sobel`, `prewitt`, `scharr`, `dilation`, `erosion`, `median` |
| Interior verified, border a documented gap | `roberts`, `kirsch` (border unverified against any tried model) |

Independent oracles used per kernel (never the implementation re-run
against itself, per `AGENT-CONSTRAINTS.md`): a DC/constant-field fixed
point for every edge operator; order-statistic bounds (`min <= output <=
max` for any percentile) for `median`; the erosion/dilation duality
`erode(x) = 255 - dilate(255-x)` under inversion; and, for every filter,
the reference binary's own raw pixel output on a small generated
`lavfi`/`geq` input, pinned into a regression test.

## Left for a follow-up

Nine more filters `planning/16-filters.md` §4.2 counts in this crate:
`morpho` (a generalised erode/dilate/open/close/gradient/tophat/blackhat
filter taking a second video stream as its structuring element — needs
`vaco-filter-framesync`, not yet a dependency here), `inflate`/`deflate`
(not reached, but almost certainly a thin variant of the confirmed
`morph.rs` engine), `edgedetect`, `blurdetect` (not reached), `convolve`/
`deconvolve` (frequency-domain, two-video-stream convolution — needs an
FFT matched bit-exactly to the reference to be worth shipping at all),
`corr`, `xcorrelate` (two-stream correlation measures, not reached). None
of them block the nine filters that landed.

## How to change it

- A new fixed-kernel edge operator: add a module following `sobel.rs`
  (three lines: `DESC` via `edge::pad_desc`, `create` via
  `edge::create_two_gradient`) if it fits the two-gradient shape, or
  `roberts.rs`/`kirsch.rs`'s pattern otherwise.
- `inflate`/`deflate` are the most likely next filters here: they almost
  certainly reuse `morph.rs`'s engine with fixed
  `coordinates`/`threshold`, the same way `dilation.rs`/`erosion.rs` do —
  confirm the exact fixed parameters against the reference before wiring
  them up.
- Gotcha: `common::plane_selected`'s bitmask, `common::to_i32`'s saturating
  cast, and `common::sample_clamped`'s clamp-to-edge are used throughout
  this crate; a change to any of them is a change to every filter that
  uses clamp-to-edge (not `convolution`/`sobel`/`prewitt`/`scharr`, which
  use `convolution::Kernel`'s separate zero-border logic instead).

## Configuration

No environment variables, flags or config files. Every option is the
reference's own CLI filter-argument surface (`vaco-opts` `#[derive(Options)]`
structs per module), documented in each module's own doc comment against
`ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` (all internal, layer 5). No external crates, and no
`vaco-filter-framesync` dependency yet — `morpho`, the one filter here
that would need it, is left for a follow-up.
