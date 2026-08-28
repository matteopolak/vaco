# vaco-filter-scope

T3 measurement/visualisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Two implemented:
`histogram`, `waveform`.

## Scope reconciliation

#480's own `Crate(s): vaco-filter-*` field is a roadmap-era wildcard, the
same shape that misled #478's dispatch toward a crate name that did not
exist. Before writing any code, `planning/ASSIGNMENTS.md`, every sibling
`FT-4.12*`/`FT-4.11` GitHub issue and the generated registry were checked
(posted as a comment on #480 first):

| What | Where it actually belongs |
|---|---|
| Palette (`palettegen`, `paletteuse`, `elbg`), stack (`hstack`, `vstack`, `xstack`), the "overlay family" (`blend`, `xfade`, `mix`, `multiply`, `xmedian`, `displace`, `remap`, `feedback`) | `#111` (FT-4.11, T2, separate open issue) — not claimed here |
| `scale2ref`, `colorspace`, `colordetect`, `pixdesctest`, `zoompan` | The T1-tier remainder of `vaco-filter-scale`'s row, orphaned by #463 (FT-4.1a)'s narrower scope — a real gap, but not T3 and not ticketed under #480 |
| `histogram`, `thistogram`, `waveform`, `vectorscope`, `oscilloscope`, `datascope`, `pixscope`, `ciescope`, `graphmonitor`, `agraphmonitor`, `drawgraph`, `adrawgraph` | `planning/16-filters.md`'s `vaco-filter-scope` row — the actual unclaimed T3 remainder, confirmed absent from every `vaco-component.toml` and the generated registry |

This crate implements two of those twelve. The rest are excluded or
deferred, each for a distinct, specific reason (see below) rather than a
blanket "out of time".

## What it is

One module per filter (`src/{histogram,waveform}.rs`), each exposing `pub
const DESC: FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ScopeRegistry`](../../crates/filter/vaco-filter-scope/src/registry.rs)
— the same shape every other T2/T3 filter crate in this project uses.
`src/common.rs` carries the small 8-bit-plane helpers this whole filter
family carries its own copy of (D19 governs shared *types*, not these tiny
per-crate predicates).

Both filters are **converters**, not passthrough filters: they always
synthesise a new picture (fixed dimensions, fixed `Gray8` pixel format)
rather than transforming the input in place, so both use
`NodeFormats::converter` and a `configure` hook that rewrites the output
`LinkFormat`'s width/height — the same shape `vaco-filter-video-geometry::scale`
uses for the same reason (output geometry the negotiation engine must know
about, decided from the input format rather than fixed at parse time).

### `histogram`

A per-value bar chart. Output is always `256` pixels wide (one column per
8-bit value) and `level_height (+ scale_height)` tall. Measured directly
against the reference (`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo`):

```text
bar_height = ceil(count[v] / max(count) * level_height)
column v lit (255) for rows [level_height - bar_height, level_height)
```

Confirmed at two different count ratios (`1/1` trivially, and `1/3` twice
with different absolute counts) that the rounding is `ceil`, not `round` —
a `1/3` ratio at `level_height=100` gives a `34`-row bar, not `33`.
`scale_height` rows are a plain horizontal gradient (column `x` reads back
byte value `x`), checked directly rather than assumed from the option's
name.

Not implemented: `levels_mode=logarithmic`; `display_mode=overlay`/`parade`
(only `stack` is implemented, and multi-plane `stack` — verified only for
the single-plane case — stacks each selected plane's own `level_height`
block, the mode's documented meaning but not separately re-probed per
plane); bit depths above 8.

### `waveform`

The classic column-mode waveform monitor. Output is `(source width) x
256`. Measured directly:

```text
for each source pixel (x, y) with value v:
    output(x, 255 - v) += intensity * 255      // mirror=true, the default
```

Confirmed the `255 -` inversion (a probe with distinct per-row values found
hits at row `255-v`, not row `v`) and confirmed the accumulation is
genuinely additive, not "any hit lights the pixel": a column with four hits
at one value reads four times a column with one hit at that value (`40`
versus `10`, at the default `intensity=0.04`). Whether hits are summed as
floats and truncated once, or truncated individually and then summed, was
not distinguished — both give the same integer result at the magnitudes
tested, and this crate did not chase the distinction further.

Not implemented: `mode=row`; `mirror=false`; `display=overlay`/`parade`;
bit depths above 8; non-luma planes.

## Left out, each for a distinct reason

| Filter | Why |
|---|---|
| `thistogram` | Attempted, not shipped. The output shape (`width x 256`, `width` a temporal window) was measured, but the temporal-buffering semantics (which column a given frame lands in as the window scrolls) were not pinned down with enough confidence to ship rather than guess. |
| `vectorscope` | Attempted, not shipped. Output shape (`256x256`) confirmed, but `vectorscope` has no `intensity` option the way `waveform` does — a different, unmeasured accumulation rule, not assumed to match `waveform`'s. |
| `oscilloscope`, `datascope`, `pixscope` | Not attempted: all three render text (pixel values, axis labels, or trace statistics) into the frame, and this tree has no text-rendering primitive yet (`FT-3.5`/`TextRenderer` is still an open issue) — a shared missing dependency, not a per-filter gap. |
| `graphmonitor`, `agraphmonitor` | **Not expressible against the current `vaco-filter-core` surface**, checked directly: `FilterContext` exposes only the current node's own pads — there is no API to enumerate other nodes, their links, or queue depths, which is exactly what these filters draw. Recorded as `planning/INTERFACE-GAPS.md` gap 22. |
| `ciescope` | **Not a D7 case.** Every `system` value names a published international-standard primary set (BT.709, BT.2020, DCI-P3, SMPTE-C, …) and the CIE 1931 observer data is public. The blocker is reproducing the reference's exact chromaticity-diagram *rendering* (spectral-locus rasterisation, anti-aliasing, gamut-triangle lines) — not itself specified by any colorimetry standard, so verifying it would need extensive black-box probing this pass's time did not cover. |
| `drawgraph`, `adrawgraph` | Not attempted. These plot frame metadata rather than pixel data (the metadata mechanism itself exists in this tree — gap 11's dictionary, gap 13's console-log channel, both closed elsewhere), but connected-line/bar/dot rendering exactness is a real question `waveform` sidestepped by drawing independent per-pixel hits. Deferred for time, not blocked. |

## Framecrc comparison table

Same loop as every other filter crate in this project: `ffmpeg -bitexact -f
lavfi -i <deterministic source> -vf "<filter>=<args>" -f rawvideo -pix_fmt
<fmt> -` against the reference, cross-checked against this crate's pure
functions and pinned into unit tests. No `vaco` CLI/muxer exists yet to
drive an actual `-f framecrc` invocation (`planning/14-cli.md` is still a
plan document).

| Filter | Args | Source | Result |
|---|---|---|---|
| `histogram` | `level_height=50:scale_height=0:components=1` | `gray`, 16x16 flat @128 | **exact** — single bin, full height |
| `histogram` | `level_height=100:scale_height=0:components=1` | `gray`, count ratio 12:4 | **exact** — `ceil` scaling confirmed |
| `histogram` | `level_height=100:scale_height=0:components=1` | `gray`, count ratio 3:1 | **exact** — second ratio, rules out coincidence |
| `histogram` | `level_height=50:scale_height=10:components=1` | any | **exact** — gradient scale bar |
| `waveform` | `mode=column:intensity=1` | `gray`, per-row distinct values | **exact** — mirror inversion confirmed |
| `waveform` | `mode=column:intensity=0.04` (default) | `gray`, 1-hit vs 4-hit columns | **exact** — additive accumulation confirmed |

## How to change it

- Both filters follow the same shape: an `Opts` struct via
  `vaco_opts::Options`, a `configure` hook fixing output geometry, a
  `filter_frame` doing the actual per-pixel accumulation, and a `create`
  function using `NodeFormats::converter` (not `passthrough` — these
  filters do not preserve the input's format or dimensions).
- If you add `thistogram` or `vectorscope`, re-read this doc's "left out"
  table first — both have real measurement started, recorded in `git`
  history and in this file, that a fresh attempt should build on rather
  than repeat.
- `graphmonitor`/`agraphmonitor` need a real `vaco-filter-core` capability
  (cross-node link/queue introspection) before they are attempted again —
  see `planning/INTERFACE-GAPS.md` gap 22 for what specifically is missing.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`. No external crate beyond what the workspace already
declares; no new dependency was added to the workspace's `Cargo.toml`.
