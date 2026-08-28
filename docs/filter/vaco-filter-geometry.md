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
`shuffleframes`, `shuffleplanes`, `alphaextract`, `pixelize`, `perspective`,
`framepack`, `mergeplanes`, `alphamerge`, `extractplanes`.

The last four were the multi-input/multi-output filters an earlier pass of
this crate declined for lack of an adapter — see *Multi-input filters,
picked up* below for what changed and closed `planning/INTERFACE-GAPS.md`
gap 10.

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
  fitted homography) are both implemented. `interpolation=cubic` (no
  bicubic kernel in this crate yet) is a named "not implemented" error —
  it used to silently run bilinear, and a real `ffmpeg 8.1` `cmp` confirms
  the reference's `cubic` output genuinely differs from `linear`, so this
  was a real divergence, not a rounding difference.

### Multi-input filters, picked up

`vaco-filter-core`'s `adapt.rs` gained two new adapters —
[`Paired`](../filter/vaco-filter-core.md) (N-in 1-out, strict lockstep) and
[`Fanout`](../filter/vaco-filter-core.md) (1-in N-out) — closing
`planning/INTERFACE-GAPS.md` gap 10. That is what let this crate pick up
the four filters an earlier pass declined:

- **`framepack`** (`Paired`, two inputs) — `sbs`/`tab`/`lines`/`columns` are
  measured byte-for-byte against a 4x2 `gray8` probe (constant `0x10` left,
  `0x20` right): `sbs` is a plain horizontal concat, `tab` a plain vertical
  one, `lines`/`columns` interleave whole rows/columns starting with
  `left`. `frameseq` is temporal, not spatial — two full-size frames
  (`left` then `right`) at half the input's frame period each, confirmed
  via the `Stereo3D` side data's `view - left`/`view - right` ordering.
  Measured refusal: a mismatched left/right time base is an error at
  configure time (`Left and right time bases differ`), not something
  `Paired` (or the reference) reconciles — one more confirmation that this
  filter's shape is lockstep, not framesync.
- **`mergeplanes`** (`Paired`, generalised past two inputs) — its input
  count is fixed at construction from the non-deprecated `map<N>s`/
  `map<N>p` options (1 to 4, `format`'s own plane count). This is
  `Paired`'s reason to generalise past exactly two: `mergeplanes` is
  genuinely N-in-1-out with the same strict-lockstep, no-repeat contract
  `framepack` measures. The deprecated `mapping` hex option is not
  implemented.
- **`extractplanes`** (`Fanout`) — generalises `alphaextract`'s single
  fixed-channel plane copy to any of `y`/`u`/`v`/`r`/`g`/`b`/`a`, with a
  dynamic output pad per requested one. Measured: output pad order follows
  the flags' *canonical* order (`y, u, v, r, g, b, a`), not the order
  written in the option string — `planes=v+y+u` still gives pad 0 = Y, pad
  1 = U, pad 2 = V, confirmed against each channel extracted separately.
  Refuses (rather than silently mis-copying) a packed channel — one
  sharing bytes with others in a single plane, like `rgb24`'s R/G/B.
- **`alphamerge`** — the one genuine surprise: measured against the
  reference, it carries the full `eof_action`/`shortest`/`repeatlast`/
  `ts_sync_mode` surface, identical to `overlay`'s. It needs **neither**
  new adapter; it uses `vaco-filter-framesync`'s `Synced`, exactly like
  `vaco-filter-video-composite`'s `overlay` already does. Scope cut:
  adds alpha *in place* (`yuv420p` → `yuva420p`, `gbrp` → `gbrap`, and
  their `10le` variants) rather than reproducing the reference's own
  format-negotiation quirk of converting an RGB-family main input to
  packed `argb` — see `alphamerge.rs`'s doc for the measurement.

### Left out, with the reason (see `src/lib.rs` for the full list)

`shear` and `lenscorrection` (measured formulas that a second probe
contradicted, or a normalisation convention not pinned down in time);
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
  `fillborders`, or `shufflepixels`'s PRNG, this is the crate to add it to.

## Configuration

No crate-level configuration; every knob is a per-filter option, documented
in each module's own doc comment (measured against `ffmpeg -h
filter=<name>`, `ffmpeg 8.1`).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-expr` (option/expression parsing),
`vaco-frame`, `vaco-pixfmt`, `vaco-color` (frame/format model),
`vaco-scale` (solid-colour fill, `fill.rs`), `vaco-filter-core`,
`vaco-filter-framesync` (`alphamerge`'s `Synced`), `vaco-filter-graph` (the
filter framework itself).
