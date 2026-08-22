# `vaco-codec-cabac`

Layer 3. Context-adaptive binary arithmetic coding, ITU-T H.264 clause 9.3 — the
entropy coder H.264, HEVC and VVC are built on.

## What it is

The arithmetic coder and nothing else. It knows about bins, contexts and an
arithmetic interval; it does not know what a macroblock, a coding unit or a
motion vector is. That separation is what lets one engine serve every codec in
the family — H.264 and H.265 use the *identical* engine and the *identical*
tables, differing only in how their context initialisation values are spelled.

| Module | Contents |
|---|---|
| `tables` | Table 9-44 (`rangeTabLPS`) and Table 9-45 (`transIdxLPS`/`transIdxMPS`), plus two tables derived from them at compile time |
| `context` | `ContextModel`, both initialisation formulas, the context-set helpers |
| `decode` | `CabacDecoder` — `DecodeDecision`, `DecodeBypass`, `DecodeTerminate`, the binarizations |
| `encode` | `CabacEncoder` — the clause 9.3.4 counterparts |

### What is deliberately not here

**Per-syntax-element context initialisation values.** H.264's Tables 9-12 to
9-33 and H.265's equivalents are indexed by `ctxIdx` assignments that only a
specific codec's slice syntax defines. Holding them here would make this crate
know what a macroblock is, which `10-architecture.md` §1.5 forbids of a shared
layer. Both *derivation formulas* are here — `ContextModel::init_h264` (clause
9.3.1.1) and `ContextModel::init_hevc` (H.265 clause 9.3.2.2) — together with
`init_contexts` / `init_contexts_hevc`, which apply them to a whole set. The
values belong to `vaco-codec-h264` and `vaco-codec-hevc`.

**`ctxIdxInc` derivation.** Which context a bin uses depends on neighbouring
blocks, and neighbour availability is a codec concept.

**Why there is an encoder** when Vaco ships no H.264 or HEVC encoder (D9 puts
both outside the default build): a CABAC decoder cannot be tested against
hand-written bit patterns. The standard publishes no worked bitstream, and a bin
sequence reaches bytes only through the whole adaptive state machine. An encoder
written from clause 9.3.4 turns correctness into a property — encode arbitrary
bins with arbitrary contexts, decode, get the same bins — which exercises every
state transition, every renormalisation width and the carry propagation at once.
It costs about 150 lines and it is the only real oracle available.

## How it works

### The inner loop is the specification's shape, and that was a measurement

Two plausible optimisations were implemented first, and **both are slower**.
`benches/cabac.rs` keeps all four combinations so the result stays visible and a
future toolchain that changes it is caught.

Apple M5, 8192 bins over 64 contexts, min of 300 samples, three runs agreeing:

| Decision | Renormalisation | skewed (~6% ones) | even (~50% ones) |
|---|---|---|---|
| **branchy (spec)** | **per-bit (spec)** | **15.7 µs** | **17.2 µs** |
| branchy | whole-width | 23.5 µs | 22.8 µs |
| branchless | per-bit | 21.7 µs | 24.0 µs |
| branchless | whole-width | 29.0 µs | 30.3 µs |

The shipped engine measures 16.4 µs / 18.2 µs, matching the winning row (the
small extra gain comes from the packed context, below). Moving to it was a
**1.76x speed-up** over the version this crate was first written with.

**Branchless decision costs ~35%.** The argument for it was that the MPS/LPS
outcome is a coin flip and therefore the worst thing to leave to a branch
predictor. That is wrong about the machine: replacing the branch with masked
selects makes every step of a bin depend on the previous one, and the
out-of-order engine can no longer start the next bin's table load before the
current one resolves. Speculating and occasionally being wrong beats never
speculating — and it wins on the *even* corpus too, which is what rules out
"the benchmark was too skewed" as an explanation.

**Whole-width renormalisation costs ~45%.** `RenormD` is specified as a per-bit
loop, and computing the shift count from `leading_zeros` to do one
`BitReader::get(n)` looks strictly better. It is not: `get` with a *variable*
width carries an internal `if n == 0` early return, a `min(32)` clamp and a
variable `64 - n` shift, none of which survive when the width is the constant 1.
The loop body is cheaper than the thing meant to replace it, and it runs zero or
one times for almost every bin.

The same result showed up a third time in `decode_bypass_bits`: pulling all `n`
bits from the reader in one `get(n)` measured 1.2x *slower* than a plain loop,
because the serial comparison chain dominates completely. That method is a
loop now, and exists for its interface rather than for speed.

### The one optimisation that does pay: the packed context

`ContextModel` is one byte, `(pStateIdx << 1) | valMPS` — which is exactly what
clause 9.3.1.1's `preCtxState` derivation already produces. That lets

```text
if (pStateIdx == 0) valMPS = 1 - valMPS
pStateIdx = transIdxLPS[pStateIdx]
```

be folded into the transition table at compile time: `TRANS[256 + state]` is the
LPS successor *including* the flip. This removes a **conditional** without
removing a **branch the processor was predicting well**, which is exactly the
distinction the measurements above turned on. It also makes a whole context set
a plain `[ContextModel; N]` that clones for free, which is what wavefront
threading needs.

`TRANS` and `LPS_RANGE` are 512 entries each although only 128 states exist: the
upper half mirrors the lower, so indexing by a whole `u8` is provably in bounds
and LLVM removes the check. `tests/spec.rs` asserts the mirroring and re-derives
both tables from the normative ones.

### The invariant everything rests on

**`ivlOffset < ivlCurrRange`, always.** The specification states it as a
constraint on conforming bitstreams (clause 9.3.1.2 forbids an initial offset of
510 or 511); this crate *enforces* it, because it is also the bound that keeps
`offset` from growing without limit.

With the invariant violated, `DecodeBypass` is `x ↦ 2x + 1 − range`, whose fixed
point is `range − 1`. Start above it and the value doubles away every bin until
it overflows — and the fuzzing profile turns overflow checks on deliberately, so
that is a panic on a malformed bitstream. So `CabacDecoder::new` clamps a
non-conforming initial offset and records it in `malformed()`.

This is also **the one place the engine deviates from the literal
specification**. Clause 9.3.3.2.4 reduces `ivlCurrRange` by 2 before the
`DecodeTerminate` comparison and leaves it reduced either way, which on the
terminating path can leave `ivlOffset >= ivlCurrRange`. Harmless in the
specification, because decoding stops there. Not harmless here, so the reduction
is not committed on the terminating path: the returned bin is identical, and a
caller that keeps going on a malformed stream still cannot break the engine.

**That deviation exists because the invariant test found the bug.** The property
test caught it on its first run, on step 335 of a pseudorandom buffer.

### Every loop over input has a ceiling

Bypass runs of ones, truncated-unary prefixes and `EGk` prefixes are all
terminated by the *bitstream*, which means an adversarial bitstream terminates
none of them:

| Loop | Ceiling |
|---|---|
| `decode_tu` | the caller's `c_max` |
| `decode_bypass_egk` | 32 prefix bins, then `malformed` is set |
| `decode_bypass_bits` | `n`, clamped to 32 |
| `renorm` | `range` at least 2, so at most 7 iterations |
| `CabacEncoder`'s `bitsOutstanding` | `with_limit`, then `overflowed()` |

Nothing in this crate can hang on any input.

## How to change it

- **Touching `decode_decision` or `renorm`** — re-run
  `cargo bench -p vaco-codec-cabac` and check that `e_shipped` still lands on
  `b_branchy_bitwise`. If it does not, the change cost something. Do not delete
  the other three candidates: they are the evidence, and the whole shape of this
  crate rests on a measurement that contradicted the obvious reasoning twice.
- **Any change to the engine** — the `offset < range` assertion in
  `tests/proptest_engine.rs` and in the fuzz target must still hold after
  *every* operation. It is not a nicety; it is what makes the arithmetic
  bounded.
- **Adding a binarization** — it goes on `CabacDecoder` with a matching
  `CabacEncoder` method, because the round-trip property test is how it gets
  verified. A decode-only binarization has no oracle.
- **Adding a codec's context tables** — they go in the codec crate, not here.
  Pass `&[ContextInit]` to `init_contexts`, or `&[u8]` of `initValue` bytes to
  `init_contexts_hevc`. Both truncate to the shorter of the two slices rather
  than panicking.
- **Gotcha: `ContextModel` is `(pStateIdx << 1) | valMPS`, not the reverse.**
  Every table index in `tables` assumes it.
- **Gotcha: the tables are indexed by the *packed* state**, so `LPS_RANGE` is
  addressed as `(state >> 1) * 4 + q`, not `state * 4 + q`.

## Configuration

None. No features, no environment variables. The only tunable is
`CabacEncoder::with_limit`, which caps encoder output in bytes;
`DEFAULT_MAX_BYTES` is 16 MiB, larger than any real slice and small enough that
a fuzz case driving carry propagation cannot exhaust memory.

## Dependencies

| Crate | For |
|---|---|
| `vaco-bitstream` | `BitReader` — the renormalisation loop's bit source, and `Padded` for the fast path |
| `vaco-core` | the shared error taxonomy |

Dev-only: `proptest`, `divan`. No external runtime dependencies.

## Verification

- `tests/spec.rs` (28 tests) — the tables' structural properties, the derived
  tables re-derived, both initialisation formulas worked by hand from clause
  9.3.1.1, `DecodeDecision` traced step by step in both MPS and LPS outcomes,
  `DecodeTerminate` at its boundary, and the engine invariant over 500
  pseudorandom buffers.
- `tests/proptest_engine.rs` (9 properties) — encode/decode round trips for
  mixed bin scripts, for every one of the 128 context states, and for `EGk` at
  every order; the invariant over arbitrary bytes; determinism; totality of both
  initialisation formulas and of the encoder.
- `fuzz/fuzz_targets/cabac_engine.rs` — the invariant after every operation,
  termination, `range` staying in its proved interval, and the encoder round
  trip. Last run: `exit=0 execs=#1803858` (90 s), no artifacts in
  `fuzz/artifacts/`.

## Specification

ITU-T H.264 (ISO/IEC 14496-10) clause 9.3 — 9.3.1.1 context initialisation,
9.3.1.2 engine initialisation, 9.3.3.1 binarizations, 9.3.3.2 the decoding
process including Tables 9-44 and 9-45, 9.3.4 the encoding process. ITU-T H.265
(ISO/IEC 23008-2) clause 9.3.2.2 for the HEVC context initialisation formula;
its engine and tables are identical to H.264's.

The tables are format-dictated: a conforming decoder must contain exactly these
numbers in exactly this order, which is the merger case D9/D15 describe. Nothing
here was taken from any implementation.
