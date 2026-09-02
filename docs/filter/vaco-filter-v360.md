# vaco-filter-v360

360-degree video projection conversion (`v360`), converting between
projections such as equirectangular (a full spherical panorama) and flat
(a normal rectilinear view) — extracting a normal-looking view from a
360 video, or the reverse. Crate did not exist before this pass.

## Scope: two projections of the reference's twenty-five

The real `v360` filter (`ffmpeg -h filter=v360`, this environment's
`ffmpeg` 9.0.1) supports 25 named projections, stereo 3D, cubemap face
order/rotation/padding, linked FOV derivation, off-axis offsets and
per-axis flips — roughly 5100 LOC upstream. This crate ships
`equirect` and `flat`/`rectilinear`/`gnomonic` (one projection under
three names) in every direction, plus `yaw`+`pitch` re-projection and
`h_flip`/`v_flip`. Every other named projection is rejected with a clear
`Error::Unsupported` naming it, rather than silently misprojected.

## The geometry is measured, not assumed — and one place it did not fit

Every sign convention and formula (`src/geometry.rs`) was pinned down by
probing the real reference with single-pixel markers in a synthetic
equirectangular image, then a *stronger* off-axis reverse check: fix a
marker at a known world direction, run the real reference, find which
output pixel it moved to, and confirm that pixel's own local ray
reproduces the known direction under the candidate formula. `yaw`,
`pitch`, and their composition (`Yaw(Pitch(·))`) all pass this check
cleanly, both off-axis and on real photographic content (`src/v360.rs`'s
`oracle` tests measure PSNR against real `ffmpeg`, landing in the high
30s-to-40+ dB band).

**`roll` does not fit any formula this pass could find, and is refused
rather than shipped as a guess.** The first check (a 90-degree, on-a-
marker probe) looked correct — but 90 degrees is a special angle where
`sin=1, cos=0` makes more than one plausible formula agree by
coincidence. A generic 20-degree probe, run the same rigorous way, did
**not** confirm the same formula (error ~10-33% of a unit vector's
length, i.e. tens of degrees — not a rounding gap), whether `roll` was
applied alone or combined with `yaw`/`pitch` in any of the 6 possible
composition orders. Confirmed a third way on real content: `roll=20`
alone measured against real `ffmpeg` lands at PSNR ~12 dB, a plainly
structured defect. `Filter::new` refuses any nonzero `roll` outright with
a clear error. This is the same "investigated, did not fit, not shipped"
call `vaco-filter-color` made for `colorize`/`eq`, applied here to a
rotation formula instead of a colour one — see `src/geometry.rs`'s own
doc for the full measurement trail, including the specific numbers that
ruled out all 6 orderings.

## What it is

- [`geometry`](../../crates/filter/vaco-filter-v360/src/geometry.rs) —
  pure spherical/perspective math, no pixel data: `Dir` (a unit vector,
  `+x` right/`+y` up/`+z` forward), `Projection::{Equirect,Flat}` with
  `dir_from_uv`/`uv_from_dir`, and `rotate_yaw`/`rotate_pitch`/
  `rotate_roll`/`orient` (the last composing only `yaw`+`pitch`, per the
  finding above).
- [`v360`](../../crates/filter/vaco-filter-v360/src/v360.rs) — the filter
  itself: for each output pixel, compute its local ray in the output
  projection, `orient` it into world space, look it up in the input
  projection, and sample (nearest or bilinear, reusing
  `vaco_filter_vdsp::affine::bilinear_sample`). Output size (`w`/`h`)
  can differ from input, same shape as `vaco-filter-video-geometry::scale`.
- [`registry`](../../crates/filter/vaco-filter-v360/src/registry.rs) —
  the one-name `FilterRegistry`.

## Configuration

Matches `ffmpeg -h filter=v360`'s option table for the options this crate
accepts: `input`/`output` (projection name), `interp` (`near`/`line`),
`w`/`h` (output size, `0` keeps the input's own), `yaw`/`pitch` (degrees),
`h_fov`/`v_fov`/`ih_fov`/`iv_fov` (degrees; `0` resolves to this crate's
own default of `90` for `flat`, **not** the reference's own aspect-ratio/
`d_fov` auto-derivation — measured to differ on ~21% of bytes for a real
fixture, so the two are not interchangeable), `h_flip`/`v_flip`
(genuinely implemented). `roll` is accepted at the option-parsing level
(so a filtergraph string naming it does not fail to *parse*) but any
nonzero value is refused at filter creation with a clear error. Every
option the reference has for a projection this crate does not implement
(cubemap face order/rotation/padding, stereo, `d_fov`, off-axis offsets,
`alpha_mask`, `rorder`, `ih_flip`/`iv_flip`, `in_trans`/`out_trans`,
`reset_rot`) is not accepted at all.

## How to change it

- A new projection goes in `geometry::Projection` as a new variant with
  its own `dir_from_uv`/`uv_from_dir`, verified the same way `Flat` was:
  an off-axis reverse check against real `ffmpeg`, not just an on-axis
  spot check (the `roll` finding is the worked example of why on-axis
  alone is not enough).
- If `roll`'s real formula is ever found (a genuinely different
  composition, or a different rotation entirely — e.g. rotating the
  *output pixel grid* independently of the 3D ray, which this pass ruled
  out only for the isotropic-FOV case), replace `orient` and remove the
  refusal in `v360::Filter::new`, updating `Vaco-Provenance` accordingly.
- Cubemap support (`c3x2` at minimum, the reference's own default output)
  is the highest-value next addition per the plan's own row; it needs its
  own `Projection` variant handling 6 sub-images in a layout, not just a
  new formula.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-vdsp` (`bilinear_sample`, reused rather
than duplicated).

## Fuzzing

`fuzz/fuzz_targets/filter_v360_options.rs` (option parsing, the one
registered name, through the real filtergraph parser).
