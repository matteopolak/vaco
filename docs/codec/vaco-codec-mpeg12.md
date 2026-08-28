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
| `m1_i` | 64x48 | MPEG-1, I only | 12.9 | 21 | 2.25 |
| `m1_ip` | 64x48 | MPEG-1, I+P | 44.8 | 97 | 6.85 |
| `m1_ipb` | 64x48 | MPEG-1, I+P+B | 44.2 | 97 | 6.60 |

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

That said, **"reference-quality" is not "framemd5-identical"**: max-abs-
deviation of 2 means these are not byte-identical to the reference, and
T2-01a's own acceptance bar is explicitly framemd5 equality. This crate's
floating-point IDCT (Annex A specifies an accuracy bound, not a mandated
bit-exact algorithm — the same choice and the same caveat as
`vaco-codec-jpeg`'s own docs) cannot guarantee literal byte-identity
against a specific reference decoder's own rounding without reimplementing
that reference's exact integer IDCT, which is out of this session's scope.

**MPEG-1 remains genuinely wrong**, and differently: the error is small,
diffuse across nearly every macroblock (not concentrated in one, unlike
the CBP bug above), present from frame 0 of an intra-only fixture (ruling
out any inter-prediction or reference-propagation explanation), and grows
with content complexity — not a bitstream desync, something closer to a
per-coefficient rounding or reconstruction difference specific to MPEG-1.
One concrete hypothesis was tested this session and eliminated: H.262
Annex D.9.1's description of ISO/IEC 11172-2's different IDCT mismatch
control (correcting every nonzero-even coefficient independently, instead
of MPEG-2's single sum-parity-conditional `F[7][7]` correction) was
implemented and measured *worse* than not implementing it at all, in both
possible correction directions (avg MAD rose from ~12-44 to ~24-51 across
the three MPEG-1 fixtures) — so this crate deliberately keeps applying
MPEG-2's rule unconditionally rather than a worse, unverified MPEG-1-
specific one. DC precision tables, the linear `quantiser_scale` row (the
only one MPEG-1 uses), and `intra_vlc_format`/escape-coding selection were
all re-checked correct. The actual cause is not yet found — see
`TECH-DEBT.md`.

**This means T2-01a's own "framemd5-identical to reference" acceptance bar
is not met for either format**, so no issue claiming it is closed by this
work — but MPEG-2 decode is now correct in every way this session's
differential testing can measure short of that literal bit-exactness bar.

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
- **The remaining MPEG-1-specific accuracy gap**: not a desync (see
  "Measured accuracy" above) — start by comparing per-coefficient
  dequantised values, not bit positions, against a hand-computed reference
  for one intra macroblock in `m1_i`, since the error is present from
  frame 0 of intra-only content and grows with coefficient count.
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
modes, 4:2:2/4:4:4 chroma sampling (this crate only ever allocates
`Yuv420p` frames — T2-01b/#356, not attempted this session), and
spatial/SNR/temporal scalability extensions (not parsed at all). See
`TECH-DEBT.md` for the open MPEG-1-specific accuracy gap (small, diffuse,
present from frame 0 of intra-only content — not a decode desync), which
is this crate's remaining accuracy blocker now that MPEG-2 measures
reference-quality across every fixture on hand.
