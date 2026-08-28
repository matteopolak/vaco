# `vaco-codec-msac`

Layer 3. Boolean-entropy decoders shared by VP8 and VP9 (D-04).

## What it is

Both VP8 and VP9 code their entire compressed payload — everything but the
uncompressed frame tag and, for VP9, the partition-size prefix — through a
boolean arithmetic coder: one bool at a time, at an 8-bit probability,
optionally shaped into a small alphabet by a binary tree walk. This crate is
the two engines and nothing else. It knows about probabilities, ranges and
tree shapes; it does not know what a macroblock, a coefficient token or a
motion vector is. `vaco-codec-vp8` (and, eventually, a VP9 decoder) own that.

| Module | Contents |
|---|---|
| `vp8` | `Vp8BoolDecoder` — RFC 6386 §7.3's byte-buffered engine (2-byte init, `value >= split<<8` comparison), plus `read_flag`/`read_literal`/`read_signed_literal`/`read_magnitude_and_sign`/`read_tree` convenience methods |
| `vp9` | `Vp9BoolDecoder` — the VP9 spec's §9.2 bit-at-a-time engine (1-byte init via `f(8)`, a mandatory zero marker bit, `value < split` comparison) |
| `tree` | `read_tree`/`read_tree_at` — the tree-walk shared by both, since it turns out to be identical once each engine supplies its own `read_bool` |

### What is deliberately not here

**AV1's multi-symbol range coder.** `msac` is `libaom`'s own name for its
arithmetic coder, which is where this crate's name comes from, but AV1's
coder decodes an *N*-ary symbol in one step via a cumulative-distribution
table — a genuinely different algorithm. VP9 has no such thing: every VP9
syntax element with more than two outcomes is a binary tree over the same
bit-at-a-time engine `vp9` provides here (checked against the VP9 spec
§9.3). AV1 decode was out of scope for the package this crate was built for,
so its engine is not implemented.

**Per-syntax-element trees and probability tables.** `tree::read_tree` walks
*a* tree; which tree, and which probabilities index it, is spec vocabulary
that belongs to the codec crate (`KF_YMODE_TREE`, `COEFF_TREE`, and so on all
live in `vaco-codec-vp8::tables`).

## How it works

### Two engines, one tree-walker

VP8's and VP9's boolean coders are numerically distinct — different initial
fill (2 bytes vs. 1 byte plus a marker bit), different comparison direction,
different renormalisation shift-in — so they cannot share one `read_bool`.
But RFC 6386 and the VP9 spec each describe their tree-coded syntax elements
the same way: walk a small integer array, two entries per node, taking the
next index (positive) or terminating on a leaf (`entry <= 0` returns
`-entry`). That walk doesn't care which engine supplied the bit, so it is
written once in `tree::read_tree_at` against an `impl FnMut(usize) -> bool`
closure, and both `vp8::BoolDecoder::read_tree` and
`vp9::BoolDecoder::read_tree` are one-line wrappers around it.

### Cross-validating with a shared oracle

Neither RFC 6386 nor the VP9 spec publishes a worked encode/decode pair for
its boolean coder, so both `vp8.rs` and `vp9.rs` carry a `BoolEncoder` test
type built from the spec's own encode-side arithmetic (bottom/range/bit-count
for VP8; the equivalent for VP9's marker-bit variant). The two are
deliberately *the same* byte-buffered algorithm: the VP9 spec states its
engine is "logically identical" to VP8's once you account for the leading
marker bit, so the VP8-style encoder plus one prepended marker bool is a
valid oracle for the VP9 decoder too — this is exercised directly in
`vp9.rs`'s test module rather than duplicated.

### Error model

Neither decoder returns `Result` from a per-bool read — an under-length
partition has nothing sensible to fail *onto* mid-symbol, since the
tree-walk must still terminate at a leaf. Both engines mirror
`vaco-codec-cabac`'s convention instead: reads past the end of the supplied
buffer return zero bits (VP8) or `newBit = 0` (VP9, which is what the VP9
spec's own §9.2.2 prescribes for the equivalent case), and the caller checks
`overrun()` once per syntax structure rather than after every bin.

## How to change it

- **Adding a convenience method to one engine** (e.g. a new fixed-width
  literal helper) — add the matching method to the other engine too, or note
  in the doc comment why it doesn't apply, so the two decoders don't drift
  into different feature sets for what is conceptually the same interface.
- **Gotcha: VP8's engine buffers 2 bytes up front; VP9's buffers 1 byte plus
  consumes a marker bit before returning from `new`.** A change to either
  engine's `new` that doesn't preserve this will desync every subsequent
  read by a fixed offset — the kind of bug that survives unit tests on
  short, hand-built inputs and only shows up on real bitstreams (see
  `vaco-codec-vp8`'s doc for exactly this shape of bug, found downstream in
  the partition-splitting logic rather than here).
- **Extending `tree::read_tree_at`** — it is deliberately engine-agnostic
  (`impl FnMut(usize) -> bool`, not a concrete decoder type). Keep it that
  way; a VP9 base decoder needing it will import it same as
  `vaco-codec-vp8` does.

## Configuration

None. No features, no environment variables.

## Dependencies

| Crate | For |
|---|---|
| `vaco-core` | the shared error taxonomy (not currently returned by a hot-path read, but kept for API consistency with the rest of the signal layer) |
| `vaco-bitstream` | nothing beyond byte-slice access — neither engine's shift-in rule matches `BitReader`'s big-endian bit cursor closely enough to reuse it directly, so both manage their own bit position |

Dev-only: `proptest`. No external runtime dependencies.

## Verification

- `src/vp8.rs` / `src/vp9.rs` unit tests — the `BoolEncoder` oracle
  round-tripped through the corresponding `BoolDecoder` for both fixed bin
  sequences and pseudorandom probability/bit scripts, plus panic-freedom
  proptests over arbitrary byte buffers (an under-length or empty partition
  must decode to a deterministic all-zero-ish result, never panic).
- `src/tree.rs` unit tests — a small hand-built tree walked to each of its
  leaves.
- 13 tests total, `cargo clippy -p vaco-codec-msac --all-targets -- -D
  warnings` clean.

## Specification

RFC 6386 (`rfc-6386`) §7 for VP8; the VP9 Bitstream & Decoding Process
Specification v0.6 (`vp9-bitstream-spec-v0.6`) §9.2-9.3 for VP9. No table in
this crate came from an existing decoder implementation (D7) — both engines'
constants are the specifications' own algorithmic parameters (initial range
255, minimum range 127/128, the marker-bit rule), not transcribed data
tables.
