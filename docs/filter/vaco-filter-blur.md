# vaco-filter-blur

T2 blur and sharpen video filters (FT-4.6a, GitHub issue #468). Nine
implemented: `boxblur`, `avgblur`, `gblur`, `unsharp`, `cas`, `dblur`,
`guided` (self-guided mode only), `varblur`, `yaepblur`. Two left for a
follow-up: `sab`, `smartblur`.

## Scope correction

The brief that requested this crate named a partial list ending in "…" and
explicitly warned not to trust it in either direction. The first pass at
this crate checked the reference binary directly (`ffmpeg -hide_banner
-filters`, `ffmpeg -h filter=<name>`, `ffmpeg 8.1`, 2026-08-23) and
implemented fourteen filters, including the convolution/edge/morphology
family (`convolution`, `sobel`, `prewitt`, `roberts`, `scharr`, `kirsch`,
`dilation`, `erosion`, `median`) and `maskedclamp`.

That was the wrong crate boundary. The orchestrator caught it against the
authoritative source — not the reference binary this time, but this
project's own crate-decomposition plan, `planning/16-filters.md` §4.2,
which has a named row per crate:

```text
vaco-filter-blur    | unsharp, cas, avgblur, gblur, dblur, varblur, yaepblur,
                       guided, boxblur, smartblur, sab
vaco-filter-convolve | convolution, morpho, erosion, dilation, inflate,
                        deflate, median, sobel, prewitt, roberts, scharr,
                        kirsch, edgedetect, blurdetect, convolve,
                        deconvolve, corr, xcorrelate
vaco-filter-key      | ... maskedmerge, maskedclamp, maskedmax, maskedmin, ...
```

The nine convolution/morphology filters moved to a new crate,
`vaco-filter-convolve` (see its own doc) — the code and its tests were
already written and measured, so nothing was discarded, only refiled.
`maskedclamp` belongs to a third crate, `vaco-filter-key`, not assigned to
either agent working these two crates, and was dropped from both.

This crate's own eleven-name list is `unsharp, cas, avgblur, gblur, dblur,
varblur, yaepblur, guided, boxblur, smartblur, sab`. All eleven names were
re-verified against `ffmpeg -hide_banner -filters` and `ffmpeg -h
filter=<name>` a second time (2026-08-23, `ffmpeg 8.1`) before this pass's
work started — every name the plan lists exists in this reference build,
and none of the option tables the plan implies were missing. Nine are now
implemented; see "Left for a follow-up" below for the other two and why.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::BlurRegistry`](../../crates/filter/vaco-filter-blur/src/registry.rs)
— the same shape `vaco-filter-convolve`/`vaco-filter-audio-eq` use.
`src/common.rs` holds the shared 8-bit plane helpers every filter here
builds on, in particular `box_pass` — the clamp-bordered box average
`boxblur`, `avgblur` and `unsharp`'s internal blur all share — and, new
this pass, `sample_bilinear`, the off-grid sampling `dblur`'s rotated line
needs and no other filter here does.

`varblur` is this crate's first two-input filter: it is built directly
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
than this brief's time budget. The reference supports higher depths for
most of these filters; this is a recorded gap, not a silent one.

### The measured border and rounding rules

`boxblur`/`avgblur`/`unsharp`'s internal blur extend the border by
**replicating the nearest real sample** (clamp-to-edge). Measured with a
corner impulse against `boxblur=luma_radius=1:luma_power=1`: the corner
pixel comes back `113`, not `28` (`255/9`, the zero-padded answer) — the
corner's own 3x3 window sees four replicated copies of itself. See
`src/common.rs`'s `box_pass` doc for the full arithmetic.

A rounding convention split was also found and is easy to miss:
**`boxblur` rounds to nearest; `avgblur` truncates.** The same corner probe
gives `57` from `boxblur` and `56` from `avgblur` at the same position
(`510/9 = 56.67`) — see `src/avgblur.rs`'s doc.

### Performance slice: row-major vertical accumulation

`common::box_pass` keeps the horizontal sliding-window pass and now walks
the vertical pass row by row, maintaining one running sum per column. This
preserves the separable `O(width*height)` arithmetic while matching the
row-major storage layout; callers and rounding/border rules are unchanged.

Measured end to end through `vvmpeg` on this machine (Apple silicon, 10
cores), with a 90-frame 1920x1080 yuv420p Y4M fixture and
`boxblur=luma_radius=8:luma_power=2`, release `dist` builds in a private
target directory, `-threads 1`, and ten rotated before/after/ffmpeg rounds:

| implementation | median wall | median child CPU | output |
|---|---:|---:|---|
| before (column-major vertical pass) | 2.904 s | 2.895 s | 279,936,000 bytes |
| after (row-major vertical pass) | 1.130 s | 1.120 s | 279,936,000 bytes |
| ffmpeg 9.0.1 | 0.501 s | 0.570 s | 279,936,000 bytes |

The candidate is 2.57x faster by wall time and 2.58x by child CPU time than
the prior implementation. Instruments' `CPU Counters` exported `Cycles`
fell from 12,949,219 to 7,628,092 in paired runs (0.589x). Samply still
resolves `common::box_pass` as the hot CLI callee (99.06% of in-process
samples after the change; 99.69% before). Before and after output hashes
are identical (`b25a3f7956948abf…`); the candidate is deterministic at
`-threads 1/2/4/8`, with the same byte count and hash at each setting.
Against ffmpeg, the candidate's mean absolute byte difference is 0.0204
(max 34; 98.6% of bytes equal), a small, unstructured rounding/border
deviation already documented for this filter family rather than a geometry
or sequence drift.

### `unsharp`: verified via an analytic invariant

A box average of a linear ramp equals the ramp's own value at the window's
centre (true by construction of the mean, independent of the reference).
So for any interior pixel of a linear ramp, `unsharp` must be the identity
for *any* amount — confirmed both analytically and directly against the
reference. The one measured gap: the very edge column does not reconcile
with `box_pass`'s own replicate-border arithmetic by exactly one count,
which is the border pixels' rounding specifically, not the interior
formula. See `src/unsharp.rs`'s doc.

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

### `cas`: AMD's published formula, right shape, unresolved constants

Implemented from AMD's own public FidelityFX Contrast Adaptive Sharpen
description (an independently published spec, not FFmpeg source — a
legitimate `AGENT-CONSTRAINTS.md`/D7 source): a per-pixel four-neighbour
cross feeds a min/max-ratio "amplification" term, scaled by a
strength-dependent peak weight, into a renormalised sharpening blend.

Measured against the reference (`ffmpeg 8.1`, a `mod(X*53+Y*19,256)` test
pattern): even `strength=0` visibly sharpens (`106 -> 105` at one pixel),
refuting "`strength` gates sharpening on/off" and confirming the peak
weight range does not reach `0` at either end — consistent with AMD's own
"mild" (`-1/8`) to "aggressive" (`-1/5`) framing. But inverting the blend
formula against the measured `strength=0`/`strength=1` samples did not
converge on one clean constant set: several interior pixels imply a
saturated weight the plain formula does not reproduce. Shipped as a
structural, published-spec implementation — verified via the flat-field
identity invariant (a property of the blend's own algebra, holds for any
`strength`) — not a framecrc pin. See `src/cas.rs`'s doc.

### `dblur`: directional box blur, not the reference's asymmetric kernel

Measured (`angle=0:radius=1` on a single-pixel impulse): the reference's
response along the blur line is `23, 46, 115, 44, 17` — **not symmetric**
around the impulse — which rules out any plain symmetric box or triangular
kernel taken along the line. That is the signature of a recursive or
otherwise order-dependent construction, the same class of finding as
`gblur`'s. This crate ships a symmetric box blur sampled with
`common::sample_bilinear` along `(cos(angle), sin(angle))`, verified via
`radius=0` identity and flat-field fixed-point invariants — a real,
well-defined directional blur, but not the reference's exact algorithm.
See `src/dblur.rs`'s doc.

### `yaepblur`: variance-gated blend, sigma trend confirmed, formula not solved

Measured (`radius=1`, an interior step edge): larger `sigma` visibly blurs
more (`sigma=1000000` moves a pixel most of the way to its local box
average; `sigma=1` barely moves it), confirming `sigma` trades blur
strength against edge preservation — but even the `sigma=1000000` limit
came out one count off a plain box average at one probed pixel and exact
at another, ruling out "large `sigma` reduces to `common::box_pass`
exactly" as a description of the reference. This crate ships a standard,
independently published adaptive-smoothing formula (local variance versus
`sigma`, the same shape as a Wiener/MMSE filter), verified via the
flat-field identity invariant, which happens to match the reference's own
flat-field behaviour exactly (not just structurally — a real measured
point of agreement), and a directional "more sigma, more blur" trend test.
See `src/yaepblur.rs`'s doc.

### `varblur`: two-input variable-radius blur, two open anomalies

Built as a `FrameSyncFilter` (`vaco-filter-framesync`, `FsInput::dual`).
Measured (`ffmpeg 8.1`, `yuv420p`): the reference's `varblur` produced an
all-zero output for a `gray8` input under this crate's probe setup, even
though every sibling filter in this crate accepts `gray8` — unexplained,
not reproduced deliberately. With `yuv420p`, a radius map reading a
constant `0` (which, with `min_r=0`, should mean "no blur, identity") still
measurably spread an impulse across two adjacent columns at equal weight
rather than leaving it untouched. Both anomalies are recorded rather than
silently modelled around. This crate ships the straightforward reading —
`radius(x,y) = round(min_r + (max_r-min_r)*ctrl(x,y)/255)`, then an
ordinary clamp-bordered, truncating box average — verified via a
constant-main-field fixed-point invariant that holds regardless of how the
per-pixel radius is actually computed, and exercised end-to-end through
the real `Graph`/`Synced` scheduler in `tests_graph.rs` (not just its pure
helper functions). See `src/varblur.rs`'s doc.

### `guided`: the He et al. (2010) formula, self-guided mode only

`guidance=off` (self-guided, the default) is implemented directly from the
published algorithm (box filters of `I`, `I*I`, and the derived
coefficients `a`/`b`); `guidance=on` (a second, external guide stream) and
`mode=fast` (subsampled) are **not** implemented and `create` rejects them
explicitly rather than silently downgrading to the self-guided/basic case.
Verified via a flat-field identity invariant that is a property of the
published formula's own algebra (`var_I = 0` forces `a = 0`, `b = I`,
regardless of `radius`/`eps`) — not probed against the reference in this
pass. See `src/guided.rs`'s doc.

## What is verified versus structural

| Confidence | Filters |
|---|---|
| Framecrc-level (interior, against small generated inputs run through the reference directly) | `boxblur`, `avgblur` |
| Interior verified, border a documented gap | `unsharp` |
| Structural only, each with a measured refutation of the naive reading and an independent algebraic invariant | `gblur` (IIR, not FIR), `cas` (right shape, constants not solved), `dblur` (measured asymmetric response), `yaepblur` (sigma trend confirmed, formula not solved), `varblur` (two measured anomalies, including non-identity `radius=0`), `guided` (`guidance=off` only, not probed against the reference) |

Independent oracles used (never the implementation re-run against itself,
per `AGENT-CONSTRAINTS.md`): a DC/constant-field fixed point for every
blur (including `cas`, `dblur`, `yaepblur`, `varblur`, `guided`, each shown
to be a property of that filter's own formula's algebra, not merely
asserted); a direct analytic invariant for `unsharp` (a box average of a
linear ramp equals the ramp's own centre value); Gaussian-kernel
normalisation for `gblur`; a monotonic sigma-versus-blur trend for
`yaepblur`; and, for every framecrc-level filter, the reference binary's
own raw pixel output on a small generated `lavfi`/`geq` input, pinned into
a regression test. Every fix in this pass was falsified once (the
underlying bug reintroduced, the test confirmed to fail, then reverted) —
see the crate's git history for the three concrete cases (`inflate`'s
rounding, `morpho`'s self-exclusion, `cas`'s flat-field algebra — the
first two live in `vaco-filter-convolve`, checked from this crate's sibling
work in the same pass).

## Left for a follow-up

Two filters this crate's own roadmap row still does not implement: `sab`
(shape-adaptive blur, a multi-pass per-pixel-adaptive-radius algorithm) and
`smartblur` (edge-aware blur). Both were not reached in this pass's time
budget; neither was probed enough to know whether they share `gblur`'s IIR
blocker, so no claim is made either way. `guided=on` and `guided`'s
fast/subsampled mode are also deliberately unimplemented (see above) and
rejected at creation rather than silently downgraded. None of them block
the nine filters that did land.

## How to change it

- `boxblur`, `avgblur` and `unsharp` all call `common::box_pass`; a change
  to its rounding or border behaviour changes all three at once — check
  every module's pinned regression test before changing it.
- `dblur` is this crate's only user of `common::sample_bilinear`; a change
  to its border convention changes only `dblur`.
- A new filter driven by a second video stream follows `varblur`'s pattern
  (a `FrameSyncFilter` over `FsInput::dual`, `event.get(1)` for the
  secondary frame) or `vaco-filter-video-composite::overlay`'s — check
  `planning/INTERFACE-GAPS.md` gap 10 first in case `Paired<F>` has landed
  by the time you read this, which would remove the need to hand-write the
  event loop.
- Gotcha: `common::plane_selected`'s bitmask, `common::to_i32`'s saturating
  cast, and `common::sample_clamped`'s clamp-to-edge are used by every
  filter in this crate; a change to any of them is a change to all nine.
- Gotcha: `cas`'s `create` and `guided`'s `create` both parse named string
  options (`mode`, `guidance`) rather than plain integers, following
  `vaco-filter-video-composite::overlay`'s `eval`/`format`/`alpha`
  pattern. Correction, 2026-08-28: `vaco-opts` does support named-integer
  options centrally via `#[derive(OptEnum)]` — this predates that idiom
  rather than working around a real gap; not migrated here since the
  String form already works and is tested.

## Configuration

No environment variables, flags or config files. Every option is the
reference's own CLI filter-argument surface (`vaco-opts` `#[derive(Options)]`
structs per module), documented in each module's own doc comment against
`ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` (all internal, layer 5), and, new this pass,
`vaco-filter-framesync` for `varblur`'s two-input wiring. No external
crates.
