# `vaco-cbs-jpeg` — JPEG coded bitstream syntax (D-21b)

## What it is

The `vaco-codec-cbs` `CbsCodec` implementation for JPEG: splits a file into
its marker segments and entropy-coded scan spans (ITU-T T.81), reassembles
byte for byte, and types the three segments worth editing directly.

Layer 4 (`crates/codec/`), independent of `vaco-codec-jpeg` — see "Why a
separate crate" below.

## How it works

| Module | What it covers |
|---|---|
| `header` | `SOF0`/`SOF1`/`SOF2` (frame header), `DQT` (quantisation tables), `DHT` (Huffman tables) — read and write |
| `cbs` | `JpegCbs`/`JpegContent`/`JpegFraming`, the split/assemble/read_unit/write_unit wiring |

A `CbsUnit`'s `data` is the segment exactly as it appears on the wire —
`0xFF <marker>` for a no-payload marker, or the full `0xFF <marker> <len:be16>
<payload>` for a length-prefixed one — so `assemble` is a plain concatenation
of every unit's bytes, with no re-framing choice to make (unlike a NAL
start-code width, or a superframe's grouping).

Everything following an `SOS` segment up to (but not including) the next
marker that is not a byte-stuffed `0xFF 0x00` and not a restart marker
(`RST0..=RST7`) is a distinct unit, tagged with `SCAN_DATA_UNIT_TYPE` (`0x100`
— outside the `0..=0xFF` range every real marker byte occupies).

## Why no bit-packed-codec-shaped write-side deviations

Every field in `SOF`/`DQT`/`DHT` is byte-aligned and self-delimiting by its
own length or count field. Unlike H.264/HEVC/AV1/VP9's CBS layers — each of
which documents at least one place where the parsed value under-determines
the original bits — there is no such gap here: given a parsed `FrameHeader`/
`QuantTable`/`HuffmanTable`, there is exactly one way to write it back, and it
is always the way it was read.

## Why a separate crate, and why it does not reuse `vaco-codec-jpeg`

`vaco-codec-jpeg`'s own `FrameHeader`/`QuantTable`/`HuffmanTable` and marker
constants are all `pub(crate)` — not reachable from outside that crate, and
not worth widening the visibility of six marker bytes across a crate boundary
for. This crate reproduces the handful of ITU-T T.81 Table B.1 constants it
needs independently.

## How to change it

- **A fourth typed segment** (`APP14`'s Adobe transform field, say): add a
  variant to `cbs::JpegContent` and its read/write pair in `header.rs`;
  everything else stays `JpegContent::Raw`.
- **Verifying a change**: `ffmpeg -f lavfi -i testsrc2=size=WxH -frames:v 1
  out.jpg` produces a real baseline JPEG (this crate's own
  `tests/fixtures/baseline.jpg`); a small Python script walking `0xFF
  <marker> <len:be16>` segments dumps a real payload as a Rust byte array.
- **Progressive (multi-`SOS`) JPEGs**: not capturable in this environment —
  `ffmpeg`'s built-in `mjpeg` encoder is baseline-only. The scan-boundary loop
  that must not stop at the first `SOS`'s own entropy data is verified
  against a hand-built two-scan file instead (`cbs.rs`'s
  `a_hand_built_two_scan_file_splits_and_reassembles`), built directly from
  §B.2.3's marker/length shape.

## Configuration

None.

## Dependencies

`vaco-bitstream` (`ByteReader`, byte-oriented), `vaco-limits` (budget
accounting), `vaco-codec-cbs` (the `CbsCodec`/`Cbs` shape this crate fills
in). Deliberately not `vaco-codec-jpeg` — see above.

## Specification

`Vaco-Spec-Ref: itu-t-t81-199209` §B.1.1, §B.2.2, §B.2.3, §B.2.4.1, §B.2.4.2.
