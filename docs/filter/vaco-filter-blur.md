# vaco-filter-blur

T2 blur and sharpen video filters (FT-4.6a, GitHub issue #468). Four
implemented: `boxblur`, `avgblur`, `gblur`, `unsharp`.

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
varblur, yaepblur, guided, boxblur, smartblur, sab`. Four are implemented;
see "Left for a follow-up" below for the other seven and why.

## What it is

One module per filter (`src/<name>.rs`), each exposing `pub const DESC:
FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::BlurRegistry`](../../crates/filter/vaco-filter-blur/src/registry.rs)
— the same shape `vaco-filter-convolve`/`vaco-filter-audio-eq` use.
`src/common.rs` holds the shared 8-bit plane helpers every filter here
builds on, in particular `box_pass` — the clamp-bordered box average
`boxblur`, `avgblur` and `unsharp`'s internal blur all share.

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

## What is verified versus structural

| Confidence | Filters |
|---|---|
| Framecrc-level (interior, against small generated inputs run through the reference directly) | `boxblur`, `avgblur` |
| Interior verified, border a documented gap | `unsharp` |
| Structural only | `gblur` (not the reference's algorithm — see above) |

Independent oracles used (never the implementation re-run against itself,
per `AGENT-CONSTRAINTS.md`): a DC/constant-field fixed point for every
blur; a direct analytic invariant for `unsharp` (a box average of a linear
ramp equals the ramp's own centre value); Gaussian-kernel normalisation for
`gblur`; and, for every filter, the reference binary's own raw pixel
output on a small generated `lavfi`/`geq` input, pinned into a regression
test.

## Left for a follow-up

Seven more filters this crate's own roadmap row names: `cas` (AMD's
published Contrast Adaptive Sharpen, a specific per-pixel min/max-ratio
formula), `dblur` (directional blur — a 1D blur rotated to an arbitrary
angle, needing bilinear sampling this crate's nearest-pixel
`common::sample_clamped` does not provide), `varblur` (a per-pixel radius
read from a second video stream — needs `vaco-filter-framesync`, not yet a
dependency here), `yaepblur` (edge-preserving blur via a local-variance
gate whose exact threshold formula was not measured), `guided` (the He et
al. 2010 guided filter — well published, and the best candidate for a
follow-up, but its box-filter-of-products construction was not reached),
`sab` (shape-adaptive blur, a multi-pass per-pixel-adaptive-radius
algorithm), `smartblur` (edge-aware blur, likewise not reached). None of
them block the four filters that landed.

## How to change it

- `boxblur`, `avgblur` and `unsharp` all call `common::box_pass`; a change
  to its rounding or border behaviour changes all three at once — check
  every module's pinned regression test before changing it.
- A new filter driven by a second video stream (`varblur`, `guided`'s
  "on" guidance mode) needs `vaco-filter-framesync` added as a dependency;
  see `vaco-filter-convolve`'s sibling `maskedclamp` (before it moved to
  `vaco-filter-key`) or `vaco-filter-video-composite::overlay` for the
  `Synced`/`FrameSyncFilter` pattern.
- Gotcha: `common::plane_selected`'s bitmask, `common::to_i32`'s saturating
  cast, and `common::sample_clamped`'s clamp-to-edge are used by every
  filter in this crate; a change to any of them is a change to all four.

## Configuration

No environment variables, flags or config files. Every option is the
reference's own CLI filter-argument surface (`vaco-opts` `#[derive(Options)]`
structs per module), documented in each module's own doc comment against
`ffmpeg -h filter=<name>`.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph` (all internal, layer 5). No external crates. No
`vaco-filter-framesync` dependency at present — the one filter here that
would have needed it (`maskedclamp`) turned out not to belong to this
crate at all.
