# vaco-filter-draw

Shared drawing kernels used by more than one filter crate: format-aware
colour parsing, and plane-correct fill/blend/box operations that work
across subsampled chroma and 8-through-16-bit pixel depths. Plan
`16-filters.md` §4.1's row, GitHub #458 (FT-3.1) — this project's
equivalent of the reference's `drawutils`.

## Why it is its own crate

`vaco-filter-draw-vf`'s `drawbox`/`drawgrid` already ship a colour parser
and a blend routine, but both are `pub(crate)` and scoped to exactly the
case that filter needed (planar RGB, 8-bit, no alpha). `overlay`,
`drawtext`'s box background, and any filter compositing over a real
decoded frame need the general case — YUV as well as RGB, subsampled
chroma, 9/10/12/16-bit depths — and per D19 that belongs in one shared
crate rather than being re-derived (and re-narrowed) by the next caller.
This crate does **not** replace `vaco-filter-draw-vf`'s own narrower copy;
migrating that caller onto this crate is separate, unstarted work, noted
here rather than done.

## What it is

- [`color`](../../crates/filter/vaco-filter-draw/src/color.rs):
  `AVColor`-grammar parsing (`#RRGGBB[AA]`, `0xRRGGBB[AA]`, the reference's
  full named-colour table, an `@alpha` suffix) into `Rgba`. `Rgba` itself
  is re-exported from `vaco_core`, not redefined — an early draft
  duplicated it and `cargo xtask dup-check` caught it.
- [`sample`](../../crates/filter/vaco-filter-draw/src/sample.rs): generic
  component pack/unpack against a `vaco_pixfmt::PixFmtDescriptor` — the
  piece that makes every other module here work on any packed-or-planar,
  8-or-16-bit format without a per-format match arm.
- [`solid`](../../crates/filter/vaco-filter-draw/src/solid.rs): resolves
  an `Rgba` into a destination format's own native code values — RGB
  channels directly, YUV via `vaco_color::MatrixCoefficients` (defaulting
  to BT.601 limited-range; see that module's own doc for the measurement
  pinning that default).
- [`fill`](../../crates/filter/vaco-filter-draw/src/fill.rs): writes a
  resolved colour into every sample of a region of a `vaco_frame::Frame`,
  chroma-subsampling and bit-depth aware.
- [`blend`](../../crates/filter/vaco-filter-draw/src/blend.rs): the same
  region, alpha-composited over existing content instead of overwritten.
- [`rect`](../../crates/filter/vaco-filter-draw/src/rect.rs): clips an
  `(x, y, w, h)` rectangle to the frame and to each plane's own
  chroma-decimated geometry, and derives a border-only ring for
  `thickness`-style box drawing.

## What is out of scope

Palette, bitstream-packed, hardware-surface and floating-point formats
(`PixFmtFlags::PALETTE`/`BITSTREAM`/`HW_ACCEL`/`FLOAT`) are rejected with
`Error::Unsupported` rather than silently misinterpreted — no caller needs
them yet, and guessing a byte layout for them risks exactly the
plausible-but-wrong-frame failure mode this project has hit before with
other formats.

## How to change it

- A new colour syntax or named-colour table entry goes in `color.rs`.
- A new fill/blend shape (not a rectangle) needs its own module alongside
  `rect.rs`, reusing `sample`/`solid` rather than re-deriving component
  pack/unpack.
- If a caller needs palette or floating-point support, extend `sample`'s
  read/write to cover it rather than special-casing the caller — every
  module here is written to be format-agnostic through that one seam.

## Configuration

No options of its own — this is a library crate, not a registered filter
(no `FilterDesc`, no fuzz target for option parsing). Callers own their
own option surfaces and call into this crate's functions directly.

## Dependencies

`vaco-core` (`Rgba`), `vaco-color` (`MatrixCoefficients`), `vaco-pixfmt`,
`vaco-frame`. Dev-only: `vaco-limits`, `vaco-sampfmt`, `vaco-chlayout`,
`proptest`.
