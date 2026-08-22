# `vaco-format-nalu`

Layer 4. NAL unit framing shared by H.264, HEVC and VVC: the two framings, the
conversion between them, RBSP extraction, and a resumable scanner for parsers
fed in chunks.

## What it is

The substrate every H.26x parser and every container carrying H.26x sits on. It
is deliberately small and it deliberately owns *no* codec syntax — the meaning of
`nal_unit_type` belongs to the codec crate, and only the header's bit *layout*
lives here.

Written from ITU-T H.264 Annex B, §7.3.1 and §7.4.1; ITU-T H.265 §7.3.1.1-2;
ITU-T H.266 §7.3.1.1-2; and ISO/IEC 14496-15 §5.3.3. Per D7, from the
specifications only.

### What it is *not*, and why

`vaco-bitstream` (layer 0) already owns three primitives, and this crate
reimplements none of them:

| Primitive | Where it lives |
|---|---|
| `find_start_code` — the word-skip scanner | `vaco_bitstream::annexb` |
| `to_rbsp` / `to_ebsp` — the escaping rule | `vaco_bitstream::annexb` |
| whole-buffer iterators over both framings | `vaco_bitstream::{annexb, avcc}` |

They are at layer 0 because D14.1 puts `vaco-codec-core` below
`vaco-format-core`: a demuxer must be able to look inside a parameter set
without depending on a codec crate. This crate is the layer above, and every
scan and every escape below calls through to those.

`tests/agreement.rs` and the `nalu_framing` fuzz target both assert byte
equality with layer 0's versions. That is the point: two components in the same
process disagreeing about where a NAL unit ends would surface as a bug
somewhere else entirely.

## How it works

### 1. One iterator over both framings

```rust
let nals: Vec<Nal<'_>> = units(buf, Framing::AnnexB).collect();
let nals: Vec<Nal<'_>> = units(sample, Framing::LengthPrefixed(LengthSize::FOUR)).collect();
```

`Nal` carries two things layer 0's iterators cannot give and every caller
recomputes otherwise:

- `offset` — where the unit's bytes begin in the source, which a demuxer needs
  for `Packet::pos` and a parser needs to report consumed bytes;
- `start_code_len` — 3 or 4, which is what has to be re-emitted when converting
  framings.

`LengthSize` is a validated newtype over {1, 2, 4}. ISO/IEC 14496-15 encodes
`lengthSizeMinusOne` in two bits and *reserves* the value 2 (a three-byte
length); no muxer writes it and no decoder implements it, so a record declaring
it is malformed rather than exotic and the type cannot hold it.

### 2. `RbspBuf` — de-escaping and padding are the same copy

The idea worth reading first. Every H.26x parser does three things to every NAL
unit, tens of thousands of times per file:

1. strip emulation-prevention bytes,
2. put the result somewhere `BitReader`'s eight-byte refill can read,
3. reuse both buffers next time.

Done with layer 0's primitives that is **two** copies into **two** buffers:
`to_rbsp` yields a `&[u8]`, and `Padded::from_slice_copying` copies it again.
`RbspBuf` writes the de-escaped bytes straight into a buffer whose tail already
holds `Padded::PAD` zeros, so `RbspBuf::padded()` and `RbspBuf::reader()` are
free.

`benches/scan.rs` measures both shapes side by side
(`iterate_and_deescape` against `iterate_and_deescape_two_copies`).

Budget accounting is by **high-water mark**, not per fill: a parser that runs ten
thousand 4 KiB units through one `RbspBuf` is charged once, for the peak, which
is what it actually costs.

### 3. `Scanner` — resumable framing

A `Parser` is handed a buffer that *grows*: the driver appends the next chunk
and calls again with the same unconsumed prefix. Scanning from the beginning
each time is quadratic in the number of chunks, and on a stream that never
produces a complete unit — exactly the shape a fuzzer finds — that is a hang
rather than a slowdown.

`Scanner` records how far it has looked, so a re-presented prefix is not
re-examined and total work stays linear however the stream is chopped up.

**The two-byte tail is the subtle part.** After an unsuccessful search over
`buf`, only `buf.len() - 2` bytes are genuinely ruled out: the final two could be
the `00 00` of a start code whose `01` has not arrived. Getting this wrong is the
classic chunked-parser bug — the unit boundary lands exactly on a chunk boundary
and vanishes.

> A caller that also needs to classify the Annex B `zero_byte` must retain
> **three** bytes rather than two, because a four-byte start code is
> `00 00 00 01`. `vaco-parse-h264` hit exactly this: the same stream fed in
> one-byte chunks reported a three-byte start code where a whole-buffer parse
> reported four.

### 4. Framing conversion

`annexb_to_length_prefixed` and `length_prefixed_to_annexb`, both budgeted, both
appending rather than clearing so an access unit can be built in one buffer.

The one failure that can actually happen: a NAL unit longer than a one- or
two-byte length prefix can express. Truncating the length there is how a file
gets written that no decoder can read, so it is `InvalidData` rather than a debug
assertion.

A malformed *length prefix* on the way in is not an error — the conversion stops
and reports how many units it emitted. A truncated final unit in an otherwise
good sample is a damaged file, not an unusable one, and the caller decides
whether to care. This matches `vaco_bitstream::avcc::nal_units`, which ends its
iteration in the same place.

### 5. `NalHeader` — three codecs, three layouts, one struct

| Codec | Layout |
|---|---|
| H.264 | `forbidden_zero_bit(1) nal_ref_idc(2) nal_unit_type(5)` |
| HEVC | `forbidden_zero_bit(1) nal_unit_type(6) nuh_layer_id(6) nuh_temporal_id_plus1(3)` |
| VVC | `forbidden_zero_bit(1) reserved(1) nuh_layer_id(6) nal_unit_type(5) nuh_temporal_id_plus1(3)` |

Note VVC puts the layer id **first**, which is the difference that catches
people. `nuh_temporal_id_plus1 == 0` is reported rather than wrapped, because it
is forbidden and a resynchronising parser wants to know.

## How to change it

- **Adding a framing.** VVC's `vvcC` has the same length-prefix shape, so
  `Framing::LengthPrefixed` already covers it; nothing here needs to change.
- **A faster scanner.** Do not add one here. `find_start_code` in
  `vaco-bitstream` is the project's single definition of where a start code is,
  and `vaco-simd` is slated to own a vectorised version that must agree with it
  exactly. `benches/scan.rs` measures four candidates against each other
  (including `memchr::memmem`) precisely so a change can be argued from numbers;
  if one wins, the finding belongs to `vaco-bitstream`'s owner.
- **The `Nal` struct is `Copy` and borrows its source.** Keep it that way — an
  owning variant would put an allocation in the hot loop.
- **Gotcha: `start_code_len` is 0 for length-prefixed units.** The prefix belongs
  to the framing, not to the unit, so `framed_len()` does not include it there;
  a caller adds the framing's own width.

## Configuration

None. No features, no environment variables, no constants a caller can change.
The one tunable is the `Budget` a caller passes to `RbspBuf::fill`,
`escape_into` and the two conversions.

`Padded::PAD` (64) comes from `vaco-bitstream` and is asserted equal to
`vaco_pool::BITSTREAM_PADDING` in that crate's tests.

## Dependencies

- `vaco-bitstream` — the scanner, the escaping rule, `BitReader`, `Padded`,
  `ByteReader`.
- `vaco-limits` — `Budget`.
- `vaco-core` — the error taxonomy.
- dev only: `proptest`, `divan`, `memchr` (a benchmark candidate, not a runtime
  dependency).

No external runtime dependencies. `#![forbid(unsafe_code)]`. Builds for
`wasm32-unknown-unknown` (D18).

## Performance

Measured with `divan`, Apple M5, `aarch64-apple-darwin`, min of 100 samples over
1 MiB corpora. Ratios rather than verdicts, per plan 12's PF-0.1 rule.

### `RbspBuf` against the two-copy shape

| corpus | one pass (`RbspBuf`) | two copies | ratio |
|---|---|---|---|
| dense — a start code every 4 KiB | 690 µs | 705 µs | **1.02x** |
| many_units — a start code every 11 bytes | 743 µs | 1307 µs | **1.76x** |

Honest reading: on realistic payload the saved copy is nearly free, and the win
arrives when units are small and numerous — MPEG-TS, and anything a fuzzer
generates. Worth having, not transformative.

### Start-code scanning — a finding for `vaco-bitstream`'s owner

Four scanners, four corpora, all agreeing on every boundary before any timing
was taken (`benches/scan.rs` asserts that first). Times are the minimum of 100
samples over 1 MiB:

| corpus | naive 3-byte window | `find_start_code` (word skip) | `memchr::memmem` | `memchr` + confirm |
|---|---|---|---|---|
| dense | 363 µs | 104 µs | **28.7 µs** | 42.0 µs |
| many_units | 307 µs | **180 µs** | 231 µs | 176 µs |
| sparse — 25% zero bytes | 2063 µs | 914 µs | **33.2 µs** | 2024 µs |
| zeros | 769 µs | 661 µs | **22.9 µs** | 1918 µs |

Against `find_start_code`, `memmem` measures **3.6x faster on dense payload,
27.5x on zero-heavy data and 28.9x on all zeros**, and **0.78x** (i.e. 1.29x
slower) on a corpus that is mostly boundaries, where per-call setup dominates a
few-byte span.

Two things follow, and neither is a change to this crate:

1. The word-skip scanner is a large improvement on the naive one — 3.5x on
   dense payload — so the reasoning behind it was sound. That was the question
   worth asking, and the answer is yes.
2. **The adversarial profile is the concerning half.** `sparse` and `zeros` are
   what an attacker sends, and they are exactly where the gap is widest: 914 µs
   against 33 µs is a 27x denial-of-service lever on a scan that touches every
   byte of every file. `memchr` is already a pre-declared workspace dependency.

This crate does **not** act on that. `find_start_code` is the project's single
definition of where a start code is, `vaco-simd` is slated to own a vectorised
version that must agree with it exactly, and adding a third implementation here
would be duplication of precisely the kind this crate exists to avoid. The
numbers are recorded for whoever owns that decision; `benches/scan.rs` is where
they can be re-taken.

## Testing

- `tests/agreement.rs` — property tests asserting byte-for-byte agreement with
  `vaco-bitstream` on both framings and on RBSP extraction, plus the invariants
  (units non-empty, in order, disjoint; de-escaping never grows; escaping
  round-trips; framing conversion preserves the unit sequence).
- `fuzz/fuzz_targets/nalu_framing.rs` — the same properties on arbitrary bytes,
  plus the one that needs a fuzzer to find: the incremental `Scanner` must locate
  exactly the boundaries a whole-buffer scan does, for every chunking.
- `benches/scan.rs` — four scanners over four corpora, and the framing path with
  and without the second copy.
