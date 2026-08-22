# `vaco-codec-golomb`

Layer 3. Exp-Golomb coding, ITU-T H.264 clause 9.1 — the codec-level layer that
sits above `vaco-bitstream`'s two-code minimum.

## What it is

`ue(v)`, `se(v)`, `te(v)`, `me(v)`, the order-`k` forms, the 64-bit forms, the
whole encoder side, the mappings as pure functions, codeword-cost functions for
rate-distortion loops, and a budget-carrying wrapper for reads driven by
untrusted syntax loops.

### Why this exists when `vaco-bitstream` already has `ue`/`se`

`vaco-bitstream` is layer 0 and deliberately carries only the two codes a
*demuxer* needs to look inside a parameter set — D14.1 requires that a format
crate never depend on a codec crate, so those two live at the bottom. Everything
else in clause 9.1 is codec knowledge and lives here:

| Here | Why not at layer 0 |
|---|---|
| `te(v)`, clause 9.1.1 | needs a `cMax` only slice syntax supplies |
| `me(v)` + Table 9-4, clause 9.1.2 | macroblock-level knowledge |
| order-`k` signed, 64-bit forms | only entropy coding uses them |
| the encoder side | a demuxer never writes Exp-Golomb |
| pure mappings and codeword costs | rate-distortion, not parsing |
| `BoundedGolomb` | layer 0 has no `vaco-limits` budget in scope |

There is one deliberate overlap: **`ue_v` re-implements `ue(v)`**, in a faster
shape. That is justified by measurement (below) and held safe by a differential
property test and a fuzz assertion that the two never decode the same bytes
differently.

## How it works

### Modules

| Module | Contents |
|---|---|
| `map` (public) | `se_value` / `se_code_num`, and the `*_bit_len` cost family. Pure `u32`↔`i32` transforms, no bitstream |
| `read` | `GolombDecode`, an extension trait on `BitReader` |
| `write` | `GolombEncode`, an extension trait on `BitWriter` |
| `tables` | Table 9-4 and the `ChromaArrayType` / `MbPartPredMode` selectors |
| `bounded` | `BoundedGolomb`: reader + `Budget`, one fuel unit per element |

### The faster `ue(v)`

The obvious implementation is `peek(32)` → `leading_zeros` → `skip(lz + 1)` →
`get(lz)`: two extractions from the reader's cache.

But a codeword with a prefix of 15 zeros or fewer is at most `2·15 + 1 = 31`
bits, so it is **already inside the word that was peeked**. Take the top
`2·lz + 1` bits of that word — that is `2^lz + suffix`, which is exactly
`codeNum + 1` — subtract one, and skip the whole codeword in one step.

Measured on Apple M5, `cargo bench -p vaco-codec-golomb`, min of 300 samples
over 4096 codewords, three runs agreeing. Both shapes are written side by side
in `benches/golomb.rs` (`shape_a_two_step` / `shape_b_one_peek`) so the
comparison cannot be an artefact of a crate boundary:

| Corpus | two-step | one-peek | |
|---|---|---|---|
| realistic (`codeNum` mostly < 2^12) | 7.25 µs | **6.87 µs** | 1.05x |
| uniform to 2^31 (prefixes of 16–31 zeros) | 17.0 µs | **12.1 µs** | 1.40x |

**`#[inline(always)]` is load-bearing and was found by measurement.** With plain
`#[inline]` the library function measured *identical to the two-step version*
while the same code inside the benchmark crate was 1.40x faster — LLVM declined
the cross-crate inline, and an out-of-line call spills the reader's cache,
position and bit count out of registers, costing more than the shape saves.

### Termination — the property that matters most

Every prefix is a `leading_zeros` over a fixed-width word with an explicit cap
(31 for the `u32` forms, 63 for `ue_v64`). **Nothing here loops on input.** An
all-zero buffer is rejected in constant time; `ue_v64`'s loop runs at most twice
by construction and has the ceiling check inside it. That is the difference
between a clean rejection and a fuzz hang, and it is asserted in three places:
a unit test, a property test, and the fuzz target's progress-or-flag invariant.

### Three layers of bounds

| Problem | Answer |
|---|---|
| absurd prefix | the 31-zero cap — structural, free, always on |
| plausible codeword, implausible value | `ue_v_max` / `se_v_range` / `te_v_checked` / `ue_k_max`, ceiling at the read site |
| a syntax *loop* that never ends | `BoundedGolomb`, one fuel unit per element, plus `ue_v_counted` which charges a declared count up front |

`BoundedGolomb` cannot be constructed without a `Budget`, for the reason
`vaco-limits` gives: an `Option<&mut Budget>` gets passed `None`.

### Table 9-4

Transcribed from ITU-T H.264 clause 9.1.2, Table 9-4 — four columns selected by
`ChromaArrayType` and by intra/inter prediction. A conforming decoder must
contain exactly these values in exactly this order; they are the format, not an
authorial choice (D9/D15 merger). Nothing was taken from any implementation.

The transcription check that makes this safe: **each column is a permutation**,
because the mapping must be invertible for an encoder to exist. `tables.rs`
asserts that for all four columns and asserts the inverse round-trips, which
catches a transposed digit that spot-checking would miss.

## How to change it

- **Adding a code** — put the pure mapping in `map`, the read in `read`, the
  write in `write`, and a cost function next to the mapping. The four move
  together; a code with a read and no write is how the round-trip tests stop
  covering it.
- **Touching `ue_v`** — re-run `cargo bench -p vaco-codec-golomb` and compare
  `shape_a_two_step` against `shape_b_one_peek`. If the two converge, the
  `#[inline(always)]` and the extra branch have stopped paying and the simpler
  shape should win. Do not remove those two benches; they exist so a toolchain
  bump that changes the answer is visible.
- **Any change to a read** — the differential test against
  `vaco_bitstream::GolombRead` must still pass on *garbage*, not just on valid
  input. Two implementations agreeing on valid input and diverging on malformed
  input is a differential bug.
- **Gotcha: `te(v)` with `cMax == 1` reads one bit and the value is its
  inverse.** Clause 9.1.1. Everyone gets this backwards once; there is a test
  named after it.
- **Gotcha: the domain edges are real.** `ue(v)` cannot represent `u32::MAX`
  (32-zero prefix), `se(v)` cannot represent `i32::MIN`. The writers clamp and
  `debug_assert` rather than panicking, because an encoder handed a bad value
  should produce a wrong bitstream in release, not take the process down.

## Configuration

None. No features, no environment variables, no runtime knobs. The only tunable
is the `Limits` a caller puts in the `Budget` it hands to `BoundedGolomb`;
`Limits::strict` allows 2^26 fuel units, which is far more syntax elements than
a real frame contains and far fewer than an unbounded loop wants.

## Dependencies

| Crate | For |
|---|---|
| `vaco-bitstream` | `BitReader`, `BitWriter`, `BitstreamError` |
| `vaco-limits` | `Budget` — `BoundedGolomb`'s fuel |
| `vaco-core` | the shared `Error` type `BoundedGolomb` reports through |

Dev-only: `proptest`, `divan`. No external runtime dependencies.

## Verification

- `tests/spec.rs` — clause 9.1's codeword table, Table 9-3, Table 9-4 spot
  checks, order-`k` worked examples, and the differential agreement with
  `vaco-bitstream` over both a dense corpus and 2000 pseudorandom buffers.
- `tests/proptest_roundtrip.rs` — round trips for every code, bijectivity of the
  signed mapping, cost functions against the writer's actual output, and
  progress-or-flag over arbitrary bytes.
- `fuzz/fuzz_targets/golomb_codec.rs` — progress-or-flag, cross-implementation
  agreement, bounded reads terminating, and encode/decode identity.
  Last run: `exit=0 execs=4914797` (60 s), no artifacts in `fuzz/artifacts/`.

## Specification

ITU-T H.264 (ISO/IEC 14496-10): clause 9.1 (`ue(v)`), 9.1.1 (`se(v)`, `te(v)`,
Table 9-3), 9.1.2 and Table 9-4 (`me(v)`). ITU-T H.265 clause 9.2 defines the
same `ue(v)`/`se(v)` and adds no variant this crate lacks.
