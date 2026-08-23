# vaco-filter-color

Colour and LUT-driven video filters: `colorchannelmixer`, `lut`, `lutrgb`,
`lutyuv`, `lut2`, `pseudocolor`, `colorlevels` (7/29).

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-color` row
is 29 filters, all verified against `ffmpeg -filters`/`ffmpeg -h
filter=<name>` (8.1) with no discrepancy from the plan in either
direction: `curves`, `colorbalance`, `colorcontrast`, `colorcorrect`,
`colorize`, `colorlevels`, `colortemperature`, `huesaturation`, `hue`,
`vibrance`, `exposure`, `selectivecolor`, `grayworld`, `greyedge`,
`normalize`, `monochrome`, `midequalizer`, `geq`, `colormap`, `limitdiff`,
`tonemap`, `eq`, `histeq`, `colormatrix`, plus the seven implemented
here. The crate began life mis-scoped as a plane/component-shuffling
crate (`vaco-filter-component`, GitHub issue #476) and was corrected
into this row mid-flight — see the issue for the full story.
`colorlevels` landed in this pass; the six before it carried over from
before the correction.

**Left for follow-up, stated honestly** (22 filters): each is a real
GitHub-issue-sized unit of work, but three deserve a specific note
because probing them found real walls rather than just "not attempted":

- `colorbalance`'s shadows/midtones/highlights weighting is not a simple
  threshold or linear ramp — a sweep of `rs=1.0` found a flat plateau
  from `v=0` to `v≈24`, then a non-linear falloff to `0` by `v=64`. The
  linear-in-`rs` scaling is confirmed; the per-pixel weighting curve
  itself is not.
- `exposure` and `grayworld` both force `gbrpf32le` (planar 32-bit float)
  output, which this crate's whole `sample` engine deliberately excludes
  (`PixFmtFlags::FLOAT`) — these need a float-sample accessor this crate
  does not have yet, not just a new filter module.
- `geq`/`tonemap` were flagged in advance as the likely filters to leave
  (a full expression-evaluated generator, and dynamic-range conversion)
  and were not investigated in this pass either.

## What it is

Seven filters that recolour a frame from its own pixel data: a 4x4
linear channel mixer, three names for one lookup-table engine
(`lut`/`lutrgb`/`lutyuv`), a two-input lookup table (`lut2`), a
false-colour remap (`pseudocolor`), and a per-channel input/output range
remap (`colorlevels`).

## How it works

### The shared bit-depth engine

[`sample`](../../crates/filter/vaco-filter-color/src/sample.rs) reads
`PixFmt::descriptor()`'s per-component table (plane, byte step, offset,
post-load shift, bit depth) and exposes `u16`-in/`u16`-out `read`/`write`.
Every filter here reads and writes through it, so `yuv420p`, `rgb24`,
`rgba` (a **packed** format — this mattered, see below) and
`yuv420p10le` are all one code path. `Component::step` is a **pixel
stride** (`rgb24`'s is 3, `rgba`'s is 4), not a container width; the
container is `depth <= 8 ? 1 byte : 2 bytes`. An early version of this
module checked `step` where it should have checked `depth` and silently
rejected every packed RGB format as "not addressable" — caught by this
crate's own unit tests, not by inspection.

### `lut`/`lutrgb`/`lutyuv` are one implementation under three names

Measured (`ffmpeg 8.1`): `lutrgb=r=128` on a `yuv420p` input does **not**
force an RGB conversion, and `lutyuv`/`lut` behave identically to `lutrgb`
except for which option names (`y`/`u`/`v` vs `r`/`g`/`b`, both aliasing
`c0`/`c1`/`c2`) are documented. All three share one `Filter`/`Opts` in
`lut.rs`; `c0..c3` are precomputed as one lookup table per channel
(`0..=maxval` entries), since the reference's own bound variables
(`val`/`clipval`/`maxval`/`minval`/`negval`/`w`/`h`, probed one at a time
against `ffmpeg -vf lut=c0=<candidate>`) never depend on frame number.

### `pseudocolor` binds every output channel to the *same* input value

Unlike `lut`, `pseudocolor`'s `c0..c3` all see the `index`-selected
channel's value (default channel 0) — measured by setting `c0=200:c1=50:
c2=90` and confirming every output pixel became exactly `(200,50,90)`
regardless of the source data. Named `preset=` colour ramps (21 of them)
are parsed but not implemented — see `pseudocolor.rs`'s doc for why.

### `colorchannelmixer`: measured formula and its gap

`out_channel = rr*R + rg*G + rb*B + ra*A` (and the `g`/`b`/`a` rows),
gains applied to the raw sample value with no 0..1 normalisation, rounded
and clamped. Confirmed with a controlled two-colour probe. `pc`
(preserve-colour mode) and `pa` are parsed but inert — reproducing the
seven preserve-colour blends needs source access this project's D7
forbids.

### `colorlevels`: measured formula and its truncation rule

`t = clamp((in/max - imin) / (imax - imin), 0, 1)`, `out = floor((omin +
t*(omax-omin)) * max)`, independently per R/G/B(/A) channel. Confirmed at
an input white point, a value clamping past it, and exactly at an input
black point — including that the reference truncates rather than rounds
the final sample, the same rule `vaco-filter-lut` measured independently
for a completely different filter family. Requires an RGB pixel format
(measured: forces an `rgb24` conversion for YUV input, same restriction
as `colorkey`/`lut3d` elsewhere in this project). `preserve` (seven
colour-preservation modes) is parsed but inert, for the same reason
`colorchannelmixer`'s `pc`/`pa` are: the reference does not document the
blending formula and reproducing it needs source access D7 forbids.

## How to change it

- Add a new colour filter from the row above as its own module, following
  `colorchannelmixer.rs`'s shape: `pub const DESC`, an `Opts` deriving
  `vaco_opts::Options`, a `Filter` implementing
  `vaco_filter_core::adapt::FrameFilter`, and a crate-private `create`.
- Register it in `vaco-component.toml` (one `[[component]]` block) and run
  `cargo xtask gen-registry`.
- Add the name to `registry.rs`'s `NAMES` and the `match` in
  `ColorRegistry::create`.
- If the filter needs two synchronised inputs, follow `lut2.rs`'s
  `vaco_filter_framesync::FrameSyncFilter` shape rather than
  `FrameFilter` — see that crate's own docs for the adapter contract.

## Configuration

No crate-level configuration or environment variables; every filter's
knobs are its own `vaco_opts::Options` struct, documented in each module.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-expr` (the `lut`/`lut2`/`pseudocolor`
expression language), `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync` (for `lut2`).
