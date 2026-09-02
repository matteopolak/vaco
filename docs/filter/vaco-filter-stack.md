# vaco-filter-stack

Multi-input video stacking filters — `planning/16-filters.md` §4.2's
`vaco-filter-stack` row, GitHub issue #111 (FT-4.11)'s real unclaimed
remainder. Three implemented: `hstack`, `vstack`, `xstack`.

## Scope reconciliation

#111's title mentions "palette/GIF, stack, overlay family, temporal", but
its own `Crate(s):` field names only `vaco-filter-palette` and
`vaco-filter-stack`. Checked against `ffmpeg -hide_banner -filters`, plan
16 §4.2, `planning/ASSIGNMENTS.md` and the generated registry before any
code (posted as a comment on #111 first):

| What | Where it actually stands |
|---|---|
| `temporal` | Already fully shipped — `vaco-filter-temporal`, #475 |
| `overlay` (the literal filter) | Already fully shipped — `vaco-filter-video-composite`, #465, built on `vaco-filter-framesync` |
| `hstack`, `vstack`, `xstack` | The real, unclaimed remainder — this crate |
| `palettegen`, `paletteuse`, `elbg` | The real, unclaimed remainder — sibling `vaco-filter-palette` crate |
| `latticepal` | Listed in plan 16's row, but `ffmpeg -h filter=latticepal` on the installed reference returns `Unknown filter` — a D6/D17 unverifiability exclusion, not a D7 authorial-data one. Not attempted. |
| `showpalette` | A real `ffmpeg` filter absent from every plan-16 row — a genuine unticketed gap, not claimed here. |
| `blend`, `xfade`, `mix`, `multiply`, `xmedian`, `displace`, `remap`, `feedback` | Plan 16 assigns these to a never-created `vaco-filter-overlay` crate — real, unclaimed T1/T2 work, likely what #111's title means by "overlay family" beyond the literal filter. Flagged, not built this pass (priority order was stack, then palette). |

## No framework gap: `vaco-filter-framesync` already fits

`vaco-filter-framesync/src/opts.rs` documents `FsInput::uniform` as built
specifically for "`hstack`, `vstack`, `maskedmerge`" — every input drives,
none is a secondary. Its measured defaults (`eof_action=repeat`,
`shortest=false`) match this family's own measured default behaviour
exactly (see below), so all three filters are built as
`vaco_filter_framesync::FrameSyncFilter`s wrapped in
`vaco_filter_framesync::Synced` — the same shape
`vaco-filter-video-composite::overlay` uses, not the `Paired` adapter gap
10 added (`Paired` cannot express `eof_action=repeat`, the same reason
`overlay` itself was not ported onto it, and this family's default
behaviour depends on exactly that). No framework gap was found; none
needed filing.

## What it is

One module per filter (`src/{hstack,vstack,xstack}.rs`), each exposing
`pub const DESC: FilterDesc` and a crate-private `fn create`, aggregated
by `registry::StackRegistry`. `src/common.rs` carries a small
per-crate `ensure_addressable` (rejects hardware/bitstream/palette
formats, but — unlike this project's pixel-*math* filter crates — does
not restrict to 8-bit, since concatenating rows is a pure byte move that
works at any bit depth the plane API can address).

All three use dynamic-arity pads via
`vaco_filter_graph::registry::pads::video(n)` (capped at
`pads::MAX == 64` — a real, structural framework limit, distinct from the
reference's own `2..=INT_MAX` range, stated plainly rather than silently
truncated) and `NodeFormats::passthrough(n, 1, MediaType::Video, ..)`,
which ties every input's and the output's pixel format together — the
negotiator's own repair step splices in a conversion filter if two
sources start out in different formats, so `on_event` can assume every
frame it sees already shares one format.

### `hstack`

`ffmpeg -h filter=hstack`: `inputs` (`2..=INT_MAX`, default `2`),
`shortest` (bool, default `false`). Measured directly (`ffmpeg 8.1`,
`-bitexact`, hand-built `rawvideo`/`lavfi` sources):

- Output width is the exact sum of every input's width. Output height
  must be the *same* across every input, or the reference refuses to
  configure at all ("height does not match") — not a resize, not a crop.
- `shortest=false` (the default) continues to the *longest* input's
  length, freezing each shorter input's last frame; `shortest=true` ends
  at the shortest input's length. Confirmed with a 1-frame and a
  `loop`-extended 5-frame input: `5` output frames at the default, `1`
  with `shortest=true` — exactly `FrameSyncOpts`'s own
  `eof_action=Repeat` default, reached for free through `FsInput::uniform`
  rather than reimplemented.

### `vstack`

The same shape, rotated: output height is the sum of input heights,
width must match. `ffmpeg`'s own `-h filter=vstack` shares one
`"(h|v)stack AVOptions"` block with `hstack`, and both options were
confirmed to behave identically.

### `xstack`

`ffmpeg -h filter=xstack`: `inputs`, `layout`, `grid` (`<image_size>`),
`shortest`, `fill`. Measured:

- With neither `layout` nor `grid` given, the reference only accepts the
  default `inputs=2` case, laid out exactly like `hstack`. `inputs=4`
  with neither option given is a hard `configure` error, not a guessed
  default grid.
- `grid=COLSxROWS` arranges inputs in row-major (raster) order: input `i`
  goes to cell `(i % cols, i / cols)`. Confirmed with a `2x2` grid and
  four distinct flat values landing top-left/top-right/bottom-left/
  bottom-right in that order, each cell its own input's size.

Not implemented: the free-form `layout=` string (plan 16's own "shared
layout parser" dependency — a small expression language for per-input
`x_y` position strings) — `create` rejects it with a clean error rather
than guessing at a parser. `fill` (colour for unmatched grid cells) is
not implemented either: this module requires `inputs == cols * rows`
exactly, and requires every cell in a column/row to share that
column's/row's size — a genuinely mixed-size grid was not measured.

## Framecrc comparison table

| Filter | Args | Source | Result |
|---|---|---|---|
| `hstack` | `inputs=2` | two `yuv420p` inputs, matching height, differing width | **exact** — output width is the sum, confirmed |
| `hstack` | `inputs=2`, mismatched height | two `yuv420p` inputs | **exact** — reference's own `configure` error reproduced |
| `hstack` | `inputs=2:shortest=false` (default) | 1-frame + 5-frame (`loop`-extended) | **exact** — `5` output frames, freeze-last-frame confirmed |
| `hstack` | `inputs=2:shortest=true` | same two inputs | **exact** — `1` output frame |
| `vstack` | `inputs=2`, mismatched width | two `yuv420p` inputs | **exact** — same-shape rotation of `hstack`'s rule |
| `xstack` | `inputs=4:grid=2x2` | four distinct flat-value `8x8` `gray` inputs | **exact** — raster placement (top-left/top-right/bottom-left/bottom-right) confirmed |
| `xstack` | `inputs=4`, no `layout`/`grid` | any | **exact** — reference's own `configure` error reproduced |

No `vaco` CLI/muxer exists yet to drive an actual `-f framecrc`
invocation; comparisons
are against the reference's raw pixel output and cross-checked against
this crate's own tests.

## How to change it

- All three filters follow the same shape: an `Opts` struct via
  `vaco_opts::Options`, a `FrameSyncFilter` impl (`inputs`/`opts`/
  `configure`/`on_event`), and a `create` function building dynamic pads
  via `pads::video(n)` and wrapping the filter in `Synced::new`.
- If you implement `xstack`'s `layout=` string, it needs its own small
  parser (per-input `x_y` position expressions referencing other inputs'
  `w`/`h`) — plan 16 calls this out as a distinct dependency
  ("shared layout parser"), not something to improvise inline.
- The `blend`/`xfade`/`mix`/`multiply`/`xmedian`/`displace`/`remap`/
  `feedback` group (plan 16's `vaco-filter-overlay` row) is real,
  unclaimed work this crate does not touch — see the scope
  reconciliation table above.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync`. No external crate beyond
what the workspace already declares; no new dependency was added to the
workspace's `Cargo.toml`.
