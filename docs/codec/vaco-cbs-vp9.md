# `vaco-cbs-vp9` — VP9 coded bitstream syntax (D-21a)

## What it is

The `vaco-codec-cbs` `CbsCodec` implementation for VP9: `uncompressed_header()`
(VP9 Bitstream & Decoding Process Specification v0.6, §6.2) and the Annex B
superframe index, both read and write.

Layer 4 (`crates/codec/`), independent of `vaco-parse-vpx` and `vaco-codec-vp9`
by design — see "Why a separate crate" below.

## How it works

| Module | What it covers |
|---|---|
| `header` | `uncompressed_header()`, §6.2 — the whole thing, both directions |
| `superframe` | Annex B's superframe index — split and reassemble |
| `cbs` | `Vp9Cbs`/`Vp9Content`/`Vp9Framing`, wiring the above into `CbsCodec` |

`Vp9Content` pairs a typed `Vp9Header` with `tail: Vec<u8>` — the compressed
header and tile data that follow the uncompressed header, copied verbatim.
Unlike H.264/HEVC/AV1's `Content` enums, there is nowhere else for those bytes
to live: `CbsUnit::data` is replaced wholesale on a write, so a `Content` that
dropped the tail would have no way to put it back.

## Why a separate crate, and why it does not reuse `vaco-parse-vpx`

`vaco-parse-vpx::vp9::parse_uncompressed_header` reads just enough of the
header to populate `CodecParameters` and stops right after `frame_size()`. A
CBS layer needs something that reader has no use for: **the exact byte offset
the header ends at**, so everything past it — loop filter, quantisation,
segmentation and tile-column parameters, none of which `CodecParameters`
needs — has to be read in full anyway. Given that, and that both
`vaco-parse-vpx` and `vaco-codec-vp9` were under active ownership elsewhere in
the tree while this was written, this crate is fully self-contained: no
dependency on either.

One consequence: `superframe::sub_frame_ranges` is a second, small (~20 line)
implementation of `vaco-parse-vpx::superframe::last_subframe`'s algorithm,
generalised to return every sub-frame's range rather than just the last. This
is a known, documented duplicate (see `lib.rs`'s own doc and
`xtask/src/dup_check.rs`'s `DISTINCT` entries for `Vp9Header`/`LoopFilterParams`/
`TileInfo`/`QuantizationParams`/`LoopFilterDeltas`), left for a consolidation
pass once both crates are free rather than reached into while active.

## How to change it

- **A new header field**: add it to `header::FrameHeader` (or a sub-struct)
  and its read/write pair in `header.rs`, at the exact position §6.2's table
  gives it. Every field after it depends on the read cursor landing correctly.
- **Verifying a change**: capture real frames the way this crate's own tests
  do — `ffmpeg -f lavfi -i testsrc2=size=WxH:rate=R -c:v libvpx-vp9 -pix_fmt
  yuv420p -crf Q -b:v 0 -g N` to an IVF file, then a small Python script to
  walk the IVF frame records (4-byte little-endian size, 8-byte timestamp,
  payload) and dump one as a Rust byte array. Parse it, re-encode the header,
  and diff against the original prefix.
- **Paths no real encoder in this environment reaches**: `frame_size_with_refs`'s
  `found` branch (a reference's own size, not an explicit `frame_size()`),
  loop-filter deltas, and segmentation. None appeared in ten captured real
  frames (one key, nine inter). Covered instead by hand-built fixtures in
  `header.rs`'s own tests, following the same convention
  `vaco-parse-vpx::vp9`'s tests already use for paths this environment's
  encoder does not reach.
- **A visible multi-frame superframe** could not be captured either — the same
  wall `vaco-parse-vpx::superframe`'s tests document. `cbs.rs`'s
  split/reassemble test uses a hand-built index over real frame-header bytes
  plus arbitrary tile payload instead; the splitter does not need to
  understand the tile bytes it moves.

## Configuration

None.

## Dependencies

`vaco-bitstream` (byte- and bit-oriented reading/writing), `vaco-limits`
(budget accounting), `vaco-codec-cbs` (the `CbsCodec`/`Cbs` shape this crate
fills in). Deliberately not `vaco-parse-vpx` or `vaco-codec-vp9` — see above.

## Specification

`Vaco-Spec-Ref: vp9-bitstream-spec-v0.6` §6.2, Annex B.
