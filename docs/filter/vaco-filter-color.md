# vaco-filter-color

Colour and LUT-driven video filters: `colorchannelmixer`, `lut`, `lutrgb`,
`lutyuv`, `lut2`, `pseudocolor`, `colorlevels`, `hue` (8/29).

**2026-08-23 continuation pass**: added `hue` (chroma-vector rotation and
saturation scale; `h`/`s` implemented as constants, `b`/brightness parsed
but not implemented — see `hue`'s own module doc). Also establishes,
rather than assumes, that this crate's `sample` engine genuinely cannot
carry float-plane formats: see "Float-plane support: established, not
assumed" below.

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-color` row
is 29 filters, all verified against `ffmpeg -filters`/`ffmpeg -h
filter=<name>` (8.1) with no discrepancy from the plan in either
direction: `curves`, `colorbalance`, `colorcontrast`, `colorcorrect`,
`colorize`, `colorlevels`, `colortemperature`, `huesaturation`, `hue`,
`vibrance`, `exposure`, `selectivecolor`, `grayworld`, `greyedge`,
`normalize`, `monochrome`, `midequalizer`, `geq`, `colormap`, `limitdiff`,
`tonemap`, `eq`, `histeq`, `colormatrix`, plus the eight implemented
here. The crate began life mis-scoped as a plane/component-shuffling
crate (`vaco-filter-component`, GitHub issue #476) and was corrected
into this row mid-flight — see the issue for the full story.
`colorlevels` landed in one pass; `hue` in the next (2026-08-23
continuation); the six before those carried over from before the
correction.

**Left for follow-up, stated honestly** (21 filters): each is a real
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
  does not have yet, not just a new filter module. **Established, not
  re-assumed, in the 2026-08-23 continuation pass** — see "Float-plane
  support: established, not assumed" below.
- `geq`/`tonemap` were flagged in advance as the likely filters to leave
  (a full expression-evaluated generator, and dynamic-range conversion)
  and were not investigated in this pass either.

## Float-plane support: established, not assumed

This pass was asked to determine *why* `exposure`/`grayworld` are stuck —
does `gbrpf32le` itself not exist, or does the format exist but this
crate's engine cannot carry it — before doing any more work in that
direction. Checked directly against `crates/model/vaco-pixfmt/src/table.rs`
rather than re-guessed:

* **The format exists and matches the reference.** `PixFmt::Gbrpf32le` is
  in the `-pix_fmts` table (`d("gbrpf32le", &[c(2,4,0,0,32),c(0,4,0,0,32),
  c(1,4,0,0,32)], 3, 0, 0, 96, F::PLANAR.union(F::RGB).union(F::FLOAT))`)
  — three 32-bit-per-component planes, `PLANAR | RGB | FLOAT`.
* **What cannot carry it is this crate's `sample` module's read/write
  primitives, by construction.** [`sample::read`]/[`sample::write`] are
  `u16`-in/`u16`-out: they mask a value to `comp.depth` bits and shift it
  into place in a byte-aligned integer container. For a 32-bit IEEE-754
  float component, "mask to `comp.depth` (32) bits" is not a lossy
  downscale of the *value* — reinterpreting an `f32`'s raw bits as an
  integer and truncating them to fit `u16` produces a different, mostly
  meaningless number, not an approximation of the original float. There
  is no way to route a float sample through this accessor pair and get
  the right answer out, regardless of which filter calls it.
* **`sample::is_addressable` also rejects it independently.** It checks
  `PixFmtFlags::FLOAT` explicitly, and separately requires every
  component's `depth <= 16` — `gbrpf32le`'s `depth=32` fails that check
  on its own even before the `FLOAT` flag is consulted.

**Conclusion: this is a genuine infrastructure gap, not per-filter
friction to route around.** Recorded as interface gap 15 in
`planning/INTERFACE-GAPS.md` rather than bodging a one-off float path
inside `exposure`/`grayworld` specifically — a float accessor built for
one filter and not designed as a general `sample`-module capability would
just move the gap, not close it, and the next filter needing float planes
(there is at least one more in this crate's own row, and float formats
appear elsewhere in the pixel format table) would hit the same wall again.

## What it is

Eight filters that recolour a frame from its own pixel data: a 4x4
linear channel mixer, three names for one lookup-table engine
(`lut`/`lutrgb`/`lutyuv`), a two-input lookup table (`lut2`), a
false-colour remap (`pseudocolor`), a per-channel input/output range
remap (`colorlevels`), and a chroma-vector rotation and saturation scale
(`hue`).

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

### `hue`: measured chroma rotation, and what was not pinned down

`u_dev = U-128`, `v_dev = V-128`, rotated by `h` degrees and scaled by `s`
(`u_dev' = (u_dev*cos(rad) - v_dev*sin(rad))*s`, `v_dev'` the matching sine
term), then re-centred, rounded, and clamped into `0..=max`. Confirmed
step by step on `color=red` (`Y=81,U=90,V=240`): `h=90,s=1` pins the
rotation direction and argument order (`U=16,V=90`, matching only
`(u_dev, v_dev)`, not `(v_dev, u_dev)`); `s=2.0` confirms scaling clamps
rather than wraps (`V` would overflow past 255 and clamps there instead);
`h=45,s=1` forces a fractional intermediate result and pins **round**, not
floor/truncate (`U=22`, not the floor-consistent `21`). `h`/`s` are
implemented as constants set once at `create` time rather than the
reference's full per-frame expression language (`vaco-expr` integration
for time-varying `h`/`s`, matching the reference's own fade examples, was
not attempted this pass). `b` (brightness) is parsed but not implemented:
measured to be asymmetric around zero (`b=1.0` measures `Y+25`, `b=-1.0`
measures `Y-26`, not `Y-25`), which rules out a single linear `Y' = Y +
k*b` term and was not decomposed further in the time available.

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
