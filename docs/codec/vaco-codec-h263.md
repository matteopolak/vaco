# `vaco-codec-h263`

## What it is

ITU-T H.261 (03/93 free base text) and baseline ITU-T H.263 (03/96 free
base text, no annexes) video decode. Both formats' decoders live in one
crate because they share most of their shape — picture/GOB/macroblock/
block layers, the same zigzag scan, the same 8x8 IDCT — while differing
enough in bitstream framing and motion compensation that keeping them as
two independent `Decoder` implementations (`H261Decoder`, `H263Decoder`)
inside shared modules was clearer than either merging them or splitting
the crate.

Scope: H.261's full mandatory syntax (both CIF and QCIF, the optional loop
filter, integer-pel motion compensation). H.263's mandatory baseline
syntax, plus three H.263+ annexes: **D** (Unrestricted Motion Vector,
both the original `PTYPE`-bit and the `PLUSPTYPE`/Table D.3 encodings),
**K** (Slice Structured mode, replacing the GOB layer), and **T**
(Modified Quantization). Annexes E (Syntax-based Arithmetic Coding), F
(Advanced Prediction), G (PB-frames), I (Advanced INTRA Coding), J
(Deblocking Filter) and P (Reference Picture Resampling) are not
implemented — see "H.263+ annexes: what landed and what didn't" below for
why each one was or wasn't worth the cost. A picture using any
unimplemented mode is decoded as a flat mid-grey `CORRUPT` frame rather
than silently producing wrong pixels. Encode is out of scope entirely for
both formats.

## How it works

### Bitstream framing: two different alignment guarantees

H.261's `PSC`/`GBSC` start codes carry **no** byte-alignment guarantee —
the spec has no stuffing mechanism to force one — so `h261.rs` scans for
its shared 16-bit prefix (`0000 0000 0000 0001`) bit by bit
(`h261::find_prefix`). H.263's `PSTUF`/`GSTUF` stuffing rules guarantee
every start code begins on a byte boundary, so `h263.rs` scans byte by
byte for `00 00 1xxxxxxx` (`h263::find_prefix`) — `vaco_bitstream::annexb`
was built for H.264's fixed `00 00 01` pattern and doesn't fit either
case, so both scanners are hand-written for this crate.

### Picture/GOB/macroblock addressing: two different models

H.261 addresses macroblocks with a differential VLC (`MBA`) inside each
GOB (1..33, with an escape for gaps over 33) — a macroblock the encoder
doesn't mention is *skipped*, and every GOB (including a GOB with no
macroblock data at all) gets its own explicit header. H.263 has no
per-macroblock addressing at all: every macroblock in a picture is either
explicitly coded or (in a P-picture) marked "not coded" by a single `COD`
bit, visited in a fixed raster order. §5.2 of the H.263 base text also
allows every GOB header but the first to be entirely absent, which
ffmpeg's own `h263` encoder does for a whole QCIF picture — so
`h263::decode_gob` decodes macroblocks in raster order across row
boundaries within *one* call, checking for a genuine start code
(`h263::at_start_code`) before every macroblock rather than stopping after
one row and asking the outer scan to find a header that was never sent.

### Block decode: entropy, inverse scan, dequantisation, IDCT

Both formats share the same three-stage shape `vaco-codec-mpeg12` uses —
inverse zigzag (`block::inverse_scan`, using the same scan pattern H.262's
own default zigzag uses), dequantisation (`block::dequantise`), inverse
DCT (`block::inverse_transform`, delegating to
`vaco_codec_dsp_idct::mpeg2::Idct8x8` — that module is a generic 8x8 DCT-
III implementation despite its name, reused here rather than duplicated).
Unlike H.262, neither format has a weighting matrix or a separate
mismatch-control step: [`block::dequant_ac`] is one function shared by
both, because H.261 §3.2.5's two sign-conditional cases and H.263
§6.2.1's absolute-value-plus-sign form turn out to be the identical
formula once written out (substituting `level = sign * |level|` into
H.263's form reproduces H.261's two cases exactly).

Coefficient *decoding* itself is genuinely different between the two:
H.261's `TCOEFF` (`block::decode_h261_coefficients`) has a separate
End-of-Block code and the same "first coefficient of a non-intra block"
ambiguity H.262's own Table 7-3 has (resolved the same way: by table
position, not by the bits alone). H.263's `TCOEF`
(`block::decode_h263_coefficients`) embeds a `LAST` bit in every code (or
the 7-bit escape word) instead, so there is no separate terminator and no
first-coefficient special case — but its escape marker isn't itself a
`(run, level)` row, so it needed its own small decode loop
(`block::decode_h263_event`) rather than reusing the generic
[`vlc::decode`] matcher directly.

### Motion compensation: one seam holds, one is new, one doesn't apply

Half-pel bilinear interpolation (`motion::sample_half_pel`,
`avg2`/`avg4`) is the *same* formula H.262 and H.263 both specify
(`a=A; b=(A+B+1)/2; c=(A+C+1)/2; d=(A+B+C+D+2)/4`) — H.263 uses it
directly; H.261 doesn't (integer-pel only, chroma vectors derived by
halving-then-truncating rather than by half-pel interpolation —
`motion::h261_chroma_mv`). Vector *prediction* is where the two formats
genuinely diverge: H.261 predicts from the single preceding macroblock
(DPCM-style, reset to zero at GOB-row starts and non-MC neighbours,
`h261::decode_gob`'s own `reset_pmv`), while H.263 predicts the median of
three neighbouring macroblocks' vectors (§6.1.1, `h263::predictors` +
`motion::median3`) — a mechanism with no MPEG/H.261 analogue at all, so it
needed new per-macroblock vector storage (`ActivePicture::mv_grid`) that
neither H.261 nor `vaco-codec-mpeg12` needed.

### No B-pictures, no field pictures

Neither format has B-pictures, so `picture.rs`'s `RefPicture` holds a
single reference — no `previous`/`recent`/`held` triple, no reordering
delay. Neither has separate field pictures either, so there's no
frame/field addressing to parameterize motion compensation over.

## H.263+ annexes: what landed and what didn't

`Vaco-Spec-Ref: itu-t-h263` (01/2005 edition — the free 03/96 text the
baseline decoder above cites predates Annexes I through X entirely; the
01/2005 edition is the same freely published ITU-T recommendation, its
current in-force revision, cumulative over the amendments that added
them). All annex work lives behind `PLUSPTYPE` (`plus.rs`), the extended
picture header H.263+ substitutes for the fixed 13-bit `PTYPE` whenever
its bits 6-8 read `"111"` — `h263::decode_plus_picture` is the entry
point, parallel to the baseline `PTYPE` branch in
`h263::decode_access_unit`.

**Landed:**

- **Annex D (Unrestricted Motion Vector), both encodings.** The original
  H.263 version 1 form (`PTYPE` bit 10, no `PLUSPTYPE`) reinterprets Table
  14's existing codes with a wider, sign-matched range
  (`motion::h263_umv_vector_legacy`) — no new VLC table. The `PLUSPTYPE`
  form uses Table D.3, a "regularly constructed reversible" code the spec
  itself contrasts with Table 14's ambiguity ("every entry... has a
  single value") — decoded algorithmically
  (`block::decode_table_d3`) rather than transcribed, and verified
  against the spec's own worked example (`-13` → `"0 11 01 11 10"`).
  Reconstruction is a plain sum (`motion::h263_umv_vector_plus`): Tables
  D.1/D.2's per-format ranges and the `UUI == "01"` unrestricted case are
  both encoder-side sending restrictions, not a decoder-side wraparound
  rule, so this crate does not special-case either — a design correction
  made after an early, over-engineered version wrapped the result into an
  absolute range that turned out not to exist on the decode side at all
  (see the bugs section below).
- **Annex K (Slice Structured mode).** Replaces the GOB layer with a
  slice layer: `h263::decode_first_slice` for the one abbreviated slice
  that follows the picture header directly (no `SSC`, no `SSBI`/`SQUANT`
  — `Vaco-Spec-Ref: itu-t-h263` K.2's own text), `h263::decode_slice` for
  every later one. Both the free-running (raster-order-to-end,
  `h263::decode_gob` reused unchanged with an absolute starting index
  instead of a row) and Rectangular Slice (`h263::decode_slice_rect`,
  wrapping every `SWI`-wide row into the next picture row down) submodes
  are implemented; Arbitrary Slice Ordering needed no code at all, since
  this crate already trusts each slice's own `MBA` literally rather than
  assuming bitstream order. `h263::mba_field_width`/`swi_field_width`
  transcribe Tables K.2/K.3's "Default" columns only — the "RRU mode"
  columns are unreachable, since a Reduced-Resolution-Update picture is
  already turned away by `plus::parse` (see below).
- **Annex T (Modified Quantization).** Variable-length `DQUANT`
  (`block::decode_mq_dquant`, Table T.1's small-step deltas or a direct
  6-bit new value), a separate chrominance quantiser `QUANT_C`
  (`block::quant_c`, Table T.2), and the `EXTENDED-ESCAPE`/
  `EXTENDED-LEVEL` coefficient path (`block::decode_extended_level`,
  Figure T.1's bit-rotation, decoded bit-by-bit against the figure's own
  diagram rather than as a derived rotate amount) with the wider
  `|REC| < 4096` reconstruction clip Annex T's own restriction 1 requires
  (`block::dequant_ac_mq`).

**Skipped, for cost — the primary text (already the freely available
01/2005 edition used above) was read for all six, this is a scope
decision, not a provenance one:**

- **Annex F (Advanced Prediction)** needs overlapped block motion
  compensation touching every macroblock's reconstruction (not just
  4-vector ones — OBMC applies whenever the mode is on at all), redefined
  per-block motion vector predictors (Figure F.1's four distinct
  patterns), and a chroma-vector rounding table (F.1) distinct from the
  one this crate already has. This is the single biggest, riskiest
  remaining piece — a genuine rewrite of the motion-compensation
  pipeline, not an additive change — and was set aside rather than rushed
  in the same pass as D/K/T.
- **Annex E (Syntax-based Arithmetic Coding)** replaces every VLC in the
  format with arithmetic coding — a different entropy layer entirely, not
  an additive mode on top of the existing one.
- **Annex G (PB-frames)** and **Annex I (Advanced INTRA Coding)** each
  need their own new prediction/reconstruction machinery (a second,
  bidirectionally-predicted picture interleaved with the primary one; a
  spatial-neighbour DC/AC coefficient predictor) comparable in size to
  Annex F's, with no `PLUSPTYPE` mode bit forcing them together with
  anything already landed.
- **Annex J (Deblocking Filter)** is a post-reconstruction filter over
  the whole picture, structurally unlike anything else in this pass
  (D/K/T all change what one macroblock decodes to, not what happens to
  every macroblock afterward), and interacts with Annex K's own slice
  boundaries in a way (filters across them unless Independent Segment
  Decoding is also active) that would need re-testing once landed.
- **Annex P (Reference Picture Resampling)** changes the reference
  picture's own geometry between pictures (resampling to a new size) —
  `plus::parse` already turns away any picture whose `MPPTYPE` sets the
  `RPR` bit, since this crate's single-`RefPicture` design assumes the
  reference is always the previous decoded frame at the same size.

`plus::parse` bails to `unsupported` (same flat-mid-grey `CORRUPT`
convention as the baseline decoder) for any picture using one of the six
skipped annexes, Reference Picture Resampling/Reduced-Resolution Update,
or an `MPPTYPE` picture-type code naming Improved PB/B/EI/EP (Annexes
M/O) — rather than misreading the scalability/RPS-specific fields
(`ELNUM`/`RLNUM`/`RPSMF`/`TRPI`/`TRP`/`BCI`/`BCM`/`RPRP`) that only ever
follow one of those.

### RV10/RV20 (RealVideo) and FLV1 (Sorenson Spark): no family hooks added

Both were checked against this project's clean-room bar (D7) before
writing anything, the same check applied to the MPEG-4 Part 2 decision on
issue #360: is either variant's divergence from baseline H.263 documented
somewhere that isn't someone else's decoder source? RealVideo's RV10/RV20
bitstream layer has never been openly specified by RealNetworks; every
public description of its H.263-derived quirks (a different picture
header, a proprietary variable-block-size mode) traces back to black-box
reverse-engineering of the reference codec, published as source
(originally in other open-source decoders) rather than as an independent
specification document. Sorenson Spark (FLV1) is nominally H.263-based
but its own divergences (its own 3-bit source-format table with extra
custom-size codes, no start codes, a different `MVD` table) are likewise
undocumented outside decoder source — the already-registered
`adobe-swf-19`/FLV container-level source covers the *container* format
FLV1 streams travel in, not the video bitstream's own H.263 divergences.
Neither variant clears the bar this project already applied to #360;
neither was implemented, and no placeholder source was registered for
either (the same lesson as the `iso-14496-2` placeholder fix: a citation
that looks like evidence of access no one actually has is worse than no
citation).

## D-22 seam findings (a second, different consumer)

`vaco-codec-mpeg12`'s own `TECH-DEBT.md` entry lists four pieces as
generic to the MPEG family, worth extracting into a shared decode core
(D-22, epic #25) once one exists. This crate is that core's second real
consumer, from a genuinely different codec family, and found:

- **Half-pel interpolation transfers exactly; B-picture averaging does
  not apply.** `form_prediction`'s interpolation formula is identical to
  what H.263 needs (`motion::sample_half_pel` here). `average_predictions`
  (B-picture bidirectional averaging) has no counterpart in either format
  — neither has B-pictures.
- **The `previous`/`recent`/`held` reference-delay scheme does not apply
  at all.** Same reason: no B-pictures means no reordering, so
  `picture.rs` only ever needs one reference.
- **The three-stage block pipeline's *shape* transfers; its
  dequantisation *formula* does not.** `inverse_scan`/`dequantise`/
  `inverse_transform` is exactly the right decomposition again, but
  H.261/H.263's dequantisation needed restating from scratch (no
  weighting matrix, no mismatch control — see "Block decode" above) —
  reuse at the *shape* level, not the formula level.
- **The generic `vlc::decode` matcher transfers directly for ordinary
  tables, but not for a table with a non-uniform escape row.** This
  crate's own `vlc.rs` is nearly a copy of `vaco-codec-mpeg12`'s, and
  handles every fixed-length-value table here (`MBA`, `MVD`, `CBP`,
  `MTYPE`, `MCBPC`, `CBPY`) as-is. H.263's `TCOEF` escape code isn't a
  `(bits, value)` row of the same type as the other 102, so it needed a
  hand-written variant (`block::decode_h263_event`) rather than reusing
  `vlc::decode`'s single-type signature — a genuine limit of that
  function's generality, not a bug in it.
- **A new seam this crate found that D-22's original list didn't
  anticipate:** H.263's median-of-three motion vector prediction has no
  MPEG/H.261 analogue and needed new per-macroblock state
  (`ActivePicture::mv_grid`) neither `vaco-codec-mpeg12` nor H.261 needed.
  A future D-22 core built only from the MPEG-family list would have
  missed this entirely.

## D-22 seam findings (a third consumer, same core, extended not replaced)

The annex work re-exercises the same seams the baseline H.263 decoder
already tested against `vaco-codec-mpeg12`'s D-22 list, from inside a
single crate this time rather than across crates — a different angle on
the same question. Mixed results again:

- **Held: half-pel interpolation, unchanged.** Annex D's extended-range
  vectors still resolve to the same `motion::sample_half_pel` call; only
  the *value* the vector holds changed, not how it's used to sample.
- **Held, in a new way: the three-stage block pipeline's shape.**
  Annex T's `QUANT_C` substitution and widened clip
  (`block::dequantise_ranged`) slot into the *existing* `dequantise`
  shape as a parameter, not a new pipeline — the shape identified as
  transferable from MPEG stayed transferable a second time, for a
  same-family extension this time rather than a cross-family one.
- **Did not apply, confirmed a second time: B-picture/reference-delay
  machinery.** None of D/K/T touch picture ordering or reference count;
  the "no B-pictures" seam finding from the baseline report needed no
  revisiting.
- **Did not hold, in a way the baseline report's own words already
  flagged as a risk: the "GOB-header emptiness is not tracked" baseline
  simplification does not carry over to Annex K.** The baseline decoder's
  own "Known gaps" section states `predictors()` "always takes the
  non-empty-header branch" for GOB row crossings — but `predictors()`
  itself has no such branch at all; it only special-cases *absolute*
  picture boundaries, never a GOB/row boundary specifically. That
  discrepancy between the comment and the code was harmless for baseline
  streams, because ffmpeg's own encoder was observed sending an empty
  header for every row but the first — the row-crossing case the comment
  describes essentially never fires. Annex K's slices always carry a
  real, non-empty header (§K.1's own rule 1: "the prediction of motion
  vector values are the same as if a GOB header were present"), so the
  gap between the comment's claim and the code's actual behavior stopped
  being harmless the moment slices were real: an early version of this
  annex work reused `predictors()` completely unchanged and measured a
  slowly *accumulating* per-frame error (avg MAD growing from 0.058 at
  frame 1 to 9.07 by frame 49 against a real ffmpeg fixture, almost all
  of it gone once fixed — see the bugs section below). Fixed by adding an
  explicit per-macroblock slice id (`ActivePicture::mb_slice_id`,
  parallel to `mv_grid`) that `predictors()` now consults, but *only*
  when `slice_structured` — baseline decode is bit-for-bit unaffected
  (confirmed by re-running the six baseline fixtures from "Measured
  accuracy" after this fix landed: identical numbers).
- **Newly discovered, and specific to this crate's own byte-alignment
  design: a start-code scan that assumes it is already byte-aligned
  misses one that is reachable through pure stuffing bits.**
  `h263::at_start_code`'s original form only recognised a start code when
  `r`'s bit position was *already* a multiple of 8. That is harmless for
  the same reason the GOB-emptiness gap above was harmless: baseline GOB
  rows mostly have no boundary to find there at all, so the "already
  aligned" check was rarely asked to notice a boundary mid-picture.
  Annex K's slices always end in `SSTUF` (zero stuffing bits, fewer than
  8, padding to the next `SSC`'s byte alignment) — a boundary that is
  *almost never* already byte-aligned by chance, since macroblock
  coefficient data is variable-length. Without recognising it, the
  decode loop read those stuffing zero bits as one more macroblock's own
  `COD`/`MCBPC` bits, corrupting the last few macroblocks of essentially
  every row before the outer byte-level scan re-synchronised at the next
  slice's real start code. This is the dominant seam this crate's own
  byte-oriented design (as opposed to H.261's bit-oriented one) had not
  needed to confront before Annex K made a boundary-after-almost-every-
  row the normal case rather than the rare one. Fixed by extending
  `at_start_code` to peek past a run of zero stuffing bits up to the next
  byte boundary before giving up (see its own doc comment for the exact
  peek arithmetic) — this also fixes the same latent risk for a baseline
  stream that legitimately sends a non-empty mid-picture GOB header on a
  non-byte-aligned boundary, not only for Annex K.

## Real bugs found and fixed while building the differential harness

All four were found by comparing this crate's own `examples/decode_dump`
output against `ffmpeg`-decoded reference YUV for streams `ffmpeg`
encoded itself (`-bitexact`, both `h261` and `h263` encoders, QCIF and
CIF, intra-only and mixed I/P) — never by reading `ffmpeg`'s decoder
source, per this project's clean-room constraint.

1. **H.261 coefficient decode stopped one VLC read short of a
   still-present `EOB`.** `decode_h261_coefficients`'s loop bound was
   `while n < 64`, which exits as soon as 64 coefficient positions are
   filled — but an encoder still transmits an explicit `EOB` even when
   every position is already filled, and stopping one read early left
   that `EOB` unconsumed for the *next* block to misinterpret as its own
   first code. This is the exact bound `vaco-codec-mpeg12`'s own
   `decode_coefficients` already documents at its own loop
   (`while n < 65`) — the same lesson, a separate instance of it, caught
   here by the same symptom: two macroblocks decoding pixel-perfect, the
   third corrupting for no visible reason. Fixed by widening the bound to
   `n < 65`, matching MPEG's own reasoning exactly.
2. **H.263: an intra macroblock's `TCOEF` was read unconditionally
   instead of being gated by `CBP`.** H.261 unconditionally transmits all
   six blocks' full coefficient data for every intra macroblock — no
   `CBP` needed. H.263 does *not* carry that rule forward: §5.4 states
   `INTRADC` is unconditional for an intra block, but "`TCOEF` is present
   if indicated by `MCBPC` or `CBPY`" — the very same `CBP` gate an inter
   block's residual uses. Copying H.261's "intra means always coded" rule
   into H.263's block loop made every intra block with a clear `CBPY`/
   `CBPC` bit read a `TCOEF` sequence that was never transmitted,
   consuming the *next* block's own `INTRADC` and beyond as garbage
   coefficient codes. Fixed by threading `has_tcoef` (the `CBP` bit)
   through `decode_h263_coefficients` independently of `intra` (which now
   only controls whether `INTRADC` is read first).
3. **H.263: a row-at-a-time GOB design silently dropped every row below
   the first.** §5.2 allows a GOB header to be entirely absent for every
   row but row 0 ("the GOB header may be empty, depending on the encoder
   strategy") — ffmpeg's own `h263` encoder does exactly that for a whole
   QCIF picture. A design that decoded one macroblock row and then handed
   back to the outer byte-level scan to find the next GOB header found
   nothing there (because none was sent) and stopped, leaving rows
   1..`mb_height`-1 at the frame's initial contents. Fixed by decoding in
   raster order across row boundaries within one call
   (`h263::decode_gob`), checking for a genuine start code
   (`h263::at_start_code`) before every macroblock — which also correctly
   stops *early* on a GOB header a more conservative encoder does send.
4. **H.261: an un-addressed GOB tail was never treated as skipped.** MBA
   addressing means an encoder with nothing left to say for the rest of a
   GOB just stops — the trailing macroblocks up to address 33 are never
   mentioned at all, not even via a skip-run between two addressed
   macroblocks (`fill_skipped` only ever covered *internal* gaps). Missing
   this left every GOB's un-addressed tail at the frame buffer's initial
   (uninitialised) contents, invisible whenever a GOB happened to use all
   33 addresses and glaringly wrong whenever it legitimately didn't (which
   real encoders do constantly once part of a scene stops changing).
   Fixed by running the same `fill_skipped` logic once more after
   `decode_gob`'s own loop ends, for whatever addresses were never
   reached.
5. **H.261: the outer scan's forward-progress guard cut into the very
   next GOB header it was trying to find.** After fix 4, an *empty* GOB
   (a header with zero macroblock data — explicitly legal per §4.2.2:
   "transmitted once... even if no macroblock data is present") has
   `decode_gob` return the exact bit position its own macroblock data
   would have started at, which can be identical to where the *next*
   GOB's header begins. The outer loop's own "make progress even if a GOB
   call returns no progress" fallback used `mb_start + 1` — one bit past
   that shared position — stepping over the very start of the next
   header on a scan that never looks backward to recover it. Fixed by
   comparing against `sc_bit + 1` (this iteration's own found prefix)
   instead, which still guarantees forward progress without needing to
   step past a position `decode_gob` may have legitimately returned
   unchanged.

### Bugs found while building the H.263+ annex differential harness

Found the same way as the five baseline bugs above: comparing this
crate's own decode against `ffmpeg`-decoded reference YUV for a real
`ffmpeg -c:v h263p -bitexact -umv 1` fixture (QCIF, standard 30000/1001
framerate — a non-standard rate was tried first and dropped once it
became clear it forced `CPFMT`/custom-PCF parsing to be exercised too,
which was not the point of this particular fixture). ffmpeg's own h263p
encoder was observed, empirically, to always route `-umv` through
`PLUSPTYPE` (never the legacy `PTYPE`-bit-10 form) and to always set the
Slice Structured mode bit alongside it regardless of whether
`-structured_slices` was requested — meaning Annex D's `PLUSPTYPE` path
and Annex K could not be differentially tested independently of each
other against this encoder; the legacy `PTYPE`-bit-10 UMV path and Annex
T were verified only by hand-crafted-bitstream unit tests
(`motion::tests`, `block::tests`), a weaker but still real tier, same
caveat already noted for the annexes ffmpeg's encoder cannot produce at
all.

6. **The picture-layer `PEI`/`PSUPP` chain was never read on the
   `PLUSPTYPE` path.** The baseline picture header calls
   `skip_pei_chain` right after `CPM`/`PSBI`; `decode_plus_picture`
   read `PQUANT` and went straight to slice/GOB decode without it.
   Figure 8 does not list `PEI` among `PLUSPTYPE`'s own optional fields
   because it isn't one — it keeps its ordinary position from Figure 7,
   right before the picture data, in *both* headers. Even with no
   supplemental data at all, `PEI` is one `"0"` bit that still has to be
   read; omitting it left every `PLUSPTYPE` picture's first slice/GOB
   exactly one bit short of alignment, corrupting `SEPB1` onward for
   the very first slice of every picture that used the extended header.
   Found by hand-verifying the abbreviated first slice's own bits
   (`SEPB1` decoded as `0`, which the spec fixes at `1`) against a
   brute-force search for the bit offset where `SEPB1 == 1` and
   `MBA == 0` both held — off by exactly one bit, and `skip_pei_chain`
   was the one-bit gap. Fixed by calling it in `decode_plus_picture`
   too, right after `PQUANT`.
7. **`at_start_code` needed a stuffing-aware branch — see the seam
   finding above** for the fuller explanation; the same fix is listed
   here because it was found the same way (a real ffmpeg fixture
   decoding to 44-55% pixel-exact on an intra picture that should have
   been ~99%, root-caused to garbage macroblocks appearing at every row
   boundary once the `PEI` fix above got the picture header itself
   aligned correctly).
8. **`predictors()`'s picture-boundary-only substitution needed a
   slice-boundary case for Annex K — see the seam finding above** for
   the fuller explanation; found after fixes 6 and 7 already brought an
   intra picture (frame 0 of the UMV/slice fixture) to 99.2% exact,
   pixel-identical to this crate's own established baseline accuracy,
   while every following P-picture still showed a small but steadily
   *growing* per-frame error — the signature of a systematically wrong
   (rather than occasionally wrong) motion-vector predictor, not a
   remaining VLC or dequantisation bug.

## Measured accuracy

Measured with `examples/decode_dump`, comparing every plane of every
frame against `ffmpeg -i <file> -pix_fmt yuv420p -f rawvideo -` (the same
`ffmpeg` build used to encode the fixture, with `-bitexact` at encode
time). All figures below are five-frame runs (`testsrc`, QCIF/CIF, 25
fps) covering intra-only (`-g 1`) and mixed I/P (`-g 100`) GOP structures
for both formats:

| Fixture | Format | Frames | avg MAD (all frames) | max diff | % exact |
| --- | --- | --- | --- | --- | --- |
| QCIF, intra-only | H.261 | 5/5 | 0.006 – 0.008 | 1 | 99.2 – 99.4% |
| QCIF, mixed I/P | H.261 | 5/5 | 0.008 – 0.011 | 1 | 98.9 – 99.2% |
| CIF, mixed I/P | H.261 | 5/5 | 0.003 – 0.004 | 2 | 99.6 – 99.7% |
| QCIF, intra-only | H.263 | 5/5 | 0.006 – 0.008 | 1 | 99.2 – 99.4% |
| QCIF, mixed I/P | H.263 | 5/5 | 0.008 – 0.011 | 1 | 98.9 – 99.2% |
| CIF, mixed I/P | H.263 | 5/5 | 0.003 – 0.004 | 2 | 99.6 – 99.7% |
| QCIF, UMV + Annex K, I-only | H.263+ | 1/1 | 0.008 | 1 | 99.2% |
| QCIF, UMV + Annex K, mixed I/P (50 frames) | H.263+ | 50/50 | 0.008 – 0.78 (growing) | 1 – 17 | 99.2% – 85.9% |

The last row is the one real-world differential fixture available for
the annex work (`ffmpeg -c:v h263p -bitexact -umv 1`, which — per the
finding above — always couples Annex D's `PLUSPTYPE` path with Annex K).
Its own I-picture (frame 0, no motion vectors at all) matches this
crate's established baseline accuracy exactly, confirming the picture-
header and slice-layer work is sound; the P-pictures accumulate a small,
slowly growing error not yet root-caused beyond ruling out the two bugs
above (both fixed, and responsible for the bulk of a much larger error
before the fix — avg MAD peaked at 9.07 by frame 49 pre-fix, 0.78
post-fix). The remaining gap is sparse (roughly 1 in 40 pixels by frame
49) and small in magnitude (1-2 of 255 code values per affected pixel) —
consistent with one more small, not-yet-isolated rounding or edge-case
difference in the UMV/Annex-K motion-vector path, not a structural
misread. Annex T and the legacy (non-`PLUSPTYPE`) Annex D path have no
equivalent real-fixture number at all — see the bugs section above for
why — only the hand-crafted-bitstream unit tests in `block::tests` and
`motion::tests`, verified against the spec's own worked examples (Table
D.3's `-13` example; Table T.1/T.2's own examples) rather than against a
second independent implementation.

Not literally framemd5-identical to the reference in any of these —
`vaco-codec-jpeg`'s own precedent applies here too: `vaco-codec-dsp-idct`
runs the inverse DCT in `f32`, and neither H.261's Annex A nor H.263's
equivalent accuracy framing mandates a bit-exact integer transform, only
an error bound. A ±1 difference on ~99% of samples, with no macroblock or
frame-level divergence, is the expected shape of that gap — not evidence
of a remaining decode bug. No systematic bias, growing error, or
localized corruption was found in any of the six fixtures above after the
five bugs in the previous section were fixed; each of those, before its
own fix, showed a qualitatively different and much larger symptom
(complete GOB/row corruption, cascading picture-to-picture error growth),
not this small a uniform gap.

## How to change it

- **New VLC table row found wrong**: every table lives in `src/tables.rs`
  with its own doc comment citing the spec table/clause. Follow the CBP
  lesson from `vaco-codec-mpeg12` (and bug 1 above): when transcribing a
  table, assert each row's *exact bit length*, not only prefix-freedom
  and value coverage — a code transcribed one bit short still passes both
  of those checks while silently shifting every later code.
- **A new H.263+ annex (E/F/G/I/J/P, or a later one)**: start from
  `plus::parse`'s own bail list (currently SAC/Advanced Prediction/
  Advanced INTRA/Deblocking Filter/Reference Picture Selection/
  Independent Segment Decoding/Alternative INTER VLC/RPR/RRU/Improved
  PB/B/EI/EP) and this file's "what landed and what didn't" section above
  for why each one already skipped was skipped. `mb_type == 2`
  (`INTER4V`) inside `try_decode_one_mb` is the one per-macroblock case
  that also needs unlocking for Advanced Prediction mode specifically —
  it currently fails the whole picture the same as any other
  unimplemented mode.
- **The legacy (non-`PLUSPTYPE`) `UMV`/`SAC`/`AP`/`PB-frame` bits in the
  fixed 13-bit `PTYPE`**: still gate `unsupported` in the non-`PLUSPTYPE`
  branch of `h263::decode_access_unit` for every mode except `UMV`
  (`umv_legacy`, unlocked by this pass — see
  `motion::h263_umv_vector_legacy`).
- **A GOB spanning more than one row (larger source formats)**: this
  crate hard-codes one macroblock row per GOB (`h263::decode_access_unit`
  treats `gn` directly as a row index), which is correct for every
  conformance stream on hand but not universally — see "Known gaps".
- **The loop filter (H.261's `FIL`)**: `motion::h261_loop_filter`,
  applied per 8x8 block before the residual is added
  (`h261::reconstruct_macroblock`). Its own doc comment cites §3.2.3's
  exact tap/rounding rule.

## Configuration

No environment variables, feature flags, or config files. `Limits`
(passed to both `H261Decoder::new`/`H263Decoder::new`) bounds allocation
the same way every other decoder in this workspace does.

## Dependencies

- `vaco-bitstream` — `BitReader` for both formats' bit-level reads; the
  bit-level (H.261) and byte-level (H.263) start-code scanners are this
  crate's own, not `vaco_bitstream::annexb` (see "How it works").
- `vaco-codec-core` — `Decoder`/`Machine`/`Caps` (both decoders use
  `Caps::DELAY.union(Caps::SUBFRAMES)`, matching `vaco-codec-mpeg12`'s own
  reasoning: a packet is not guaranteed to hold exactly one picture).
- `vaco-codec-dsp-idct` — the shared 8x8 IDCT (`mpeg2::idct8x8_f32`,
  generic despite the module name).
- `vaco-frame`, `vaco-pixfmt`, `vaco-packet`, `vaco-limits`, `vaco-core` —
  the same model/limits types every decoder in this workspace uses.

## Known gaps

- **H.263 GOBs spanning more than one macroblock row.** §5.2 permits
  this; every conformance stream checked here uses exactly one row per
  GOB (true of every real encoder's QCIF/CIF output on hand). A stream
  using more rows per GOB is decoded on the wrong row boundaries rather
  than rejected.
- **H.263's "outside the GOB" motion-vector-prediction border rule is not
  distinguished from "outside this row, inside the picture" for the
  *baseline GOB layer*.** §6.1.1 rule 3 treats a neighbour as unavailable
  if it's outside the *picture* **or** outside the *current GOB* when
  that GOB's own header was non-empty. `predictors()` has no GOB-boundary
  branch at all (only the absolute-picture-boundary one) — this was
  previously mis-described in this same section as "always takes the
  non-empty-header branch," which is not what the code does; see the
  third D-22 seam report above for how that discrepancy was found. It
  remains true for the baseline GOB layer (harmless for essentially every
  real encoder's output, which sends an empty header for every row but
  the first) but no longer true for Annex K's slice layer, which now has
  its own, correct per-slice version of this same rule
  (`ActivePicture::mb_slice_id`).
- **`mb_type == 2` (H.263 `INTER4V`)** stops decoding the rest of the
  picture rather than reading four vectors per macroblock — only
  meaningful under Advanced Prediction mode, out of scope (see "what
  landed and what didn't" above).
- **Annexes E, F, G, I, J, P** are not implemented at all — skipped for
  cost, not provenance; see "what landed and what didn't" above for the
  reasoning behind each.
- **RV10/RV20 and FLV1** have no family hooks — their divergence from
  baseline H.263 does not clear this project's clean-room provenance bar
  (documented only in decoder source, not in an independently published
  specification); see "RV10/RV20 and FLV1" above.
- **Annex D/T's real-fixture differential testing is coupled to Annex K**
  (ffmpeg's own h263p encoder was observed always enabling Slice
  Structured mode alongside any other `PLUSPTYPE` mode), and Annex T has
  no exposed ffmpeg encoder toggle at all — see "Measured accuracy" for
  what tier of testing each annex actually got.
- **A small, not-yet-isolated residual error accumulates across
  P-pictures in the one real UMV+Annex-K fixture available** (avg MAD
  0.78, ~86% pixel-exact by frame 49, down from an intra-picture 99.2%)
  — see "Measured accuracy" for the numbers and the two bugs already
  ruled out.
- **Neither decoder reaches literal framemd5-identical output** — see
  "Measured accuracy". Reference-quality (±1, no localized error) on
  every *baseline* fixture checked and on the one annex fixture's
  intra picture; not bit-exact anywhere, and not yet reference-quality on
  the annex fixture's P-pictures specifically (see the point above).
- **No B-frame, PB-frame, or field-picture support** — neither format's
  baseline syntax has any of these; not a gap relative to this crate's
  stated scope, listed here only so a future annex pass knows what
  `picture.rs`'s single-reference design would need to grow.
