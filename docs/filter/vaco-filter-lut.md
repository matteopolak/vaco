# vaco-filter-lut

3D/Hald/1D lookup-table video filters: `lut3d`, `haldclut`, `lut1d`,
`haldclutsrc`.

**Scope note**: `planning/16-filters.md` §4.2's `vaco-filter-lut` row is
`lut1d`, `lut3d`, `haldclut`, `haldclutsrc` — all four names verified
against `ffmpeg -filters`/`ffmpeg -h filter=<name>` (8.1), no discrepancy
in either direction. All four are now implemented. `lut3d`/`haldclut`
carried over from a prior mis-scoped crate (GitHub issue #476's
`vaco-filter-component` — see that issue for the correction); `lut1d` and
`haldclutsrc` landed in this pass, closing the row.

**Left for follow-up, stated honestly**: `.3dl`/`.dat`/`.m3d` file parsing
for `lut3d`'s `file` option;
`cubic`/`cosine`/`spline` interpolation for `lut1d` and
`tetrahedral`/`pyramid`/`prism` for `lut3d`/`haldclut` (each is now a
named "not implemented" error rather than a silent fall back to linear/
trilinear — see "Interpolation" below); non-default
`DOMAIN_MIN`/`DOMAIN_MAX`; `haldclut`'s `clut=first` (now also a named
error rather than silently behaving like `clut=all`).

## What it is

Four filters built around one shared idea — remap a pixel through a
precomputed lookup table: `lut3d` and `lut1d` load one from a `.cube` text
file (3D and 1D respectively), `haldclut` decodes one from a second video
input carrying a Hald CLUT image, and `haldclutsrc` generates that Hald
CLUT image in the first place. `lut3d`/`haldclut` share
[`lut3d::Cube3d`](../../crates/filter/vaco-filter-lut/src/lut3d.rs), the
trilinear/nearest sampler; `lut1d` has its own per-channel-independent 1D
analogue in `lut1d.rs`.

## How it works

### `.cube`: a documented format, not reference behaviour

`LUT_3D_SIZE N` followed by `N^3` `"r g b"` rows (red fastest-varying), or
`LUT_1D_SIZE N` followed by `N` `"r g b"` rows for the 1D case, is the de
facto Adobe/Iridas `.cube` specification — a public format, not something
probed from the reference binary — so `Cube3d::parse`/`Lut1d::parse` are
written directly against that spec. `TITLE`/`DOMAIN_MIN`/`DOMAIN_MAX`
lines are recognised and skipped without being applied (every test file
used the default `0..1` domain). Confirmed the reference accepts this
same file shape for `lut1d`, not a different format:
`lut1d=file=x.cube` where `x.cube` has `LUT_1D_SIZE 3` produced measured
output matching the file's own table (see `lut1d.rs`'s doc).

### `lut1d`/`lut3d`/`haldclut` share one channel model, apply it differently

`lut3d`/`haldclut` treat `(R, G, B)` as one point in a 3D space and
trilinear/nearest-interpolate a single joint sample. `lut1d` is simpler:
each output channel reads **only its own column** of the table — the red
sample never sees the table's G/B columns — confirmed with a 2-row table
`[(0,0,0), (0.5,0.5,0.5)]` applied to `rgba` `0x80808080`: alpha passed
through unchanged at `0x80`, R/G/B all became `0x40`.

### Hald CLUT layout: measured against `haldclutsrc`, now generated too

`haldclutsrc=level=8` produces a `512x512` `rgb24` image (`512 = 8^3`).
Its content matches the standard Hald convention: flatten a cube of size
`N = level^2` with index `r + g*N + b*N^2` (red fastest) into a square
raster of side `level^3`, row-major — confirmed by reading the identity
image's own pixels (`haldclutsrc.rs`'s doc has the pixel-by-pixel probe
for `level=2` and `level=3`). `haldclut::decode_hald` reads this layout
back into a `Cube3d`; `haldclutsrc::fill_row` now generates it, verified
both against direct reference probes and, as an independent property, by
round-tripping the generated image through `decode_hald` and checking the
recovered cube is the identity (`generated_image_decodes_back_to_the_identity_cube`).

### Corrected: the reference truncates the final sample, not rounds it

`lut3d`/`haldclut` shipped calling `.round()` when converting an
interpolated `[0, 1]` value back to an integer sample. Building `lut1d`
surfaced the opposite, from three independent angles: `lut1d` itself
(`nearest`/`linear` on a 3-point table, several fractional values landing
on `.5` all rounded *down*, ruling out both round-half-away-from-zero and
round-half-to-even), `lut3d` (a size-2 half-scale cube on `0x808080`
measures `0x7f7f7f`, not the rounded `0x80808080`), and `haldclutsrc`'s
own pixel generation (`level=3`'s `1*255/8 = 31.875` measures as `31`).
All three now truncate; see each module's own doc for the exact probes
and each crate's `truncates_rather_than_rounds_*` regression test (each
one falsified by temporarily restoring `.round()` and confirming the test
fails before restoring the fix).

### Interpolation: nearest and linear/trilinear only

`lut1d`'s `cubic`/`cosine`/`spline` and `lut3d`/`haldclut`'s
`tetrahedral`/`pyramid`/`prism` need more surrounding points than a
two-point (1D) or eight-corner (3D) neighbourhood and were out of this
crate's time budget. These used to silently fall back to linear/
trilinear with no error — accepted, wrong, undetectable short of a
differential comparison. Verified concretely (real `ffmpeg 8.1`): the
same 2-level `.cube` and the same `0x808080` pixel give `0x69` under
`trilinear` and `0x26` under `tetrahedral` — a large, real divergence,
not a rounding difference. Each unimplemented value is now a named
"not implemented" error instead. `lut1d`'s default is `linear`
(implemented), so only an explicit non-default request is affected;
`lut3d`/`haldclut`'s default is `tetrahedral`, so **a bare
`lut3d=file=…`/`haldclut` now errors by default** — pass
`interp=trilinear` (or `nearest`) explicitly to get a working filter,
which is a real, deliberate behaviour change from "wrong but ran" to
"correct but requires an explicit option," not a refinement.

### Format restriction

All four filters require `PixFmt::is_rgb()` on the pixel format they read
or produce (a `.cube`/Hald table is defined over R/G/B), forcing an
upstream conversion for YUV input; `haldclutsrc` always produces `rgb24`
regardless of what a downstream filter would prefer (measured: requesting
another format still reports `rgb24` on the link).

### Attempted and abandoned: `.3dl`/`.dat`/`.m3d`

Two probe attempts at Autodesk `.3dl` (a 2-point mesh header followed by
8 rows for a 2-level cube; the same 8 rows with no header at all) both
produced `Parsed_lut3d_0: Unexpected EOF` — the reference wants
substantially more input than either guess supplied, meaning its
mesh-size detection does not work the way either probe assumed. Rather
than risk a third guess that matches nothing verifiable (the
`fillborders=reflect` failure mode this project's constraints document
warns about), `.3dl`/`.dat`/`.m3d` parsing is left unimplemented. See
`lut3d.rs`'s module doc for the exact commands tried.

## How to change it

- A `.3dl`/`.dat`/`.m3d` parser: add it beside `Cube3d::parse` in
  `lut3d.rs`, producing the same `Cube3d::from_samples(size, data)` shape
  so `haldclut.rs` needs no changes. Start by finding a real reference
  `.3dl` file (not a hand-guessed one) to probe against — the two guesses
  this pass tried both failed outright.
- Tetrahedral/pyramid/prism interpolation: add variants to the `Interp`
  enums in `lut3d.rs`/`haldclut.rs` and a `Cube3d::sample_*` method
  alongside `sample_trilinear`/`sample_nearest`. Same shape for `lut1d`'s
  `cubic`/`cosine`/`spline`.
- A new colour/LUT filter is `vaco-filter-color`'s row, not this crate's —
  this crate's row is closed at four filters.

## Configuration

`lut3d`'s and `lut1d`'s `file` option reads from the local filesystem at
graph-build time (`std::fs::read_to_string`); no other environment or
configuration surface.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync` (for `haldclut`'s two
inputs).
