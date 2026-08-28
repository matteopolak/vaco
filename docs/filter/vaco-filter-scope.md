# vaco-filter-scope

T3 measurement/visualisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Three implemented:
`histogram`, `waveform`, `datascope`.

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

This crate implements three of those twelve. The rest are excluded or
deferred, each for a distinct, specific reason (see below) rather than a
blanket "out of time".

## What it is

One module per filter (`src/{histogram,waveform,datascope}.rs`), each
exposing `pub const DESC: FilterDesc` and a crate-private `fn create`,
aggregated by
[`registry::ScopeRegistry`](../../crates/filter/vaco-filter-scope/src/registry.rs)
— the same shape every other T2/T3 filter crate in this project uses.
`src/common.rs` carries the small 8-bit-plane helpers this whole filter
family carries its own copy of (D19 governs shared *types*, not these tiny
per-crate predicates). `src/font8x8.rs` carries the embedded bitmap font
`datascope` (and, next, `pixscope`/`oscilloscope`) draws text with — see
its own doc comment and the "The bitmap-font hypothesis" section below.

`histogram` and `waveform` are **converters**, not passthrough filters:
they always synthesise a new picture (fixed dimensions, fixed `Gray8`
pixel format) rather than transforming the input in place, so both use
`NodeFormats::converter` and a `configure` hook that rewrites the output
`LinkFormat`'s width/height — the same shape `vaco-filter-video-geometry::scale`
uses for the same reason (output geometry the negotiation engine must know
about, decided from the input format rather than fixed at parse time).
`datascope` is different again: measured directly against the reference,
its output *pixel format* always matches the input's own (only the
dimensions are independent, fixed by the `size` option), so it uses
`NodeFormats::passthrough` with the same `configure`-time width/height
override — the shape `vaco-filter-video-geometry::crop` uses.

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

### `datascope`

Draws each sample's raw value as text on a fixed grid. Measured directly
(`ffmpeg 8.1`, `-bitexact`, hand-built `rawvideo`, an all-black and an
all-white `32x32` `gray` source, and a synthetic per-column gradient):

- Output size is exactly the `size`/`s` option, independent of the
  input's own dimensions; the pixel *format* passes through unchanged
  (`gray` in, `gray` out; `yuv420p` in, `yuv420p` out with chroma forced
  to the neutral `128`).
- The canvas is **not** a copy or crop of the source: an all-white source
  produces the identical `0` background an all-black one does, everywhere
  outside the glyphs. Every frame starts from a fresh zero-filled canvas.
- Grid cell `(row, col)` displays the source sample at `(x_option + col,
  y_option + row)`, confirmed with a gradient source (`value = 10*x mod
  256` per column) read off left-to-right in both `format=hex` and
  `format=dec`, and confirmed again that `x=2` shifts which two source
  columns disappear from the front of the sequence rather than cropping
  the canvas.
- Digits sit on an exact `8`-pixel intra-number pitch in both formats;
  `format=hex` cells sit on a `20`-pixel pitch, `format=dec` on `30`.

Implemented for `mode=mono` only. Not implemented: `mode=color`/`color2`;
`axis`; `opacity` (no visible effect could be isolated against an
already-solid-black canvas — plausibly reserved for `axis` mode or for
compositing via `overlay`, neither measured here); RGB pixel formats;
`components` beyond plane 0; bit depths above 8.

**This filter can never be framecrc-identical to the reference**, on any
input, no matter how exactly the rest of its behaviour is measured — see
"The bitmap-font hypothesis" below.

## The bitmap-font hypothesis (resolved: held)

`oscilloscope`/`datascope`/`pixscope` were previously blocked on the same
prerequisite as `drawtext`: a working `TextRenderer` (fontdb, shaping,
glyph cache — GitHub `FT-3.5`/#462), itself blocked on a `rustybuzz`
provenance question under D10. The hypothesis: these three do not need
that stack — the reference draws them with a small, compiled-in,
fixed-width bitmap font instead, no shaping, no font file. It held,
checked two ways:

1. `ffmpeg -h filter=datascope`, `-h filter=pixscope` and `-h
   filter=oscilloscope` (`ffmpeg 8.1`) expose no font/fontfile/fontsize
   option on any of the three.
2. Pixel-dumping both `datascope`'s and `pixscope`'s rendered text (an
   all-black and an all-white synthetic source through each) shows crisp,
   non-antialiased glyphs on an exact pixel grid pitch — the signature of
   a blitted bitmap table, not a shaped/antialiased font renderer —
   and `pixscope`'s statistics-overlay glyphs visually match `datascope`'s
   digit family (same font, same mechanism, two filters).

This is a materially smaller prerequisite than #462 in full and does not
touch that issue's `rustybuzz` question at all. A bitmap font's glyph
table is itself authorial data under D7, so the reference's own table
cannot be transcribed. `src/font8x8.rs` transcribes glyphs instead from
[Unscii](https://github.com/viznut/unscii), an independent, unrelated,
Public Domain bitmap font project (`fontfiles/unscii-8.hex`, the plain
8x8 variant — not the GPL-Unifont-derived `-full` variant), registered as
source `unscii-8-font` in `provenance/sources.toml` and as a `[[table]]`
entry in `provenance/vaco-filter-scope.toml`. Its glyph shapes visibly
differ from the reference's own font wherever compared (its `'0'` uses a
different internal stroke gap) — expected and desired, since it confirms
this is a different, independently-sourced table rather than a disguised
transcription of the one this tree may not read.

The permanent consequence: because the glyph shapes differ from the
reference's, no frame containing this font's text can ever match the
reference byte-for-byte. That is a structural ceiling on verifiability,
not a bug — the same shape of permanent exclusion `hqx` already
documents in `vaco-filter-artistic` for *implementability*, here for
*framecrc comparability* instead.

A comment to this effect belongs on #462: two separate packages
(`drawtext`'s shaped-text stack, and this family's fixed-width blit) were
sharing one blocked issue, and only one of them is actually blocked on
the `rustybuzz` question.

## Left out, each for a distinct reason

| Filter | Why |
|---|---|
| `thistogram` | Attempted, not shipped. The output shape (`width x 256`, `width` a temporal window) was measured, but the temporal-buffering semantics (which column a given frame lands in as the window scrolls) were not pinned down with enough confidence to ship rather than guess. |
| `vectorscope` | Attempted, not shipped. Output shape (`256x256`) confirmed, but `vectorscope` has no `intensity` option the way `waveform` does — a different, unmeasured accumulation rule, not assumed to match `waveform`'s. |
| `oscilloscope`, `pixscope` | Not shipped this pass. The bitmap-font blocker is resolved (see above — both confirmed to use the same embedded-font mechanism `datascope` now implements), so these are next in this crate's queue rather than blocked on a shared missing dependency. |
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
| `datascope` | `s=64x32:mode=mono:format=hex:components=1` | `gray`, all-`0`/all-`0xFF` `32x32` | **structural, not exact** — digit values (`00`/`FF`), canvas-is-always-black, and grid pitch all confirmed; text pixels can never match byte-for-byte (independent font, see above) |
| `datascope` | `s=180x40:mode=mono:format=dec:components=1` | `gray`, 10x-per-column gradient | **structural, not exact** — decimal formatting and left-to-right column sequence confirmed; same permanent text-pixel ceiling |
| `datascope` | `x=2:y=0`, same gradient | `gray` | **structural, not exact** — `x` offset shifts the sampled source column, confirmed |

## How to change it

- `histogram`/`waveform` follow the same shape: an `Opts` struct via
  `vaco_opts::Options`, a `configure` hook fixing output geometry, a
  `filter_frame` doing the actual per-pixel accumulation, and a `create`
  function using `NodeFormats::converter` (they do not preserve the
  input's format or dimensions). `datascope` uses `NodeFormats::passthrough`
  instead (it preserves the input's pixel format, only its dimensions are
  independent) — see `crop.rs` in `vaco-filter-video-geometry` for the
  same shape used elsewhere.
- If you add `pixscope` or `oscilloscope`, start from `src/font8x8.rs` —
  it is already sourced, registered and tested; do not re-derive or
  re-source a font. Re-measure each filter's own layout (cell/margin
  positions, what text it draws) independently; do not assume
  `datascope`'s pitch constants apply verbatim.
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
