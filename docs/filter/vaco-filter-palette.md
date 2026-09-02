# vaco-filter-palette

T2/T3 palette video filters — `planning/16-filters.md` §4.2's
`vaco-filter-palette` row: `palettegen`, `paletteuse`, `elbg`. The crate
did not exist when claimed — the real, unclaimed
remainder of #111 (FT-4.11) after `vaco-filter-stack` took
`hstack`/`vstack`/`xstack` for the same issue.

## `latticepal` is not attempted, and not a gap

Checked directly: `ffmpeg -hide_banner -filters | grep lattice` finds
nothing in the installed `ffmpeg 8.1` reference. There is no oracle to
measure it against and no reference behaviour to reproduce, so it is not
implemented rather than guessed.

## What it is

All three filters share one original median-cut colour quantiser
([`quantize`](../../crates/filter/vaco-filter-palette/src/quantize.rs))
— not a transcription of the reference's own quantiser (a different,
well-documented public algorithm, Heckbert 1982), implemented instead
from general algorithmic knowledge (D6/D7).

- [`palettegen`](../../crates/filter/vaco-filter-palette/src/palettegen.rs)
  — accumulates a full-stream 8-bit RGB colour histogram (alpha ignored)
  and emits one `16x16` RGBA palette image at end of stream.
  `stats_mode=diff`/`single` are parsed but not distinguished from
  `full` — this pass always accumulates the whole stream, documented
  rather than silently ignored.
- [`paletteuse`](../../crates/filter/vaco-filter-palette/src/paletteuse.rs)
  — maps each main-input pixel to its nearest colour (plain Euclidean RGB
  distance, no dithering) in the palette read from the second input. The
  reference's default is `dither=sierra2_4a` (error diffusion); this ships
  the undithered baseline only.
- [`elbg`](../../crates/filter/vaco-filter-palette/src/elbg.rs) —
  posterizes a **single frame** to `codebook_length` colours with the same
  median-cut quantiser. **Not** the reference's actual ELBG
  (Enhanced Linde–Buzo–Gray): the reference iteratively refines a codebook
  via generalized-Lloyd relaxation plus utility-driven cell splitting over
  `nb_steps` iterations; median-cut is a different, simpler, one-shot
  member of the same vector-quantisation-for-posterization family.
  `nb_steps`/`seed` are parsed for option compatibility but do not affect
  output (median-cut is deterministic).

All three require an addressable, non-hardware, non-palette RGBA input —
enforced by requesting an exact `Rgba` pixel format on every relevant pad,
so the negotiator inserts a conversion upstream rather than this crate
misreading another format's byte layout.

## How to change it

- A new palette filter goes in its own `src/<name>.rs`, registered in
  `src/registry.rs` and `vaco-component.toml`.
- If `elbg`'s real iterative algorithm is ever implemented, it belongs
  alongside `quantize.rs`'s median-cut as a second, distinct quantiser —
  not a rewrite of it, since `palettegen`/`paletteuse` are correctly
  specified against median-cut already.
- `paletteuse`'s dithering modes (`bayer`, `sierra2`, `sierra2_4a`, ...)
  are the largest named gap; each is an independent per-pixel error-
  diffusion or ordered-dither pass over the existing nearest-colour
  lookup, addable one at a time without touching `quantize.rs`.

## Configuration

No crate-level configuration. Per-filter options are documented in each
module's own doc comment, matching `ffmpeg -h filter=<name>`'s option
table for name, default and range, including options this crate parses
but does not act on.

## Dependencies

`vaco-core`, `vaco-opts`, `vaco-frame`, `vaco-pixfmt`, `vaco-filter-core`,
`vaco-filter-graph`, `vaco-filter-framesync` (`paletteuse`'s two-input
shape).

## Fuzzing

`fuzz/fuzz_targets/filter_palette_options.rs` (option parsing, every
registered name, through the real filtergraph parser).
