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

Most of this section is historical — from #419/#420's original scope, before
CABAC reconstruction, then CABAC B slices, then CAVLC reconstruction all
landed (see the later "## Round" sections below for each). Kept for the
refusals that are still real rather than rewritten, since a stale "not yet
implemented" that quietly became false is exactly the failure
`planning/AGENT-CONSTRAINTS.md`'s "never pin the absence of something the
project is building" names.

~~`H264Decoder::send_packet` still resolves only a real slice's entropy mode
... `mb::decode_slice_cavlc` is not yet wired into it~~ — **CAVLC now
reconstructs real pixels**, see "Round: CAVLC reconstruction" below.

Within #419's own scope, explicitly out rather than merely unimplemented
(see `mb.rs`'s own module doc for the full list and reasons):

- ~~**CABAC's B-slice `mb_type`/`sub_mb_type`**~~ — **implemented and
  byte-exact** as of the "B slices, multi-reference and `b-pyramid`" round
  below, which is also where the reasoning that used to live in this bullet
  (Table 9-27's bin string does not decompose into a clean arithmetic
  formula, so it was transcribed from JM 19.1 and bit-traced rather than
  hand-derived) is now recorded. What is still refused on the B path is
  **temporal direct** (`direct_spatial_mv_pred_flag == 0`).
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

**I_PCM was cheap, as expected, and closed one real gap.** `decode_slice_cabac`
now handles it: byte-align, skip `256 * ChromaFormatFactor = 384` raw
`pcm_byte[i]` (`u(8)`, no bit-depth dependency — the 2002 draft this
crate's tables are checked against predates that extension the same way
it predates the 8x8 transform), then re-initialise only the arithmetic
*engine* (clause 9.3.1.2 — fresh `ivlCurrRange = 510`, `ivlOffset` from
the next 9 bits) while leaving context *models* untouched (9.3.1.1 is not
re-invoked). `CabacDecoder` renormalises exactly one bit at a time with
no read-ahead (its own module doc explains why), so `into_reader()`
already hands back a `BitReader` positioned exactly where the raw bytes
start — no new state-tracking needed.

**The single most important finding this round was about the measurement
itself, not the decoder.** This test's own assertions —
`stats.macroblock_count == total_mbs` and `!cabac.malformed()` — are
*necessary* but were never *sufficient*: CABAC's arithmetic engine can
decode wrong values throughout an entire slice and still have
`end_of_slice_flag` fire at a macroblock-count-plausible point, since
neither check depends on what was actually decoded. `tests/
macroblock_layer.rs`'s CAVLC test already closes the equivalent gap with
`more_rbsp_data()`; this test never had the CABAC counterpart. Adding
`assert_slice_ends_at_rbsp_trailing_bits` (checks that what follows
`end_of_slice_flag` is exactly clause 7.3.2.10's `rbsp_slice_trailing_bits()`
— one stop bit, zero padding to the byte boundary, then zero or more
all-zero `cabac_zero_word`s) found that **all three corpora actually
diverge at slice 0** — not slice 10, not "36 of 36 macroblocks visited",
not "reaches slice 6's `I_PCM`" as the last two dispatches reported. Every
one of those was real progress on the bugs it found, correctly described,
but measured with a check too weak to see that not one single slice had
ever been bit-exact. This is the same failure shape `AGENT-CONSTRAINTS.md`
already tracks for a fuzz harness that could not reach its own state
space, a metric too narrow, a lag window that never contained the answer,
and a gate whose target list omitted a crate — here it is a test's own
assertions.

**What the corrected measurement narrowed the search to.** Address-by-
address cross-checking against `ffmpeg -debug mb_type` (letter meanings
confirmed by reading `get_type_mv_char`/`get_segmentation_char` in
FFmpeg's own `libavcodec/mpegutils.c` source directly, not assumed from
familiarity) found that `cabac_i_only.264`'s slice 0 — an all-`Intra4x4`
slice — has every single macroblock's classification match the reference
exactly, yet the arithmetic engine ends a bit or two short of
`rbsp_trailing_bits()`. That rules out a `ctxIdxInc`/context-table bug in
anything reachable before residual decode in an all-intra slice, all
re-verified against primary text this round: `MB_TYPE_I` (Table 9-12),
`SKIP_P`/`MB_TYPE_P` (Table 9-13), `PREV_INTRA4X4`/`REM_INTRA4X4`/
`INTRA_CHROMA_PRED_MODE`/`QP_DELTA` (Table 9-17), `CBP_LUMA`/`CBP_CHROMA`
(Table 9-18), and `cbf_cond_term`/`cbp_luma_cond_term`/
`cbp_chroma_cond_term`/`mvd_abs_term`'s formulas — all matched. That
leaves `residual_block_cabac` itself as the leading suspect: this
macroblock-layer measurement is the first time it has ever been driven by
real encoder output rather than hand-built fixtures or its own
round-trip test encoder. Not isolated further within this round's time
budget; exact per-corpus repros and everything ruled out are in
`tests/macroblock_layer_cabac.rs`'s `#[ignore]` strings, and the state is
also recorded in `planning/TECH-DEBT.md` for a clean handoff.

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

**Two more passes since**, both against `vaco-codec-cabac`'s public API
only (that crate is `agent:codec-bits`'s, status `done` — not edited).
`tests/cabac_bypass_egk_oracle.rs` added the round-trip correctness
assertion `fuzz/fuzz_targets/cabac_engine.rs` never had (that target only
checks `decode_bypass_egk`/`decode_uegk` don't panic, not what they
return) and cleared a specific hypothesis: coefficient signs and
`coeff_abs_level_minus1`'s `EGk` suffix are the only *bypass*-coded
elements this crate reads, and a fault confined to bypass would explain
every `mb_type` matching a reference decoder exactly while the slice
still came up short. Every realistic value round-trips cleanly, and
instrumenting the real call site against all three corpora found the
32-bin prefix ceiling engages zero times across 243 real calls (largest
observed value 418, six orders of magnitude below where the ceiling
matters). Then a sixth real bug was found and fixed:
`decode_cbp_cabac`'s luma `coded_block_pattern` neighbour derivation
computed one same-macroblock bit with the *left* neighbour's rule and
fed it to both the left and above `ctxIdxInc` terms — right for `q=0`,
wrong or entirely unsourced for `q=1`/`q=2`/`q=3` (re-derived by hand
from clause 6.4.7.2 and Table 6-2; see `mb.rs`'s own module doc for the
exact case-by-case account). Fixing it measurably changed two of the
three corpora's own slice-0 divergence point, confirmed by comparing
exact before/after trailing-bit patterns, but none reach a clean end yet
— the third corpus's mismatch is byte-for-byte unchanged, meaning its own
divergence sits elsewhere. `residual_block_cabac`'s scan-loop timing
against real per-coefficient state is the only surface in this function
left unexplored.

## Deblocking (clause 8.7): chroma and inter `bS` closed, one residual open

*(Added after `H264Decoder` was wired to real reconstruction — #419-#425,
`a81e2d2` — and dispatched to chase the CABAC desync `E2E-GAPS.md` §1b
reported at "2 of 25 frames, then `end_of_slice_flag` fired before every
macroblock was decoded". That error is real, but does **not** reproduce
with a single reference frame: `tests/macroblock_layer_cabac.rs`'s own
`cabac_ip_simple.264` fixture (`-refs 1`), decoded through the current
`H264Decoder` via the real CLI, runs all 25 frames without error. It does
reproduce with multiple references — `cabac_ip_multiref.264` (`-refs 4`)
genuinely stops short on 7 of 50 slices, confirmed against a real,
JM-verified-conformant corpus; see
`every_slice_in_a_real_multiref_cabac_stream_visits_every_macroblock`
(new) in `tests/macroblock_layer_cabac.rs` for the exact slices and the
smallest repro found so far (slice 4, 35 of 36 macroblocks). Root cause
still not isolated — `ref_idx_lX`'s own CABAC binarisation was checked
directly against clause 9.3.3.1.1.6 as the leading suspect and matches
exactly, ruling it out. What blocks a byte-close *single-reference* decode
today is `crate::deblock`, not the CABAC layer — see below.)*

`crate::deblock::deblock_picture_luma` handled only the all-intra case
(`bS = 4`/`3`, refusing any non-intra macroblock with `Error::Unsupported`)
and had no chroma counterpart at all — `decoder.rs` applied it to I slices
only and never touched `Cb`/`Cr`, leaving every P frame's luma undeblocked
and every frame's chroma undeblocked, silently (no error — `decoder.rs`'s
own doc, before this fix, explains why that was judged "demonstrably not
broken" rather than wrong). Measured directly against `ffmpeg`'s real
(deblocking-on) decode of `cabac_ip_simple.264`'s exact 25 frames: chroma
already differed on frame 0 (an I slice), concentrated at 4x4/macroblock
edges — the "structured, not small-and-unstructured" shape
`AGENT-CONSTRAINTS.md` calls a real defect, not rounding noise.

Fixed both gaps: `boundary_strength` now implements clause 8.7.2.1's Table
8-18 in full (collapsed to this decoder's single-reference-list, non-MBAFF,
frame-only scope — `intra` wins first, then a real per-4x4-block
coefficient-presence check for `bS = 2`, then a real `ref_idx`/motion-vector
comparison for `bS = 1`), and `deblock_picture_chroma` filters `Cb`/`Cr`
using `bS` derived at luma granularity and mapped down for 4:2:0 (only
luma edge positions 0 and 8 have a real chroma column/row; each of luma's
four per-4-row `bS` groups covers two chroma samples). Verified against a
locally built, instrumented JM 19.1 reference decoder
(`vcgit.hhi.fraunhofer.de/jvet/JM`, BSD, Tier A) rather than re-derived from
the specification a second time — its own `get_strength_ver`/`_hor`
(`loop_filter_normal.c`) is the primary source for the collapsed rule
above, and a locally patched build (forcing the decoder's own
last-macroblock `end_of_slice_flag` shortcut to actually read the bit) is
what settled the original CABAC-desync question: the real arithmetic
engine, at the exact state reached after genuinely decoding the picture's
last macroblock, returns the terminating bit at the same position both
`vaco-codec-h264` and JM compute — not a desync.

`DeblockCtx` caches the vertical and horizontal `bS` grids once per
macroblock. Luma, Cb and Cr all consume that same luma-derived result, so
the two chroma passes no longer repeat the residual and motion-state walk.
On a 125-frame 1920x1080 default-`libx264` fixture, 12 interleaved
single-thread rounds measured candidate/baseline medians of `0.96454x`
wall time and `0.96434x` CPU time; same-session candidate/`ffmpeg` medians
were `8.53254x` wall and `8.47069x` CPU. A 1 ms native sample profile moved
`boundary_strength` from 396/3016 self samples (13.13%) to 167/2878
(5.80%). Raw retired-instruction and cycle totals were unavailable: the
headless Xcode CPU Counters template exposes sampled bottleneck categories,
not hardware totals, and the Linux Cachegrind container was unavailable in
that session. Both default-B and all-P 125-frame fixtures remained byte
exact against `ffmpeg` at 1, 2, 4 and 8 threads, including the full
388,800,000-byte output count per run.

A second, real bug turned up while wiring the general `bS` case:
`MbSummary::residual.luma_ac` is indexed by `luma4x4BlkIdx` (clause 6.4.3's
z-scan order, matching `residual_luma()` and `crate::mb::blk_xy`), but
`MbSummary::mv_blocks` is genuinely raster-ordered (`row * 4 + col`, its own
doc says so verbatim) — two different conventions on two fields of the
same struct, and the first version of `boundary_strength` used the same
raster index for both. `deblock::raster_to_luma4x4_blk_idx` converts before
the residual lookup now. This alone took frame 0 (I slice) to byte-exact on
all three planes and cut the P-frame drift roughly two orders of magnitude
(max sample error 5-15 and growing every frame, down to 1-2 for the first
several frames).

**Closed since; this paragraph is kept as the record of what the wrong diagnosis looked like.** The residual described below was *not* a borderline rounding or activity-condition difference in `filter_luma_line`. Its two real causes were a `TC0_TABLE` that was wrong in 23 of 52 rows and a motion-vector-prediction bug that made `P_Skip` mispredict next to intra neighbours -- see "Byte-exact against ffmpeg" below. Note what the hand-trace recorded: every input was checked *except* the table itself, because the table had already been "corrected once against the oracle" and so read as settled.

**What remains open**: from frame 1 onward, a small residual (max absolute
sample error up to 8 by frame 24 of 25, concentrated at specific
macroblock-boundary edges) survives. Hand-traced one instance to a `bS = 2`
edge between a two-partition `P16x8` macroblock and a neighbour with real
residual — every input checked (both sides' QP, cross-checked against
`ffmpeg -debug qp`; the `bS` value itself; `ALPHA_TABLE`/`BETA_TABLE`/
`TC0_TABLE`; the pre-filter sample values, verified byte-exact via the
undeblocked reference) matched a correct decode, yet the filtered output
still differs by 1-2. The likely shared cause: `vaco_codec_dsp_deblock`'s
own `TC0_TABLE` already carries one documented, oracle-found transcription
fix (`indexA == 30`'s `bS == 3` column) and an acknowledged, deliberately
not-yet-chased ~0.2% residual in the same tC0-clipped branch, discovered
independently against the all-intra `cabac_i_only.264` fixture (which does
exercise `bS < 4` on intra content, unlike frame 0 above, which is all
`bS = 4`/`3` and never touches `tC0` at all). This dispatch's own `bS = 1`/
`bS = 2` edges reach exactly that branch. Plausible, not confirmed — no
second wrong table entry was found the way the first one was.
`tests/reconstruct.rs`'s `cabac_ip_simple_full_deblocking_matches_ffmpegs_real_decode`
(new, `#[ignore]`d with the full account) and
`cabac_ip_simple_frame_zero_full_deblocking_matches_ffmpeg` (new, passing —
locks in the frame-0 fix) carry the exact numbers.

## Byte-exact against ffmpeg: what closed the 13.70% drift

**

The corrected measurement, and the one every claim below is made against:
per plane, per frame, **byte for byte**, whole sequence, naming the first
differing frame and its magnitude. Two independent references agree on the
expected bytes -- `ffmpeg 9.0.1` and a locally built, instrumented **JM
19.1 `ldecod`**, which was byte-exact against ffmpeg on every clip used
here. `tests/decoder_output_matches_ffmpeg.rs` is that check, embedded, run
through the *registered* `H264Decoder`'s public
`set_extradata`/`send_packet`/`receive_frame` surface rather than through
`crate::reconstruct`'s `pub(crate)` internals.

Result on 25 frames of 320x240 `libx264` `testsrc2`, per configuration:

| Configuration | Before | After |
|---|---|---|
| Main, `-bf 0 -refs 1` | 17.62% of bytes differ, from frame 0 | **byte-exact, 25/25, all planes** |
| High, `-bf 0 -refs 1` (x264's default profile) | 6.51%, from frame 0 | 0.0008% (24 of 2 880 000 bytes), from frame 22 |
| Main, `-refs 3` | 2 frames then refused | 2 frames **byte-exact** then refused (multi-reference CABAC desync, still open) |
| Main, B frames | 2 frames then refused | 2 frames **byte-exact** then refused (CABAC B slices, still out of scope) |
| Baseline | refused | refused (CAVLC reconstruction, still unimplemented) |

Four distinct defects, found in this order because frame 0 was taken to
byte-exactness before any later frame was looked at:

1. **`vaco_codec_dsp_deblock::tables::TC0_TABLE` was wrong in 23 of its 52
   rows** -- off by one row for every `indexA >= 16`, plus further
   divergence for `indexA >= 41`. That module's own doc said, honestly,
   that its tables were transcribed from recollection; they now come from
   JM 19.1's `loop_filter.h` `CLIP_TAB`, checked entry by entry,
   mechanically. `ALPHA_TABLE`/`BETA_TABLE` were checked the same way and
   were already correct. This was the whole of frame 0's error (60 luma
   samples, max delta 3, all in the `bS = 3` tC0-clipped branch).
   It also **retracts an earlier "oracle-guided" single-entry correction**
   at `indexA == 30`: that row's correct value is `[1, 1, 2]` and the
   pre-existing `[1, 2, 3]` was wrong in its *first two* columns, so
   editing the third moved one fixture's whole-picture match percentage the
   right way for the wrong reason. Fitting a table entry to an aggregate
   difference percentage is not a check on that entry.
2. **An intra neighbour was treated as an absent one** in clause 8.4.1's
   motion vector prediction. `MvInfo::as_motion_neighbour` answered
   `available: false` for any neighbour carrying no motion for the list
   being predicted, collapsing clause 6.4's *macroblock* availability into
   clause 8.4.1.3.2's `mvLXN = (0, 0)`, `refIdxLXN = -1` substitution.
   Clause 8.4.1.1's `P_Skip` zero-motion test, clause 8.4.1.3.1's "`B` and
   `C` both unavailable" shortcut and the `C -> D` substitution all read the
   first and got the second. Every `P_Skip` macroblock with an intra left or
   above neighbour predicted `(0, 0)` instead of the median, and then fed
   that error forward as a reference picture. `MvInfo::mb_available` now
   carries the two answers separately. **This alone took the whole Main
   sequence from 17.29% to byte-exact**, and every derived motion vector
   now matches JM's own, 4x4 block by 4x4 block, across all 25 pictures.
3. **`Intra_8x8` was missing from `deblock::is_intra`** -- so clause
   8.7.2.1's intra branch, which is tested first, never fired for it and
   every `Intra_8x8` macroblock deblocked at `bS = 2` where the answer is
   `4` (macroblock edge) or `3` (internal).
4. **Two more 8x8-transform deblocking gaps**: `bS = 2` read
   `MbResidual::luma_ac`, which is all-`None` when
   `transform_size_8x8_flag` is set (the coefficients are in `luma8x8`), so
   it could never fire; and the internal luma edges at offsets 4 and 12
   were filtered, which clause 8.7 does not do when there are no 4x4
   transform block boundaries there.

### Round two: the fixture corpus was the bug, twice more

The table above was measured on `testsrc2`, `smptehdbars` and `mandelbrot`.
A wider corpus -- `life`, `zoneplate`, `cellauto`, `sierpinski` and friends
at eight sizes -- found two more defects that those fixtures could not
express, and both were **content-dependent, not size- or profile-dependent**:

5. **`Intra_8x8`'s `Vertical_Right` and `Horizontal_Down` dropped a term.**
   Clause 8.3.2.2.5/8.3.2.2.6's `zVR < -1` / `zHD < -1` branches step back
   along the opposite edge by `y - 2*x` and `x - 2*y`; both arms used `y`
   and `x` alone. At 4x4 that term barely matters; at 8x8 every position
   past the diagonal reused the first one's value. **High profile,
   640x360 `mandelbrot`: 7.66% of bytes differing from frame 0 (the IDR
   alone was 21474 luma samples wrong, max delta 48) -> byte-exact.**
   Found by dumping all nine modes' predictions from this decoder and from
   JM and diffing: 1592 blocks, **0 mode mismatches, 332 prediction
   mismatches** -- modes agreeing while predictions did not is what ruled
   out the CABAC/most-probable-mode side in one step. Per-mode failure
   rates were 124/168 (`Horizontal_Down`) and 33/60 (`Vertical_Right`)
   against 10/362 for DC. Flat synthetic content selects two or three of
   the nine modes; directional content selects all nine.

6. **Weighted prediction (clause 8.4.2.3) was not implemented at all.**
   `pred_weight_table()` was parsed by `vaco-parse-h264` and ignored by the
   decoder. **`weighted_pred_flag` is x264's own default for P slices**, so
   nearly every real file carries a weight table -- including every fixture
   this crate already decoded byte-exact. On most content the encoder picks
   the neutral weight and clause 8.4.2.3.2 collapses to a plain copy, which
   is exactly why ignoring it looked correct. ffmpeg's `life` source
   flickers globally, x264 then picks real weights
   (`logWD = 4, w = 15, o = -3` on the clip used here), and **every inter
   macroblock carrying luma residual was wrong from the first P picture**
   (17 of 17 on frame 2; the 57 wrong intra macroblocks were all downstream
   of an inter neighbour). 10.58% -> byte-exact.

   The localisation is the reusable part: motion vectors matched JM's own
   `mv_info` 4x4-by-4x4; per-4x4 coefficient presence matched JM's
   `s_cbp[0].blk` bitmask (1 macroblock differed in 9750, and that one is
   JM tracking `Intra_16x16` DC separately). With motion and coefficient
   placement both excluded, prediction samples were recomputed
   independently from clause 8.4.2.2.1 -- and disagreed with ffmpeg even
   for `frac == (0, 0)` blocks, where the prediction is a plain copy and
   *cannot* be wrong. A copy that disagrees is not a filter bug; it is a
   missing transform of the copied samples.

**Two JM instrumentation attempts produced misleading data and were thrown
away rather than believed**: dumping `currSlice->mb_pred` and dumping
`dec_picture->imgY` at the end of `mb_pred_p_inter8x8` both read buffers JM
had not filled at that point, and the second "proved" a prediction mismatch
that did not exist. What caught them was a control -- re-running the same
comparison on a clip already known byte-exact -- and hand-checking one
`mv = (0,0)` block against the reference frame. **An oracle needs its own
control run before its output is evidence.**

### Verification corpus

Main and High profile, `-bf 0 -refs 1`, 25 frames each, per plane per frame
byte for byte against ffmpeg 9.0.1 (itself confirmed byte-exact against a
locally built JM 19.1 `ldecod` on every clip used):

- Sources: `life`, `mandelbrot`, `zoneplate`, `cellauto`, `sierpinski`,
  `testsrc`, `testsrc2`, `smptehdbars`, `rgbtestsrc`, `gradients`.
- Sizes: 176x144, 352x288, 416x240, 640x360, 640x480, 720x576, 1280x720,
  and 322x242 (not a multiple of 16, so frame cropping is exercised).

**Every combination byte-exact.** When adding a fixture, prefer directional
and high-detail content: it is the only kind that selects all nine
`Intra_8x8`/`Intra_4x4` modes, and content with global brightness change is
the only kind that makes an encoder emit non-neutral prediction weights.

**The residual this section used to leave open is closed** by defect 6 below (it was weighted prediction all along, not `Intra_8x8` prediction as the paragraph below guessed). Kept as written, because the wrong guess is the useful part: it named the right macroblock and the right slice type and still misattributed the cause, because it reasoned from the coding mode of the macroblock rather than from what was actually different about its inputs. Original text: **the one open residual, stated precisely** rather than rounded to "clean":
High profile leaves 24 bytes differing across frames 22-24 (2, 7, 15 luma
samples; max delta 5; chroma byte-exact). It is **not** deblocking --
decoding both sides with the loop filter disabled (`ffmpeg
-skip_loop_filter all`) still differs, first at the same frame 22 -- and it
localises to one macroblock: frame 22, macroblock (15, 13), an `Intra_8x8`
macroblock inside a **P** slice, differing only along its top row
(`x = 12, 13`, `y = 0`), i.e. in prediction from above/above-right
neighbours that are themselves inter. **A second, independent repro has the
same signature** (352x288 `testsrc`, High, 0.0049%, first differing frame 12,
macroblock (4, 15), again `Intra_8x8` inside a P slice), so this is a
systematic gap in `Intra_8x8` prediction with inter neighbours rather than a
one-macroblock oddity.

Main `-bf 0 -refs 1` is byte-exact on five clips spanning five resolutions
and five content types: 64x64 `cabac_ip_simple` (the embedded fixture),
176x144 `smptehdbars`, 320x240 `testsrc2`, 352x288 `testsrc`, 640x480
`mandelbrot`.

**What `check_scope` and the decoder still refuse, unchanged**: CAVLC
reconstruction, CABAC B slices, MBAFF/field pictures,
`constrained_intra_pred_flag`, 4:2:2/4:4:4, `SI` slices, and a
multi-reference CABAC slice that desyncs (refused as `InvalidData` rather
than emitting a partial picture). Nothing was moved from "refused" to
"attempted" by this work.

**Method note, because it is the transferable part.** Every one of these
four fell to the same technique and none of them to reading specification
prose: build the reference decoder locally, confirm it agrees with ffmpeg
byte for byte on the exact clip, then dump the same intermediate state from
both and diff it -- macroblock types and motion vectors from JM's
`exit_picture`, boundary strength and per-edge `p`/`q` samples from this
crate's own filter loop. Defect 2 was found by diffing 4x4 motion vectors
against JM's; defect 3 by dumping `bS` on an IDR picture, where no value
below 3 is reachable, and seeing a 2.

## How to change it

`deblock.rs` owns both the clause 8.7.2.1 `boundary_strength` oracle and
`BoundaryStrengthGrid`, its per-picture cache. Any change to edge indexing
must keep the direct-vs-cached mapping test and full decoder output oracle;
4:2:0 chroma uses luma edge indices 0 and 2, with each strength covering two
chroma samples. `frame_task.rs` constructs one `DeblockCtx` and uses it for
all three planes, which is what makes cross-plane caching useful.

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

## Round: B slices, multi-reference and `b-pyramid` — a fully stock `libx264` file

**Result, stated the way it should be: 160 of 160 clips byte-exact at
x264's own defaults, no encoder flags at all** — B frames, `b-pyramid`,
three reference frames, weighted P — 25 frames per clip, per plane per
frame byte for byte against plain `ffmpeg`, over ten `lavfi` sources
(`life`, `mandelbrot`, `zoneplate`, `cellauto`, `sierpinski`, `testsrc`,
`testsrc2`, `smptehdbars`, `rgbtestsrc`, `gradients`) x eight sizes
(176x144 through 1280x720, including 322x242 for cropping) x Main and
High. The same corpus is 160/160 at `-bf 0 -refs 1` (unchanged) and
160/160 at `-bf 3 -refs 1`. 854x480 `yuvtestsrc`, 1920x1080 `mandelbrot`
and 178x146 `testsrc2` are byte-exact in both profiles at both `-bf 0
-refs 1` and full defaults. It was effectively 0 before this round: the
first B slice was refused.

Four defects, one of them the gated B-frame residual, three of them the
long-standing multi-reference CABAC desync and its `b-pyramid` companion.

### 1. `bS` read a reference index for a list the block never used

`MvInfo::ref_idx_l0`/`ref_idx_l1` returned the raw stored array entry. That
entry is `0` — a valid reference index — for a list the partition does not
predict from, not clause 8.4.1.3.2's `refIdxLX = -1`. So
`deblock::boundary_strength` resolved a uni-predicted B partition's unused
list to `RefPicList1[0]` and treated the block as bi-predicted, answering
clause 8.7.2.1's "do the two sides use the same set of reference pictures"
against a picture the block never referenced. A handful of edges came out
`bS = 1` where the answer is `0`, or the reverse.

A P slice hid it completely: `RefPicList1` is empty there, so the raw index
never resolves to anything. That is exactly the reported signature — every
I and P frame byte-exact, every B frame off by max 3-5 over 1-2% of
samples.

**How it was localised, in one step:** re-encode the same source with
`--no-deblock` and decode again. Every frame byte-exact. That single
experiment excluded implicit weighted bi-prediction, spatial direct motion
derivation, dequantisation, the inverse transform and chroma-DC
distribution *together*, and pointed at clause 8.7 — rather than narrowing
one candidate at a time. The prior round's leading hypothesis (a chroma-DC
residual-level divergence) was not the cause; nor was implicit weighting,
which had already been ruled out.

### 2. A B slice's `mb_type` intra suffix read its first bin twice

Table 9-27's B rows make `1 1 1 1 0 1` the entire "Intra, prefix only"
prefix, and Table 9-11 gives the suffix `ctxIdxOffset == 32` — **the same
ctxIdx the prefix's last bin uses**. `cabac_mb_tables::MB_TYPE_B`'s own doc
already said so ("index 5 here (ctxIdx 32) is the shared context between
the prefix's last bin and the suffix's first") and the table already
carried ctxIdx 33..=35 in rows 6..=8, which nothing had ever read.
`decode_mb_type_b_prefix`'s closing `act_sym += bit(5)` *is* the suffix's
`binIdx == 0` (JM's `readMB_typeInfo_CABAC_b_slice` reads it in the same
place), but the `NeedsIntraSuffix` arm then called the whole of
`decode_mb_type_intra_suffix`, which starts by reading that bin again — at
ctxIdx 17, P/SP's suffix offset. One extra bin, and the rest of the slice
is out of step.

Only an intra macroblock inside a B slice reaches it, which is why plenty
of B content was byte-exact without ever firing it. `-bf 3` was enough to
make it common: 58 of 160 clips passed before the fix, 160 of 160 after.

### 3. `ref_idx_lX`'s `ctxIdxInc` missed two of clause 9.3.3.1.1.6's own zero-conditions

Neither is reachable at `num_ref_idx_lX_active_minus1 == 0`, where
`ref_idx_lX` is not in the bitstream at all. Together they *are* the
multi-reference CABAC desync `tests/macroblock_layer_cabac.rs` had carried
as an open defect for several rounds (7 of 50 slices, then 5 of 50, now
0 of 50 — that test is un-ignored).

- **Partition 0 was not published before partition 1 read it.** For a
  16x8/8x16 macroblock, clause 6.4.11.7 makes partition 1's neighbour
  partition 0 of the *same* macroblock, but
  `decode_two_partitions_cabac`'s `ref_idx` pass wrote nothing to the
  motion grid until both values were read, so the lookup saw a
  never-written `MvInfo::default()`. The `mvd` pass in the same function
  and the sub-macroblock path both already had the immediate-write rule,
  with a comment explaining exactly why; the `ref_idx` pass did not.
- **A skipped or direct neighbour must contribute `condTermFlagN = 0`** —
  skip by name, direct through `predModeEqualFlag`, since
  `MbPartPredMode(B_Direct_16x16, 0)` and `SubMbPredMode(B_Direct_8x8)` are
  `Direct`, which is neither `Pred_LX` nor `BiPred`. A direct block's
  derived motion is deliberately stored as an ordinary `L0`/`L1`/`Bi`
  prediction (clause 8.4.1.3's neighbour derivation for a later
  macroblock's MV prediction reads it that way), so `pred` cannot express
  this. `MvInfo::direct_or_skip` is the bit that can.

### 4. Adaptive reference-picture marking (clause 8.2.5.4) was not implemented

`decoder.rs` always ran clause 8.2.5.3's sliding window. x264 emits
`adaptive_ref_pic_marking_mode_flag == 1` for every `b-pyramid` stream —
its default — to unmark the pyramid's own reference B picture with
`memory_management_control_operation == 1`. That is never the picture the
sliding window would evict: the window always takes the smallest
`FrameNumWrap`, and the B reference is *newer* than the anchor it sits
between. So the decoder evicted the wrong picture, `RefPicList0` shifted,
and every later inter macroblock predicted from the wrong reference.

This one has no CABAC signature at all — nothing in the parse depends on
the DPB — so it shows up as large, structured, accumulating pixel error
with a perfectly clean bit trace. It was found by dumping each slice's
resolved `RefPicList0`/`RefPicList1` as POCs from both this decoder and JM
and diffing: the two agreed for nine pictures and then JM's list 0 held a
POC this decoder's DPB no longer had.

MMCO 1 and 5 are implemented. 2/3/4/6 are long-term reference management,
which this decoder has no `RefPicList` section for (clause 8.2.4.3's
`idc == 2` reordering is refused for the same reason), and are refused as
`Unsupported` rather than silently dropped.

### Method

Every one of the three CABAC defects was localised the same way, and the
split that did the work is worth naming: **dump the same thing from both
decoders at descending granularity, and stop at the first level that
disagrees.**

1. Per-macroblock syntax elements (`mb_skip_flag`, `mb_type`,
   `sub_mb_type`, `coded_block_pattern`, `mb_qp_delta`) — this said
   "everything matches through macroblock 84, `mb_type` at 85 does not",
   which is a macroblock, not a slice.
2. Every context-coded CABAC bin as `(pStateIdx, valMPS, bit)` — this said
   "bin 3933 of slice 5; the four bins before it agree exactly; at 3933 the
   *state* differs". A differing state with matching history means a
   **different context was selected**, which excludes the arithmetic engine
   and the binarisation tree and points straight at a `ctxIdxInc`
   derivation. All three defects had that same fingerprint.

The oracle was controlled first, per the lesson §13 already records: JM was
run on a clip already known byte-exact against `ffmpeg` and matched it
25/25 before any of its traces were believed.

For defect 1 the split was cheaper still — one re-encode with
`--no-deblock` — and for defect 4 no bin trace helps at all, because the
bitstream parse is not what is wrong. Reaching for the finest-grained tool
first would have cost more and shown less in both cases.

### And one more harness that measured the wrong thing

`reconstruct.rs`'s `cabac_ip_simple_full_deblocking_matches_ffmpegs_real_decode`
had been `#[ignore]`d for rounds over "a small residual from frame 1
onward, max |delta| <= 8, concentrated at specific macroblock-boundary
edges", with a recorded hypothesis pointing at `filter_luma_line`'s
borderline arithmetic or a second wrong `TC0_TABLE` entry. Neither was
real. The harness passed `&[]` as `deblock_picture_luma`/`_chroma`'s
`ref_list0_poc`, and with an empty list `deblock::ref_poc` answers `None`
for every `ref_idx` — so clause 8.7.2.1's "the two sides use a different
set of reference pictures" is unsatisfiable and every such edge comes out
`bS = 0` where the answer is 1. The real decoder has always passed real
POCs, and decodes that same fixture byte-exact through the public
`H264Decoder`. Giving the harness one distinct identity per
`RefPicList0` position makes the test pass byte-exact on all 25 frames;
it is un-ignored.

The cheap check that would have caught it years earlier: **run the same
fixture through the public API before believing an internal-API test's
failure.** Two paths, one fixture, different answers means the harness is
a suspect, not just the code under it.

### What `check_scope` and the decoder still refuse

CAVLC reconstruction (`decode_slice_cavlc` verifies bit consumption only;
`decoder.rs` never reconstructs from it, so Baseline-profile files are
refused), **temporal direct** (`direct_spatial_mv_pred_flag == 0` — a
materially different derivation, and not x264's default), long-term
reference pictures (MMCO 2/3/4/6 and `ref_pic_list_modification`'s
`idc == 2`), MBAFF and field pictures, `constrained_intra_pred_flag`'s
neighbour substitution, 4:2:2/4:4:4, `SI` slices, more than one slice per
picture, and a CABAC slice whose `end_of_slice_flag` fires early (refused
as `InvalidData` rather than emitting a partial picture — no longer
reproducible on any clip in the corpus above, but kept).

## Round: CAVLC reconstruction — baseline profile, byte-exact

**CAVLC now reconstructs real pixels.** `mb::decode_slice_cavlc` used to
verify bit-exact *consumption* only, discarding every decoded coefficient
and motion vector; `H264Decoder::send_packet` refused every
`entropy_coding_mode_flag == 0` slice outright. Both are gone: CAVLC now
drives the exact same downstream pipeline CABAC does. This was possible
without touching `crate::reconstruct`, `crate::intra`, `crate::dequant`,
`crate::motion` or `crate::deblock` at all — every one of those modules
already took "prediction/reconstruction doesn't know which entropy coder
produced its input" as a design constraint, so the whole of this round is
new code inside `mb.rs` plus a two-way dispatch in `decoder.rs`, nothing
else.

### What changed, in one paragraph

`decode_slice_cavlc`/`decode_macroblock_cavlc` were rewritten to populate
`CabacGrids` — the same neighbour-state type `decode_slice_cabac` already
used for motion-vector prediction, `Intra_4x4` mode inference and
macroblock availability, despite the name: nothing about it is CABAC-
specific, only CABAC was the only caller before this round. Three new
functions (`decode_one_partition_cavlc`, `decode_two_partitions_cavlc`,
`decode_sub_mb_pred_cavlc`) mirror their CABAC counterparts' exact
neighbour-lookup/`predict_mv`/grid-publication logic, substituting plain
`te(v)`/`se(v)` reads for CABAC's context-coded ones — clause 8.4.1's
prediction maths has nothing to do with which entropy coder produced
`ref_idx`/`mvd`. `cavlc_residual_to_forward` converts
`cavlc::CavlcResidual`'s reverse-scan, run-length representation into the
forward-scan `positions`/`levels` shape `CabacResidual` already uses
(clause 7.3.5.3.2's own reconstruction algorithm, walking the decoded
levels from lowest frequency to highest and accumulating each one's own
zero run into a strictly increasing scan position), so `MbResidual` is
identical in shape regardless of entropy mode and `crate::reconstruct`
needs no changes at all. `decoder.rs`'s dispatch is a plain `if
pps.entropy_coding_mode { CabacDecoder ... } else { decode_slice_cavlc on
the same BitReader ... }` feeding the same `stats.macroblock_count !=
total_mbs` sanity check and the same `H264FrameTask` construction either
way.

### The one real bug this found, and how it was isolated

**`mb::more_rbsp_data` silently dropped a picture's own last macroblock**
whenever the slice's last real syntax element ended off a byte boundary
with only a few bits of real data left before `rbsp_trailing_bits()`. It
read [`vaco_bitstream::BitReader::remaining_bytes`], whose own doc says
plainly: "If the reader is not byte-aligned the partial byte is skipped."
After a mid-byte read — nearly every syntax element here ends one — that
silently discarded up to seven bits of real, unconsumed data sitting in
the tail of the current byte. When a slice's final macroblock finished
mid-byte with nothing left but one closing `mb_skip_run` and the true
trailing pattern, "skip the partial byte" landed exactly on the buffer's
own logical end: `remaining_bytes()` returned empty, `more_rbsp_data`
reported "nothing left", and the picture's very last macroblock was never
read at all — `stats.macroblock_count` came up one short of the real
total on real `libx264 -profile:v baseline` content specifically. Fixed
with a bit-precise replacement (`BitReader::bits_left`/`peek`, no byte
rounding): more than 8 bits left is always real data (`rbsp_trailing_bits`
is at most one stop bit plus seven padding bits); 8 or fewer, and the
exact remaining bit pattern is checked against "a single stop bit at the
top, all zero below" directly.

This is the same shape of bug `planning/AGENT-CONSTRAINTS.md` already
names twice over — a small, synthetic fixture (this crate's own
`cavlc_ipb.264`/`cavlc_ip_simple.264`, 64x64 and single-slice respectively)
never happened to land a real syntax element's own end exactly against the
buffer's last byte, so the bit-exact-*consumption* tests already in this
repository could never have found it; only a real, larger `libx264
-profile:v baseline` picture (320x240, 300 macroblocks) did, and only
because its own last macroblock happened to be immediately followed by a
short closing skip run rather than landing on a full byte by chance.

**How it was isolated, since this is exactly the kind of divergence a
wrong oracle can misreport**: JM 19.1 (`jm-reference-software`,
`provenance/sources.toml`) was cloned and built from source (its own
`CMakeLists.txt` needed two portability fixes for this machine — dropping
`-msse4.1` on arm64 and disabling `-Werror` for a handful of pre-existing
`-Wunused-but-set-variable`/`-Wmisleading-indentation` warnings in
unrelated files; no decode logic touched), then rebuilt a second time with
its own `TRACE` macro enabled, which dumps every decoded syntax element
and value to `trace_dec.txt` — an oracle this crate did not have to
instrument by hand, unlike JM's own comment-based `TRACE_STRING` markers
used for CABAC work elsewhere in this file's history. The trace's own
per-picture buffering (a slice's macroblocks are decoded only after the
*next* slice's own header has already been parsed and traced) means the
`POC: N MB: 0` block immediately following a given slice's header actually
belongs to the *previous* slice — confirmed against the file's own POC
arithmetic (`PicOrderCnt == 2 * frame_num` for this `pic_order_cnt_type ==
2` corpus) before trusting a single value from it, the same
"control your oracle" discipline this project's own notes ask for. Once
correctly aligned, this crate's own `mb_type`/`mb_skip_run` sequence
matched JM's, value for value, for the picture's entire 300 macroblocks
except the one JM alone still had left to read — isolating the defect to
`more_rbsp_data` specifically, not to any classification, motion, or
residual logic, before a single line changed. A from-scratch, independent
Python Exp-Golomb decoder (not derived from this crate's own
`vaco-codec-golomb`, JM's `vlc.c`, or any other implementation) additionally
confirmed the very first macroblock's own `mb_type` value the hard way,
by hand-decoding the slice header and first `ue(v)` reads directly from
the raw NAL bytes — the "hand-check one case whose answer is knowable
without any oracle" step this project's own notes ask for before trusting
an instrumented trace.

### Verification

`ffmpeg -v error -i F -map 0:v:0 -f rawvideo -pix_fmt yuv420p - | shasum -a
256` against `vaco -threads N -i F -map 0:v:0 -c:v rawvideo -f rawvideo -`,
fresh `libx264` encodes, `lavfi` sources chosen for detail and motion
(`mandelbrot`, `life`, `zoneplate`, `testsrc2`, `smptehdbars`) at 176x144,
320x240, 322x242 (not a multiple of 16), 352x288, 416x240, 640x480,
1280x720:

- **`-profile:v baseline`** (always CAVLC, no B slices): every size above,
  byte-exact, `N` in `{1, 2, default}`. One 10-second/300-frame, 640x480
  clip included specifically to stress a longer decode, also byte-exact.
- **`-profile:v main -coder 0`**, `-bf 0`/`-bf 2`/`-bf 3`, up to 3
  references: byte-exact at every thread count.
- **`-profile:v high -coder 0 -bf 2 -refs 3 -x264opts no-8x8dct`**:
  byte-exact — CAVLC's own 8x8-transform refusal (`pps.transform_8x8_mode`,
  unrelated to this round) is a real, still-active `check_scope` gate,
  confirmed by the same profile *without* `no-8x8dct` failing with exactly
  that `Error::Unsupported` and nothing else.
- **The existing CABAC regression set is unchanged**: a fresh stock
  `libx264` (CABAC, B-pyramid, weighted P, High profile, x264's own
  defaults) at 1024x576 and 322x242, byte-exact at `-threads 1` and the
  CLI's own default thread count, same as before this round.
- `cargo +nightly fuzz run h264_decode`/`h264_decode_threaded`, 60s each:
  clean, no findings, no new corpus behaviour change in either target
  (this round added no new code either fuzz target's harness reaches
  differently — the entropy dispatch lives in `decoder.rs`, which both
  targets already drive).

### What `check_scope` still refuses, on the CAVLC path specifically

`transform_size_8x8_flag`/`Intra_8x8` (`pps.transform_8x8_mode` — CAVLC's
own tables were never checked against the 8x8 transform, the same reason
CABAC's own gate on this existed before its High-profile round), `I_PCM`
(byte-alignment padding not derivable from this module alone the way
CABAC's `decode_terminate` signals it unambiguously), MBAFF and field
pictures, `constrained_intra_pred_flag`'s neighbour substitution, 4:2:2/
4:4:4 chroma, `SI` slices, and temporal direct (`direct_spatial_mv_pred_flag
== 0`, matching CABAC's own identical refusal). None of these is new —
every one was already refused before this round; CAVLC reconstruction did
not lift any of them, it only stopped refusing the *combination* of
CAVLC with everything else already in scope.

## Neighbouring-partition availability (clauses 6.4.11.7 and 8.4.1.3)

Motion-vector prediction asks two questions about each of its `A`/`B`/`C`/`D`
neighbours, and clause 6.4.11.7 answers them **separately**. Collapsing them
has now produced three distinct defects in this crate, in three different
directions, so the shape is worth stating once:

| question | clause | where the answer lives |
|---|---|---|
| Is there a decoded macroblock there at all? | 6.4.8/6.4.9 | `MvInfo::mb_available` |
| Is it this macroblock, at a partition not yet decoded? | 6.4.11.7 | `mb.rs`'s `resolve_c`, positionally |
| Does that partition predict from *this* list? | 8.4.1.3.2 | `MvInfo::as_motion_neighbour` |

The third is *not* an availability question. Clause 8.4.1.3.2 gives an
intra neighbour, or one whose `predFlagLX` is 0, `mvLXN = (0, 0)` and
`refIdxLXN = -1` — and leaves it **available**, which is what clause
8.4.1.3.1's "if `B` and `C` are both not available and `A` is available,
`mvLXB = mvLXA`" shortcut and clause 8.4.1.1's `P_Skip` zero-motion test
then read. Answering it with `available: false` was defect 2 of the Main
round above.

### Why "not yet decoded" cannot be answered from the grid

Clause 7.3.5.2's `sub_mb_pred()` decodes **all four** quadrants'
`ref_idx_l0`, then all four `ref_idx_l1`, then all four `mvd_l0`, then all
four `mvd_l1`. Four passes over the same four partitions, so at every
instant during them the live grid holds real, already-decoded data for
partitions whose motion vector does not exist yet, *and* holds nothing for
partitions that are fully decoded but do not use the list the current pass
is for. Neither state is what clause 6.4.11.7 is asking about. Availability
is a property of the partition's **scan position**, so `resolve_c` decides
it from the scan position: `partition_scan_index` flattens clause 6.4.2.2's
two nested 2x2 "Z" scans to `mbPartIdx * 4 + subMbPartIdx`, and a `C` inside
the current macroblock with a larger index than the current partition's is
not yet decoded.

**Only `C` ever needs this.** `A`, `B` and `D` sit at `(x-1, y)`,
`(x, y-1)` and `(x-1, y-1)`, all of which have a strictly smaller scan index
for every partition rectangle the syntax can produce —
`mb.rs`'s `only_c_can_reach_a_not_yet_decoded_partition` enumerates all of
them and proves it rather than leaving it asserted. Exactly four partitions
have a not-yet-decoded `C`, and they are the last sub-partition of a
left-column quadrant whose above-right 4x4 falls into the right-column
quadrant that follows it: the `8x4` bottom and the `4x4` `subMbPartIdx == 3`
of `mbPartIdx` 0 and of `mbPartIdx` 2. `c_is_not_yet_decoded_for_exactly_four_partitions`
pins that set.

### The regression that made the distinction expensive

`f970c23` reached the "not yet decoded" case by moving `mb_available` out of
the `ref_idx` passes and into the `mvd` passes — set only once a partition's
real motion vector existed. That fixed `CANL3_SVA_B` and `CABA2_SVA_B`, and
**byte-exactly broke every stock `libx264` stream carrying B-frames**, from
QCIF to 4K, for one round.

The `mvd` passes run list 0 across all four quadrants before list 1, so
availability became *per list*. A `B_L1_8x8` quadrant was still
"unavailable" while a later quadrant's own list-0 prediction read it as
`A`, `B` or `D` — clause 8.4.1.3.1's `B`/`C`-unavailable shortcut then
firing, or not firing, against inputs the specification does not give it. A
P slice cannot express this, because every partition there predicts from
list 0 and the two passes coincide; so did every baseline-profile and
`-bf 0` fixture, all of which stayed byte-exact and made the regression look
like a CAVLC-versus-CABAC question when it was a B-slice question.

The two rules are orthogonal and both are needed, which the crate's two
regression tests are built to demonstrate rather than assert:

| | `cabac_b_8x8_mixed_list_quadrants` | `cabac_p_8x8_same_mb_c_neighbour` |
|---|---|---|
| `f970c23` (availability in the `mvd` passes, no positional rule) | **fails** | passes |
| `f970c23^` (availability in the `ref_idx` passes, no positional rule) | passes | **fails** |
| both rules (current) | passes | passes |

No single-rule variant passes both, so neither test can be satisfied by
reverting the other's fix. Measured with each half ablated in turn.

## Frame threading (`-threads N`), on by default

`H264Decoder` is split into a serial header/entropy stage and a parallel
reconstruct/deblock/crop stage, so several pictures decode at once. The full
design, the determinism argument, the memory accounting and the measured
scaling are in **`docs/codec/frame-threading.md`**; the crate-side facts are:

- `decoder::H264Decoder::split_packet` is the serial half — parse, CABAC, the
  DPB, reference lists, clause 8.2.5 marking, POC, and every output-ordering
  decision. `frame_task::H264FrameTask` is the parallel half.
- A DPB entry is now two halves. Its bookkeeping (`poc`, `frame_num`, the
  per-4x4 motion field) is final as soon as the slice is entropy-decoded, so
  reference-list construction and `ColocatedField` never wait for a pixel; its
  samples are a `PictureRef` the frame task publishes when it finishes.
- `ColocatedField` now holds `Arc<Vec<MvInfo>>` shared with the DPB entry that
  produced it rather than a clone. The grid is one entry per 4x4 luma block of
  a whole picture — over 32,000 at 4K — and it is immutable once decoded.
- **Output is bit-identical at every thread count**, verified against ffmpeg's
  own `rawvideo` on all five regression fixtures at 1, 2, 4 and 8 threads. The
  ordering decisions are all applied in decode order in `collect_one`, not at
  dispatch, which is what makes that true rather than lucky.
- `-threads 1` spawns nothing and runs each picture inline at dispatch — the
  same call sequence as before frame threading existed. The CLI's own default
  when `-threads` is unstated is `min(available_parallelism, 4)`
  (`cli::default_thread_count`), not 1 — see `docs/codec/frame-threading.md`'s
  "Should it be on by default" for the three conditions that gated the flip
  and the evidence each one closed. `H264Decoder::new` and the bare `Decoder`
  trait still default to one thread; only the CLI resolves an unstated
  `-threads` to more than that.
- **Publication is row-level above one thread.** `reconstruct` and `deblock` run
  interleaved a macroblock row at a time, the filter one row behind, so
  `frame_task` publishes a band of a reference picture as soon as every row it
  holds is final — and the next picture starts predicting from it there rather
  than waiting for the whole picture. That is what makes an all-P stream (both
  large fixtures here) parallel at all: 3.07x at four threads, against 1.26x
  when a reference was published whole. `reconstruct::row_reference_reach`
  derives what each macroblock row has to wait for from that row's own motion
  vectors. `docs/codec/frame-threading.md` has the boundary conditions and the
  numbers.

## Motion compensation: partition-level prediction (A1)

`planning/PERF-PROGRAMME.md` item A1, `planning/E2E-GAPS.md` §28.
`crate::reconstruct::sample_luma_block`/`crate::interp::luma_qpel_sample`
predicted clause 8.4.2.2.1's luma quarter-pel sample one 4x4 block (one
output pixel, for the `interp` half) at a time, even when several
adjacent 4x4 blocks shared identical motion — the common case: 89.2% of
the partition rectangles counted directly on `h264_4k.mp4` were a whole
16x16 macroblock. `crate::reconstruct::partition_rects` now decomposes a
macroblock's own 4x4 motion grid (`MbSummary::mv_blocks`) into the
maximal same-motion rectangles first (a real H.264 sub-partitioning is
already such a tiling, so a greedy grow-right-then-grow-down scan
recovers it exactly, and merging two partitions that coincidentally carry
identical motion is still correct — clause 8.4's prediction value depends
only on the resolved motion at a position, never on the bitstream's own
partition boundary), and `sample_luma_partition`/`luma_qpel_partition`
predict the whole merged region in one call: fetch the `(w+5)x(h+5)`
reference window once, build only the half-pel planes (`H`/`V`/`J`) the
partition's own fractional motion actually needs, combine per pixel.
`sample_luma_block`/`luma_qpel_sample` are unchanged and kept, unused by
this path, as the scalar oracle three differential test families check
bit-for-bit (`crate::interp::tests::partition_matches_the_per_pixel_oracle_at_every_fractional_position_and_shape`,
`crate::reconstruct::tests::sample_luma_partition_matches_sample_luma_block_for_every_shape_and_edge_case`,
`crate::reconstruct::tests::partition_rects_recovers_known_shapes`).

### Full-pel fast-path measurement gate

`luma_qpel_partition` currently gathers the full `(w + 5) x (h + 5)` halo
before it observes the `(frac_x, frac_y) == (0, 0)` full-pel case. A proposed
fast path may copy only the `w x h` output region, but only after a fresh quiet
profile attributes material cost to this callee. The semantic oracle remains
the existing per-pixel comparison; its full-pel case must continue to cover
interior and edge-clamped positions.

`scripts/perf-h264-fullpel.py` is the exact end-to-end gate for this candidate.
It is deliberately build-agnostic: give it separately built baseline and
candidate binaries plus the same ffmpeg-generated H.264 fixture. It streams
rawvideo into independent byte counters and SHA-256 digests rather than writing
frames to disk. It refuses a non-quiet lane, checks baseline/candidate/ffmpeg
identity at threads `1,2,4,8`, then runs 12 rotated baseline/candidate/ffmpeg
rounds at each count and records wall and child CPU seconds with paired ratios.

```sh
python3 scripts/perf-h264-fullpel.py \
  --fixture "$E2E_DIR/h264_4k.mp4" \
  --baseline /private/tmp/vaco-h264-baseline/dist/vaco \
  --candidate /private/tmp/vaco-h264-candidate/dist/vaco \
  --max-load 2 --out /private/tmp/h264-fullpel.json
```

The required profile is a separate pre-edit Samply capture of the baseline
binary. `perf-baseline-profile-run.sh` expects its symbolicator beside its
scratch output, so the reproducible command is:

```sh
PROFILE=/private/tmp/h264-fullpel-profile
mkdir -p "$PROFILE"
cp scripts/perf-baseline-symbolicate.py "$PROFILE/symbolicate.py"
dsymutil /private/tmp/vaco-h264-baseline/dist/vaco -o "$PROFILE/vaco.dSYM"
SCRATCH="$PROFILE" VACO_BIN=/private/tmp/vaco-h264-baseline/dist/vaco \
VACO_DSYM="$PROFILE/vaco.dSYM/Contents/Resources/DWARF/vaco" \
  scripts/perf-baseline-profile-run.sh baseline h264-fullpel -- \
  -threads 1 -i "$E2E_DIR/h264_4k.mp4" -map 0:v:0 -c:v rawvideo -f null -
```

Cachegrind evidence must be collected through `scripts/perf-icount.py` on its
supported Linux path: it must be deterministic, show the affected callee and
whole-process `Ir` lower, and never be treated as a hardware-cycle result.
Generate one specification per Vaco binary, run each with `--repeats 2 --top
30`, and compare the two JSON files' `h264_decode_sd_640x480` `vaco` records.
Real cycles are optional and only valid through the strict Linux
`scripts/perf-hwcycles.py` gate; a missing PMU is reported as unavailable, not
estimated from time.

Measured: ~6% faster at `-threads 1`, ~8% at `-threads 4` (median
CPU-seconds ratio, `h264_4k.mp4`, interleaved before/after) — a real,
reproducible win, but well under this item's own realistic ceiling
(1.40x) and its stop-condition bar (1.18x). Two real performance bugs
(not correctness bugs) were found and fixed while getting there, both
recorded in full in `planning/E2E-GAPS.md` §28 because neither was
visible from reading the code: computing every half-pel plane
unconditionally cost more than the fetch-count reduction it was meant to
win, and a `Copy` `Option<[[u8; 16]; 16]>`'s own `.map()` copied the
whole array per pixel. Chroma (`sample_chroma_2x2`) was left untouched in that
round: the item's own stop condition gates chroma work on the luma kernel
clearing the ratio bar, which it did not.

### Batched chroma prediction

`sample_chroma_2x2` predicts the four chroma samples covered by one luma 4x4
block. Those four bilinear filters overlap: together they read one 3x3 source
window. `interp::chroma_mc_2x2` therefore decomposes the motion vector and
derives the four weights once, fetches the nine source samples once, and emits
the 2x2 result as a group. The previous per-sample `chroma_mc_sample` remains
the scalar oracle; the exhaustive differential test covers every one of the 64
fractional motion positions, positive and negative integer displacements, and
asserts the nine-fetch window contract.

The helper has no configuration or new dependency. If the chroma block shape
or supported chroma format changes, change the fetched window and the grouped
output together, then extend the oracle comparison before changing the decoder
call site. The current 3x3-to-2x2 shape is specific to 4:2:0 chroma and clause
8.4.2.2.2's two-tap bilinear interpolation.

Measured on 2026-09-04 from clean sibling snapshots of commit
`45f969fc9b569d339718890f2d11e92fe3de0d04` (Git tree
`ef52995df0350304533d89c10fbf6985c45ac5e7`). The source manifests were
`b0baaf71d1e106e123ee1602f9ca139306162d69d934eda40e89491b2242b0b5`
for baseline and
`d4a5a293c0d604673748316efda43b88e4477292b89c31e4c6f4f1df5746e9a7`
for candidate; `diff -qr` reported only `interp.rs`, `reconstruct.rs`, and this
document. The measured three-file patch had SHA-256
`885602a9c3f6db70c1781f355b4727be53f5f0d3fc6d31ffbcde199535822132`;
the baseline/candidate binary hashes were respectively
`b6312cc478733bc518f00b4f1e5c178749e7577a3208c2e20b946f23ec95ac51`
and `dd0cf2fe47d64e31e50707417e3e60df79cd1721394eeba8bd9ed696a9838e38`.
The workload was the 75-frame 3840x2160 High-profile fixture with SHA-256
`eb9ace2e0eed0a65dfe96dff3eaf45eca82095db3c6e95aee2bc406fd3480dc8`.
Across 12 baseline/candidate/ffmpeg rounds, rotating every command through each
position four times, the paired candidate/baseline median was `0.99038` for
wall time and `0.99050` for child CPU time. Median wall/CPU seconds were
7.427833/7.291755 for baseline, 7.278514/7.162277 for candidate, and
0.795534/0.814037 for same-session ffmpeg 9.0.1. The candidate won 7/12 wall
pairs and 8/12 CPU pairs, so this is a modest, noisy roughly 1% result rather
than a large speedup. Paired median wall/CPU ratios against ffmpeg moved from
9.1987/8.8518 to 9.0980/8.7315.

The pre-edit 4 kHz Samply discovery profile contained 42,500 samples, with
40,338 Vaco leaves and 3,277/3,284 unique addresses resolved (99.8%). The
post-change profile contained 30,711 samples, with 28,965 Vaco leaves and
2,543/2,551 addresses resolved (99.7%). Outer `sample_chroma_2x2` moved from
11.07% to 10.37%; innermost `copied<u8>` fell from 15.56% to 3.35%, while
`predict_chroma_inter` stayed stable at 2.13% to 2.02%. The grouped helper was
inlined, so it has no separate symbol. No cycle result is claimed: macOS did
not expose a trustworthy process-total counter, and wall time was not
relabelled as cycles.

Correctness was checked by streaming rawvideo into an independent byte counter
and SHA-256. Baseline, candidate, and ffmpeg each emitted exactly 75 frames and
933,120,000 bytes with SHA-256
`b00b7d2206af9a8775ee569e2c06626fa325d160c0b2798386ecd2f3f87e7220`
at 1, 2, 4, and 8 threads (12/12 decodes byte-exact).

## Uncoded chroma residual fast path

Each chroma component stores one optional DC residual and four optional AC
residual blocks. When all five entries for that component are `None`, clause
8.5.3's residual is identically zero and motion or intra prediction is already
the final 8x8 output. `add_chroma_residual` detects that authoritative uncoded
state before inverse scan, dequantisation, IDCT, and per-sample addition.

The predicate is component-local: a coded Cb block cannot force Cr through the
transform path or let it skip its own coded data. To change the residual
representation, update `chroma_residual_is_zero` alongside `MbResidual` and
extend `zero_chroma_residual_is_component_local` for every newly stored block.
There is no option or environment-variable control and no new dependency; the
path relies only on `MbResidual`'s existing `None`-means-uncoded contract.

Measured on 2026-09-04 from clean sibling snapshots of commit
`230ead8b03a998848e1c8fa5a4e07a7fa6fb5ad8` (Git tree
`386e0fcbe633bd668f7ae5a51a5b3c15c5d75c11`). The archived source had SHA-256
`993b523ae8551432bebae6bf46c04d096b558e7ef283ac1c44cfa06e246775c5`;
content manifests were
`e504c742f75b4247e8c3045c85590ee578978b64adad848f5a512ae0526e5d5d`
for baseline and
`13ba07c6cf6b9732c8ad5deacfb8b842a14959b3761b2fd1e55b4d1e0917d867`
for candidate. `diff -qr` reported only `reconstruct.rs` and this document,
and the two-file patch had SHA-256
`88034b66e61c14994c41fa730d50aeddfb0cee858911484a603c08f724636e95`.
The baseline/candidate binary hashes were respectively
`4caaa7fc78e2cb1fd20423e9c4d71d21fa4b48ac31f20c84245adf03e8e30236`
and `3f3d3738790e8a34e284206c4bc2227e53f6095f4729cbd2dc338cb85a2fe0a0`.

The workload was the 75-frame 3840x2160 High-profile fixture with SHA-256
`eb9ace2e0eed0a65dfe96dff3eaf45eca82095db3c6e95aee2bc406fd3480dc8`,
generated by ffmpeg 9.0.1 from `testsrc2` at 25 fps with libx264 defaults.
Across 12 baseline/candidate/ffmpeg rounds, rotating every command through each
position four times, the paired candidate/baseline median was `0.97733` for
wall time and `0.97627` for child CPU time, with 10/12 candidate wins for both.
Median wall/CPU seconds were 7.331088/7.250532 for baseline,
7.223975/7.127280 for candidate, and 0.807893/0.831161 for same-session ffmpeg.
Paired median wall/CPU ratios against ffmpeg moved from 9.0936/8.7710 to
8.9593/8.6110.

The pre-edit 4 kHz Samply profile contained 31,931 samples, with 30,269 Vaco
leaves and 2,641/2,648 unique addresses resolved (99.7%). Its outer
`reconstruct_mb` share was 28.81%; the innermost residual `Option::as_ref`
lookup was 1.25%. The post-change profile contained 45,924 samples, with
43,509 Vaco leaves and 3,322/3,330 addresses resolved (99.8%); the corresponding
shares were 30.15% and 0.85%. The candidate profile took 16.5 seconds despite a
12-second capture request, so these shares confirm code-path shape only and are
not a timing comparison; the interleaved A/B above is the speed result.

Linux instruction evidence was collected by [Actions run
33914744533](https://github.com/matteopolak/vaco/actions/runs/33914744533)
from temporary candidate `a86586f6cb9d36fcdf0439e93a05b0baafeeaef6`,
parented to `841945e4eb23e465b498298d238522f39ebd9a2f`; that evidence-only
commit changed exactly the two H.264 paths above plus the temporary workflow,
which was excluded from the landed change and whose remote ref was deleted.
The x86-64 dist binary hashes were
`5faf014c51a45c1f6f89d40d5f69df394241593dca49e6c13e00368785fe6a41`
for baseline and
`2d7e64b099f2392069b5c15a73c7aab8b0b794580bfba4b38ae886cfc37da25e`
for candidate. The 50-frame 640x480 fixture, generated by ffmpeg 6.1.1, had
SHA-256 `f04e026fb9bc94e575c41c5dec1885c4fbcf0e4039e3d203f7ba8b810374ee1a`.

Cachegrind 3.22.0 measured baseline instruction runs of 5,156,144,839 and
5,156,144,880 and candidate runs of 5,033,869,478 and 5,033,869,478:
candidate/baseline `0.976286`, or 2.371% fewer instructions. Relative spread
was `7.95e-9` for baseline and zero for candidate, under the recorded `1e-7`
Vaco repeatability gate. Same-session ffmpeg runs were 422,481,352 and
422,491,031 instructions (`2.29e-5` spread, under its `1e-3` gate); raw
baseline/candidate ratios against ffmpeg were 12.2044/11.9150. The full
instruction artifact had SHA-256
`32f91fac799d5beb40662d98a63ec9dc95ad7a1a2c02e126a0e544405eaccd85`.
The same Linux binaries and fixture emitted 23,040,000 rawvideo bytes with
SHA-256 `8f048f8c4894110c7d9164600107ef239d88bb289d8d100bbeafb40caaadafca`
for baseline, candidate, and ffmpeg.

The workflow also attempted 12 rotating rounds with Linux `perf stat` after
setting `perf_event_paranoid=-1`, but the hosted Azure VM reported both
`cycles` and `instructions` unavailable for every command. The strict harness
discarded all such samples; no Linux cycle total, hardware-instruction total,
or fallback cycle estimate is reported. Cachegrind `Ir` above is a simulated
instruction count, not a hardware cycle count.

Correctness was checked by streaming rawvideo into an independent byte counter
and SHA-256. Baseline, candidate, and ffmpeg each emitted exactly 75 frames and
933,120,000 bytes with SHA-256
`b00b7d2206af9a8775ee569e2c06626fa325d160c0b2798386ecd2f3f87e7220`
at 1, 2, 4, and 8 threads (12/12 decodes byte-exact). No macOS cycle result is
claimed: this host exposes no trustworthy process-total PMU counter, and wall
or CPU time was not relabelled as cycles.

## Uncoded inter-luma residual fast path

An inter-predicted luma block whose residual entry is `None` is already final
after motion compensation: `MbResidual` defines that state as an implicitly
all-zero transform block. `reconstruct_inter_mb` therefore copies the predicted
4x4 or 8x8 samples directly instead of inverse-scanning a zero block,
dequantising it, running IDCT, adding zero, and clipping every sample. Coded
blocks still take the previous arithmetic path unchanged.

The branch lives in `add_inter_luma_residual_4x4` and
`add_inter_luma_residual_8x8` in `reconstruct.rs`. If residual storage changes,
keep their fast-path predicate tied to the representation's authoritative
"uncoded" state; do not infer it from coefficient values after scanning. The
two unit tests pin that an uncoded block returns the selected prediction region
exactly, while the end-to-end oracle below covers both coded and uncoded blocks.

Measured on 2026-09-04 with a fresh 75-frame 3840x2160 High-profile
`testsrc2`/libx264 fixture (SHA-256
`eb9ace2e0eed0a65dfe96dff3eaf45eca82095db3c6e95aee2bc406fd3480dc8`),
12 baseline/candidate/ffmpeg rounds rotated each command through every run
position four times. Load average stayed 2.40-3.88. Median wall time moved from
7.007173 s to 6.782464 s (candidate/baseline `0.96793`); median child CPU time
moved from 6.980129 s to 6.760583 s (`0.96855`). The candidate won all 12 wall
and all 12 CPU pairs. Relative to same-session ffmpeg 9.0.1 `-threads 1`, the
wall ratio moved from 9.528x to 9.223x and the CPU ratio from 9.497x to 9.198x.

The corresponding 4 kHz Samply profiles resolved 2470/2475 baseline addresses
(99.8%) and 2405/2412 candidate addresses (99.7%). `idct4x4` self time fell
from 1.36% to 0.08%, innermost `reconstruct_inter_mb` from 4.31% to 3.51%, and
innermost `clamp` from 3.43% to 2.47%; outer `reconstruct_mb` moved from 30.16%
to 29.43%. No cycle result is claimed: macOS did not expose a suitable
process-total counter, and wall time was not relabelled as cycles.

Correctness was checked by streaming rawvideo directly into SHA-256. The saved
baseline, candidate, and ffmpeg each emitted exactly 933,120,000 bytes with
SHA-256 `b00b7d2206af9a8775ee569e2c06626fa325d160c0b2798386ecd2f3f87e7220`
at 1, 2, 4, and 8 threads (12/12 decodes byte-exact). This fast path has no
option or environment-variable control and introduces no new dependency; it
uses the existing `MbResidual`, dequantisation, and IDCT interfaces.

## Dispatched Intra16x16 DC prediction

The production Intra16x16 luma DC arm now uses
`vaco_codec_dsp_intrapred::simd::dc_predict`. `PictureReconstructor` detects
`vaco_simd::Caps` once when it is constructed and carries that token through
macroblock reconstruction; it does not repeat feature detection in the hot
loop. Vertical, horizontal, and plane prediction are unchanged. The original
scalar `predict_intra16x16` remains the independent oracle, including its
invalid-mode-to-DC fallback. The differential regression checks DC and that
fallback with both, either, and neither neighbour edge available.

If the neighbour representation or supported bit depth changes, update the
single `predict_intra16x16_dc` adapter and extend the scalar-oracle matrix before
changing the dispatched call. Do not add a second H.264-specific sum kernel:
the DSP intrapred crate is the source of truth for dispatch and arithmetic.
There is no configuration knob and no new dependency.

The pre-edit 4 kHz Samply profile used a 375-frame 3840x2160 gray all-intra
libx264 fixture (SHA-256
`34a02df57866d827de473c6caa7959beed8fe3590bb0e48d458efe4283f6c851`).
It recorded 46,207 samples. `reconstruct_mb` held 12,106 self samples (26.20%)
and 16,172 inclusive samples (35.00%); RVA-to-line attribution placed 5,715
self samples (12.37% of the whole profile) at the Intra16x16 reconstruction
call where the scalar DC helper was inlined. This confirmed the callee path
before the source change.

The local A/B used non-git sibling snapshots of commit
`5f61f13caba67334f21c1154a17421c670c31184` (tree
`d23724b725a26444b69b777d35e06494459f4ab0`) and the measured two-source-file
patch SHA-256
`2661921e4ce64fed786407a1d43723e02c1cc3842e7bfad8c6717a57ef3d720a`.
Baseline/candidate Apple-silicon binary hashes were respectively
`07ccee0d8ee1e010aa516eb701ae8ff7d975262ff9383fda29d93d57e5b2c5da`
and `e4e320e29788e97c2c6f573812490437142b5a19f302455fdd69c3e4107b48b0`.
Across 12 rotating baseline/candidate/ffmpeg rounds, mean child CPU seconds
were 9.7842/9.6467/0.6692 and mean wall seconds were 10.0808/9.8375/0.6850.
The paired medians were noisier and did not show a time win:
candidate/baseline was `1.00752` for CPU and `1.00409` for wall, with 5/12 CPU
and 4/12 wall wins. Paired median CPU ratios against same-session ffmpeg 9.0.1
moved from `14.9839` to `15.1573`; wall ratios moved from `15.2893` to
`15.2670`. These load-sensitive times are context, not the acceptance metric.

Deterministic instruction evidence came from [Actions run
33928955216](https://github.com/matteopolak/vaco/actions/runs/33928955216),
using temporary read-only evidence commit
`caf292093bdd97510adf6e0a5a1f80c3e3bdfe67`, parented to the same baseline;
the workflow and evidence-only lockfile were excluded from the landed change
and the temporary ref was deleted. On the 100-frame 640x480 all-intra fixture
(SHA-256
`a25281945ef023df407aaa6db7ed2e3e20d623fff8361c20cd47b7b75a08cd56`),
Cachegrind 3.22.0 reported `reconstruct_mb` instruction counts of
645,173,700/645,173,700/645,173,700 for baseline and
642,645,800/642,645,800/642,645,800 for candidate: zero spread and ratio
`0.99608183`, or 0.391817% fewer instructions in the measured callee.
Whole-process medians were 2,147,994,470 and 2,145,670,233 instructions
(`0.99891795`, 0.108205% fewer). Same-session ffmpeg's median was 269,728,475,
so the whole-process Vaco/ffmpeg ratio moved from `7.96354` to `7.95493`.
Vaco whole-process spreads were at most `3.82e-8`; ffmpeg's dynamic-loader
startup made its spread 0.5171%, which is reported rather than hidden.
The x86-64 baseline/candidate binary hashes were
`cb561e441ee71cdd2e6e29df6b09a28b6ad61567bbfe63a9499ff57081a85632`
and `8534989dafb8003643a524ac6330603f231b43ec23465eef40fb57aff4b9ede6`.
No hardware cycle result is claimed; elapsed or CPU time was not relabelled as
cycles.

Local baseline, candidate, and ffmpeg each emitted exactly 75 frames and
933,120,000 rawvideo bytes with SHA-256
`12f0bf451c2ac0d1cce1e94c15bcadfeec19989a28523b086d1a7e53e148a0ef`
at 1, 2, 4, and 8 threads (12/12 exact). The Linux evidence fixture likewise
emitted exactly 46,080,000 bytes with SHA-256
`c8672756c7b0543cc88c0f1647b5457e23860a024e093dd6d07cb03589136606`
for baseline, candidate, and ffmpeg.

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
