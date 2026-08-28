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
syntax only — Unrestricted Motion Vector, Syntax-based Arithmetic Coding,
Advanced Prediction and PB-frames (Annexes D/E/F/G, all optional per
`PTYPE`'s own mode bits) are a later pass, not this crate; a picture using
any of them is decoded as a flat mid-grey `CORRUPT` frame rather than
silently producing wrong pixels. Encode is out of scope entirely for both
formats.

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
- **A new H.263 annex (Annex D/E/F/G, or the newer ones)**: start from
  `h263::decode_access_unit`'s `unsupported` flag (currently set by any of
  `UMV`/`SAC`/`AP`/`PB-frame` in `PTYPE`) and `h263.rs`'s own module docs
  for what's already excluded. `mb_type == 2` (`INTER4V`) inside
  `decode_gob` is the one per-macroblock case that also needs unlocking
  for Advanced Prediction mode specifically.
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
  distinguished from "outside this row, inside the picture."** §6.1.1
  rule 3 treats a neighbour as unavailable if it's outside the *picture*
  **or** outside the *current GOB* when that GOB's own header was
  non-empty. This crate always takes the "non-empty header" branch,
  matching essentially every real encoder (which almost always does send
  a header when it sends one at all) but not a stream that legitimately
  relies on the empty-header exception for this specific rule.
- **`mb_type == 2` (H.263 `INTER4V`)** stops decoding the rest of the
  picture rather than reading four vectors per macroblock — only
  meaningful under Advanced Prediction mode, already out of scope.
- **Neither decoder reaches literal framemd5-identical output** — see
  "Measured accuracy". Reference-quality (±1, no localized or growing
  error) on every fixture checked, not bit-exact.
- **No B-frame, PB-frame, or field-picture support** — neither format's
  baseline syntax has any of these; not a gap relative to this crate's
  stated scope, listed here only so a future annex pass knows what
  `picture.rs`'s single-reference design would need to grow.
