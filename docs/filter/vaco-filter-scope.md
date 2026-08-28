# vaco-filter-scope

T3 measurement/visualisation video filters — `planning/16-filters.md` §4.2's
`vaco-filter-scope` row, GitHub issue #480 ("FT-4.12g"). Nine implemented:
`histogram`, `waveform`, `datascope`, `thistogram`, `graphmonitor`,
`agraphmonitor`, `pixscope`, `drawgraph`, `adrawgraph` (the last two via
GitHub issue #473, FT-4.10 — see `vaco-filter-draw-vf`'s own doc for that
issue's full scoping).

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

This crate implements eight of those twelve (`graphmonitor`/`agraphmonitor`
and `drawgraph`/`adrawgraph` each count as one pair). The rest are excluded
or deferred, each for a distinct, specific reason (see below) rather than a
blanket "out of time".

## What it is

One module per filter
(`src/{histogram,waveform,datascope,graphmonitor,pixscope}.rs` —
`graphmonitor.rs` covers both `graphmonitor` and `agraphmonitor`, since
the only difference is the input pad's media type), each exposing
`pub const DESC: FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::ScopeRegistry`](../../crates/filter/vaco-filter-scope/src/registry.rs)
— the same shape every other T2/T3 filter crate in this project uses.
`src/common.rs` carries the small 8-bit-plane helpers this whole filter
family carries its own copy of (D19 governs shared *types*, not these tiny
per-crate predicates) — including, as of this pass, the font-blit helpers
(`draw_glyph`/`draw_text`) `datascope`, `graphmonitor` and `pixscope` all
need, moved there from `datascope`'s own module when a second consumer
showed up. `src/font8x8.rs` carries the embedded bitmap font `datascope`,
`graphmonitor`/`agraphmonitor` and `pixscope` draw text with (and, if
shipped, `oscilloscope` would too) — see its own doc comment and "The
bitmap-font hypothesis" section below.

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

### `pixscope`

Draws a marker box on the source, a magnified view of the pixels under
it, and a live per-channel statistics panel. **Shipped this pass, after
correcting a prior pass's own finding in the open** (recorded in
`planning/INTERFACE-GAPS.md`'s surrounding history, not silently
overwritten): the reference refuses any source smaller than `640x480`
(`"min supported resolution is 640x480"`, undocumented in `-h` and found
only by trying), and the earlier "the zoom window does not magnify"
conclusion was measured on an all-black source below that floor. Above
it, the window **does magnify** — `7x7` (default `w`/`h`) source pixels
blown up to a fixed on-screen `294x294`px block grid, `42`px per source
pixel exactly at the default, confirmed to stay the same on-screen size
at `w=5` too (a different, non-integer per-cell size).

The sampled window's pixel bounds are `round(coord * dimension) - 1`,
spanning `w`/`h` consecutive pixels — not the naive `centre - w/2` a
symmetric box would suggest. Confirmed at three different anchors
(`x=0.5`/`0.25`/`0.1` on an `800`-wide canvas) and two window sizes
(`w=7`, `w=5`, both showing the same constant `-1` bias rather than one
that scales with `w`). At a frame edge the window **clamps**, shifting to
stay entirely on-screen, rather than shrinking to fewer columns or
wrapping to the far edge — confirmed with `x=0` (would need columns
`-1..5`) producing a full `0..6` window instead.

The marker box is a `1`px-stroke, unfilled outline `w+3` by `h+3` pixels
(`10x10` at the default), confirmed by an exact `36`-pixel perimeter
count. The stats panel is fully read off a real render (reading UI text
off rendered pixels is black-box measurement, not reading the reference's
source or its font table): two 4-line groups, `"CH AVG MIN MAX RMS"` then
one row per channel, and `"CH STD"` the same shape; channel labels and
plane order confirmed against both a `yuv444p` source (`Y`/`U`/`V`) and a
`gbrp` source (`R`/`G`/`B`, plane order `G,B,R` — this project's own
established convention, see `vaco-filter-color::exposure`'s test doc).

The five statistics, each pinned at **four** independent points (a flat
field, a single-column-outlier `7x7` window, a symmetric seven-value
ramp, and the edge-clamped ramp above):

```text
AVG = round(mean(v), 1)                          // arithmetic mean, not median
MIN = min(v)
MAX = max(v)
RMS = round(sqrt(mean(v^2)), 1)                  // raw values, not deviations from the mean
STD = round(sqrt(mean((v - mean)^2)), 2)          // population (÷N), not sample (÷N-1)
```

The outlier probe (seven pixels at `250`, forty-two at `10`) alone rules
out three plausible alternatives: `AVG=44.3` (not the mostly-`10` median)
confirms arithmetic mean; `RMS=94.9` matches `sqrt(mean(v^2))` and not
`sqrt(mean((v-mean)^2))` (which would print a visibly different, smaller
number — that formula is exactly what `STD` already is); `STD=83.98`
matches a population divisor and not a sample one (which would print
`84.85`). The ramp probe (`STD=20.00`, `RMS=86.3` against hand-computed
`20.0`/`86.348`) and the edge-clamped probe (`STD=2.00`, `RMS=3.6`
against `2.0`/`3.6056`) independently confirm the same two formulas.

Implemented for `yuv444p`/`gray`-family and `gbrp`-family 8-bit sources
only. Not implemented: `o` (opacity — drawn fully opaque, the same
unexplored-effect choice `datascope`'s own `opacity` already made);
colour-coding (one colour for every line — no field here needs a second
colour to distinguish it, and it cannot buy back the framecrc-exactness
the font ceiling already forecloses); packed RGB, subsampled chroma, and
alpha formats; bit depths above 8; the panel's exact reference pixel
columns (a readable approximation, for the same "cannot be exact anyway"
reason `datascope`'s own margins were not chased further).

**Cannot be framecrc-identical to the reference**, for the same permanent
reason as `datascope`/`graphmonitor` — see "The bitmap-font hypothesis"
below.

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
| `oscilloscope` | **Its `st` statistics text was located this pass**, reversing an earlier "not located" finding from two smaller probes — a third, larger-canvas, multi-frame attempt found it sitting in the trace box's own bottom row: a single white line, `"{ch} avg:{avg:.1f} min:{min} max:{max}"` per channel, no zero-padding (unlike `pixscope`'s). Confirmed to share the same font mechanism (no font option) and confirmed the trace/grid's bare existence (`g=1` draws a plain grid; each enabled component traces a distinct-coloured connected line at partial opacity). **Not shipped**: the trace line's own per-pixel geometry — how `t` tilts it, how `s`/`tx`/`ty`/`tw`/`th` map to exact box pixels, per-component colour assignment — is a materially separate measurement task from finding the stats text, and was not attempted this pass. |
| `ciescope` | **Not a D7 case.** Every `system` value names a published international-standard primary set (BT.709, BT.2020, DCI-P3, SMPTE-C, …) and the CIE 1931 observer data is public. The blocker is reproducing the reference's exact chromaticity-diagram *rendering* (spectral-locus rasterisation, anti-aliasing, gamut-triangle lines) — not itself specified by any colorimetry standard, so verifying it would need extensive black-box probing this pass's time did not cover. |

### `drawgraph`/`adrawgraph`

Plot up to four frame-metadata values as a scrolling line graph. **Shipped
for `mode=line`/`slide=frame` (both defaults).** Expected to need this
crate's font mechanism the way `datascope`/`pixscope` do; a real render
(`signalstats,drawgraph=m1=lavfi.signalstats.YAVG:slide=picture`) instead
showed a plain coloured line trace with no text anywhere, and `-h`
confirms no font option on either filter — pure geometry like `waveform`,
not text-bound.

Measured directly, with flat-luma sources giving an exactly known
`lavfi.signalstats.YAVG`:

- **Value-to-pixel mapping** (nine points: `min`/`max`/midpoint at three
  graph heights): in-range values map through
  `row = ceil(margin + (max-v)/(max-min) * (height-1-2*margin))`, but the
  margin did not resolve to one clean constant — the top and bottom
  margins measured *unequal* at `height=201` (`15` vs `13`), and
  `margin = round(0.07*(height-1))` (a single, symmetric value) was the
  closest single-formula fit found in the time available, exact at
  `height=101` and within one pixel elsewhere. **Out-of-range values
  clamp to the absolute canvas edge** (`row=0` or `row=height-1`), a
  different rule from the in-range formula evaluated past its domain —
  confirmed independently with `min=100:max=150` fed values `0` and
  `255`.
- **`fg1..4`'s hex colour has a real byte-order bug: written
  `0xAARRGGBB`, applied as opaque `(B, G, R)`.** `fg1=0x11223344`
  (intending `A=11,R=22,G=33,B=44`) painted `(R=0x44,G=0x33,B=0x22)` — R
  and B swapped, G untouched, alpha always ignored (confirmed
  pixel-identical output with the same RGB at `A=0x00` and `A=0xff`).
- **`bg` is a normal `<color>`, unaffected by that bug.** `bg=0x112233`
  painted `(R=0x11,G=0x22,B=0x33)` exactly as written — `fg1..4` and `bg`
  are two different colour grammars on the same filter, not one binding
  set applied twice.

Not implemented: `mode=bar`/`dot`; `slide=replace`/`scroll`/`rscroll`/
`picture`; `fg1..4` as genuine value-dependent expressions. Reads
metadata via gap 11's `Frame::metadata_get`, proven through a real
`Graph` end-to-end test (`tests/drawgraph.rs`), not just unit tests of
the pixel-mapping formula.

## Framecrc comparison table

Same loop as every other filter crate in this project: `ffmpeg -bitexact -f
lavfi -i <deterministic source> -vf "<filter>=<args>" -f rawvideo -pix_fmt
<fmt> -` against the reference, cross-checked against this crate's pure
functions and pinned into unit tests. No `vaco` CLI/muxer exists yet to
drive an actual `-f framecrc` invocation (`planning/14-cli.md` is still a
plan document).

**This row now has a permanent split, not just a temporary one.**
`histogram`, `waveform`, `thistogram` and `drawgraph`/`adrawgraph` draw no
text, so nothing rules out framecrc-exactness for them — `thistogram`
reaching it first closed that question for every non-text filter this
crate is likely to implement, and `drawgraph`/`adrawgraph` confirming the
same "no font" property is why they are shipped at all. `datascope`,
`graphmonitor`/`agraphmonitor` and `pixscope` draw with an
independently-sourced font (D7 forbids transcribing the reference's own
table — see "The bitmap-font hypothesis" above), so no frame containing
their text can *ever* match the reference byte-for-byte. "Framecrc-
identical across this crate's whole corpus" is therefore not a
temporarily-unmet goal for the text-drawing filters — it is provably
unreachable, the same way `hqx` is provably unimplementable in
`vaco-filter-artistic`. `drawgraph`/`adrawgraph` are not there yet for a
different reason: the value-to-pixel margin is a fitted approximation,
not a derived exact formula — see that filter's own section above for
the precise residual — so today's shipped behaviour is close but not
proven byte-exact, unlike `thistogram`'s own fully-pinned intensity rule.

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
| `drawgraph` | `m1=lavfi.signalstats.YAVG:min=0:max=255:slide=picture`, flat-luma sources at three heights | any (via `signalstats`) | **structural, close but not proven exact** — nine value-to-pixel points matched by a fitted margin formula (exact at one height, within 1px elsewhere); out-of-range clamp-to-edge confirmed exactly |
| `drawgraph` | `fg1=0x11223344`/`bg=0x112233` | any | **exact** — the `fg1..4` byte-swap-and-drop-alpha bug and `bg`'s normal, unaffected grammar both confirmed |
| `pixscope` | `w=7:h=7`, flat `126`/`128` `yuv444p` field | `yuv444p` | **structural, not exact** — flat-field baseline: `AVG`/`MIN`/`MAX`/`RMS` all read the flat value, `STD=0` |
| `pixscope` | same, single-column outlier (`250` vs `10`) in the `7x7` window | `yuv444p` | **structural, not exact** — `AVG=44.3`/`RMS=94.9`/`STD=83.98` confirmed against hand-computed values, ruling out median/AC-RMS/sample-STD |
| `pixscope` | same, symmetric 7-value ramp (`54..114` step `10`) | `yuv444p` | **structural, not exact** — `AVG=84.0`/`RMS=86.3`/`STD=20.00` match exactly |
| `pixscope` | `x=0` (edge-clamped window) | `yuv444p` (`mod(X,256)` ramp) | **structural, not exact** — `MIN=0`/`MAX=6` confirms clamp-not-shrink-not-wrap, plus a fourth independent statistic match |
| `pixscope` | default `w=h=7`, pure-red source | `gbrp` | **structural, not exact** — `R=255,G=0,B=0` confirms plane order (`G,B,R`) and channel labelling |

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
- `pixscope` is done; `src/pixscope.rs`'s `render_plane` is a pure
  function over a plain `&mut [&mut [u8]]`, kept separate from the
  `FrameFilter` glue specifically so the box/window/panel drawing can be
  exercised directly in tests (including a deliberate revert-and-confirm-
  fails check) without a live `Frame`/`FilterContext`. If you extend it:
  **remember the reference needs a source at least `640x480`** (smaller
  inputs are refused outright, undocumented in `-h`) — a prior pass's "the
  zoom box does not magnify" finding was measured without knowing this
  and was superseded once a real render above that floor was tried.
  `compute_stats`/`window_start` are unit-tested against four independent
  hand-computed probes each; reuse them rather than re-deriving the
  formulas. Still open if you pick this back up: the panel's exact
  reference pixel columns, `o` (opacity), and colour-coding — none of
  which can buy back framecrc-exactness the font ceiling already rules
  out, so treat them as polish, not correctness.
- If you add `oscilloscope`, start from `src/font8x8.rs` — it is already
  sourced, registered and tested; do not re-derive or re-source a font.
  Its `st` statistics text is now found and read (`"{ch} avg:{avg:.1f}
  min:{min} max:{max}"`, one line, no zero-padding), but a **larger
  canvas and several accumulated frames were needed to see it** — a
  single-frame, `800x600`-scale probe (the size that worked for
  `pixscope`) showed nothing; `1600x1200` over `10` frames did. What
  remains unmeasured is the trace/grid geometry itself: how `t` tilts the
  trace, how `s`/`tx`/`ty`/`tw`/`th` map to exact box pixels, and
  per-component colour assignment — start there, since the stats format
  is already nailed down and ready to reuse once the trace itself is
  measured.
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
