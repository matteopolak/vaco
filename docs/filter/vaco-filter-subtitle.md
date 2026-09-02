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
  (`BorderStyle=1`) instead.
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

## Configuration

Per-instance filter options only (`filename=`, and whatever `ass`/
`subtitles` otherwise accept) — no crate-level env vars or feature
flags.

## Dependencies

`vaco-ass` (script parsing/tag interpretation), `vaco-filter-text`
(`TextRenderer`, shaping and rasterisation), `vaco-filter-draw` (mask
compositing primitives), `vaco-format-subtitle` (SRT/container timing
parsers) and `vaco-filter-core`/`vaco-filter-graph` (the filter/graph
traits every filter crate implements against).
