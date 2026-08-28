# `vaco-codec-h264`

Layer 5. H.264/AVC entropy decoding (T3-01d/#417 CAVLC, T3-01e/#418 CABAC)
plus enough of the macroblock layer (#419) to drive CAVLC end to end,
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

[`mb::decode_slice_cavlc`] (#419) drives a whole CAVLC slice's macroblock
loop — `mb_type`/`sub_mb_type` classification (Tables 7-8/7-10/7-11/
7-14/7-15), `mb_skip_run`, `ref_idx`/`mvd` presence and count per partition,
`coded_block_pattern`/`mb_qp_delta`, and the neighbour-derived `nC` clause
9.2.1 needs — far enough to assert bit-exact consumption against real
`libx264` output across I/P/B slices, multiple slices per picture, and both
skipped and coded macroblocks. It does not decode CABAC's macroblock layer
(no context tables for `mb_type`/`mb_skip_flag`/etc. — see "What is not
implemented") and it does not reconstruct anything: no motion vector,
reference index, or pixel is ever produced, only read for its bit length.

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

Prediction, motion compensation, transform and reconstruction, deblocking,
DPB/reference management, threading, conformance bring-up — #420 onward.
`H264Decoder::send_packet` still resolves only a real slice's entropy mode
(verified against real `ffmpeg 8.1 -coder cavlc`/`-coder cabac` output,
`tests/decoder.rs`) and returns `Error::Unsupported` naming exactly that
gap; [`mb::decode_slice_cavlc`] is not yet wired into it, since nothing it
reads is kept beyond what bit consumption needs.

Within #419's own scope, explicitly out rather than merely unimplemented
(see `mb.rs`'s own module doc for the full list and reasons):

- **CABAC's B-slice `mb_type`/`sub_mb_type`** (Table 9-27/9-28's B column)
  — I and P/SP slices are implemented (`decode_slice_cabac`); B is refused
  outright. Table 9-27's bin string, unlike every other binarisation in
  this crate, does not decompose into a clean arithmetic formula the way
  Table 9-26 (I slices) and the P/SP table do, and hand-deriving it
  bit-by-bit from the primary text without a second, independent way to
  check the result risked exactly the class of silent, undetectable error
  this whole line of work exists to avoid.
- **CABAC's 8x8 residual category** (`ctxBlockCat` 5, `transform_size_8x8_flag`)
  — same Main-profile-corpus reason as CAVLC's; chroma DC (`ctxBlockCat` 3)
  *is* implemented on both the residual (`cabac_residual.rs`) and
  macroblock-layer (`coded_block_flag`, `mb.rs`) sides, since it is not
  avoidable the way 8x8 is (see "Verification" below for the one
  unresolved wrinkle it left).
- **MBAFF** (`mb_adaptive_frame_field_flag`) and field pictures —
  `decode_slice_cavlc` refuses outright rather than silently getting the
  frame-only neighbour derivation wrong for it. Neighbour availability
  changes shape entirely under MBAFF (macroblock pairs, parity-dependent
  derivation); scoping it out was a deliberate choice, not an oversight.
- **The 8x8 luma transform** (`transform_size_8x8_flag`, `Intra_8x8`,
  High-profile only) — the primary source this crate's tables are verified
  against predates it entirely, and the test corpus is encoded Main
  profile specifically to avoid emitting it.
- **`constrained_intra_pred_flag`'s neighbour substitution rule**, 4:2:2/
  4:4:4 chroma, `SI` slices, `I_PCM` — each refused explicitly by
  `check_scope` rather than attempted incorrectly.

## Verification: what is and is not claimed

**Reference-verified**: `H264Decoder`'s slice-header location and
`entropy_coding_mode_flag` resolution, against real `ffmpeg 8.1`-encoded
CAVLC and CABAC elementary streams (`tests/decoder.rs`, fixtures under
`tests/fixtures/`).

**Reference-verified, bit-exact**: [`mb::decode_slice_cavlc`] against two
real `ffmpeg 8.1`/`libx264 -coder cavlc` elementary streams
(`tests/macroblock_layer.rs`, `tests/macroblock_layer_simple.rs`):

- `cavlc_ipb.264` — Main profile, I/P/B slices, **two slices per picture**,
  B-pyramid (`-bf 2`), multiple reference frames (`num_ref_idx_l0/l1_active`
  up to 3). 50 slices (6 I, 16 P, 28 B), every one asserted to end with
  nothing but `rbsp_slice_trailing_bits()` unconsumed — not merely "no
  error returned".
- `cavlc_ip_simple.264` — Main profile, I+P only, single reference, single
  slice per picture — the isolation case that caught the multi-slice bug
  below before the fuller corpus's B-slice content did too.

Two real bugs this measurement caught that no amount of hand-built-fixture
or self-consistency testing could have, because both need a real multi-part
picture to manifest:

1. **A skipped macroblock never updated the neighbour grid.**
   `mb_skip_run` advances `CurrMbAddr` without ever calling
   [`decode_macroblock_cavlc`], so a skipped macroblock's 4x4 blocks stayed
   `NBlock(None)` (unavailable) forever, instead of clause 9.2.1's "`TotalCoeff`
   inferred to be 0" — silently steering the *next* real macroblock's `nC`
   onto the wrong `coeff_token` table row. Exactly the "works on I-slices,
   drifts on P" shape the dispatch warned about: an I-slice has no
   `mb_skip_run` at all, so an I-only corpus could never have found this.
2. **`more_rbsp_data()` was checked one branch too late.** Clause 7.3.4's
   `slice_data()` checks `moreDataFlag = more_rbsp_data()` immediately after
   a *nonzero* `mb_skip_run`, before ever deciding whether to call
   `macroblock_layer()` for the macroblock the skip run landed on. A
   two-slice picture's non-final slice can end with exactly this shape — a
   skip run consuming the rest of *that slice's own* macroblocks, with only
   `rbsp_slice_trailing_bits()` left — and `CurrMbAddr` is still less than
   the *picture's* total macroblock count at that point, since a slice
   boundary is not a picture boundary. Without the check, decoding read
   straight into the next slice's own NAL as if it were more of this one. A
   single-slice-per-picture corpus (`cavlc_ip_simple.264`) could never have
   found this either — it takes a real multi-slice picture, which is why
   the dispatch's "build the corpus for the branches" instruction named
   multiple slices per picture explicitly.

**Built, but not yet bit-exact**: [`mb::decode_slice_cabac`], covering I
and P/SP slices (`mb_type`, `sub_mb_type`, `mb_skip_flag`,
`coded_block_pattern`, `mb_qp_delta`, intra-4x4 pred-mode flags, `ref_idx`,
`mvd`, and `coded_block_flag` including chroma DC). B slices are refused
outright (see "What is not implemented"). Against three real
`libx264 -coder cabac` corpora built for this
(`tests/macroblock_layer_cabac.rs`, fixtures `cabac_ip_simple.264`,
`cabac_ip_multiref.264`, `cabac_i_only.264` — the last one all-intra,
generated specifically to isolate whether a bug was P-slice-specific), bit
consumption still diverges from the real bitstream partway through, so all
three tests are `#[ignore]`d with an exact reproduction case in the ignore
reason rather than deleted or left silently passing. What building this
layer did catch, before the remaining divergence:

1. **The residual context tables were structurally wrong, not just
   imprecise.** `cabac_residual.rs`'s original `ContextSet` used one shared
   `(m, n)` table across all `ContextCategory` values and ignored
   `cabac_init_idc` for P/B slices entirely, despite being marked
   "provisional pending an independent check". Cross-checking against the
   primary text found every value wrong; fixed by transcribing Tables
   9-19/9-20/9-21 in full, per category and per `cabac_init_idc`.
2. **The same skipped-neighbour bug CAVLC had, with a CABAC-shaped twist.**
   `mb_skip_flag` is itself context-coded from neighbours' *own* skip
   status (clause 9.3.3.1.1.1), not merely their availability — tighter
   than CAVLC's `mb_skip_run`, where a skipped macroblock only needed to
   stop being `NBlock(None)`. A skipped macroblock here must contribute a
   *decoded* skip state to its neighbours' context derivation.
3. **Chroma DC's `coded_block_flag` was never actually read.** The
   original code inferred `ctxBlockCat=3`'s presence from
   `coded_block_pattern` alone and called `residual_block_cabac`
   unconditionally, skipping the per-block flag clause 7.3.5.3.3 actually
   requires (its own context table, ctxIdx97-100, per Table 9-30). Found
   against `cabac_ip_multiref.264`, where an `Intra16x16` P-slice
   macroblock with `cbp_chroma=1` hit a premature `end_of_slice_flag`.
   Fixed, but fixing it did not by itself reach bit-exactness — see below.

**A second pass found two more real bugs, and ate the chroma-DC repro
above entirely.** Before hand-debugging, the fixed `h264_entropy` fuzz
harness (widened from a one-byte to a two-byte selector — see below) was
run against the newly-reachable `ChromaDc` category on its own: 10.5M+
executions, no crash, so whatever remained was a silent semantic
divergence rather than anything the residual decoder panics or hangs on.
Hand-review then found:

4. **`intra_chroma_pred_mode` was decoded but never stored.**
   `decode_macroblock_cabac` read the value correctly, but
   `set_mb_info` hardcoded `intra_chroma_pred_mode: 0` into
   `CabacMbInfo` regardless — so clause 9.3.3.1.1.8's own
   `condTermFlagN` (`... intra_chroma_pred_mode for mbAddrN == 0 ...`)
   could never see a neighbour's real, nonzero value. This is the very
   first context-coded element read inside every intra macroblock, ahead
   of `coded_block_pattern` and all residual reads, so a wrong context
   here shifts the arithmetic engine's range/offset for everything
   decoded afterward in that slice — a plausible explanation for a
   downstream symptom as far away as chroma DC's flags looking wrong.
   This single fix ate the previous exact repro entirely: it no longer
   reproduces on any of the three corpora.
5. **`ref_idx_cond_term`'s comparison was inverted.** Clause 9.3.3.1.1.6
   derives `refIdxZeroFlagN = (ref_idx_lX[mbPartIdxN] > 0) ? 0 : 1` and
   folds it into `condTermFlagN` such that `condTermFlagN = 1` exactly
   when the neighbour's own `ref_idx` is greater than 0; the code had
   `r <= 0` where it needed `r > 0`. Found by re-checking the primary
   text bin-by-bin against the function rather than from recollection —
   recollection had the polarity backwards.

The unresolved divergence, narrower now: `cabac_ip_simple.264` gets
through 10 full slices before diverging at slice 10, macroblock address
3, decoded as `Intra4x4` where `ffmpeg -debug mb_type`'s own per-macroblock
type dump shows it should be `mb_skip_flag = 1`. The `ctxIdxInc` feeding
that specific read checks out by hand against clause 9.3.3.1.1.1 (left
neighbour address 2, available and not skipped → 1; above neighbour off
the top of the picture → 0), and `SKIP_P`/`MB_TYPE_P`/`CBP_LUMA`/
`CBP_CHROMA` were each re-verified whole against Table 9-13/9-18 and
matched exactly — so the true divergence is upstream of this address,
most likely within address 2's own decode, not found within this round's
time budget. `cabac_ip_multiref.264` now gets further too: every one of
its 36 macroblocks is visited and classified before
`CabacDecoder::malformed()` trips at the slice's own end-of-slice
bookkeeping. `cabac_i_only.264` no longer diverges at all in the sense
that matters: it decodes correctly through slice 5 and then hits `I_PCM`
at slice 6, which `decode_slice_cabac` refuses on purpose (out of scope)
rather than attempting unverified — a scope boundary, not a bug. Exact,
per-corpus repros are in `tests/macroblock_layer_cabac.rs`'s `#[ignore]`
strings so whoever picks this back up starts from the current minimal
cases rather than re-deriving them.

What full self-consistency does still cover: hand-built fixtures cited to
the exact table row/spec clause they exercise, round-trips through
`vaco-codec-cabac`'s own test-only encoder mirroring this crate's exact
bin sequence, and the `h264_entropy` fuzz target. That target's own
selector was widened from one byte to two specifically because
`ContextCategory` now has 5 values and `CabacInit` has 4, which no longer
fit in the bits left over after entropy-mode/`nC`/`max_num_coeff` in a
single byte — the one-byte version silently made the newly-added
`ChromaDc` category unreachable by fuzzing, exactly the "a harness too
narrow for its own state space reports false clean" failure shape now
recorded on `AGENT-CONSTRAINTS.md`. Fixed, and re-run (multiple passes,
5M-10M+ executions each, no crash) both generally and specifically
targeted at the newly-reachable `ChromaDc` cross product before any
hand-debugging in this round — clean, meaning whatever divergence
remained was a silent semantic one, not something this residual-layer
fuzz target could ever catch on its own.

## How to change it

`cavlc_tables.rs` holds every CAVLC constant; `cavlc.rs` is the only module
that decodes against them (and the only place a `TotalCoeff` exclusion is
added or lifted — lifting one requires re-running the exhaustive
prefix-conflict self-check first). `cabac_residual.rs` holds `ContextSet`'s
per-category, per-`cabac_init_idc` tables (Tables 9-19/9-20/9-21) and
`residual_block_cabac`; `cabac_mb_tables.rs` holds every macroblock-layer
context-initialisation table (`(m, n)` pairs from Tables 9-11 through
9-33, including the B-slice `mb_type`/`sub_mb_type` rows kept
`#[allow(dead_code)]` for whoever lands B support) — both are pure data
modules with no decode logic of their own, so a new `ContextCategory` (8x8)
or a new binarisation means adding its table there and wiring it into the
matching function in `cabac_residual.rs` or `mb.rs`. `mb.rs` holds the
whole macroblock layer for both entropy modes: `classify_mb_type`/
`classify_sub_mb_type` are the only places Tables 7-8/7-10/7-11/7-14/7-15
are transcribed and are shared by both `decode_slice_cavlc` and
`decode_slice_cabac`; `NeighbourGrid` is CAVLC's `nC` state, `CabacGrids`
is CABAC's equivalent (macroblock info, `coded_block_flag` per 4x4 block
*and* per chroma-DC macroblock, and per-partition `MvInfo`) — the two
are separate because CABAC's `ctxIdxInc` derivations need neighbours'
*decoded* values (skip status, `coded_block_flag`, `ref_idx`/`mvd`), not
merely their availability the way CAVLC's `nC` does. Picking up the
open chroma-DC divergence (see "Verification") means starting from
`tests/macroblock_layer_cabac.rs`'s exact repro and instrumenting
`decode_residual_cabac`'s chroma-DC branch and the `cbf_cond_term`/
`cbp_chroma_cond_term` helpers immediately above it in `mb.rs` — every
other path (table transcription, the generic `ctxIdxInc` formula,
`blk_xy`) was manually re-checked against primary text during this work
without finding the error, so the bug is more likely in ordering or state
carried between macroblocks than in a single formula. `decoder.rs` is the
only place that touches `vaco-parse-h264`; wiring `mb::decode_slice_cavlc`/
`decode_slice_cabac` into `H264Decoder::send_packet` for real output is
#420's job, once prediction and reconstruction exist to do something with
what it reads — and CABAC's wiring should wait for the divergence above to
be resolved first.

## Configuration

`vaco_limits::Limits`/`Budget` bound every allocation (`residual_block_cavlc`/
`residual_block_cabac`'s coefficient arrays, sized from attacker-controlled
`TotalCoeff`/`max_num_coeff`/scan-position counts).

## Dependencies

`vaco-codec-cabac` (the arithmetic engine and general binarisation
primitives — `decode_tu`, `decode_bypass_egk` — this crate builds
`coeff_abs_level_minus1`'s bin-by-bin decode over), `vaco-codec-golomb`
(the `pic_parameter_set_id` pre-scan in `decoder.rs`, and every
`ue(v)`/`se(v)`/`te(v)`/`me(v)` `mb.rs`'s macroblock layer reads —
`BoundedGolomb`, `ChromaArrayType`, `MbPartPredMode`, `cbp_from_code_num`'s
Table 9-4 mapping — none of it re-transcribed here), `vaco-parse-h264`
(slice-header location and parameter-set bookkeeping — not
re-implemented here), `vaco-bitstream`, `vaco-limits`, `vaco-codec-core`,
`vaco-frame`, `vaco-packet`.
