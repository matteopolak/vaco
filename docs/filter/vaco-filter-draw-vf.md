# vaco-filter-draw-vf

Pure-geometry drawing filters over one video input — `planning/16-filters.md`
§4.2's `vaco-filter-draw-vf` row, GitHub issue #473 (FT-4.10, "T2
text/drawing"). Two implemented: `drawbox`, `drawgrid`.

## Scope reconciliation

#473's title names `drawtext, drawbox, drawgrid, drawgraph` together and its
`Crate(s):` field says `vaco-filter-text` for all four — a roadmap-era guess,
the same shape #478/#480/#111 each turned out wrong in a different way.
`planning/16-filters.md` §4.2 disagrees with that field, and its own two
tables disagree with a naive single reading of each other too:

| Filter | Where it actually belongs |
|---|---|
| `drawbox`, `drawgrid` | This crate's own row, `vaco-filter-draw-vf` (§4.2's `vaco-filter-draw-vf` line) |
| `qrcode`, `qrcodesrc`, `qrdecode` | Also this crate's row on paper, but needs an external `qrcode` crate dependency — a reviewed decision under D10 this pass does not make. Not attempted. |
| `drawgraph`, `adrawgraph` | §4.2's `vaco-filter-scope` line, not this crate — implemented there this pass (see `docs/filter/vaco-filter-scope.md`), alongside that crate's other six filters |
| `drawtext` | `vaco-filter-text`, blocked on #462's `TextRenderer` (itself blocked on a `rustybuzz` provenance question under D10). Not attempted. |

Reported before writing code, the same discipline #480/#111's own scoping
comments used.

## What it is

One module per filter (`src/{drawbox,drawgrid}.rs`), each exposing
`pub const DESC: FilterDesc` and a crate-private `fn create`, aggregated by
[`registry::DrawVfRegistry`](../../crates/filter/vaco-filter-draw-vf/src/registry.rs)
— the same shape every other T2/T3 filter crate in this project uses.
`src/color.rs` carries a minimal `AVColor`-string parser (`#`/`0x` hex,
`@alpha` suffix, and a handful of unambiguous named primaries) and the
per-channel alpha-blend helper both filters share.

Both filters use `vaco-expr` for their geometry options (`x`/`y`/`w`/`h`,
and `drawbox`'s `thickness`) — the same `Bindings`/`Expr` shape
`vaco-filter-video-geometry::crop`/`pad` already established, reused here
rather than re-derived.

**Neither filter draws any text.** Pure geometry over one input, drawn
directly onto the source frame (a converter that declares the same format
on both pads, not a passthrough — see "How to change it" for why). That is
what makes both **framecrc-exact**, within this pass's format scope: no
font, no independently-sourced glyph table, nothing this project's own D7
rule keeps it from matching the reference byte-for-byte.

### `drawbox`

Draws a colour-filled or outlined rectangle. Measured directly (`ffmpeg
8.1`, `-bitexact`, `gbrp` sources):

- **Every geometry option is a `vaco-expr` expression, evaluated exactly
  once.** There is no `eval=init/frame` choice (unlike `crop`/`pad`) and no
  way to make it re-evaluate: `x=10*n` was rejected outright (`n`, the
  frame counter, is not a bound name), and `x=10*t` produced the identical
  box position on every one of five output frames rather than one that
  moved with playback time.
- **`t` is not time — it is the filter's own resolved `thickness`.**
  `x=t` alone resolved to `x=3` at the default `thickness=3`, and to `x=9`
  once `thickness=9` was passed explicitly — an exact match at two
  independent values, not a coincidence. This lets a geometry expression
  inset a box by its own stroke width (`x=t/2`).
- Bound names confirmed valid: `iw`, `ih`, `dar`, `sar`, `hsub`, `vsub`,
  `w`, `h`, `t`. Confirmed invalid: `n`, `main_w`, `main_h`. `x`'s own
  expression could not reference `x` itself (self-reference errors), but
  could reference `y` — the reverse direction was not independently
  checked, so this module resolves `w`, `h`, `x` (with `y` unresolved,
  fed as `0`), then `y` (with the now-resolved `x`): the option list's own
  declaration order, not confirmed as the reference's evaluation order.
- **The colour blend is `floor(src*(1-a) + color*a)` per channel, not
  `round`** — pinned at three different alpha values (`0.5`, `0.3`,
  `0.33`), each landing exactly on the floored result
  (`255*0.3=76.5 -> 76`, `100*0.5+255*0.5=177.5 -> 177`,
  `10*0.67+255*0.33=90.85 -> 90`).
- **Hex colours are `RRGGBBAA`, alpha last** — `color=0x11223344` on a
  black background produced `R=4, G=9, B=13`, matching
  `floor(0x11/0x33 * (0x44/255))` for each channel exactly. `drawgraph`'s
  own `fg1..fg4` *expression* defaults (`"0xffff0000"`) are a different,
  `AARRGGBB` convention this module does not parse.
- `thickness=fill` means "fill the whole rectangle", confirmed by an
  exact lit-pixel-count match against `w*h`. `replace=true` assigns the
  colour directly, skipping the blend arithmetic.

Not implemented: `box_source` (reads a rectangle from frame side data —
no producer of that side data exists in this tree yet). Only planar RGB
(`gbrp`-family, plane order `G,B,R`), 8-bit, no-alpha sources — see
`src/color.rs`'s own doc for why converting an arbitrary named colour
into a YUV frame's own colour model was out of scope this pass rather
than merely untried, and why the reference's full ~140-name colour table
(several of which, like `green` = `0,128,0`, disagree with their
CSS/web namesakes) was not transcribed.

### `drawgrid`

Draws a repeating colour grid. Same expression/blend mechanism as
`drawbox` (see above). The one thing measured specifically for this
filter:

- **Grid lines repeat in both directions from the `(x, y)` offset, by
  `w`/`h` — not just forward from it.** `x=15:y=15:w=6:h=6` on a `20x20`
  canvas lit column/row `15` as expected, but *also* row `3`
  (`15 - 6 - 6`, two whole periods backward) — the grid is
  `(coordinate - offset) mod period`, confirmed independently from a
  forward-only probe (`x=5:w=6` lighting `5, 11, 17`).

## Framecrc comparison table

| Filter | Args | Source | Result |
|---|---|---|---|
| `drawbox` | `x=0:y=0:w=32:h=32:t=fill:color=white@0.5`, flat `100` | `gbrp` | **exact** — `floor` blend confirmed |
| `drawbox` | `color=0x11223344:t=fill`, black background | `gbrp` | **exact** — hex layout (`RRGGBBAA`) confirmed |
| `drawbox` | `x=t`, `thickness` default vs `thickness=9` | `gbrp` | **exact** — `t` is the filter's own thickness, not time |
| `drawgrid` | `x=5:y=5:w=6:h=6:t=1`, any | `gbrp` | **exact** — forward period confirmed |
| `drawgrid` | `x=15:y=15:w=6:h=6:t=1`, any | `gbrp` | **exact** — backward period confirmed |

No `vaco` CLI/muxer exists yet to drive an actual `-f framecrc` invocation
; comparisons are against
the reference's raw pixel output and cross-checked against this crate's
own unit tests, which pin the same probes.

## How to change it

- Both filters follow the same shape: `vaco_opts::Options` for the raw
  strings, `vaco_expr::{Bindings, Expr}` parsed once at `create` time, a
  `resolve` free function (unit-tested directly, independent of
  `FrameFilter`) that turns the parsed expressions plus the input's own
  `iw`/`ih`/`sar` into concrete pixel geometry, and a `FrameFilter` that
  calls it once per frame (cheap — it is arithmetic, not I/O) and paints.
- If you extend either to more pixel formats, `src/color.rs`'s `Rgba` is
  already channel-agnostic (`r,g,b,a`); the part that is `gbrp`-specific
  is each filter's own `(plane, channel)` pairing in `filter_frame` — a
  YUV-family addition needs an actual measured RGB-to-YUV conversion
  formula for whichever colour space the frame declares, not a guessed
  matrix, and should probably become its own doc section reporting what
  was measured, the same way this one does.
- `drawgraph`/`adrawgraph` do not belong in this crate — see
  `vaco-filter-scope`'s own doc if you are looking for them.

## Configuration

No crate-level configuration, environment variables, or feature flags.
Runtime configuration is entirely per-filter-instance, via each filter's
`Opts` (parsed from the filtergraph argument string).

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-expr`, `vaco-frame`, `vaco-pixfmt`,
`vaco-filter-core`, `vaco-filter-graph`. No new dependency was added to
the workspace's `Cargo.toml`.
