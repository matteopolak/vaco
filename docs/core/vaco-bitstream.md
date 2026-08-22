# `vaco-bitstream`

Layer 0. Bit and byte readers/writers, Exp-Golomb, Annex-B start-code framing and
RBSP escaping.

## What it is

Every codec parser and every container that carries codec-level syntax sits on
this crate, so its per-read cost is multiplied by roughly the number of syntax
elements in the world. That single fact drives every design decision below.

Written from ITU-T H.264 (V14, 2020) §7.3.1, §7.4.1.1, §9.1 and Annex B, and
ISO/IEC 14496-15 §5.3.3, per D7.

## How it works

### 1. Sticky overrun, not `Result` per read (plan 11 §8.2, F13)

`BitReader::get` returns a value, not a `Result`. Past the logical end it returns
zeros — deterministically, the same values `FFmpeg`'s zero padding produces, so a
parser written straight from the spec behaves identically — and the reader
remembers. The parser checks once per syntax structure:

```rust
let profile_idc = r.get(8);
let level_idc   = r.get(8);
let sps_id      = r.ue();
r.check()?;              // one check for the whole structure
```

The rejected alternative turns a 40-line SPS parser into 40 `?`-laden lines and
blocks inlining. What the sticky model gives up is early exit; what it keeps is
the property that matters — **a truncated or malformed bitstream can never panic
and never reads out of bounds**. `try_get` exists for the rare site that must
branch immediately, such as a length prefix about to size an allocation.

### 2. Overrun is derived, not flagged

`overrun()` is `bit_pos() > logical_bits`, computed from state the reader keeps
anyway. So the sticky model costs *zero* instructions in the read path — not even
one predictable branch. A separate `flagged` bool covers only what position
cannot express: a malformed Exp-Golomb prefix, an out-of-range width.

### 3. The padded body and the checked tail (plan 11 §8.3, F9)

The reader keeps an MSB-aligned 64-bit cache. Reads *out of the cache* are
register operations — shift, mask, subtract — with **no bounds check at all**.
Refills happen eight bytes at a time, so one bounds comparison covers the four to
eight syntax elements that refill feeds.

```
refill:  data.get(pos..).and_then(first_chunk::<8>)
           Some(word) -> body: one u64 load                    (hot)
           None       -> tail: eight bytes, zero past the end   (cold, #[inline(never)])
```

`Padded` is what makes that split pay. A padded buffer carries 64 zero bytes past
its logical end, so the body path keeps running 56 bytes *past* where the data
stops and a short buffer never enters the tail path at all. This is `FFmpeg`'s
over-read trick with the padding moved **inside the allocation**, where it is
memory we own rather than slop we hope the allocator left mapped.

`Padded::new` verifies the padding is present and zero — 64 byte comparisons,
once per buffer. That is what lets the type be constructed safely by any crate
without `unsafe`, a sealed trait, or a cross-crate privacy trick. A buffer that
fails the check simply is not `Padded`, and the caller falls back to
`BitReader::new`, which is correct and only slightly slower near the end.

**The refill merge is `cache |= chunk >> cache_bits`, with no mask.** `pos`
advances only by the whole bytes that fit, so bits of `chunk` below the new
`cache_bits` stay in the cache as garbage — but they are exactly the bits the
*next* refill will load, so the next `|=` writes the same values over them. The
OR is idempotent. `tests/reader.rs::refill_boundaries_are_crossed_correctly`
reads a whole buffer one bit at a time against a naive reference; that is the
test that catches a broken version of this argument.

### 4. Exp-Golomb has no unbounded loop

`ue()` is one `peek(32)`, one `leading_zeros()`, one `skip`, one `get`. A prefix
of more than 31 zeros cannot produce a `u32` and is malformed in every codec that
uses this coding, so it flags the reader and returns 0. **That cap is the
difference between a fuzz hang and a clean rejection.** `ue_long` does the same
one octave up, at 63. `skip` is likewise constant time for any `n`, so a
container declaring a huge payload cannot turn a skip into a loop.

`se()` needs no range check and emits none: `ue()` returns at most `2^32 - 2`, so
the magnitude is at most `2^31 - 1` either way. Removing the `i64` +
`i32::try_from` that an earlier version had was worth **44%** on the header
benchmark — the reader is hot enough that this level of detail shows up.

### 5. Start-code scanning

Two skips compose in `annexb::find_start_code`. The classic three-byte stride: if
`buf[i+2]` is not zero, no start code begins at `i`, `i+1` or `i+2`. On top of
that, a `u64` with no zero byte cannot hold the first two bytes of a start code
beginning at any of its first seven positions, so advance seven. Video payload is
overwhelmingly non-zero, so the word skip carries most of the scan: **4.9× a
naive three-byte window scan**, 20.5 GB/s against 4.2 GB/s.

This is the scalar reference. `vaco-simd` will own a vectorised `scan` (plan 11
§5); the two must agree exactly, which is why `fuzz/fuzz_targets/annexb_nal_iter.rs`
differentially compares this function against a naive scan at every offset.

## What it costs — measured

`cargo bench -p vaco-bitstream --bench reader -- --sample-count 300 --sample-size 20`,
Apple M5, `release` profile with fat LTO. Medians; the run-to-run spread on the
reader groups is under 2%.

Baselines, all in `benches/reader.rs`:

- **`result_per_read`** — F13's rejected option (a): *identical* cache and
  refill, `Result` on every consuming read.
- **`checked_per_read`** — no cache: one bounds-checked slice access per syntax
  element.
- **`bytewise`** — the textbook-safe reader: one bounds check per *bit*.

### `per_unit_parse` — one reader per short buffer (the D5 v0.1 workload)

512 parameter sets, each in its own ~27-byte buffer, as `ffprobe` actually sees
them. A buffer that short is *entirely* inside the last eight bytes, so the
unpadded reader spends the whole parse in the tail path.

| Variant | Median | vs padded |
|---|---|---|
| `padded` | 15.83 µs | — |
| `unpadded` | 17.68 µs | **+11.7%** |
| `result_per_read` | 25.72 µs | +62% |
| `checked_per_read` | 39.58 µs | +150% |
| `bytewise` | 36.68 µs | +132% |

### `header_parse` — the same 512 parameter sets concatenated (13.7 KB)

| Variant | Median | vs padded |
|---|---|---|
| `padded` | 17.81 µs | — |
| `unpadded` | 17.76 µs | **−0.3% (parity)** |
| `result_per_read` | 31.52 µs | +77% |
| `checked_per_read` | 44.39 µs | +149% |
| `bytewise` | 46.82 µs | +163% |

### `bulk_fixed_width` — 256 KiB of 1–16-bit fields (decode-shaped)

| Variant | Median | vs padded |
|---|---|---|
| `padded` | 84.5 µs | — |
| `unpadded` | 88.2 µs | +4.4% |
| `result_per_read` | 124.4 µs | +47% |
| `checked_per_read` | 136.9 µs | +62% |
| `bytewise` | 120.0 µs | +42% |

### What the numbers say

**Plan 11 §8.3's 1–3% estimate was measuring the wrong axis, and the conclusion
is stronger than the estimate.** The plan framed the cost as padded-vs-unpadded
on a long buffer. Measured, that gap is *zero* — on a 13.7 KB buffer the tail
path runs once and disappears into the noise.

Where the padding actually earns its 64 bytes is the **short-buffer** case, which
is the real workload: a parameter set arrives as its own NAL unit, tens of bytes
long, gets its own reader, and is read end to end inside the tail. There the
padding is worth **11.7%**, consistently, across repeated runs.

The much larger result is that both of our readers beat every safe alternative by
**1.5× to 2.6×**. The body/tail split is not a 1–3% tax being paid for safety —
it is the reason a `#![forbid(unsafe_code)]` bit reader is fast at all. The
`Result`-per-read design F13 rejected on ergonomic grounds turns out to cost
62–77% on header parsing as well, which retires the question of whether F13 was a
readability preference: it was not.

Padding remains worth keeping for the reason it was adopted, and the numbers
locate its value precisely — in per-NAL parsing, not in bulk reads.

## How to change it

- **Touching `refill`.** Re-read the idempotent-OR argument above first, and run
  `tests/reader.rs` — `refill_boundaries_are_crossed_correctly` and
  `padded_and_unpadded_readers_agree` are the two that catch a broken merge. The
  proptest `padded_and_unpadded_readers_agree` and
  `fuzz_targets/bitreader_arbitrary.rs` run the same equivalence at scale.
- **Adding a read.** Widths must be clamped, never asserted in release:
  `debug_assert!` plus `n.min(32)`. A conditional store to a flag in `get` cost
  measurable time and was removed — do not put one back.
- **Adding a variable-length code.** It must have a *structural* bound, not a
  loop-until-valid. Anything that can consume an unbounded prefix is a hang.
- **Gotcha: `check` takes `&self` and does not clear.** Plan 11 §8.4 sketches
  `check(&mut self)` as check-and-clear. Clearing is meaningless once overrun is
  derived from the position — the position stays past the end — and would hide a
  truncation from the next caller.
- **Gotcha: `BitWriter::with_capacity` takes a `&mut Budget`.** It cannot take a
  bare `usize`: `clippy.toml` denies `Vec::with_capacity` so that every
  input-derived allocation goes through a budget, and an encoder sizing its
  output from a frame header is exactly that case.
- **Gotcha: the `ue` / `se` writer domains.** `ue` covers `0 ..= u32::MAX - 1`
  and `se` covers `-(2^31 - 1) ..= 2^31 - 1`, matching what H.264 §9.1 permits.
  The excluded values need a 32-zero prefix, which the reader rejects by design.
  Both saturate and `debug_assert!`.
- **Gotcha: a flagged reader may or may not consume.** `ue` on a flagged reader
  consumes nothing (which is what stops a parser ignoring the flag from spinning
  on zero-length codewords); `ue_long` may consume a 32-bit run before flagging.
  Do not assert either way — `fuzz_targets/golomb_arbitrary.rs` has the comment
  explaining why the first version of that assertion was wrong.
- **Gotcha: `NalIter` skips empty units and trims trailing zeros**, so the units
  are a *subsequence* of the input, not a partition of it. The leading zero of a
  four-byte start code is `trailing_zero_8bits` of the previous unit and is
  trimmed there.

## Configuration

None. No features, no environment variables, no runtime tunables. The one
constant is `Padded::PAD = 64`, which matches `vaco_pool::BITSTREAM_PADDING`;
changing one without the other silently disables the fast path for pool-allocated
buffers.

## Dependencies

- `vaco-core` — the `Error` taxonomy. `BitstreamError` converts into
  `vaco_core::Error`.
- `vaco-limits` — `BitWriter::with_capacity` takes a `Budget`.
- `thiserror`.
- Dev: `proptest`, `divan`.

**Not** `vaco-simd`, despite plan 11 §8.8 listing it. That crate is being written
concurrently and its `scan` API is not frozen; taking a dependency on it now
would couple this crate's schedule to another agent's. The scalar scanner here is
the reference implementation `vaco-simd::scan` will be differentially tested
against, so it needs to exist regardless. **This should be revisited once
`vaco-simd` lands.**

Gate assessment (D10) is unchanged from plan 11 §8.8: `bitstream-io`, `bitvec`
and `nom`'s bit combinators all clear Gates 1–3 but fail on model grounds — none
offers the sticky-overrun contract, none has the `Padded` typestate, and none
provides Exp-Golomb or Annex-B handling. The benchmark above quantifies what the
model choice is worth.

## Testing

| File | Covers |
|---|---|
| `tests/reader.rs` | every width 0–64 at every offset 0–7 against a naive reference; refill boundaries; shift edges at `n = 0` and `n = 64`; `mark`/`restore`; overrun stickiness; `Padded` verification |
| `tests/golomb.rs` | codewords derived independently from H.264 §9.1; malformed prefixes reject rather than loop; `ue_max` / `se_range` |
| `tests/writer.rs` | MSB ordering, masking, alignment, `rbsp_trailing`, budgeted capacity, `RbspWriter` escaping |
| `tests/framing.rs` | endian-explicit byte reads, sub-windows, start-code scanning, NAL iteration on pathological input, RBSP escaping |
| `tests/proptest_roundtrip.rs` | write-then-read identity for every width; Exp-Golomb over the full range; padded/unpadded equivalence; truncation monotonicity; `mark`/`restore` replay; escaping round-trip; iterator well-formedness |
| `fuzz/fuzz_targets/*` | `bitreader_arbitrary`, `golomb_arbitrary`, `bitwriter_roundtrip`, `annexb_nal_iter`, `rbsp_roundtrip`, `bytereader_arbitrary` |

Run the fuzzers with `just fuzz <target>` (nightly; `cargo-fuzz` required).

No black-box differential oracle exists for a bit reader in isolation. Coverage
arrives indirectly once `vaco-parse-h264` lands and `ffprobe -show_streams` over
a corpus becomes an end-to-end differential test of this crate. Until then
`annexb::nal_units` can be checked against `ffmpeg -bsf:v trace_headers`, which
prints NAL boundaries and types — an available oracle worth wiring up early.
