# vaco-filter-geometry

T2 geometry video filters — GitHub issue #470 (`FT-4.7`).

## What it is

`planning/16-filters.md` §4.3's `vaco-filter-geometry` row, minus what other
crates already register: `crop`, `pad`, `transpose`, `hflip`, `vflip`
(`vaco-filter-video-geometry`'s T1 set) and `rotate`
(`vaco-filter-video-composite`, issue #465 — a genuine, independently
discovered overlap, caught by `cargo xtask gen-registry`'s dup-check rather
than any plan doc). Registered here: `scroll`, `field`, `il`, `tile`,
`untile`, `fillborders` (4 of its 7 modes), `swaprect`, `swapuv`,
`shuffleframes`, `shuffleplanes`, `alphaextract`, `pixelize`, `perspective`.

An earlier draft of this crate briefly registered `zoompan`, `scale2ref` and
`cropdetect` before the orchestrator corrected this crate's scope: the plan
actually assigns `zoompan`/`scale2ref` to `vaco-filter-scale` and
`cropdetect` to `vaco-filter-analysis`. All three were removed.

## How it works

### Membership history

This crate went through two scope corrections before landing on the plan's
row: an initial draft derived membership from `ffmpeg -filters` output
directly (which conflated this crate's true remit with filters the plan
puts elsewhere), and `rotate` was dropped mid-flight when `gen-registry`
refused a genuine two-agent naming collision. `src/lib.rs`'s doc has the
full accounting of what is registered, what was considered and excluded,
and why, including the plane-shuffling family (`swapuv`, `shuffleplanes`,
`alphaextract`, `shuffleframes`) that the orchestrator redirected onto this
crate from a since-removed `vaco-filter-component` (issue #476).

### `geom.rs`, `fill.rs`, `sample.rs`, `warp.rs` — shared substrate

`geom.rs` is a smaller, crate-local copy of `vaco-filter-video-geometry`'s
own `geom.rs` (byte-level plane addressing) — not a shared dependency,
because that module is `pub(crate)` in its own crate. `fill.rs` builds
solid-colour frames through `vaco_scale::Scaler`, the same way the sibling
crate's `pad` does (limited-range-correct `black`). `sample.rs` is the
nearest/bilinear per-pixel sampler `perspective` uses — **caught a real
half-pixel bug in its own tests**: an initial version blended neighbours
using `floor(src_x)` directly, off by half a pixel from where pixel centres
actually sit; fixed by shifting the coordinate by `-0.5` before flooring for
the bilinear branch only. `warp.rs` is the standard 4-point-correspondence
homography `perspective` solves (an 8x8 linear solve plus a 3x3 matrix
inverse), textbook numerical linear algebra, not reference-specific.

### Per-filter notes

- **`scroll`** — wraparound scroll; shift formula measured against a
  4-pixel ramp at three speeds including a negative one.
- **`field`** — extracts every other row (`type=top` = even rows,
  `type=bottom` = odd), output half height; measured against a 6-row ramp.
- **`il`** — deinterleave (`dst = [even rows..., odd rows...]`) and
  interleave (its measured inverse), independently per luma/chroma/alpha
  plane group; measured via a deinterleave-then-interleave round trip that
  reproduces the original frame exactly.
- **`tile`/`untile`** — grid layout (`WxH` = columns x rows, row-major cell
  order), margin/padding/color placement measured against a 7x7 dump;
  `nb_frames`/`overlap`/`init_padding` are implemented from their option
  descriptions rather than independently measured frame-for-frame.
- **`swaprect`** — a plain byte-swap of two non-overlapping rectangles,
  rejecting overlaps rather than guessing; oracle is swap-twice-restores.
- **`swapuv`** — exchanges plane 1 and plane 2; rejects RGB/non-3-plane
  formats; oracle is the same swap-twice-restores structural check.
- **`shuffleframes`** — reorders a fixed-size sliding group of frames per a
  `mapping` list; oracle is the identity mapping reproducing input order.
- **`shuffleplanes`** — remaps `map0..map3` (which input plane feeds which
  output plane); a straight read of the option table, oracle is a swap
  mapping actually exchanging plane data.
- **`alphaextract`** — copies the alpha component (component index 3, via
  its own `.plane` field rather than a hard-coded plane number) out as a
  `gray` frame; rejects formats with no alpha channel. Packed-alpha formats
  (alpha sharing a byte range with colour channels in one plane) are a
  documented gap — every alpha-bearing format this project currently tables
  happens to give alpha its own plane.
- **`pixelize`** — block reduction (avg/min/max) measured against a 4x2
  ramp; `planes` is a plain `i64` bitmask rather than the reference's
  named-flag syntax.
- **`fillborders`** — `smear`, `mirror`, `fixed` and `wrap` modes are
  measured exactly against an asymmetric-border probe. `reflect`, `fade`
  and `margins` are **not implemented**: a plausible `reflect` formula
  matched one probe and was directly contradicted by a second — see
  `fillborders.rs`'s doc for the two measurements that disagreed.
- **`perspective`** — a 4-corner projective transform; `sense=source`
  (confirmed identity-mapping default) and `sense=destination` (inverts the
  fitted homography) are both implemented. `interpolation=cubic` falls back
  to bilinear (no bicubic kernel in this crate yet).

### Left out, with the reason (see `src/lib.rs` for the full list)

`shear` and `lenscorrection` (measured formulas that a second probe
contradicted, or a normalisation convention not pinned down in time);
`framepack`, `mergeplanes`, `alphamerge` (multi-input — need `Activity`-
level synchronisation `vaco_filter_core::adapt::Simple` does not provide);
`extractplanes` (the mirror problem — dynamic *output* pad count);
`shufflepixels` (its `seed` option strongly implies a specific PRNG this
crate has not identified — shipping *a* shuffle would be confidently wrong
at every seed but the identity one); `addroi` (needs a region-of-interest
side-data variant `vaco_frame::FrameSideData` does not have, and adding one
means editing a crate this agent does not own); `ccrepack`, `stereo3d`,
`tiltandshift` (substantial standalone algorithms, deferred for time).

## How to change it

- Add a new filter as its own module, exposing `pub const DESC` and a
  crate-private `create`, then wire it into
  `registry::T2GeometryRegistry::create`/`names` and append a `[[component]]`
  row to `vaco-component.toml`.
- **Before registering any new filter name, check `vaco-filter-video-geometry`
  and `vaco-filter-video-composite` first** — this crate has already hit one
  real, independently-discovered overlap (`rotate`) that `cargo xtask
  dup-check`/`gen-registry` caught only after the fact.
- If you pin down `shear`, `lenscorrection`, `reflect`/`fade`/`margins` for
  `fillborders`, `shufflepixels`'s PRNG, or build the `Activity`-level
  machinery for a multi-input/multi-output filter, this is the crate to add
  it to.

## Configuration

No crate-level configuration; every knob is a per-filter option, documented
in each module's own doc comment (measured against `ffmpeg -h
filter=<name>`, `ffmpeg 8.1`).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-expr` (option/expression parsing),
`vaco-frame`, `vaco-pixfmt`, `vaco-color` (frame/format model),
`vaco-scale` (solid-colour fill, `fill.rs`), `vaco-filter-core`,
`vaco-filter-graph` (the filter framework itself).
