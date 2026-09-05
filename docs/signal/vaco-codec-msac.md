# `vaco-codec-msac`

## What it is

Layer 3 entropy engines consumed by `vaco-codec-av1`, `vaco-codec-vp8`,
and `vaco-codec-vp9`. They own range arithmetic and adaptation; codec crates
own syntax-specific CDF contexts, probability tables, and tree shapes.

| Module | Public engine | Coding model |
|---|---|---|
| `av1` | `Av1SymbolDecoder` / `av1::SymbolDecoder` | AV1 §8.2: one N-ary symbol per cumulative-distribution lookup, with optional CDF adaptation |
| `vp8` | `Vp8BoolDecoder` | RFC 6386 §7.3: byte-buffered boolean arithmetic decoder |
| `vp9` | `Vp9BoolDecoder`, `Vp9BoolEncoder` | VP9 §9.2: boolean arithmetic coding with a leading marker bit |
| `tree` | `read_tree`, `read_tree_at`, `write_tree`, `write_tree_at` | Shared binary-tree traversal for VP8/VP9 syntax elements |

VP9's multi-outcome syntax is a tree of boolean reads. It does not use AV1's
N-ary range algorithm. Keeping the distinct engines behind this one crate
avoids conflating their initialization, normalization, and padding rules.

## How it works

Create a decoder over exactly one tile or partition. VP8 primes two bytes;
VP9 primes one byte and consumes a zero marker. Their common tree traversal
accepts a closure supplying each node's boolean read.

AV1 initializes a 15-bit value/range pair and adapts the caller's CDF in
place. For N outcomes, supply N+1 entries: ascending cumulative thresholds,
the fixed terminal value 32768, then the adaptation counter (0..=32).
The counter and thresholds belong to the codec's context bank. The
`disable_cdf_update` constructor argument preserves those entries when set.
`vaco-codec-av1::symbol::SymbolDecoder` is a compatibility re-export of this
shared implementation, so real AV1 tile decoding follows the same path as
the shared-core tests.

Reads remain deterministic on exhausted input. VP8/VP9 flag `overrun()`
after reading padding. AV1 allows the specification's fourteen padding bits
and flags exhaustion when `SymbolMaxBits < -14` (§8.2.4). Its `exit_symbol`
advances over remaining real bits but does not validate the trailing-bit
pattern. Callers must check exhaustion at syntax boundaries; bounded loops
remain their responsibility.

## How to change it

Change AV1 arithmetic in `src/av1.rs`; change syntax context selection and
default CDFs in `vaco-codec-av1`. Do not restore a second symbol engine in
that codec. Valid AV1 CDFs have 2..=16 outcomes, sorted thresholds bounded
by 32768 and a terminal value of 32768. They are codec-owned state, not raw
untrusted CDF arrays.

Changes to tree traversal must preserve both boolean engines' callers.
Changes to initialization or padding require reference traces and codec
fixtures: matching a self-written encoder alone is insufficient.

## Configuration

No feature flags or environment variables. AV1's per-tile
`disable_cdf_update` controls adaptation. The shared crate's availability
does not enable AV1 codec registration; its broader reconstruction scope is
documented in [the AV1 codec reference](../codec/vaco-codec-av1.md).

## Dependencies

`vaco-bitstream` supplies AV1's `BitReader`. VP8/VP9 manage their own input
cursors. `vaco-core` provides the shared error taxonomy; `proptest` is
test-only. There are no external runtime libraries.

## Verification

`tests/av1_oracle.rs` compares 640 decoded symbols against the installed
libaom 3.14.1 library: 128 reads each for skewed 2-, 3-, 4-, 7-, and 16-outcome
CDFs. `tests/libaom_trace.c` regenerates the frozen binary trace. The harness
only declares the reference ABI and links the installed library; no reference
decoder source is embedded. Source/ABI provenance is in
`provenance/vaco-codec-msac-oracle.toml`.

Unit tests cover adaptive CDF ordering/counter saturation, a fixed adaptive
spec trace, disabled adaptation, padding exhaustion, wide literals, both
boolean engines, and tree traversal. The existing
`msac_bool_decoders` fuzz target exercises all three engines, including
AV1 alphabet sizes 2..=16, adaptation, determinism, and sticky exhaustion.
Codec fixtures check integration; the AV1 codec retains its independently
documented reconstruction gaps.

## Specification

AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1
(`aom-av1-spec`) §8.2; RFC 6386 (`rfc-6386`) §7; VP9 Bitstream & Decoding
Process Specification v0.6 (`vp9-bitstream-spec-v0.6`) §9.2–9.3.
