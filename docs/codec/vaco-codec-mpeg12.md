# `vaco-codec-mpeg12`

Layer 4. ITU-T H.262 / ISO/IEC 13818-2 (MPEG-2 video) and ISO/IEC 11172-2
(MPEG-1 video) native **decode** (T2-01a, epic #36). Encode (T2-01c/#357) is
out of scope for this crate.

## What it is

An MPEG-1/2 video decoder built directly on `vaco-parse-mpegvideo`'s header
parsing conventions (this crate does not depend on that crate at runtime —
see "Known gaps" for why) and `vaco-codec-dsp-idct`'s `mpeg2` inverse DCT
module. It decodes `sequence_header`/`sequence_extension`/
`group_of_pictures_header`/`picture_header`/`picture_coding_extension`/
`quant_matrix_extension`, then walks `slice()`/`macroblock()` to reconstruct
I/P/B frame pictures — both `frame_pred_frame_dct == 1` (progressive-style)
and `== 0` (interlaced frame pictures using per-macroblock field/frame
prediction, the common real-world `ffmpeg -flags +ilme+ildct` shape).
Written from the free 1995 ITU-T H.262 Recommendation text directly (D6/D7
clean-room — FFmpeg source was never read); its provenance id is
`itu-t-h262` in `provenance/sources.toml`, cited via `Vaco-Spec-Ref:`
trailers in this crate's commits and per-table in
`provenance/vaco-codec-mpeg12.toml`.

D-22 (epic #25, a shared decode core for the MPEG family) does not exist
yet. This crate is the first real consumer of what it would factor out of;
`TECH-DEBT.md` lists the specific pieces (motion compensation, B-picture
reference management, the inverse-scan/dequantise/IDCT pipeline, the
generic VLC decoder) that are generic to the family rather than to H.262
specifically, for whoever picks up D-22 next.

## How it works

### Header and sequence state

`headers.rs` parses each header/extension into a plain struct; `decoder.rs`
keeps a `Sequence` (dimensions, matrices, macroblock grid, and — critically
— whether a `sequence_extension()` was ever seen, which is how this crate
tells MPEG-1 from MPEG-2 without a separate flag anyone has to remember to
set). MPEG-1 streams have no `picture_coding_extension()` at all;
`PictureCodingExtension::mpeg1_default` synthesizes the MPEG-1-equivalent
defaults (linear quantiser scale, 8-bit intra DC precision, frame pictures,
Table B.14 always) the moment the first slice of a picture with no such
extension is seen.

### Slice and macroblock decode

`macroblock.rs`'s `decode_slice` walks `macroblock_address_increment`
codes, dispatching to `decode_skipped_macroblock` (§7.6.6.2/§7.6.6.4 —
P-picture implicit-zero-vector skip, B-picture same-direction-as-previous
skip) or `decode_coded_macroblock` for each real macroblock. The latter
reads `macroblock_type` (a different VLC table per picture type),
`frame_motion_type`/`dct_type` when present, motion vectors
(`motion.rs::decode_vector`, implementing §7.6.3.1's reconstruction formula
including the field-and-frame-picture PMV halving/doubling special case),
`coded_block_pattern`, and finally each block's residual
(`block.rs::decode_coefficients`).

### Block decode: entropy, inverse scan, dequantisation, IDCT

`block.rs` runs §7.2's VLC decode (Table B.14/B.15, `first_coefficient_only`
handling for the non-intra first-AC-coefficient special case — see "Real
bugs found and fixed" below), §7.3's inverse scan (zigzag or alternate, picked by
`alternate_scan`), §7.4's dequantisation including §7.4.4's mismatch
control, and finally `vaco_codec_dsp_idct::mpeg2::Idct8x8<f32>` for the
inverse transform — Annex A specifies an accuracy bound, not a mandated
integer algorithm, so this crate (like `vaco-codec-jpeg`, under the
identical contract) uses that module's floating-point evaluation rather
than reimplementing a bit-exact integer IDCT.

### Motion compensation and B-picture reference management

`motion.rs::form_prediction` reads a reference plane offset by a half-pel
motion vector, using the `//` operator's spec-defined "round to nearest,
ties away from zero" (§4.1) — not truncating division — via `avg2`/`avg4`.
It is parameterized over `row_scale`/`row_parity` so the same code serves
both frame-based addressing and field-based-within-a-frame-picture
addressing (reading every other row of the same decoded frame).

Because a B-picture's references are always already-decoded pictures that
appear in the bitstream *before* it (which is *after* the B-picture's own
display position), `decoder.rs` holds the most-recently-decoded reference
picture (`held`) rather than emitting it immediately, releasing it only
once the *next* reference picture is decoded — by which point every
B-picture between them has already been decoded and emitted in the right
order. `previous`/`recent` are the forward/backward references a
B-picture reads; a P-picture reads `recent` alone.

### Chroma formats: 4:2:0, 4:2:2, 4:4:4

`macroblock::ChromaFormat` (from `sequence_extension()`'s `chroma_format`,
Table 6-5 — always 4:2:0 for an MPEG-1 sequence, which has no
`sequence_extension()` at all) is the one enum every chroma-format-shaped
decision routes through: `block_count()` (Table 6-20: 6/8/12 blocks),
`chroma_mb_pixels()` (§6.1.3's per-format subsampling: 8x8, 8x16, or
16x16 pixels of chroma per macroblock), `chroma_block_slot()`
(§6.3.17.4/Figures 6-10 through 6-12: which block index lands at which
`(plane, col, row)` within the macroblock's chroma area), and
`scale_vector()` (§7.6.3.7: how much a luma motion vector shrinks for the
chroma sample grid — both components halved for 4:2:0, only the
horizontal one for 4:2:2, neither for 4:4:4). `decoder.rs::begin_picture`
picks the output `PixFmt` (`Yuv420p`/`Yuv422p`/`Yuv444p`) from the same
value. `coded_block_pattern`'s base 6-bit VLC (Table B.9) is unchanged
across all three formats; 4:2:2 appends a 2-bit `coded_block_pattern_1`
and 4:4:4 a 6-bit `coded_block_pattern_2` (§6.2.5.3), both plain
fixed-length reads, decoded in `decode_coded_macroblock` right after the
base VLC. See "Known gaps" above for 4:4:4's one open honesty note (a
likely defect in H.262's own `coded_block_pattern_2` pseudocode,
implemented literally rather than "corrected" against a guess, and
untestable against any real corpus either way).

## Real bugs found and fixed while building the differential harness

All were found by hand-tracing a real `ffmpeg`-encoded fixture against
this crate's own bit-position instrumentation (removed before landing).
None was caught by the table-level unit tests as originally written —
regression tests for each are now in place, but the general lesson is
that a table can be individually well-formed (prefix-free, every value
present) and still be wrong at one specific length, and a decode routine
can be locally correct and still consume the wrong number of bits:

1. **The non-intra first-coefficient special case was eating a bit it
   should have left in the stream.** §7.2.2.2 modifies Table B.14 so a
   lone leading `1` bit, for the *first* coefficient of a non-intra block
   only, means (run=0, level=1) instead of the normal 2-bit `11` code —
   safe because §7.2.2.2 itself states End-of-Block can never be the first
   code of a coded block, so there is no ambiguity. The check was written
   as `r.get_bit() == 1`, which *consumes* the bit whether or not it
   matches — so on the (far more common) case where the real first bit is
   `0`, that `0` was silently dropped instead of being left for the main
   VLC table to read as the first bit of an ordinary code. Every non-intra
   block after the first coefficient of the picture was reading one bit
   short, and every macroblock past the first was reading a bitstream
   that was already off by a growing number of bits. Fixed by using
   `peek(1)` and only consuming the bit when it actually is the special
   marker (`block::decode_coefficients`).
2. **MPEG-1's escape code is not MPEG-2's escape code with different
   numbers — it is a different bit layout.** H.262 Annex D.9.3 ("Run-level
   escape syntax") documents this as a deliberate difference: MPEG-2 uses
   a fixed 6-bit run + 12-bit two's-complement level; MPEG-1 uses a 6-bit
   run + an 8-bit sign-magnitude level, extended to a further 8 bits (16
   total) when that first byte is the `0x00`/`0x80` sentinel. This crate
   initially implemented only the MPEG-2 form unconditionally, so any
   MPEG-1 stream that used an escape code at all (which real encoders do
   routinely — not a rare edge case) read the wrong number of bits and
   desynced every macroblock after it. Fixed by threading an `is_mpeg1`
   flag (`ActivePicture::mpeg1`, set from whether a `sequence_extension()`
   was ever seen) down to `decode_coefficients`'s escape branch. This is
   the one piece of MPEG-1-specific bitstream syntax in this crate that
   ISO/IEC 11172-2 itself was *not* read to implement — that document is
   not freely available and this crate's D6/D7 clean-room rule only covers
   the free `itu-t-h262` text — reconstructed entirely from Annex D.9.3's
   own stated field widths and ranges; see `provenance/vaco-codec-mpeg12.toml`
   for the exact reasoning.
3. **A packet holding more than one picture could emit more than one
   frame per `send_packet`.** `decode_access_unit` walks every start code
   in whatever byte range it is handed, and nothing guarantees a packet
   holds exactly one picture — a generic elementary-stream demuxer, or
   simply an adversarial input, can bundle several. Found within 45
   seconds by the very first run of `fuzz/fuzz_targets/mpeg12_decode.rs`,
   which tripped `Machine::emit`'s "more than one output for one input
   without `Caps::SUBFRAMES`" debug assertion. Fixed by declaring
   `Caps::SUBFRAMES` (`Mpeg12Decoder::new`) — this decoder's own handling
   of a multi-picture packet genuinely can produce more than one frame
   from one `send_packet` call, so this is the capability flag telling
   the truth about it, not a workaround.
4. **`CODED_BLOCK_PATTERN`'s three lowest-value codes were transcribed one
   bit too long.** Table B.9's last three rows (`cbp` 27, 39, 0) are 9
   bits each, one bit shorter than the four rows just above them despite
   the visual column alignment in the spec's own printed table — an easy
   miscount, and one this crate's own `coded_block_pattern_is_prefix_free_
   and_covers_every_value` test could not catch: an extra leading zero on
   all three rows keeps every code prefix-free and every value 0-63 still
   present, just three codes later than the real bitstream ever produces
   them. Found by hand-tracing a real encoder's bits at the exact point
   `coded_block_pattern()` failed to match any code in
   `crates/codec/vaco-codec-mpeg12/src/macroblock.rs`'s CBP decode: the
   raw bits were exactly the spec's correct 9-bit `000000010` (`cbp` 39),
   one bit short of this crate's wrong 10-bit table entry. This was the
   single largest accuracy fix in this crate's history — see "Measured
   accuracy" below for what it did to every MPEG-2 fixture larger or
   busier than a small test clip.

Finding bug 4 also motivated a structural change beyond the table fix
itself: `ActivePicture::supported` used to mean two different things —
"this picture's coding mode is not implemented" (checked once, correctly
whole-picture-scoped) and "a local VLC decode failure happened somewhere
in this picture" (also whole-picture-scoped, incorrectly). A single bad
macroblock therefore cost not just the rest of its own slice but every
*other*, independent slice later in the same picture, and since P/I
pictures are motion-compensation references, every later picture in the
GOP too. `ActivePicture::slice_ok` now carries the local-decode-error
meaning, reset to `true` at the start of every `decode_slice` call rather
than once per picture — a bad slice still loses the rest of itself, but a
later slice in the same picture gets a clean start. This alone measurably
narrowed the visible corruption from "the rest of the picture and every
later picture in the GOP" down to "one slice, sometimes two," which is
what made hand-tracing bug 4 to its exact bit position tractable at all.

5. **`macroblock_stuffing` (H.262 Annex D.9.2), an MPEG-1-only VLC code,
   was not implemented at all.** Unlike bugs 1-4, this was not found by
   hand-tracing a bit-position desync — it was found by re-reading D.9's
   own "differences from ISO/IEC 11172-2" list while investigating the
   still-open MPEG-1 accuracy gap (see "Measured accuracy" below) and
   noticing this crate's `macroblock_address_increment` decode loop
   (`macroblock.rs`) had no handling whatsoever for the one VLC code
   ("0000 0001 111") MPEG-1 permits any number of times directly before a
   real address-increment code, which a decoder must silently discard.
   MPEG-2 reserves this exact bit pattern and never emits it. Fixed by
   peeking for and skipping it, gated on `ap.mpeg1`, before every
   address-increment decode attempt. Measured to make no difference on
   any fixture on hand (ffmpeg's own MPEG-1 encoder apparently never
   emits it for this content) — kept anyway as a genuine, cost-free,
   spec-required correctness fix, not a fix for the accuracy gap itself.

## Measured accuracy

Differential-tested against `ffmpeg`-decoded reference `yuv420p` (max-abs-
deviation and RMS per frame, averaged and maxed over every frame in each
fixture — `examples/decode_dump.rs` is the harness, one packet per access
unit to respect `Caps::DELAY`'s at-most-one-frame-per-`send_packet`
contract):

| Fixture | Size | Coding | avg MAD | max MAD | avg RMS |
|---|---|---|---|---|---|
| `solid16` (hand-verified, one macroblock) | 16x16 | I | 0 | 0 | 0 |
| `m2_i` | 64x48 | MPEG-2, I only | 1.7 | 2 | 0.19 |
| `m2_ip` | 64x48 | MPEG-2, I+P | 1.5 | 2 | 0.16 |
| `m2_ipb` | 64x48 | MPEG-2, I+P+B | 1.1 | 2 | 0.14 |
| `m2_ilme` | 64x48 | MPEG-2, interlaced (`+ilme+ildct`) | 1.1 | 2 | 0.14 |
| `m2_oddsize` | 48x64 | MPEG-2, I+P+B | 1.0 | 2 | 0.14 |
| `m2_qcif_ipb` | 176x144 | MPEG-2, I+P+B | 1.8 | 2 | 0.11 |
| `m2_cif_ipb` | 352x288 | MPEG-2, I+P+B | 1.4 | 2 | 0.08 |
| `m1_i` | 64x48 | MPEG-1, I only | 1.96 (was 9.0) | 2 (was 9) | 0.19 (was 0.96) |
| `m1_ip` | 64x48 | MPEG-1, I+P | 1.88 (was 44.2) | 2 (was 97) | 0.15 (was 6.37) |
| `m1_ipb` | 64x48 | MPEG-1, I+P+B | 1.84 (was 44.2) | 2 (was 97) | 0.13 (was 6.38) |
| `m2_422` | 176x144 | MPEG-2, 4:2:2, I+P (`-pix_fmt yuv422p`) | 1.9 | 2 | 0.11 |
| `m2_422_ipb` | 176x144 | MPEG-2, 4:2:2, I+P+B | 1.6 | 2 | 0.10 |
| `m2_altscan` | 176x144 | MPEG-2, 4:2:0, `alternate_scan=1`, I+P | 1.9 | 2 | 0.11 |
| `m2_intravlc` | 176x144 | MPEG-2, 4:2:0, `intra_vlc=1`, I+P | 1.9 | 2 | 0.11 |
| `m2_422_alt_ivlc` | 176x144 | MPEG-2, 4:2:2 + `alternate_scan=1` + `intra_vlc=1`, I+P | 4.3 | 5 | 0.12 |

**Every MPEG-2 fixture is now reference-quality**: max-abs-deviation of 2
across the board, consistent with `vaco-codec-dsp-idct`'s floating-point
IDCT rounding against the reference's own (unspecified) rounding, not a
decode error. This is a real result from this session, not the original
state — `m2_qcif_ipb`/`m2_cif_ipb`/`m2_oddsize` previously measured avg MAD
of 200+, 234+ and 10.8 respectively, from a real, now-fixed bug (see
"Real bugs found and fixed" above). Two changes got there: splitting
`ActivePicture::supported` (whole-picture-unsupported) from a new
`ActivePicture::slice_ok` (this-slice-only local decode error) so one bad
slice no longer costs the rest of the picture and every later picture in
the GOP that references it, and a genuine table transcription bug in
`CODED_BLOCK_PATTERN` (three codes recorded one bit too long).

**The `m2_422`/`m2_altscan`/`m2_intravlc`/`m2_422_alt_ivlc` rows are new
corpus, not new numbers for an old corpus**: before T2-01b/#356, nothing
this crate had ever decoded set `alternate_scan`, `intra_vlc_format`, or a
`chroma_format` other than 4:2:0 — `ChromaFormat`/the alternate scan table
/the intra-VLC table existed and were unit-tested, but no *fixture*
actually exercised the bitstream path that selects any of them. All four
reach the same reference-quality band the 4:2:0 corpus already established
(avg MAD 1.6-4.3, max MAD 2-5 — the one row above 2 is the fixture
combining all three features at once across a full I+P GOP, consistent
with error compounding across more P-pictures rather than a new defect;
see `docs/codec/vaco-codec-mpeg12.md`'s own git history for the exact
`ffmpeg` invocations). `chroma_format == 3` (4:4:4) has no equivalent row
at all — see "Known gaps" for why that is a structural limit of this
project's tooling, not something left untried.

That said, **"reference-quality" is not "framemd5-identical"**: max-abs-
deviation of 2 means these are not byte-identical to the reference, and
T2-01a's own acceptance bar is explicitly framemd5 equality. This crate's
floating-point IDCT (Annex A specifies an accuracy bound, not a mandated
bit-exact algorithm — the same choice and the same caveat as
`vaco-codec-jpeg`'s own docs) cannot guarantee literal byte-identity
against a specific reference decoder's own rounding without reimplementing
that reference's exact integer IDCT, which is out of this session's scope.
This used to be stated as an assumption; a later session tested it
directly by black-box measurement (single-coefficient and multi-
coefficient bitstream probes against `ffmpeg -idct simple`, dequant
already tier-3 verified so any diff is IDCT-only) and confirmed it holds
— see `planning/TECH-DEBT.md`'s "The MPEG-2 framemd5 ceiling: measured,
not assumed" for the method and the decisive finding (two coefficients
that each alone produce a lone ±1 difference cancel to zero when combined,
proving the mismatch is a genuinely non-linear function of the whole
coefficient set, not a fixed per-basis-function bias). A follow-up round
asked whether closing that gap by continued measurement would converge
or diverge — it diverges (row/column separability fails, a magnitude
sweep relocates the error to unrelated pixels with no discoverable
pattern, and each probe's constraint yield is too small and unstable to
compose): see that same file's "The IDCT reverse-engineering-by-
measurement project: diverges" for the three checks. This is now a
permanent ceiling, not an open project.

**MPEG-1 remains genuinely wrong.** A later session re-examined this with
two techniques that were not available the first time: reading the
per-frame error curve's own *shape*, not just its average, and building a
matched MPEG-2 control fixture (same content, same GOP structure, same
dimensions) to isolate what is actually MPEG-1-specific.

That re-examination corrects the previous paragraph's own framing:
`m1_i`/`m2_i` and the rest of the "`_i`" fixtures are **not** intra-only —
`ffprobe` shows frame 0 as `I` and every frame after it as `P`. The
earlier "present from frame 0 of an intra-only fixture... ruling out
inter-prediction" claim was wrong on the "intra-only" premise, though its
conclusion (frame 0 is genuinely wrong on its own) still holds: frame 0's
own error (mean abs diff 0.38, max 9) is real and far larger than the
matched MPEG-2 control's own frame 0 (mean 0.01, max 1) — so there is a
real intra-decode-path difference, not only an inter-prediction one. But
the *growth* across the P-picture sequence is also far faster than the
control's own float-IDCT reference-chain creep (MPEG-1: mean 0.38 → 1.78
over 25 frames; matched MPEG-2 control: mean 0.01 → 0.06) — so whatever is
wrong is not confined to intra blocks either. Spatially, the error
concentrates in specific 8x8 blocks rather than every block equally (a
per-block heatmap on `m1_i`'s frame 0 shows many blocks at zero difference
and a few — plausibly the ones using escape-coded coefficients, given the
error's own rough correlation with content complexity — at the frame's
worst values).

Three hypotheses were tested this pass and eliminated:

- **The escape-level field's sign representation.** MPEG-1's escape
  syntax uses an 8-bit level field this crate's own comment previously
  (incorrectly) called "sign-magnitude" while the code beside it actually
  implemented two's complement — a real discrepancy between what the
  comment claimed and what shipped. Trying genuine sign-magnitude
  (matching the comment) measured *far* worse (avg MAD 209-224 across the
  three fixtures) than the existing two's-complement code (12.9-44.8) —
  so two's complement is confirmed, empirically, as the better of the two,
  and the misleading comment is fixed to say so plainly: this crate does
  not have legitimate access to ISO/IEC 11172-2's own normative text (see
  the removed `iso-11172-2` provenance placeholder, the same "looks
  registered but wasn't acquired" pattern as `iso-14496-2` for #360), so
  this one field's exact bit semantics were only ever knowable by
  differential testing, not by reading the standard.
- **Full-pel motion vectors** (`full_pel_forward_vector`, D.9.7) — a
  later round measured this directly (dumped the flag from every picture
  header in `m1_ip`/`m1_ipb`, 50 pictures total, `false` on every one) to
  test whether it explained the P-picture residual below before touching
  any code; it doesn't, on this corpus. Implemented anyway, as a small,
  separate correctness item (`macroblock::form_macroblock_prediction`
  doubles the relevant direction's motion vector before it addresses the
  reference picture) — genuinely missing before, confirmed a byte-exact
  no-op on all 15 fixtures this crate has ever measured, and covered only
  by a hand-crafted unit test since no fixture exercises it.
- **`macroblock_stuffing`** (D.9.2): MPEG-1's `"0000 0001 111"` VLC code,
  insertable any number of times before a `macroblock_address_increment`
  and required to be discarded, that MPEG-2 reserves and never emits.
  This crate did not implement it at all — a genuine, spec-required gap,
  now fixed (`macroblock.rs`, gated on `ap.mpeg1`) — but empirically makes
  no difference to any fixture on hand (avg MAD unchanged before/after),
  meaning ffmpeg's own MPEG-1 encoder does not emit this code for this
  content. Kept as a correctness fix regardless, since it is real,
  spec-required for MPEG-1, and cannot regress anything (MPEG-2 never
  matches the pattern being skipped).

Still holding from before: the dequantisation formula (hand-verified
coefficient-by-coefficient against §7.4.2.3 for a real macroblock —
matches exactly), both DCT-coefficient VLC tables (mechanically
re-extracted from the primary text independently of the existing
transcription and diffed — zero mismatches), and the linear
`quantiser_scale` table (cross-checked against Table 7-6 directly).

**The IDCT mismatch control question was re-derived, and the earlier
elimination was testing the wrong rule.** A later round found the two
variants tried previously — toggling every nonzero-even coefficient's LSB
by a uniform `+1`, and by a uniform `-1` — both misread Annex D.9.1's
"adding (or removing) one to each non-zero coefficient that would have
been even." Neither is what those six words describe as a single
per-coefficient operation: the direction is sign-dependent, `rec -=
sign(rec)` (move one step toward zero), so a uniform `+1` is only correct
for negative coefficients and a uniform `-1` only for positive ones —
each earlier attempt was right on roughly half of any block's
coefficients and wrong on the other half, indistinguishable from noise,
which is exactly why both measured worse than no MPEG-1 rule at all. The
correct rule, plus two compounding bugs the same pass found (MPEG-2's
`F[7][7]` sum-parity correction was being applied to MPEG-1 streams too,
which have no such concept; the correction must exempt an intra block's
own DC coefficient, reconstructed through `intra_dc_mult` rather than the
matrix/quantiser-scale formula the rule is stated against), is now
implemented (`block::dequantise`, gated on `ap.mpeg1`).

**This closes most, but not all, of the intra-decode gap, and none of the
P-picture gap.** `m1_i` (intra-only) improved substantially: mean abs diff
1.172 → 0.175 over the whole file (-85%), max 21 → 9 (-57%); frame 0 alone
dropped from 1166 to 189 differing pixels out of 4608 (-84%) — a real,
large effect, consistent with the heatmap correlation (error scaling with
coefficient count, absent on DC-only blocks) this fix was built to
explain. But `m1_i`'s own max deviation is still 9, not 0 — every one of
its 25 frames hits exactly that same value (see the table above), meaning
one more, smaller, consistently-reproduced defect remains even in pure
intra content. And `m1_ip`/`m1_ipb`'s P-picture max deviation (97) is
**completely unchanged**, before and after this fix, byte for byte — the
mismatch-control fix has no measurable effect once motion compensation is
in the picture, which means the earlier framing ("compounds further once
motion compensation is added on top" of the same underlying defect) does
not hold either: whatever produces the P-picture max-97 outlier is a
separate mechanism this fix does not touch at all, not a magnified version
of the intra-decode defect it does fix.

**So the actual cause is now partially found.** The intra-decode mismatch-
control defect is identified, fixed, and measured; a smaller residual
remains even there (unexplained); and the P-picture max-97 outlier is
completely unexplained and was not investigated further this round
(bounded round, per instruction) — seeing #355 close requires a new
hypothesis for that outlier specifically, not more work on mismatch
control. See `TECH-DEBT.md` for the fuller writeup.

**A follow-up round tested and killed the leading candidate for that
outlier cheaply**, exactly the way a one-measurement test is supposed to
work: full-pel motion vectors (D.9.7) are MPEG-1-only, affect only
motion-compensated pictures (matching the intra-clean/P-picture-broken
shape), and a halved vector produces large localised error — max-MAD-97
territory. Measured before touching any code: `false` on all 50 pictures
across `m1_ip`/`m1_ipb`. Dead on this corpus, at effectively no cost, and
implemented anyway as a genuine (if now-confirmed-irrelevant-here)
correctness gap. Two open threads remain, deliberately not both chased
in the same round: the P-picture max-97 outlier itself (candidates for
the next round: MPEG-1's own `motion_code`/`motion_r` reconstruction,
whose modular wraparound range differs from MPEG-2's `f_code`
derivation, or MPEG-1's half-pel interpolation rounding — not anything
already confirmed correct, which now includes dequantisation, both VLC
tables, mismatch control, and full-pel vectors); and `m1_i`'s own
residual max of 9, identical across all 25 frames of that fixture, which
looks deterministic and content-independent rather than data-dependent
and is a second, separate question from the P-picture one.

**A further round correlated the P-picture max-97 residual against motion
vector magnitude and parity, to discriminate between the two candidates
above — and eliminated both, while reframing the defect entirely.** Every
worst-error macroblock in `m1_ip`'s P-picture chain has `fwd=(0,0)`: no
motion vector, integer or fractional, at all. A wraparound bug needs a
vector near the `f_code` range limit; a half-pel rounding bug needs a
fractional component; zero has neither, so both candidates are wrong on
their own predicted shape. Tracing the exact macroblocks carrying the
outlier across all 25 frames shows why: the residual is flat at the
already-known intra ceiling for frames 0-14, jumps to 97/77 at frame
index 15 — this fixture's *second* I-picture, immediately after its one
repeated `sequence_header()` — and stays exactly flat through frame 24.
**"P-picture max-97" has been a mislabelling from the start**: the second
I-picture mis-decodes two macroblocks once, and every subsequent
P-picture macroblock at that position has zero motion vector and zero
residual, so it copies the wrong reconstruction forward unchanged rather
than re-deriving it. There is no P-picture-specific or motion-compensation
mechanism here to find.

Independently re-deriving the affected block's IDCT by hand (a textbook
2D 8-point IDCT-III, not this crate's own implementation) from its own
dumped, already-dequantised coefficients reproduces this crate's residual
exactly, ruling out `dequantise()`/`inverse_transform()` arithmetic
directly rather than by assumption — the defect is upstream, in how the
affected macroblocks' AC coefficient levels were entropy-decoded (both are
dense, near-full-spectrum blocks, ~58 of 64 AC coefficients non-zero,
consistent with a genuine sharp edge in the source content), for a reason
not yet identified. The motion-vector search space named above (`motion_code`/
`motion_r` wraparound, half-pel interpolation rounding) can be set aside
entirely for this residual; a further round would need a VLC/entropy-coding
hypothesis instead. One real candidate was raised and killed along the
way: whether `sequence_header()`'s intra/non-intra matrices should persist
across the fixture's repeated `sequence_header()` rather than reset to
default. Checked directly against H.262 §6.3.11 ("When a
sequence_header_code is decoded all matrices shall be reset to their
default values" — unconditional, every occurrence) and against this
fixture's own bitstream (both occurrences read `load_intra_quantiser_matrix
= 0`, so even the wrong reading would be a no-op here): the existing
reset-every-time implementation was already correct, recorded as a doc
comment rather than changed.

**A further round tested the leading VLC/entropy-coding candidate this
crate has: MPEG-1's escape-level 22-bit sentinel sub-case (Annex D.9.3),
the one `mpeg1`-gated branch upstream of dequantisation that specifically
handles large magnitudes — exactly what a dense, sharp-edge macroblock
produces.** An earlier round had ruled this out by usage count (fires
twice in the whole fixture); the reframing above (the defect is two
specific macroblocks, not something P-picture-wide) reopened the
question, since "fires twice" is no longer obviously too rare to matter.
Re-measured directly rather than reasoning from the old count: the
sentinel fires **9** times in `m1_ip.m1v`, not 2 — 2 at frame 0
(`mb=(3,1)`, both correctly reconstructed within the known small ceiling)
and **5** at frame 15 alone. Of those 5, exactly 2 are the known-broken
macroblocks (`mb=(3,1)`, `mb=(0,1)`) — but the other 3
(`mb=(0,2)` and `mb=(2,2)`, a different slice) fire the same mechanism,
at larger magnitudes (119-145 vs. 58-78), and reconstruct essentially
exactly (max diff 1). The sentinel path is exercised correctly in 7 of
its 9 occurrences in this fixture, including at the same magnitude order
in the very macroblock that is broken elsewhere. **This eliminates the
sentinel hypothesis**: if its byte-layout or sign interpretation were
wrong, every firing would misdecode, not 2 of 9 selectively. Sentinel
usage correlates with the defect (dense blocks need escape coding) without
causing it. Per instruction, `m1_i`'s own separate max-9 thread was not
chased this round, since the first hypothesis did not fall.

**A follow-up round used the one controlled pair this investigation has
never exploited: `mb=(3,1)` decoded at frame 0 (working) and frame 15
(broken) — same macroblock position, same code path, one correct output
and one wrong one.** Converting both frames' already-dumped, inverse-
scanned coefficients back to decode order shows the two blocks are
token-for-token structurally identical for every ordinary (non-escape)
coefficient: same zero/non-zero pattern, and each matching pair's
magnitude scales in a tight 1.33x-2.0x band (clustering almost exactly on
1.5x) between the two frames — exactly the frame 0:frame 15 quantiser-
scale ratio (6:4), consistent with the two pictures showing very similar
underlying content encoded at different precision. Checked across all
four defective sub-blocks (`mb=(3,1)` `i=1`/`i=3`, `mb=(0,1)` `i=0`/`i=2`,
16 ordinary-coefficient pairs total), every one of them lands in that
band. **Exactly one coefficient per block breaks it, every time: the
first AC coefficient, always the one decoded through the escape+sentinel
sub-case, whose frame15:frame0 ratio is 0.47-0.66 in all four blocks —
smaller than frame 0's value, the opposite direction the other 16 pairs
point.** This is where the two decodes first (and only) diverge from
their shared structure. The DC-predictor chain was checked at frame 15
specifically, not just assumed to carry over from frame 0's own
(previously validated) behaviour: all six of `mb=(3,1)`'s and all six of
`mb=(0,1)`'s reconstructed DC values are byte-identical between frame 0
and frame 15, ruling out predictor-state corruption at the picture that
actually shows the defect.

This sharpens, without simply repeating, the previous elimination:
`mb=(0,2)`/`mb=(2,2)`'s sentinel firings in the very same picture still
show no comparable anomaly against their own ground truth (exact match to
reference), so "the sentinel decode is unconditionally wrong" remains
false. What the paired comparison adds is a specific, four-times-
reproduced *symptom* — the escape+sentinel-decoded coefficient specifically
breaks a scaling relationship every other coefficient in the same blocks
obeys, always in the same direction — without a verified root cause: this
project has no legitimate access to ISO/IEC 11172-2's own text for the
sentinel sub-case's exact semantics, and guessing a replacement formula
without that access would repeat a pattern this investigation has already
paid for more than once. Recorded as the handoff point rather than acted
on. Not chased further this round, including `m1_i`'s own max-9 thread.

**A further round found the layout is not, in fact, unrecoverable without
the text: D6 permits deriving behaviour by black-box measurement, and a
byte-layout choice among a handful of concrete readings is a far better
target for it than the IDCT's unbounded rounding schedule was.** Built a
real, `ffmpeg`-encoded, `ffmpeg`-validated 16x16 MPEG-1 I-frame, patched
its one luma block's escape field to sweep the negative sentinel's
follow-up byte across the full 0-255 range (86 constructed streams, step
3), decoded each with real `ffmpeg`, and independently re-derived each
resulting pixel block from a textbook dequantise+IDCT given two candidate
levels. The shipped formula (`level = -follow_up_byte`) matched `ffmpeg`
only at the one point the two readings coincide (`follow_up_byte == 128`)
and diverged everywhere else, growing to a full-range (128) difference at
the low end — the same direction and rough scale as the gap this
investigation traced to this exact sentinel. `level = follow_up_byte -
256` (an extended two's-complement reading) matched `ffmpeg` at all 86
points, worst observed difference 1 — the same small ceiling every other
correctly-decoded case in this crate measures against. The positive
sentinel and the bare (non-sentinel) escape byte were swept too and
needed no change. Fixed (`5877247`), cited `blackbox` rather than
`iso-11172-2`, which this project does not hold and will not cite.

**Effect, measured directly:** `m1_i` max abs diff 9 → 2, `m1_ip` 97 → 2,
`m1_ipb` 97 → 2 — landing MPEG-1 on the exact same small ceiling every
MPEG-2 fixture already sits at (the measured, permanent IDCT-mismatch
ceiling this crate's own `TECH-DEBT.md` documents, not a new source of
error). This also answers the "are `m1_i`'s max-9 and the P-picture
outlier one mechanism or two" question raised alongside the pairing
technique: they were one mechanism the whole time, both intra defects,
both this same sentinel. Confirmed byte-identical, unaffected output on
every MPEG-2 fixture on hand (the `mpeg1`-gated branch this touches is
provably unreachable for `mpeg1 == false`; verified directly via a
pre-fix/post-fix binary comparison, not assumed from the gating alone).

**This means T2-01a's own "framemd5-identical to reference" acceptance bar
is not met for either format**, so no issue claiming it is closed by this
work — but MPEG-2 decode is now correct in every way this session's
differential testing can measure short of that literal bit-exactness bar,
and the round documented just above brings MPEG-1 to the identical state.

### #356 closed on a replacement bar; #355's own reason for staying open — and its later resolution

A later round proved (`planning/TECH-DEBT.md`'s two IDCT entries) that the
"framemd5-identical" bar above is not just unmet but **permanently
unreachable** for this crate without adopting a specific reference
decoder's own integer IDCT verbatim — measured by black-box bitstream
probing, not asserted. That finding forces a judgement on both open
issues carrying this bar, and they do not land the same way.

**#356 (T2-01b: 4:2:2, alternate scan, intra-VLC, all chroma formats)
closes on a replacement bar**, the same shape `vaco-mux-mxf`'s #609/#610
used for their own unreachable byte-identity criterion:

- **Reference-quality on every fixture that can be built.** The five
  fixtures this work package added (`m2_422`, `m2_422_ipb`, `m2_altscan`,
  `m2_intravlc`, `m2_422_alt_ivlc`) all sit in the same avg/max-MAD band
  the pre-existing 4:2:0 corpus already occupies (avg MAD 1.6-4.3, max MAD
  2-5 — see "Measured accuracy" above). The one fixture with a higher
  plateau (`m2_422_alt_ivlc`, max MAD 5 against every other fixture's 2)
  was checked per-frame rather than accepted at face value: it grows
  frame 0 (max 1) to frame 2 (max 5) and then holds flat through frame 9,
  the identical *shape* (fast growth, then plateau, smooth `mean` growth,
  no spikes) every other P-chain fixture in this crate shows at its own
  plateau value — combining three format changes at once shifts which
  coefficient patterns get exercised, and the proven-non-linear IDCT
  mismatch (see the TECH-DEBT entries) produces a different but equally
  bounded, equally smooth plateau. Not a new defect; the same one,
  landing slightly differently on different content.
- **Tier-3 verification on every table this round touched**:
  `TABLE_ZERO`/`TABLE_ONE`/`CODED_BLOCK_PATTERN`/`ALTERNATE_SCAN`, each
  checked line by line against two independent extractions of the primary
  text, zero mismatches.
- **Every scan/VLC combination the issue names is exercised**: zigzag and
  alternate scan, Table B.14 and B.15, each independently and combined,
  against real `ffmpeg`-encoded fixtures confirmed (via `ffprobe`) to
  actually carry the relevant flags.
- **4:4:4 is named as a real, structural, permanent limit, not hidden**:
  `ffmpeg`'s own `mpeg2video` encoder cannot produce `yuv444p` output at
  all, so this chroma format has zero possible differential-fixture
  coverage from this project's own tooling and never can — covered only
  by hand-crafted `ChromaFormat` unit tests, with the one apparent
  dimensional defect in H.262's own `coded_block_pattern_2` pseudocode
  implemented literally rather than guessed at (see "Known gaps").

Every part of that bar is met, so #356 is closed on it.

**#355 (T2-01a) was kept open, through many rounds, specifically because
the IDCT ceiling only accounted for part of its own gap** — this section
is left in place, unedited, as the record of that reasoning while it was
true; what follows is what changed.

- The **MPEG-2 portion** of #355's own corpus sits in the exact same
  reference-quality band as #356's (avg MAD 1.0-1.8, max MAD 2 across
  every MPEG-2 fixture — `m2_i`/`m2_ip`/`m2_ipb`/`m2_ilme`/`m2_oddsize`/
  `m2_qcif_ipb`/`m2_cif_ipb`). That part of #355's gap has been fully
  explained by the same measured, permanent IDCT ceiling #356 closes on
  since early in this investigation.
- The **MPEG-1 portion was not, for a long stretch of this investigation
  — but now is.** A middle round found and fixed the IDCT
  mismatch-control rule (implemented as MPEG-2's, not MPEG-1's own — see
  "MPEG-1 remains genuinely wrong" above), closing most of the
  intra-only gap (`m1_i`: max MAD 21 → 9) but leaving `m1_ip`/`m1_ipb`'s
  P-picture max MAD at 97, unchanged. A long correlation-and-elimination
  chase (motion vector magnitude and parity, a controlled same-macroblock
  pair decoded on both sides of a defect, four independently-reproduced
  sub-blocks) traced that 97 to a single cause: MPEG-1's escape-level
  22-bit sentinel sub-case's negative-magnitude formula, measured against
  a real decoder to be `level = -follow_up_byte` where it needed to be
  `level = follow_up_byte - 256` (the black-box measurement and fix are
  recorded above, in the paragraph following the controlled-pair
  localisation). Fixed. `m1_i`'s own separate max-9 residual turned out
  to be the identical mechanism, not a second one — both were this same
  sentinel, landing differently on intra-only versus P-chain content.
  Post-fix: `m1_i`/`m1_ip`/`m1_ipb` all measure max MAD 2, the identical
  ceiling every MPEG-2 fixture already sits at.

**Both formats now carry the same evidence #356 closed on** — but a
final round judged #355 directly against its own stated scope rather
than accepting that this settles it, and **#355 stays open, for a
different and narrower reason than any prior round gave.** The pixel-
accuracy question this whole investigation chased is resolved: frame-
picture decode, every chroma format, every extension #356 covers, all
sit on the same measured, permanent IDCT ceiling. But #355's own title
— "sequence/GOP/picture headers and extensions, MB layer over D-22,
**field and frame pictures, pulldown flags**" — names two things this
crate does not implement at all, checked directly in the source rather
than assumed from the ceiling closing everything else:

- **Field pictures** (`picture_structure != "Frame picture"`,
  `pce.is_frame_picture()` false): `decoder.rs::begin_picture` marks
  these `unsupported`, fills a flat mid-grey placeholder, and flags the
  frame `CORRUPT` (`Mpeg12Decoder::unsupported_pictures` counts them) —
  see "explicitly unimplemented decode paths" in `TECH-DEBT.md`. This is
  a missing decode path, not a precision question the IDCT ceiling could
  ever cover; no amount of rounding-schedule correctness touches a
  picture type this crate never attempts to reconstruct.
- **Pulldown flags**: `picture_coding_extension()` reads
  `repeat_first_field` and discards it immediately
  (`let _repeat_first_field = r.get(1);` — never stored on
  `PictureCodingExtension` at all); `top_field_first` is stored but never
  propagated to the output `Frame` (`FrameFlags::TOP_FIELD_FIRST` is
  never set — see that struct's own doc comment). Parsed for correct
  bitstream framing, not consumed for anything a caller could observe.

Both are real, checkable, and unaddressed by anything this session's
IDCT/escape-sentinel work touched — a close on "the numbers moved" would
be weaker evidence than #356's own closure, which covered its named
scope completely rather than partially. #355 stays open specifically and
only for field-picture decode and pulldown-flag propagation; every other
part of its stated scope (headers/extensions, the MB layer, frame-picture
reconstruction accuracy) is settled. Epic #36 does not close on this pair
either way — #357 (MPEG-1/2 encode) is a real, untouched gap, not a
ceiling, and nothing about this fix or this judgement touches it.

## How to change it

- **A wrong pixel value, not a crash**: suspect `block.rs` (coefficient
  decode/dequantisation/IDCT) or `motion.rs` (vector reconstruction/half-pel
  interpolation) before suspecting the VLC tables — most bugs found so far
  were about *bit consumption*, not table content — but do not assume a
  table is right just because it is prefix-free and covers every value:
  bug 4 above passed both those checks with a code one bit too long.
  Cross-check the *exact bit pattern*, length included, against the spec
  text, the way `coded_block_pattern_shortest_codes_are_exactly_9_bits`
  now does for the one row that bit us.
- **A picture or GOP going black/corrupt partway through**: hand-trace
  from the exact slice where corruption starts (`ActivePicture::slice_ok`
  going `false` is where to put a breakpoint or a temporary `eprintln!`) —
  the picture-wide/GOP-wide blast radius this used to have is fixed, so a
  new instance of this symptom should now be visibly narrower (one slice,
  maybe two) and easier to isolate to the exact failing VLC call than it
  was before that split existed.
- **The remaining MPEG-1-specific accuracy gap**: not a desync, not the
  coefficient VLC tables, not the dequantisation formula, not the escape-
  level sign convention, not full-pel vectors, not macroblock stuffing —
  see "Measured accuracy" above for what a later pass eliminated and the
  two techniques (per-frame error-curve shape; a matched MPEG-2 control
  fixture of the same content/GOP/dimensions) that did the eliminating.
  Next: since the error is present in a genuine I-picture but concentrated
  in specific blocks rather than uniform, compare per-coefficient
  dequantised values against a hand-computed reference specifically for
  one of `m1_i`'s *worst* blocks (per-8x8-block max-diff heatmap, not a
  block picked at random), since the interior/low-detail blocks already
  decode pixel-perfect.
- **A new picture type or extension**: header parsing lives entirely in
  `headers.rs`; wiring a newly-parsed field into decode means threading it
  through `ActivePicture` (`macroblock.rs`) the same way `mpeg1`/`pce` are.
- **Table content**: `tables.rs` is mechanically transcribed and Python-
  cross-verified from the spec text (see that file's own doc comment and
  `provenance/vaco-codec-mpeg12.toml`); regenerate rather than hand-edit if
  a value looks wrong, and re-run `tables::tests` (prefix-freedom and
  value-coverage) before trusting a change.

## Configuration

No feature flags beyond the registry's `codec-mpeg12` (`vaco-component.toml`
registers `mpeg1video`/`mpeg2video` decoders as `DECODER_MPEG1`/
`DECODER_MPEG2`). `Mpeg12Decoder::new` takes a `vaco_limits::Limits` for
allocation budgeting, same as every other decoder in this workspace.

## Dependencies

`vaco-core`, `vaco-bitstream` (bit reading, `annexb::find_start_code`),
`vaco-limits` (allocation budgeting), `vaco-codec-core` (`Decoder`/
`Machine`/`Caps` protocol), `vaco-codec-dsp-idct` (the `mpeg2` inverse DCT
module — not duplicated here), `vaco-pixfmt`, `vaco-packet`, `vaco-frame`.
Deliberately **not** a runtime dependency on `vaco-parse-mpegvideo`, despite
that crate existing for exactly this bitstream family — see `TECH-DEBT.md`
for the budget-accounting bug in its end-of-stream `flush()` that this
crate's own differential-test harness hit and routed around with a ~15-line
custom access-unit splitter (`examples/decode_dump.rs`) instead.

## Known gaps

Not implemented (each is a flat `CORRUPT`-flagged mid-grey placeholder
rather than wrong pixels, and counted in
`Mpeg12Decoder::unsupported_pictures`, except where noted): separate
field-coded pictures (only field prediction *within* a frame picture is
implemented — the common real-world interlaced case), dual-prime
prediction, 16x8 motion compensation, MPEG-1's full-pel motion vector
modes, and spatial/SNR/temporal scalability extensions (not parsed at
all). See `TECH-DEBT.md` for the open MPEG-1-specific accuracy gap
(present, smaller, in a genuine I-picture and growing faster than plain
float-IDCT drift across P-pictures — not a decode desync, not the
fixtures' GOP structure being intra-only, which it isn't), which is this
crate's remaining accuracy blocker now that MPEG-2 measures
reference-quality across every fixture on hand.

4:2:2/4:4:4 chroma sampling (T2-01b/#356) landed this pass — see
"Chroma formats: 4:2:0, 4:2:2, 4:4:4" below — but with one honest limit:
`chroma_format == 3` (4:4:4)'s `coded_block_pattern_2` extension is
implemented exactly as H.262's own §6.3.17.4 pseudocode publishes it, and
that pseudocode has what looks like a real dimensional defect (a 6-bit
fixed-length code that only ever drives 4 of the 12 `pattern_code[]`
entries — see `macroblock.rs`'s `decode_coded_macroblock`, the
`ChromaFormat::Yuv444` match arm, for the exact bit-shift accounting).
This crate has no way to tell whether that is genuinely how a real
encoder/decoder pair behaves, because `ffmpeg`'s own `mpeg2video` encoder
does not support `yuv444p` output at all (`ffmpeg -h encoder=mpeg2video`
lists only `yuv420p yuv422p`) — so 4:4:4 has zero differential-fixture
coverage and, structurally, never can from this project's own tooling.
4:2:0 and 4:2:2 both reach this crate's established reference-quality
baseline on real `ffmpeg`-encoded fixtures; see "Measured accuracy".
