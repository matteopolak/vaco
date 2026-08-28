# `vaco-codec-h264`

Layer 5. H.264/AVC entropy decoding (T3-01d/#417 CAVLC, T3-01e/#418 CABAC),
`encumbered = true` / `patent-encumbered-h264-decode`.

## What it is

The far side of the parse/decode line `vaco-parse-h264` (default build,
ungated) sits on one side of: entropy decoding turns coded bits into
coefficient values, which is reconstructing a sample and therefore D4's
concern, following the precedent `vaco-codec-aac` set as the first
`encumbered = true` component in the tree. Two residual-block entropy
decoders, symmetric in shape and in scope:

- [`cavlc::residual_block_cavlc`] — clause 9.2 / 7.3.5.3.1-2: `coeff_token`
  (Table 9-5), `level_prefix`/`level_suffix` (clause 9.2.2.1),
  `total_zeros` (Tables 9-7/9-8/9-9), `run_before` (Table 9-10).
- [`cabac_residual::residual_block_cabac`] — clause 9.3, built over
  `vaco-codec-cabac`'s engine: the significance map
  (`significant_coeff_flag`/`last_significant_coeff_flag`) and
  `coeff_abs_level_minus1` (`UEGk`, reimplemented bin-by-bin rather than
  through `CabacDecoder::decode_uegk`, since this element's context group
  changes between `binIdx == 0` and `binIdx >= 1`).

[`decoder::H264Decoder`] resolves a real slice's `entropy_coding_mode_flag`
(via `vaco-parse-h264`'s already-tested parameter-set/slice-header parsing)
and then returns `Error::Unsupported`, honestly — see "What is not
implemented" below.

## How it works

### The scope line, drawn the same way on both sides

Both entropy functions take exactly what a caller *outside* the macroblock
layer can supply — `nC` for CAVLC, a caller-derived `coded_block_flag`
condition and `ctxBlockCat`/category for CABAC, `max_num_coeff` for both —
and nothing that needs neighbouring-macroblock state to derive. This is the
same separation `vaco-codec-msac` draws around VP8/VP9's bool decoders,
applied to H.264: this crate does not know what a macroblock is, what
`mb_type` means, or how many residual blocks a partition has. That is
explicitly #419 onward's job (macroblock layer, prediction, transform
reconstruction), not this dispatch's.

CABAC's coverage is narrower within its own scope than CAVLC's: only
`ctxBlockCat` 0/1/2/4 (`LumaDc`/`LumaAc`/`Luma4x4`/`ChromaAc` —
[`cabac_residual::ContextCategory`]) are implemented. Chroma DC
(`ctxBlockCat` 3) and 8x8 transform blocks (`ctxBlockCat` 5, High-profile
only) use different, non-identity `significant_coeff_flag` context tables
this crate does not transcribe — deferred alongside the macroblock layer
that would need transform-size selection to reach them anyway.

### CAVLC's tables: re-verified against primary spec text, one gap remains

The first pass's tables were transcribed from recollection. An exhaustive
pairwise prefix-conflict self-consistency check (`cavlc.rs`'s own test
module, kept permanently) found real conflicts in `COEFF_TOKEN_NC2`/`NC4`/
`CHROMA_DC_420` and 7 of `TOTAL_ZEROS_4X4`'s 15 rows — the same class of
transcription bug the same day's MPEG-2 `CODED_BLOCK_PATTERN` finding
named. That check is necessary but was **not sufficient on its own**:
cross-checking every table entry-by-entry against a fetched primary source
(`provenance/vaco-codec-h264.toml`'s `iso-iec-14496-10-2002-draft`, the
original ISO/IEC 14496-10 draft text) found *additional* wrong entries that
had happened to still be prefix-free and so passed the self-consistency
check cleanly — most strikingly, over half of `RUN_BEFORE`'s `zerosLeft >
6` row, a table the first pass had reported as "fully clean."

**Corrected and now verified against that primary source, zero exclusions**:
`COEFF_TOKEN_NC0`/`NC2`/`NC4`/`CHROMA_DC_420`, `TOTAL_ZEROS_4X4`/
`CHROMA_DC_420`, `RUN_BEFORE`.

**Still not independently verified** (self-consistent, nothing more):
`COEFF_TOKEN_CHROMA_DC_422` and `TOTAL_ZEROS_CHROMA_DC_422` — the 4:2:2
chroma-DC columns (`nC == -2`). The fetched source is the *original* 2002
baseline text, which predates 4:2:2 chroma-DC support (a later amendment's
addition), so there is no corresponding table in it to check against.
`TOTAL_ZEROS_CHROMA_DC_422`'s `TotalCoeff` 2 and 3 rows still fail even the
self-consistency check and remain excluded (`Error::Unsupported`);
`COEFF_TOKEN_CHROMA_DC_422` happens to pass that check, which — given how
many of this file's now-corrected rows also once did — should not be read
as more than "not yet known to be wrong."

**A second, independent primary source was sought and not obtained**: the
standing instruction asked for two independently-hosted editions
cross-checked against each other. Only one was retrieved in this
environment — `itu.int`'s own PDF gateway rejected direct downloads
outright, and a second host's copy exceeded the fetch tool's size limit —
so this correction rests on one fetched source, not two. Recorded as a
real limitation rather than elided; a second copy would let a future pass
close the 4:2:2 gap and add real cross-edition confidence to the tables
already corrected once.

**Also found and removed, not merely relocated**: the original
implementation's `level_prefix >= 16` handling in `decode_level` (clause
9.2.2.1) — a specific bit-shift formula for high-bit-depth content — does
not appear anywhere in the fetched primary source, which has no case for
`level_prefix` past 15 at all (that extension postdates this source's
2002 baseline). Rather than keep an unconfirmed formula, `decode_level` now
implements exactly what the verified text specifies (`levelSuffixSize` is a
**fixed** 12 when `level_prefix == 15`, not `level_prefix - 3`; no `min(15)`
clamp on `level_prefix` in the shift; the `+15` bump is conditioned on
`== 15` exactly) and falls through to the base rule for `level_prefix > 15`
rather than inventing a bump term for it. Whether a later, bit-depth-extended
edition defines different behaviour there is an open question this crate
does not answer.

### Real bugs the `h264_entropy` fuzz target found and fixed

Three, all arithmetic-overflow panics on adversarial input, none reachable
by any conformant encoder but all reachable by a malformed or hostile
bitstream — exactly the class D6 exists to catch before it panics rather
than errors:

1. **CAVLC**: `run_before`'s `zerosLeft > 6` table row has codes for values
   up to 15/16 regardless of the *actual* `zerosLeft`, which can be as
   small as 7 — `zeros_left -= run` underflowed. Fixed: `run > zeros_left`
   is now checked and reported as `Error::InvalidData` before the subtract.
2. **CAVLC**: `level_prefix`'s unary read had no bound tight enough to keep
   the rest of clause 9.2.2.1's arithmetic (`1u32 << (level_prefix - 3)`,
   several additions) inside `u32`. Fixed: capped at 30 (comfortably beyond
   any bit depth/QP this specification permits) and every downstream
   arithmetic step made `checked_*`, erroring rather than overflowing.
3. **CABAC**: `decode_bypass_egk`'s own saturating accumulator can return a
   value near `u32::MAX` on an all-`0xff` bypass run, and
   `prefix + decode_bypass_egk(0)` (then `+ 1` for the level's magnitude)
   overflowed rather than saturating to match. Fixed: both additions are
   `saturating_add`.

All three have regression tests pinning the exact crashing input
(`cavlc.rs::residual_block_cavlc_refuses_a_run_before_larger_than_zeros_left`,
`cabac_residual.rs::residual_block_cabac_does_not_panic_on_an_all_ones_bypass_stream`,
plus the `level_prefix` cap exercised transitively by the same CAVLC fuzz
corpus). `cargo +nightly fuzz run h264_entropy` clean afterward:
`exit=0`, `execs=5093644` in 30s, `find fuzz/artifacts/h264_entropy -type f`
empty.

## What is not implemented

Everything from `mb_type` up: macroblock partitioning, intra/inter
prediction, motion compensation, transform and reconstruction, deblocking,
DPB/reference management, threading, conformance bring-up — #419 onward.
`H264Decoder::send_packet` resolves a real slice's entropy mode (verified
against real `ffmpeg 8.1 -coder cavlc`/`-coder cabac` output,
`tests/decoder.rs`) and then returns `Error::Unsupported` naming exactly
that gap, the same choice `vaco-codec-aac` made for the boundary between
"configuration/syntax resolved" and "samples produced".

**Not attempted this pass, by choice**: enough of #419 to drive a real
slice through both entropy paths and measure bit-exact consumption against
real `libx264` output — the goal the coordinator's follow-up dispatch set
once the tables were re-verified. Re-verifying the CAVLC tables against
primary spec text (the section above) turned out to be the larger and more
consequential half of that dispatch on its own — it found and corrected
real errors a self-consistency check alone had missed — and attempting a
necessarily narrow slice of the macroblock layer on top of it risked
exactly the "wider scope over tables you cannot yet trust" outcome the
dispatch explicitly said to avoid. Landing correct tables at a narrower
scope, and reporting #419 as not reached, was the instructed fallback.

## Verification: what is and is not claimed

**Reference-verified**: `H264Decoder`'s slice-header location and
`entropy_coding_mode_flag` resolution, against real `ffmpeg 8.1`-encoded
CAVLC and CABAC elementary streams (`tests/decoder.rs`, fixtures under
`tests/fixtures/`).

**Specification-and-self-consistency, not reference-verified**: both
residual-block entropy functions' bit-level decode. Real-corpus, bit-exact
verification of an entropy decoder against a real encoder's output needs
driving it across an entire slice's macroblock loop — knowing which syntax
element precedes each residual block and with what `nC`/`ctxBlockCat`/
`coded_block_flag` — which needs #419's macroblock layer to exist first.
Claiming that measurement now, before it is possible, would be exactly the
specification-only-dressed-as-verified gap a previous dispatch on this
project was asked to stop making. What is verified instead: hand-built
fixtures cited to the exact table row/spec clause they exercise, an
independent-of-the-table exact-bit-length/prefix-free test harness (CAVLC),
round-trips through `vaco-codec-cabac`'s own test-only encoder mirroring
this crate's exact bin sequence (CABAC), and the fuzz corpus above.

## How to change it

`cavlc_tables.rs` holds every CAVLC constant; `cavlc.rs` is the only module
that decodes against them (and the only place a `TotalCoeff` exclusion is
added or lifted — lifting one requires re-running the exhaustive
prefix-conflict self-check first). `cabac_residual.rs` holds `ContextSet`'s
`(m, n)` tables and the two decode functions; adding a fifth
`ContextCategory` (chroma DC or 8x8) means adding its own
`significant_coeff_flag`/`last_significant_coeff_flag` table and switching
on `category` where `residual_block_cabac` currently ignores it.
`decoder.rs` is the only place that touches `vaco-parse-h264`; extending it
toward #419 means adding macroblock-layer state there, not here.

## Configuration

`vaco_limits::Limits`/`Budget` bound every allocation (`residual_block_cavlc`/
`residual_block_cabac`'s coefficient arrays, sized from attacker-controlled
`TotalCoeff`/`max_num_coeff`/scan-position counts).

## Dependencies

`vaco-codec-cabac` (the arithmetic engine and general binarisation
primitives — `decode_tu`, `decode_bypass_egk` — this crate builds
`coeff_abs_level_minus1`'s bin-by-bin decode over), `vaco-codec-golomb`
(the `pic_parameter_set_id` pre-scan in `decoder.rs`), `vaco-parse-h264`
(slice-header location and parameter-set bookkeeping — not
re-implemented here), `vaco-bitstream`, `vaco-limits`, `vaco-codec-core`,
`vaco-frame`, `vaco-packet`.
