# vaco-filter-lut

3D/Hald lookup-table video filters: `lut3d`, `haldclut`.

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-lut` row also
lists `lut1d` and `haldclutsrc`; neither is implemented in this pass
(carried over from a prior mis-scoped crate, GitHub issue #476's
`vaco-filter-component` — see that issue for the correction).

## What it is

Two filters that remap RGB through a precomputed 3D lookup table: `lut3d`
loads one from a `.cube` text file, `haldclut` decodes one from a second
video input carrying a Hald CLUT image. Both share
[`lut3d::Cube3d`](../../crates/filter/vaco-filter-lut/src/lut3d.rs), the
trilinear/nearest sampler.

## How it works

### `.cube`: a documented format, not reference behaviour

`LUT_3D_SIZE N` followed by `N^3` `"r g b"` rows (red fastest-varying) is
the de facto Adobe/Iridas `.cube` specification — a public format, not
something probed from the reference binary — so `Cube3d::parse` is
written directly against that spec. `DOMAIN_MIN`/`DOMAIN_MAX` lines are
recognised and skipped without being applied (every test file used the
default `0..1` domain). `.3dl`/`.dat`/`.m3d` (also named by the plan's
row for this crate) are not implemented.

### Hald CLUT layout: measured against `haldclutsrc`

`haldclutsrc=level=8` produces a `512x512` `rgb24` image (`512 = 8^3`).
Its content matches the standard Hald convention: flatten a cube of size
`N = level^2` with index `r + g*N + b*N^2` (red fastest) into a square
raster of side `level^3`, row-major — confirmed by reading the identity
image's own pixels (red steps by `4` every column, matching
`round(i*255/63)` for `N=64`; green jumps by exactly one `N`-sized block
at the start of row 1). [`haldclut::decode_hald`] implements this.

### Interpolation: trilinear and nearest only

The reference's default, `tetrahedral`, plus `pyramid` and `prism`, need
a different geometric decomposition of the surrounding cube than
trilinear and are not implemented; requesting one silently falls back to
trilinear rather than erroring.

### Format restriction

Both filters require `PixFmt::is_rgb()` on the pixel format they operate
on (a `.cube`/Hald table is defined over R/G/B), forcing an upstream
conversion for YUV input.

## How to change it

- A `.3dl`/`.dat`/`.m3d` parser: add it beside `Cube3d::parse` in
  `lut3d.rs`, producing the same `Cube3d::from_samples(size, data)`
  shape so `haldclut.rs` needs no changes.
- Tetrahedral/pyramid/prism interpolation: add variants to the `Interp`
  enums in `lut3d.rs`/`haldclut.rs` and a `Cube3d::sample_*` method
  alongside `sample_trilinear`/`sample_nearest`.
- `lut1d`/`haldclutsrc`: new modules following the shape of the two
  filters already here.

## Configuration

`lut3d`'s `file` option reads from the local filesystem at graph-build
time (`std::fs::read_to_string`); no other environment or configuration
surface.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync` (for `haldclut`'s two
inputs).
