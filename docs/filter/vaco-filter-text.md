# vaco-filter-text

## What it is

The `TextRenderer` every glyph-drawing filter in this tree sits on
(plan 16 §6.1), plus the one filter this crate registers itself:
`drawtext` (§6.2, GitHub #462/FT-3.5, #473/FT-4.10). `vaco-ass`/
`vaco-filter-subtitle` are the other consumers of `TextRenderer`; they
live in their own crates per the plan's §6.3 split, not here.

## How it works

Shaping and rasterisation are `cosmic-text`'s job (font discovery via
`fontdb`, bidi via `unicode-bidi`, shaping via `rustybuzz`, outline and
rasterisation via `swash` — all four pulled in through the single
`cosmic-text` dependency, already reviewed in the workspace manifest).
No `FreeType` dependency exists anywhere in this crate's tree, so the
FTL attribution obligation this project tracks for `FreeType` never
arises here. What this crate adds on top:

- [`alias`](../../crates/filter/vaco-filter-text/src/alias.rs) — the
  generic-family fallback table fontconfig would otherwise provide,
  plus embedded-font loading (Matroska attachments) and `-font_dirs`.
- [`layout::TextRenderer`](../../crates/filter/vaco-filter-text/src/layout.rs)
  — a shaped-run LRU cache over `cosmic_text::Buffer`/`SwashCache`, needed
  because a `drawtext` with `%{pts}` reshapes an unchanged-looking
  string every frame otherwise, plus a bound on `SwashCache`'s own
  unbounded growth.
- [`mask::AlphaMask`](../../crates/filter/vaco-filter-text/src/mask.rs) —
  a coverage buffer independent of any one colour, so a border or shadow
  is produced by *operating on the mask* (dilate/blur/offset) rather than
  re-rasterising; needed by `drawtext`'s `borderw`/`shadowx`/`shadowy` and
  ASS's `\bord`/`\shad`/`\blur`/`\be`.
- [`mask::composite`](../../crates/filter/vaco-filter-text/src/mask.rs) —
  tints a mask and alpha-composites it into a real `vaco_frame::Frame`,
  subsampled-chroma and high-bit-depth aware, built on
  `vaco-filter-draw`'s `sample`/`solid`/`rect` primitives.
- [`expand`](../../crates/filter/vaco-filter-text/src/expand.rs) —
  `drawtext`'s `expansion=normal` `%{...}` directive set, evaluated once
  per frame: `%{pts[:fmt[:offset]]}`, `%{n}`/`%{frame_num}`,
  `%{metadata:key[:default]}`, `%{expr:EXPR}` (via `vaco_expr`).
  `%{eif:...}`/`%{gmtime}`/`%{localtime}`/`%{pict_type}`/
  `%{expr_int_format}` are named gaps; an unrecognised directive passes
  through verbatim rather than being dropped, matching the reference.
- [`drawtext`](../../crates/filter/vaco-filter-text/src/drawtext.rs) —
  the filter itself, covering `text`/`textfile`/`fontfile`/`font`/
  `fontsize`/`fontcolor`/`alpha`/`box*`/`border*`/`shadow*`/`x`/`y`/
  `line_spacing`/`text_align`/`tabsize`/`fix_bounds`/`expansion`/
  `reload`. Not implemented: `fontcolor_expr`, `ft_load_flags` (no
  `FreeType` to flag), `rtl` (accepted and parsed, not applied — a real
  bidi reorder is out of scope for this pass) — see the module's own
  doc for the complete, current gap list.

## How to change it

- A new `%{...}` expansion directive: add it to `expand.rs`'s match, no
  other module needs to change.
- A new `drawtext` option: `style.rs` for a new `TextStyle` field,
  `drawtext.rs` for parsing and for feeding it into `layout`/`mask`.
- `rtl` (bidi reordering): `unicode-bidi` is already a transitive
  dependency via `cosmic-text`; the gap is that `layout::TextRenderer`
  never calls into it for reordering, not a missing library.
- A new filter that needs to draw glyphs: depend on this crate for
  `TextRenderer`/`AlphaMask`, the same way `vaco-ass`/
  `vaco-filter-subtitle` do — do not re-implement shaping.

## Configuration

Per-instance filter options for `drawtext` only; no crate-level env
vars. `-font_dirs` (parsed in `alias.rs`) is the one CLI-level knob this
crate reads.

## Dependencies

`cosmic-text` (shaping/rasterisation stack: `fontdb`, `unicode-bidi`,
`rustybuzz`, `swash`), `vaco-expr` (`%{expr:...}`), `vaco-filter-draw`
(mask compositing primitives), `vaco-color`/`vaco-pixfmt`/`vaco-frame`
and `vaco-filter-core`/`vaco-filter-graph` (the filter/graph traits
every filter crate implements against).
