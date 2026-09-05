# vaco-filter-video-source

Video test-pattern sources (FT-4.4, GitHub epic #54, source child issue):
`pal100bars`, `pal75bars`.

## What it is

Two colour-bar generator filters. This is deliberately the narrowest of the
three FT-4.4 child crates — see "What is deferred" below for the two
independent reasons why, both of which are load-bearing facts a maintainer
extending this crate needs before adding to it.

## How it works

Finite `duration` options are converted directly from exact `Duration` values
to inverse-frame-rate ticks with nearest rounding. This keeps a `30000/1001`
source budget exact beyond the integer range of `f64`.

`bars.rs` holds a shared `Source` (the `SourceFilter` impl), one `Opts`
struct (mirroring `vaco-filter-plumbing::color`'s `size`/`rate`/`duration`/
`sar` shape exactly), and a `build` function parameterised on an
`[[u8;3];8]` colour table. `pal100::create` and `pal75::create` are the only
difference between the two filters.

### The measured layout

Byte-scanning a `pal100bars=size=490x1` row against `ffmpeg 8.1` finds
colour boundaries at `0, 62, 124, 186, 248, 310, 372, 434` of 490 — **eight**
equal-width segments, not seven bars filling the frame:

| Segment | Colour |
|---|---|
| 0 | white |
| 1 | yellow |
| 2 | cyan |
| 3 | green |
| 4 | magenta |
| 5 | red |
| 6 | blue |
| 7 | black |

`pal75bars` repeats the identical boundaries with segments 1–6 scaled to
75% amplitude; segments 0 (white) and 7 (black) are measured identical
between the two filters — confirmed by probing both at the same size and
diffing.

This crate's boundary formula (`boundary(i) = i * width / 8`, integer
division) reproduces the *colours* exactly but the *boundaries* only
approximately — the reference's measured boundaries land one to six columns
later than this formula's — see `bars.rs`'s doc comment for the actual
numbers. Output is `rgb24`, generated directly (no YUV round-trip), so it
does not reproduce the small (`255`→`253`-ish) rounding the reference shows
when converting from its native `yuv422p` through `-pix_fmt rgb24`.

## What is deferred, and why

1. **Already shipped elsewhere.** `color`, `nullsrc`, `anullsrc`, `nullsink`,
   `anullsink` are registered by `vaco-filter-plumbing` (FT-4.3, GitHub
   #467). `buffer`/`abuffer`/`buffersink`/`abuffersink` are
   `vaco-filter-core`'s own privileged `Graph` I/O API, not a leaf filter at
   all — see that crate's `lib.rs` doc. Re-registering any of these five
   names here would collide with an existing `[[component]]` row.
2. **Pattern not measured precisely enough to implement without guessing.**
   `testsrc`/`testsrc2` draw a moving gradient, a checkerboard, a rotating
   clock hand and rendered text; the text needs a font rasteriser
   (`vaco-filter-text`'s dependency footprint, outside this crate's scope),
   and the non-text pattern was not reverse-engineered to the pixel in the
   time available. `smptebars` is a genuine three-row layout (top colour
   bars, a reversed middle row, a bottom PLUGE/black row); a single-row
   probe (`smptebars=size=490x1`) found boundaries at `0, 88, 176, 264, 398,
   422` — irregular spacing that does not resolve to a clean fraction of the
   frame width, meaning the pattern is not reducible to `N` equal columns
   the way the PAL bars are, and a taller probe (to see all three rows)
   was not run before time ran out. Shipping a guessed pixel layout under a
   name that claims to be a broadcast standard is worse than not shipping
   it. `rgbtestsrc`, `yuvtestsrc`, `allrgb`, `allyuv`, `gradients`,
   `zoneplate`, `cellauto`, `life`, `mandelbrot`, `sierpinski`, `perlin`,
   `colorchart`, `colorspectrum` are simply not implemented — none were
   probed.

## How to change it

- Add `smptehdbars` (the widescreen sibling): same shape as `pal100bars`,
  new colour table — probe it first, the same way `pal100bars` was.
- To finish `smptebars`: probe a taller frame (e.g. `120x100`) and read off
  row bands, not just one row, before writing any code.
- Add a filter: follow `bars.rs`'s shape, declare the module in `lib.rs`,
  add a `[[component]]` row to `vaco-component.toml`, wire the name into
  `registry.rs`, then run `cargo xtask gen-registry`.

## Configuration

`size`/`s`, `rate`/`r`, `duration`/`d`, `sar` — identical names and defaults
to `vaco-filter-plumbing::color`. Parsed via
`#[derive(vaco_opts::Options)]` / `OptionsExt::set_from_string`.

## Dependencies

`vaco-filter-core` (`SourceFilter`/`Sourced`), `vaco-filter-graph`
(`FilterRegistry`/`Instantiate`), `vaco-pixfmt`, `vaco-frame`, `vaco-opts`,
`vaco-core`.
