# `vaco-format-subtitle-bitmap`

Layer 4. The shared bitmap-subtitle model — one palette shape, one rectangle
shape, one indexed-pixel-buffer shape — used by every demuxer/muxer in
`vaco-subtitle-bitmap` (`dvbsub`, `dvbtxt`, `sup`, `vobsub`). Issue #611,
FM-52.

---

## What it is

Four subtitle formats (DVB, PGS/Blu-ray, DVD subpicture) share exactly one
fact about what they eventually put on screen: a rectangle of pixels, each an
index into a palette of at most 256 colours. They share almost nothing about
how they compress that rectangle to get there — three unrelated run-length
grammars. This crate is the shared 20% (`Rect`, `Palette`, `IndexedBitmap`);
`vaco-subtitle-bitmap` is the four separate 80%s.

## The demuxer/decoder line — where this crate sits on it

`crates/format/` recovers packets and their timing; `crates/codec/` (a later
wave) turns a packet into pixels by running the format's run-length
decompressor. **No demuxer in `vaco-subtitle-bitmap` constructs an
`IndexedBitmap` with real pixel data** — that would mean decoding, which is
out of scope here. `IndexedBitmap` is defined in this crate because it is the
shape a future decoder's *output* takes, so the decoder and every demuxer
agree on it without the decoder crate depending on a demuxer crate (or vice
versa) to find out.

`Rect` and `Palette`, on the other hand, genuinely are constructed by
demuxers today, in exactly three places where a container states either as a
**plain, uncompressed field** — no run-length coding in between, so reading
it is header/container work, the same category as reading a PNG `IHDR`'s
width/height:

1. `VobSub`'s `.idx` file: `size: WxH` (a `Rect`) and `palette: rrggbb, …` (a
   `Palette`), both plain ASCII.
2. DVB's CLUT-definition segment (EN 300 743 §7.2.4): a table of `Y Cr Cb T`
   entries, fixed-width integers, no compression.
3. DVB's region-composition segment (EN 300 743 §7.2.3): a region's declared
   `region_width`/`region_height`, again fixed-width integers.

Everything else — an `ODS`'s run-length pixel string, a PGS `PDS`'s palette
entries — stays untouched, opaque packet payload, for a decoder to interpret.

## How it works

- **`rect.rs`** — `Rect { x, y, width, height }` (all `u32`). The only
  constructor, `Rect::new`, takes a `&vaco_limits::Limits` and rejects a
  `width`/`height` over `limits.max_dimension`, and rejects `x + width` /
  `y + height` overflowing `u32`. This is the direct answer to
  `planning/AGENT-CONSTRAINTS.md`'s "a region claiming a 65535×65535
  rectangle" example — see `rect.rs`'s own test of that exact case. `area()`
  is a checked `u64` multiply.
- **`palette.rs`** — `Palette`, a `Vec<vaco_core::Rgba>` capped at 256
  entries (`Palette::MAX_ENTRIES`), constructed only through `Palette::new`
  (which rejects over-256). `pack_argb32`/`unpack_argb32` round-trip to/from
  256 little-endian `0xAARRGGBB` words — **this project's own convention**
  for what `vaco_packet::PacketSideData::Palette`'s `Buffer` would hold,
  modelled on the reference's `AV_PKT_DATA_PALETTE` layout but not itself a
  measured fact (nothing outside this workspace observes an in-memory
  packet's side data). The entry type is `vaco_core::Rgba`, not a type of
  this crate's own — see "How to change it" below for why that matters.
- **`color.rs`** — `ycbcrt_to_rgba`: integer BT.601 (shifted, not divided —
  `clippy::integer_division` is denied workspace-wide and 1024 is `1 << 10`),
  converting DVB's `Y Cr Cb T` CLUT entries to RGBA. Transcribed from the
  public BT.601 coefficients EN 300 743's own informative annex specifies,
  not from any decoder's source (D6/D7).
- **`bitmap.rs`** — `IndexedBitmap { rect, palette, indices }`.
  `IndexedBitmap::new` validates `indices.len() == rect.area()`.
  `IndexedBitmap::allocate(budget, rect, palette)` is the decoder-facing
  constructor: it sizes the pixel buffer through `vaco_limits::Budget::alloc`
  rather than directly from `rect`'s area, which is the second line of
  defence past `Rect::new`'s per-axis check — a rectangle can pass the
  per-axis check under `Limits::permissive` (whose `max_dimension` is 65536)
  and still have an enormous *area*; `Budget` catches that at allocation
  time. See `bitmap.rs`'s
  `allocate_over_the_alloc_cap_is_rejected_even_though_each_axis_is_in_bounds`
  test for the concrete case.

## How to change it

- Adding a fifth bitmap-subtitle format: if it states a palette/rectangle as
  a plain field anywhere, reuse `Rect`/`Palette` rather than a new ad hoc
  shape — that reuse is the entire point of this crate existing separately
  from `vaco-subtitle-bitmap`.
- **Do not add a local `Rgba` type.** `vaco_core::Rgba` (in
  `vaco-core::parse`, used for `-vf`-style colour options) already has this
  exact shape and a `TRANSPARENT` constant; this crate had its own copy
  briefly, and `cargo xtask dup-check` (D19) correctly refused it as the same
  concept twice. `palette.rs` re-exports `vaco_core::Rgba` rather than
  defining one, and that is the right shape for anything else that needs an
  RGBA colour in this workspace too.
- A new colour-space conversion (e.g. if a fifth format uses a different
  transparency encoding) belongs next to `ycbcrt_to_rgba` in `color.rs`, not
  inlined into a demuxer — it is exactly as reusable as the DVB one turned
  out to be for nothing else yet, but the separation is what let it be
  unit-tested independently of any segment-parsing code.

## Configuration

None — this crate carries no options. The one runtime knob is the
`vaco_limits::Limits` passed to `Rect::new`/`IndexedBitmap::allocate`, which
callers choose (`Limits::strict()`, `::permissive()`, `::tiny()` for fuzzing).

## Dependencies

`vaco-core` (`Result`/`Error`, `Rgba`) and `vaco-limits` (`Limits`, `Budget`).
Deliberately **not** `vaco-codec-core`: this crate carries no `CodecId`, no
`CodecParameters` — those live in `vaco-subtitle-bitmap`, the crate that
actually registers with `vaco-format-core`.
