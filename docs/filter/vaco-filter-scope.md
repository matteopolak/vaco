# vaco-filter-scope

T3 measurement/visualisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Six implemented:
`histogram`, `waveform`, `datascope`, `thistogram`, `graphmonitor`,
`agraphmonitor`.

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

This crate implements five of those twelve (`graphmonitor`/`agraphmonitor`
count as one pair). The rest are excluded or deferred, each for a distinct,
specific reason (see below) rather than a blanket "out of time".

## What it is

One module per filter (`src/{histogram,waveform,datascope,graphmonitor}.rs`
— `graphmonitor.rs` covers both `graphmonitor` and `agraphmonitor`, since
the only difference is the input pad's media type), each exposing
`pub const DESC: FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ScopeRegistry`](../../crates/filter/vaco-filter-scope/src/registry.rs)
— the same shape every other T2/T3 filter crate in this project uses.
`src/common.rs` carries the small 8-bit-plane helpers this whole filter
family carries its own copy of (D19 governs shared *types*, not these tiny
per-crate predicates) — including, as of this pass, the font-blit helpers
(`draw_glyph`/`draw_text`) `datascope` and `graphmonitor` both need,
moved there from `datascope`'s own module when a second consumer showed
up. `src/font8x8.rs` carries the embedded bitmap font `datascope` and
`graphmonitor`/`agraphmonitor` draw text with (and, if shipped,
`pixscope`/`oscilloscope` would too) — see its own doc comment and the
"The bitmap-font hypothesis" section below.

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

### `thistogram`

A per-value histogram plotted as one new column per frame — a scrolling
history of the frame's own value distribution. Output is `width x 256`
(`width=0`, the default, means "use the input's own width", the same
sentinel-by-observed-behaviour shape as `nullsrc`'s `duration`). Unlike
`histogram`/`waveform`, this filter is **stateful**: it keeps a persistent
canvas and draws exactly one new column per frame, confirmed with a
4-frame, `w=2` sequence where earlier frames' columns are still present,
unchanged, in later frames' output.

```text
column = frame_count % width
intensity[v] = round(count[v] / max(count) * 255)   // round, not histogram's ceil
row v = 255 - v
```

Two `slide` modes are measured and implemented:

- `replace` (the default): overwrites only `column`, leaving every other
  column exactly as it was — a plain ring buffer.
- `frame`: same `column` indexing, but the *entire canvas* is cleared
  immediately before drawing whenever `column == 0` (a wraparound) — a
  4-frame probe at `w=2` shows column `1`'s frame-1 data vanish entirely
  from the output once frame `2` wraps back to column `0`.

Pinned three ways that the intensity rule is `round`, not `ceil`
(`histogram`'s own rule) or plain truncation: a `56`-of-`200` ratio gives
`71` (`ceil` would give `72`); a `3`-of-`8` ratio (`0.625`) gives `96`
(truncation would give `95`); an exact `1`-of-`2` tie gives `128`
(truncation would give `127`).

Not implemented: `slide=scroll`/`rscroll`/`picture` (`create` rejects
them with a clean error rather than silently behaving like `replace`);
`display_mode=overlay`/`parade`; `levels_mode=logarithmic`; `envelope`;
`components` beyond plane `0` (forces `Gray8` output, like
`histogram`/`waveform`); bit depths above 8.

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

### `graphmonitor`/`agraphmonitor`

Draws the *live filtergraph's own* node and link state as text — the real
consumer that proves `planning/INTERFACE-GAPS.md` gap 22 closed rather
than merely asserting it: `FilterContext::graph_nodes`/`graph_links`
(`vaco-filter-core`) exist because these two filters need them, and this
crate wiring them up with a real, passing `Graph`-based test
(`tests/graphmonitor.rs`) is what settles that they actually serve the two
filters that asked for them.

Measured directly (`ffmpeg 8.1`, real filtergraphs, pixel-dumped):
output is always `rgb24` at exactly the `size`/`s` option's dimensions
(a converter, like `histogram`, not a passthrough); the canvas is redrawn
from scratch every rendered frame; output is **rate-gated**, not
one-frame-in-one-frame-out (a `10fps` source through
`graphmonitor=rate=2` for `2s` produced `5` frames, not `20`, confirmed
independently for `agraphmonitor` against a `48kHz` sine); the picture is
one block per graph node — **including the monitor's own node**, and the
scheduler's own auto-inserted nodes (buffer sources, buffersinks, format
converters) — each block a header line (`"{label} {filter_name}"`) then
one line per pad, inputs before outputs, naming the peer node and a live
counter; and the **inter-line pitch is not one constant**: `10`px from a
header to its first pad line and between two consecutive pad lines of the
same direction, `12`px on the one transition from the last input line to
the first output line, `15`px from a block's last line to the next
block's header — measured by cropping an 8-block, 24-line render and
finding the same three numbers repeat exactly, block after block, and
implemented exactly rather than approximated (unlike `datascope`'s own
margin arithmetic, this one had to be chased to the pixel, because the
line count here varies with the graph rather than being a fixed grid).

Implements `size`/`s`, `rate`/`r`, and (via `FilterDesc`) the `V->V` /
`A->V` shape difference between the two names. Deliberately **not**
implemented, split by *why*:

- **Cannot be, because `NodeView`/`LinkView` genuinely do not carry the
  data** — `format`/`size`/`rate`/`timebase` (link geometry and timing are
  outside gap 22's deliberately narrow snapshot) and `pts`/`pts_delta`/
  `time`/`time_delta` (`LinkStats` counts frames and samples but never
  records a timestamp *value*). This is itself a finding about gap 22's
  own scope, not a time-boxing choice — see `vaco-filter-core`'s own
  `context.rs` doc for the design note this traces back to.
- **Available, but a scope choice not to draw it**: the reference's
  `frame_count_in`/`out`/`delta` (and `sample_count_*`) are a genuine
  *pair* of counters (arrived-at-source vs. consumed-at-destination);
  `LinkStats` keeps one post-dequeue counter, so the exact three-field
  shape isn't reproducible without also tracking a push-side count this
  crate does not keep. What is drawn instead uses every other field
  `LinkView`/`LinkStats` carries (queue depth/capacity, `at_eof`, the
  one-sided frame/sample count, peak depth, backpressure-blocked count) —
  more of gap 22's own surface than the reference's *default* `flags`
  selection shows, since no rendering here can match the reference
  byte-for-byte regardless of which fields are picked.
- `mode=compact`/`nozero`/`noeof`/`nodisabled` (only the default,
  `full`-shaped listing); `opacity` (solid-black canvas, same
  unimplemented-effect choice `datascope`'s own `opacity` already made);
  colour (`Gray8` output, not the reference's `rgb24` — no field this
  module draws needs a second colour to distinguish it, and matching the
  pixel format buys nothing once text already rules out a byte-exact
  frame).

**Cannot be framecrc-identical to the reference**, for the same permanent
reason as `datascope` — see "The bitmap-font hypothesis" below.

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
| `vectorscope` | **Partially cracked, not shipped.** The coordinate mapping is fully measured: `x = component_x` directly, `y = 255 - component_y`. The intensity accumulation is confirmed nonlinear (independent of frame size — same hit count gives the same output at both `100` and `10000` total pixels) but does not fit any single-parameter model tried (linear, `ceil`/`round`/floor, power law, exponential-IIR); reported as characterised, not shipped, rather than guessed. |
| `pixscope` | **Substantially re-characterised this pass; a prior finding corrected.** A previous pass reported the "zoom" window as a plain, unmagnified location marker. That was wrong, or measured under a condition (an all-black source) that could not have shown otherwise: against a real striped/checkerboard source at the reference's documented `640x480` minimum resolution (smaller inputs are refused — `"min supported resolution is 640x480"`, also newly found), the window **does magnify** — a `7x7` (default `w`/`h`) source region blown up to a crisp, non-antialiased `~294x294`px block grid (`294 / 7 = 42`px per source pixel exactly), sitting above the stats panel. The panel itself is now fully read (not guessed — a label is UI text on a rendered frame, not the reference's source or its font bitmap, so reading it off a pixel dump is the same black-box measurement this whole project is built on): two 4-line groups, `"CH   AVG   MIN   MAX   RMS"` then one row per active channel (`Y`/`U`/`V` for a YUV source, colour-coded — white/blue/red respectively, matching each channel's own colour), and a second `"CH   STD"` group with the same per-channel rows. Number formats read off directly: `AVG`/`RMS` are `%05.1f`-shaped (`"00016.0"`), `MIN`/`MAX` are `%05d` (`"00016"`, no decimal), `STD` is `%04.2f`-shaped (`"0000.00"`) — inferred from digit *count* and decimal-point position, not from decoding the reference's own glyph shapes. **Still not shipped**: the exact AVG/RMS/STD arithmetic (mean, root-mean-square, standard deviation are the plausible read of the labels, but the precise rounding rule was not pinned the way `histogram`'s `ceil` was), the marker-box styling, `wx`/`wy`'s exact placement formula, and RGB-mode channel labels are all unmeasured — a real implementation attempt should start from this panel structure rather than re-deriving it, but still has genuine measurement work ahead of it. |
| `oscilloscope` | Briefly probed this pass, not shipped. Confirmed to share the same font mechanism in principle (no font option), and confirmed its trace/grid rendering (`g=1` draws a plain grid, each enabled component (`c`, default `7`) traces as a distinct-coloured connected line across the trace box, sitting at partial opacity — the source's own diagonal tilt-strip pattern is visibly bleeding through beneath it). `st=1`'s statistics text was **not located** in two probes (default and enlarged trace geometry) — unlike `pixscope`, where widening the canvas past the reference's `640x480` floor immediately revealed the whole panel, oscilloscope's stats did not appear at any tried size; it may need a specific `sc`/`sc`+`st` combination, a non-flat source, or several accumulated frames rather than one, none of which this brief pass tried. |
| `ciescope` | **Not a D7 case.** Every `system` value names a published international-standard primary set (BT.709, BT.2020, DCI-P3, SMPTE-C, …) and the CIE 1931 observer data is public. The blocker is reproducing the reference's exact chromaticity-diagram *rendering* (spectral-locus rasterisation, anti-aliasing, gamut-triangle lines) — not itself specified by any colorimetry standard, so verifying it would need extensive black-box probing this pass's time did not cover. |
| `drawgraph`, `adrawgraph` | Not attempted. These plot frame metadata rather than pixel data (the metadata mechanism itself exists in this tree — gap 11's dictionary, gap 13's console-log channel, both closed elsewhere), but connected-line/bar/dot rendering exactness is a real question `waveform` sidestepped by drawing independent per-pixel hits. Deferred for time, not blocked. |

## Framecrc comparison table

Same loop as every other filter crate in this project: `ffmpeg -bitexact -f
lavfi -i <deterministic source> -vf "<filter>=<args>" -f rawvideo -pix_fmt
<fmt> -` against the reference, cross-checked against this crate's pure
functions and pinned into unit tests. No `vaco` CLI/muxer exists yet to
drive an actual `-f framecrc` invocation (`planning/14-cli.md` is still a
plan document).

**This row now has a permanent split, not just a temporary one.**
`histogram`, `waveform` and `thistogram` draw no text, so nothing rules
out framecrc-exactness for them — `thistogram` reaching it this pass
closes that question for every non-text filter this crate is likely to
implement. `datascope`, `graphmonitor`/`agraphmonitor`, and (once shipped)
`pixscope`/`oscilloscope`, draw with an independently-sourced font (D7
forbids transcribing the reference's own table — see "The bitmap-font
hypothesis" above), so no frame containing their text can *ever* match
the reference byte-for-byte. "Framecrc-identical across this crate's
whole corpus" is therefore not a temporarily-unmet goal for the
text-drawing filters — it is provably unreachable, the same way `hqx` is
provably unimplementable in `vaco-filter-artistic`.

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
| `thistogram` | `w=2:components=1:slide=frame`, 4-frame flat-value sequence | `gray` | **exact** — persistent-canvas mechanics confirmed: column advances one per frame, whole canvas clears on wraparound |
| `thistogram` | `w=2:components=1:slide=replace` (default), same 4 frames | `gray` | **exact** — ring-buffer overwrite confirmed: the non-target column survives each new frame unchanged |
| `thistogram` | any, `12`:`4`-style count ratios (`round`, not `ceil`) | `gray` | **exact** — three ratios pinned, including an exact `0.5` tie, ruling out `ceil` and truncation |
| `graphmonitor` | `s=96x32:rate=25`, a real 3-node `Graph` (source → monitor → sink) | any | **structural, not exact** — real `NodeView`/`LinkView` data confirmed reaching the render (`tests/graphmonitor.rs`, a genuine end-to-end `Graph` run, deliberately broken and restored to confirm the test has teeth); text pixels can never match byte-for-byte (independent font) |
| `agraphmonitor` | `s=96x32:rate=25`, a real audio source → monitor → video sink | any | **runs end-to-end** — confirmed the one shape difference from `graphmonitor` (reading `pts`/`time_base` off an audio frame, not a video one) does not need separate logic |

## How to change it

- `histogram`/`waveform`/`thistogram` follow the same shape: an `Opts`
  struct via `vaco_opts::Options`, a `configure` hook fixing output
  geometry, a `filter_frame` doing the actual per-pixel accumulation, and
  a `create` function using `NodeFormats::converter` (they do not
  preserve the input's format or dimensions). `thistogram` additionally
  carries state (`canvas`, `frame_count`) in its `Filter` struct across
  calls — the same pattern `vaco-filter-artistic::amplify` uses for its
  windowed buffer, just a persistent canvas instead of a bounded ring.
  `datascope` uses `NodeFormats::passthrough` instead (it preserves the
  input's pixel format, only its dimensions are independent) — see
  `crop.rs` in `vaco-filter-video-geometry` for the same shape used
  elsewhere.
- If you add `pixscope` or `oscilloscope`, start from `src/font8x8.rs` —
  it is already sourced, registered and tested; do not re-derive or
  re-source a font. Re-measure each filter's own layout (cell/margin
  positions, what text it draws) independently; do not assume
  `datascope`'s pitch constants apply verbatim. **`pixscope` needs a
  source at least `640x480`** (smaller inputs are refused outright) — a
  prior pass's "the zoom box does not magnify" finding was measured
  without knowing this and is superseded: see the `pixscope` section
  above for the corrected panel structure (`CH`/`AVG`/`MIN`/`MAX`/`RMS`
  then `CH`/`STD`, per-channel colour-coded rows, and the number formats
  read directly off a real render) and what is still unmeasured before it
  can ship (the exact statistic formulas, marker styling, `wx`/`wy`
  placement, RGB-mode labels).
- `graphmonitor`/`agraphmonitor` are done; `src/graphmonitor.rs` is the
  template for any future filter that needs `FilterContext::
  graph_nodes`/`graph_links` — the `render()` function is a pure,
  independently unit-tested layout pass over `&[NodeView]`/`&[LinkView]`,
  kept separate from the `FrameFilter` glue specifically so its pixel
  positions can be asserted exactly without decoding rendered glyphs.
- If you add `vectorscope`, its coordinate mapping (`x` direct, `y`
  inverted) is done — reuse it rather than re-measuring. Its intensity
  curve is the open problem: confirmed nonlinear and confirmed
  independent of frame size, but no single-parameter model tried this
  pass (linear, `ceil`/`round`/floor, power law, exponential-IIR) fit the
  measured curve. A fresh attempt should either try a genuinely different
  model shape (e.g. a two-parameter fit, or checking whether the
  reference clips/quantizes the accumulator at some intermediate
  fixed-point width before the final byte conversion) or measure a much
  denser intensity/count grid before proposing one — guessing a formula
  that merely interpolates the points on record here would be worse than
  leaving it open.
## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`. Dev-only: `vaco-sampfmt`, `vaco-chlayout` (both
already workspace crates), needed by `tests/graphmonitor.rs` to build a
real audio source node for the `agraphmonitor` end-to-end test. No
external crate beyond what the workspace already declares; no new
dependency was added to the workspace's `Cargo.toml`.
