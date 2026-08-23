# vaco-filter-blur

T2 blur, sharpen and convolution video filters (FT-4.6a, GitHub issue #468,
epic `planning/16-filters.md` §8.4 "T2 blur/sharpen/convolve (~28)"):
`boxblur`, `avgblur`, `gblur`, `unsharp`, `convolution`,
`sobel`/`prewitt`/`roberts`/`kirsch`/`scharr`, `dilation`, `erosion`,
`median`, `maskedclamp` — fourteen filters.

## Scope reconciliation

The brief that requested this crate named a partial list ending in "…" and
warned explicitly not to trust it in either direction. Checked directly
against the shipped reference (`ffmpeg -hide_banner -filters` and
`ffmpeg -h filter=<name>`, `ffmpeg 8.1`, 2026-08-23) rather than recalled:

- **`kirsch`/`prewitt`/`roberts`/`scharr`/`sobel`** share one option class —
  `ffmpeg -h filter=sobel` prints `kirsch/prewitt/roberts/scharr/sobel
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
- **`maskedclamp` genuinely belongs to this group** despite reading, at a
  glance, like a member of the unrelated `masked*` family
  (`maskedmerge`/`maskedmin`/`maskedmax`/`maskedthreshold`) — it clamps a
  base stream's overshoot/undershoot against two other streams, which is
  exactly the crispening-limiter role `unsharp`-style sharpening pipelines
  use it for. Including it brings this crate to fourteen filters against
  the roadmap's own "~28" estimate for the whole family (see below for the
  other fourteen), which is close enough to be reassuring rather than a
  coincidence.
- The reference's own inventory for "blur/sharpen/convolution" is
  genuinely closer to 27: `avgblur`, `bilateral`, `boxblur`, `cas`,
  `convolution`, `convolve`, `dblur`, `deconvolve`, `dilation`, `erosion`,
  `gblur`, `guided`, `kirsch`, `maskedclamp`, `median`, `morpho`,
  `prewitt`, `roberts`, `sab`, `scharr`, `smartblur`, `sobel`, `tmedian`,
  `unsharp`, `varblur`, `xmedian`, `yaepblur`. Thirteen of those are left
  for a follow-up — see "Left for a follow-up" below.
- `hqdn3d`, `atadenoise`, `removegrain`, `nlmeans`, `owdenoise` (denoise,
  issue #469) were left alone, per the brief.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::BlurRegistry`](../../crates/filter/vaco-filter-blur/src/registry.rs)
— the same shape `vaco-filter-audio-eq`/`vaco-filter-video-geometry` use.
Three shared engines back several filters each:

- `src/common.rs` — 8-bit-only plane validation, the `planes` bitmask, frame
  metadata copying, clamp-to-edge sampling, and `box_pass` (the box average
  `boxblur`/`avgblur`/`unsharp` all build on).
- `src/convolution.rs` — the generic per-plane matrix engine (also the
  `convolution` filter itself), reused by `src/edge.rs` for the two-gradient
  `sobel`/`prewitt`/`scharr` engine.
- `src/morph.rs` — the shared dilation/erosion engine.

`maskedclamp` is the one three-input filter (`base`, `dark`, `bright`),
built on `vaco-filter-framesync`'s `FrameSyncFilter`/`Synced` with
`FsInput::uniform(3)` — the same role `vaco-filter-framesync`'s own docs
name `maskedmerge` as using.

## How it works

### Scope: 8-bit formats only

Every filter here rejects any pixel format wider than 8 bits per component
(`common::ensure_8bit_addressable`), the same deliberate gap
`vaco-filter-video-composite::geom::ensure_addressable_8bit` documents for
the same reason: generic sample-width math is a separate, larger effort
than this brief's time budget. The reference supports higher depths for
most of these filters; this is a recorded gap, not a silent one.

### Two measured, incompatible border conventions

The single most important finding this crate made, because it applies to
almost every filter in it: **there is no one border rule.**

- `boxblur`/`avgblur`/`unsharp`'s internal blur/`dilation`/`erosion`/`median`
  extend the border by **replicating the nearest real sample** (clamp-to-edge).
  Measured with a corner impulse against `boxblur=luma_radius=1:luma_power=1`:
  the corner pixel comes back `113`, not `28` (`255/9`, the zero-padded
  answer) — the corner's own 3x3 window sees four replicated copies of
  itself. See `src/common.rs`'s `box_pass` doc for the full arithmetic.
- `convolution` and, because they reuse its engine, `sobel`/`prewitt`/`scharr`
  instead force a **hard zero** at any pixel whose kernel would read outside
  the frame — not a computed value using replicated or zero-padded taps, an
  outright `0`. Measured the same way: a 3x3 Sobel-shaped kernel run through
  the generic `convolution` filter gives `0` at the border where the clamp
  model predicts `40`. See `src/convolution.rs`'s doc.
- `roberts` and `kirsch` were measured to match **neither** rule at their
  borders (see below) — a genuine open question, not a third convention
  this crate chose to invent.

A rounding convention split was also found and is easy to miss:
**`boxblur` rounds to nearest; `avgblur` truncates.** The same corner probe
gives `57` from `boxblur` and `56` from `avgblur` at the same position
(`510/9 = 56.67`) — see `src/avgblur.rs`'s doc.

### The matrix engine (`convolution.rs`)

`convolution`'s `<n>rdiv=0` is a sentinel for "normalise by the matrix's own
coefficient sum", never a literal zero divisor (measured: an all-ones 3x3
kernel with `rdiv=0` on a constant field returns the field unchanged).
`bias` is added *after* the `rdiv` division, both before the final clip.
`sobel`/`prewitt` reuse this engine directly with fixed 3x3 kernels;
`scharr` needs `rdiv=16` folded in (measured: the textbook unnormalised
Scharr response of `320` on a test ramp comes back as the reference's `20`,
exactly `320/16`).

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

### `gblur`: implemented, not framecrc-matched

An impulse-response probe (`gblur=sigma=3:steps=1`) shows a peak of `59`
with roughly geometric falloff (ratio approaching ~1.6 between consecutive
taps) — the signature of a low-order recursive (IIR) filter, almost
certainly the published Young/van Vliet or Deriche recursive Gaussian
approximation that `steps` (repeated refinement passes) implies, not a
plain truncated FIR kernel. A directly normalised `sigma=3` FIR kernel's
peak would be `~34` with a bell-curve falloff, not `59` with a near-constant
ratio — refuted, not merely different.

Matching that specific IIR construction bit-exactly was out of this
brief's time budget. `gblur` here is a real, separable, truncated Gaussian
FIR convolution — verified against the properties any Gaussian kernel must
have (normalises to `1`; blurring a constant field is the identity) — but
it is **not** framecrc-equal to the reference. `steps` is parsed but does
not change behaviour.

### `maskedclamp`: a pure per-pixel formula, no border question

`out = clamp(base, min(dark,bright) - undershoot, max(dark,bright) +
overshoot)`, read directly off the option table — no neighbourhood, so none
of the border conventions above apply. Not separately measured against the
reference (this crate's probing time went to the filters where the border
rule cannot be derived from the option table alone).

## What is verified versus structural

| Confidence | Filters |
|---|---|
| Framecrc-level (interior, against small generated inputs run through the reference directly) | `boxblur`, `avgblur`, `convolution`, `sobel`, `prewitt`, `scharr`, `dilation`, `erosion`, `median` |
| Interior verified, border a documented gap | `unsharp` (analytic ramp invariant, one measured off-by-one at the edge), `roberts`, `kirsch` (border unverified against any tried model) |
| Structural only | `gblur` (not the reference's algorithm — see above), `maskedclamp` (formula from the option table, not separately measured) |

Independent oracles used per kernel (never the implementation re-run
against itself, per `AGENT-CONSTRAINTS.md`): a DC/constant-field fixed
point for every blur and edge operator; a direct O(1) analytic invariant
for `unsharp` (a box average of a linear ramp equals the ramp's own centre
value); order-statistic bounds (`min <= output <= max` for any percentile)
for `median`; the erosion/dilation duality `erode(x) = 255 - dilate(255-x)`
under inversion; Gaussian-kernel normalisation for `gblur`; and, for every
filter, the reference binary's own raw pixel output on a small generated
`lavfi`/`geq` input, pinned into a regression test.

## Left for a follow-up

Thirteen more filters this project's own roadmap counts in the same
family: `smartblur`, `bilateral`, `guided`, `sab`, `dblur`, `varblur`,
`yaepblur`, `cas`, `tmedian`, `xmedian`, `morpho`, `convolve`,
`deconvolve`. `convolve`/`deconvolve` need an FFT matched bit-exactly to
the reference to be worth shipping at all; the rest are each a genuinely
different per-pixel algorithm (adaptive radius, bilateral range weighting,
a structuring-element second input) rather than a variation on what this
crate already built. None of them block the fourteen filters that landed.

## How to change it

- A new fixed-kernel edge operator: add a module following `sobel.rs`
  (three lines: `DESC` via `edge::pad_desc`, `create` via
  `edge::create_two_gradient`) if it fits the two-gradient shape, or
  `roberts.rs`/`kirsch.rs`'s pattern otherwise.
- Changing `box_pass`'s algorithm affects `boxblur`, `avgblur` and
  `unsharp` at once — check all three modules' pinned regression tests
  before changing rounding or border behaviour.
- Gotcha: `common::plane_selected`'s bitmask, `common::to_i32`'s saturating
  cast, and `common::sample_clamped`'s clamp-to-edge are used everywhere in
  this crate; a change to any of them is a change to every filter.

## Configuration

No environment variables, flags or config files. Every option is the
reference's own CLI filter-argument surface (`vaco-opts` `#[derive(Options)]`
structs per module), documented in each module's own doc comment against
`ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` (all internal, layer 5), and `vaco-filter-framesync`
(for `maskedclamp`'s three-input synchronisation). No external crates.
