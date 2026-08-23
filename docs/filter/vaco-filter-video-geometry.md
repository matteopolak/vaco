# vaco-filter-video-geometry

Rotate-free video geometry filters (FT-4.4, GitHub epic #54, geometry child issue):
`scale`, `crop`, `pad`, `hflip`, `vflip`, `transpose`.

## What it is

Six filters that change a video frame's *size* or *layout* without changing what
each pixel means. `rotate` (arbitrary-angle rotation with interpolation) is
explicitly out of scope for this crate — hence "rotate-free" — and is not
registered here. Each is a module (`src/<name>.rs`, or `flip.rs` for the
`hflip`/`vflip` pair) exposing a `pub const DESC: FilterDesc` and a
crate-private `create`. [`registry::GeometryRegistry`](../../crates/filter/vaco-filter-video-geometry/src/registry.rs)
dispatches by name.

## How it works

### `geom.rs` — the byte-level substrate

`crop`, `pad`, `hflip`, `vflip` and `transpose` never need to interpret a
sample's *value*, only move bytes. `geom::plane_unit_bytes(format, plane)`
reads the bytes-per-pixel-group for one plane straight off
`vaco_pixfmt::PixFmt`'s component table (a packed plane's components all
share one `step`; a planar plane has exactly one component), so every one of
these filters works on any addressable pixel format without a per-format
branch. `geom::ensure_addressable` rejects hardware surfaces, sub-byte
packing and palette formats up front, since none of them have bytes this
crate can move.

### `scale` — a thin adapter, not a reimplementation

`scale.rs` evaluates `w`/`h` into concrete pixel dimensions and hands the
actual resampling to [`vaco_scale::Scaler`](../../crates/signal/vaco-scale/src/lib.rs).
It does not change pixel format — chain `format=` afterward for
resize-and-convert.

### `pad`'s fill goes through `vaco-scale`, not hand-written colour math

`fill::solid_frame` builds a small RGB24 tile of the requested colour and
runs it through `vaco_scale::Scaler` into the destination format. A uniform
image has no spatial frequency content, so every resampling kernel
reproduces the same solid colour exactly — and this is what makes `pad`'s
default black land on `Y=16` for `yuv420p` (limited range) without this
crate owning any colour-matrix code of its own.

### `transpose` — measured, not assumed

The reference's four `dir` values do not map onto "rotate clockwise" /
"rotate counter-clockwise" the way the names suggest. See `transpose.rs`'s
doc for the measurement; the summary is in the table below.

## The measured edge-case table

| Filter | Case | Measured behaviour (ffmpeg 8.1) |
|---|---|---|
| `scale` | `w` given, `h` absent | **Hard error**, `Invalid size '<w>'`, whatever `w` is (`-1`, `50`, `200` all fail identically). |
| `scale` | `h` given, `w` absent | Works. `w` keeps the input's value unchanged. |
| `scale` | `w=N:h=-1` | `h` = `round(N * in_h / in_w)` — nearest integer, not floor. |
| `scale` | `w=N:h=-2` | `h` = nearest **even** integer to `N * in_h / in_w` (not floor-to-even). |
| `scale` | both `w`/`h` resolve to `-1`/`-2` | Falls back to the input size unchanged (no anchor to compute from). |
| `scale` | any resize | `sar_new = sar_old * (in_w*out_h)/(in_h*out_w)`, unconditionally — DAR is always preserved, not just for `-1`/`-2`. |
| `scale` | absurd size (`w=h=99999999`) | Refused by `vaco_frame::FramePool`'s default 1 GiB live-byte budget before any allocation is attempted (`tests::an_outrageous_size_is_refused_by_the_frame_pool_not_attempted`). |
| `crop` | `x`/`w` on a 4:2:0 format | Both **floored** to the nearest multiple of 2, independently, *before* cropping — not rounded, not rejected. Verified with a `geq`-tagged image; see `crop.rs`'s doc for the exact bytes. |
| `crop` | default `x`/`y` | `(in_w-out_w)/2` / `(in_h-out_h)/2` — auto-centred, not `0`. |
| `pad` | default `color` | Limited-range black (`Y=16, Cb=Cr=128`) for YUV destinations, `(0,0,0)` for RGB — not `Y=0`. |
| `hflip`/`vflip` | applied twice | Identity, exercised as a graph-level round-trip test (`tests_invariants.rs`) and a `proptest` on the byte-reversal primitive. |
| `transpose` | `dir=cclock_flip` (the **default**) | The *plain* matrix transpose (`N[r][c]=O[c][r]`) — not a rotation at all. |
| `transpose` | `dir=clock` | `hflip` of the plain transpose. |
| `transpose` | `dir=cclock` | `vflip` of the plain transpose. |
| `transpose` | `dir=clock_flip` | `vflip(hflip(...))` of the plain transpose — a 180° turn of it. |
| `transpose` | 4:2:2 and other asymmetric-subsampling formats | Refused with `Error::Unsupported` rather than silently producing a mismatched buffer — see `transpose.rs`'s doc for why. |

## How to change it

- Add a filter: follow an existing module's shape (`DESC`, an `Opts` struct
  if it takes options, a filter type, `create`), declare the module in
  `lib.rs`, add a `[[component]]` entry to `vaco-component.toml`, wire the
  name into `registry.rs`, then run `cargo xtask gen-registry`.
- Keep new option/filter/state types `pub(crate)` — see `vaco-filter-audio`'s
  doc for the `dup-check` rationale, which applies here too.
- To change how bytes move (a new geometry filter, or a faster path), start
  in `geom.rs`; every filter here depends on `plane_unit_bytes` reading the
  format table correctly.

## Configuration

Options are declared with `#[derive(vaco_opts::Options)]` and parsed via
`OptionsExt::set_from_string(args, "=", ":")`, except `scale`, which reads
its `w`/`h`/`size`/`s` presence directly from `Instantiate::named`/
`positional` — see `scale.rs`'s doc for why (the reference's asymmetric
"`w` alone errors, `h` alone doesn't" behaviour cannot be expressed as a
single option default). Defaults and option names were captured with
`LC_ALL=C ffmpeg -h filter=<name>` against ffmpeg 8.1.

## Dependencies

`vaco-filter-core` (the `Filter` trait, `Simple`/`Sourced` adapters,
negotiation), `vaco-filter-graph` (`FilterRegistry`/`Instantiate`),
`vaco-scale` (`scale`'s resampling and `pad`'s fill colour conversion),
`vaco-expr` (`w`/`h`/`x`/`y` expressions), `vaco-pixfmt`, `vaco-color`,
`vaco-frame`, `vaco-opts`, `vaco-core`.
