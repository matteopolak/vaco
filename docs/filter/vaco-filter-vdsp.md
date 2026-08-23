# vaco-filter-vdsp

Shared video kernels that cross filter-crate boundaries, per
`planning/16-filters.md` §4.1. Created while building
`vaco-filter-temporal` (FT-4.12b, GitHub #475), which needed `scene_sad` for
`decimate`, `mpdecimate` and `freezedetect` and found neither the function
nor this crate existing yet under `crates/filter/`.

## What it is

Three functions, all 8-bit-plane-only for now (see below):

- `plane_sad(a, b)` — sum of `|a[i] - b[i]|` over two same-sized 8-bit
  planes.
- `block_sad(a, b, bx, by, bw, bh)` — the same sum restricted to one
  rectangular block, clipped to both planes' bounds.
- `normalised_sad(a, b)` — `plane_sad` divided by `255 * sample_count`, a
  `0.0..=1.0` "fraction of full-scale difference" independent of
  resolution.

## How it works

Plain per-sample loops over `vaco_frame::PlaneRef`, reading rows with
`.row(y)` and comparing bytes with `u8::abs_diff`. No SIMD, no threading —
this is the minimum implementation the three current callers need; the row
in the plan that names this crate (`vdsp (scene_sad)`) does not by itself
require more.

## How to change it

This crate is intentionally minimal today. Plan §4.1 also places
`edge_common`, `motion_estimation`, the box-blur core, `bbox`, SAD/hadamard
(`pixelutils`), LUT sampling/interpolation, morphology neighbourhood core
and integral images here — add them here, not as a second copy inside
whichever filter crate needs them next (`framerate`'s real
motion-compensated blend, `scdet`, `identity`/`msad`, `minterpolate`,
`edgedetect`, `cropdetect`, `blurdetect`, `boxblur`, `avgblur`, `deshake`,
`mestimate`). A `u16`/high-bit-depth variant of the SAD functions here is a
mechanical addition (same loop, wider accumulator) whenever a caller
actually needs one.

## Configuration

None — pure functions, no options.

## Dependencies

`vaco-frame`. Dev-only: `vaco-pixfmt`, `proptest`.
