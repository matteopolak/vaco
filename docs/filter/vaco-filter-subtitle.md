# vaco-filter-subtitle

## What it is

The two subtitle-burning video filters: `ass` and `subtitles` (plan 16
§6.3, GitHub #486/FT-5.1, #487/#488/FT-5.2/5.3). Both are self-contained
like the reference's own filters — `filename=...`, no second input pad
— because a subtitle file is a small, complete document read whole at
construction, not a stream.

## How it works

- [`ass_filter`](../../crates/filter/vaco-filter-subtitle/src/ass_filter.rs) drives `vaco-ass` (script parsing + tag interpretation)
  and rasterises the result through `vaco-filter-text::TextRenderer`.
  One real, stated simplification: `vaco-ass::plan_event` correctly
  splits a line into several styled runs wherever a mid-line override
  tag changes something, but this filter renders the **whole line in
  the first run's style** — mixed formatting mid-line is not applied.
  The common case (tags at the start of a line, e.g. `{\an8\c&H00FF00&}
  text`) is unaffected. `\clip` is applied by zeroing mask coverage
  outside the rectangle after rasterisation; `BorderStyle=3` (opaque
  box) is not implemented, every event renders as outline+shadow
  (`BorderStyle=1`) instead. `\frx`/`\fry`/`\frz`/`\fr` project the alpha
  mask around `\org`, or around the line's aligned position when `\org`
  is absent. The X→Y→Z transform uses a 312.5 script-pixel camera
  distance, scaled to the frame. Its inverse-mapped mask is bounded to
  the projected corners before allocation; a camera-plane crossing uses
  the frame as its finite sampling bound. It is then clipped and
  composited through the same path as unrotated text. `render_at` supplies
  its frame timestamp to `vaco-ass`, so supported style and X/Y/Z rotation
  tags nested in `\t(...)` interpolate before layout and projection. The
  plan holds only the resolved state for that instant rather than an
  unbounded animation list.
- [`subtitles`](../../crates/filter/vaco-filter-subtitle/src/subtitles.rs) dispatches on file extension: `.ass`/`.ssa` gets the
  full `ass_filter` path; everything else falls back to a simpler
  "layout and draw" path — currently implemented for **SRT only**
  ([`text`](../../crates/filter/vaco-filter-subtitle/src/text.rs)'s bottom-centred simple-text rendering over
  `vaco_format_subtitle`'s SRT timing parser). `WebVTT`/`MicroDVD`/SAMI
  are a named gap, not attempted — each needs its own cue-splitting
  rule.
- [`bitmap::composite_bitmap`](../../crates/filter/vaco-filter-subtitle/src/bitmap.rs) is the DVB/`VobSub`/PGS half (#486): a
  positioned alpha-composite of an already-decoded palette bitmap, no
  typesetting involved.
- [`registry::SubtitleRegistry`](../../crates/filter/vaco-filter-subtitle/src/registry.rs) dispatches `"ass"`/`"subtitles"` to
  `ass_filter::create`/`subtitles::create`; see `vaco-component.toml`
  for the matching `[[component]]` entries `cargo xtask gen-registry`
  reads.

## How to change it

- Mixed formatting mid-line: needs `vaco-filter-text::TextRenderer` to
  expose positioning several `layout` calls left-to-right (it doesn't
  yet) — a `vaco-filter-text` API change first, then `ass_filter.rs`.
- A new subtitle container format for the `subtitles` fallback path
  (`WebVTT`/`MicroDVD`/SAMI): add a cue-splitting rule and a branch in
  `subtitles.rs`'s extension dispatch; `text.rs`'s renderer is already
  format-agnostic once you have plain cue text and timing.
- `BorderStyle=3`: `ass_filter.rs`'s composite step would need an
  opaque-box path alongside its current outline+shadow one.
- Motion/fade/animated-clip line state, karaoke and `\p` drawings remain
  separate #488 work. Keep them outside the projective mask helper;
  transform interpolation belongs in `vaco-ass`'s point-in-time planner.
  Karaoke text is laid out syllable-by-syllable so `\k` can switch a fill,
  `\K`/`\kf` can sweep it left-to-right, and `\ko` can withhold its outline
  before highlight. `\p` drawings use bounded even-odd mask rasterisation
  for `m`, `n`, `l`, and cubic `b` paths, plus the uniform B-spline
  `s`/`p`/`c` sequence.

## Configuration

Per-instance filter options only (`filename=`, and whatever `ass`/
`subtitles` otherwise accept) — no crate-level env vars or feature
flags.

## Dependencies

`vaco-ass` (script parsing/tag interpretation), `vaco-filter-text`
(`TextRenderer`, shaping and rasterisation), `vaco-filter-draw` (mask
compositing primitives), `vaco-format-subtitle` (SRT/container timing
parsers) and `vaco-filter-core`/`vaco-filter-graph` (the filter/graph
traits every filter crate implements against). Rotation semantics come
from Aegisub's published ASS override-tag documentation. The camera
distance and checked crops were calibrated with ffmpeg-full 9.0.1/libass
0.17.5 only as a black-box pixel oracle: centered `TILT` crops are
`64:30:128:104` unrotated, `64:16:128:112` under `\frx60`, and
`32:30:142:104` under `\fry60`; moving only `\org`'s Y from 120 to 180
changes the X-rotated crop to `56:10:132:150`.
The exact transform-animation fixture changes its visible bounds from
`88x31` at 0.5 seconds to `76x76` at the 2.0-second midpoint and `31x88`
at 3.5 seconds.
