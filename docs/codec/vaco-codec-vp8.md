# `vaco-codec-vp8`

Layer 4. VP8 video decode (RFC 6386), full pixel reconstruction — intra and
inter prediction, the normal and simple in-loop deblocking filters, golden
and altref frames. Builds on `vaco-parse-vpx` (uncompressed frame-tag and
key-frame dimension parsing) and `vaco-codec-msac` (the boolean entropy
engine).

## What it is

A from-scratch VP8 decoder implementing RFC 6386 end to end: the compressed
header (§9), segmentation (§10), key- and inter-frame macroblock prediction
modes (§11, §16), intra prediction (§12), DCT/WHT coefficient decoding
(§13), dequantization and the inverse transforms (§14), the in-loop
deblocking filter (§15), motion-vector decoding (§16.3-§17) and
motion-compensated inter prediction (§18). It registers a `Decoder` for
codec id `vp8` via `VP8_DECODER`.

| Module | Contents |
|---|---|
| `header` | `FrameHeader`, `Segmentation`, `LoopFilterDeltas`, `QuantIndices`, `EntropyContext` and `parse()` — everything in the compressed first partition before per-macroblock records begin |
| `tables` | RFC 6386's mode enums, trees and probability tables (large ones via `include!`) |
| `tokens` | §13's coefficient token tree, category extra-bits and zigzag scan, one 4x4 block at a time |
| `transform` | Dequantization factors, inverse WHT, inverse DCT, residue addition |
| `predict` | Pure 16x16/8x8/4x4 intra prediction functions (no plane access — `decode` locates the edge pixels) |
| `mv` | `vp8_find_near_mvs`, `mv_ref` probability derivation, SPLITMV partition/sub-MV decode |
| `interpolate` | The two-pass 6-tap/bilinear sub-pixel motion-compensation filter |
| `loopfilter` | The normal and simple deblocking filters as pure pixel-array functions |
| `framebuf` | `Plane`, `Picture`, `RefFrames` (last/golden/altref) and the refresh/copy rules |
| `decode` | The per-macroblock orchestrator tying all of the above together, plus the `Decoder` impl |

### What is deliberately not here

**Threading.** RFC 6386 allows multiple DCT token partitions specifically so
rows can be decoded in parallel; this decoder reads only the first token
partition regardless of `log2_nbr_of_dct_partitions`, so a stream with more
than one token partition will decode incorrectly. See `TECH-DEBT.md`.

**AV1/VP9.** `vaco-codec-msac`'s VP9 boolean engine and this crate's own
prediction/transform math are written so a VP9 base decoder can reuse the
non-VP8-specific pieces, but no VP9 decode lives here.

## How it works

### The two-partition structure, and the bug that came from missing it

A VP8 frame is not one bitstream. The **first partition** holds the
compressed header *and* every macroblock's mode/skip/segment-id/motion-vector
record for the whole frame; the **token partition(s)** hold only DCT
coefficient data, and start at a byte offset the header gives explicitly.
`decode_macroblock` therefore takes two separate `BoolDecoder`s — `bd`
(continuing right after the header, in the first partition) for mode/MV
records, and `token_bd` (positioned at the start of the residual partition)
for coefficient tokens — and reads from the right one for each field. Mixing
these up (using one decoder for both) still walks legal-looking trees and
produces syntactically valid-looking output; the earliest version of this
decoder did exactly that, and every macroblock's *mode* came out wrong while
still looking plausible, until a differential test against real encoder
output caught the corruption starting at macroblock zero.

### Every accuracy bug found here shared one shape

Four bugs were found and fixed while bringing this decoder to bit-exact
against `ffmpeg -c:v libvpx` reference output, and all four are worth
knowing about because the next one will probably look the same:

1. **`predict_and_write_16` folded the Y2 DC coefficient into a Y block's
   `coeffs[0]` without updating that block's `has_coeffs` flag.**
   `has_coeffs` is computed from the AC scan only (RFC 6386 §13.3's rule for
   the token-context "neighbour has coefficients" bookkeeping), which is
   correct for that purpose — but `write_residual_block` also uses the same
   flag to decide whether to run the inverse DCT at all. A block with an
   all-zero AC scan and a non-zero Y2-derived DC therefore got skipped
   entirely, dropping its DC contribution and reconstructing flat 128
   instead of the real value. This is exactly the kind of thing that a
   solid-colour or otherwise trivial test clip never exercises (every block
   is either all-zero or has real AC energy too), which is why it survived
   the first passing test.
2. **The identical bug, independently, in `reconstruct_inter`'s own Y2
   fold-in loop** — the intra and inter reconstruction paths duplicate this
   step rather than sharing it, so fixing one left the other broken. Found
   because a 20-frame inter-coded clip kept accumulating error frame over
   frame after the intra fix alone made key frames bit-exact.
3. **`predict::b_rd` (`B_RD_PRED`, the 45-degree southeast diagonal
   4x4 mode) computed its `E`-array tap index mirrored**: `c - r + 4` is
   what RFC 6386 §12.3 works out to, and the code had `(3 + r) - c` clamped
   into the same range — same domain, reversed direction, so it compiled,
   ran, and produced a plausible-looking (if wrong) diagonal gradient. The
   other five diagonal 4x4 modes (`b_ld`, `b_vr`, `b_vl`, `b_hd`, `b_hu`)
   were checked line-by-line against the same section and are correct;
   `b_rd` was the one transcription mistake among six similar ones.
4. **The loop filter's pixel-array index convention didn't match between
   `decode.rs` (the caller) and `loopfilter.rs` (the filter math).**
   `decode.rs` built the four "before the edge" pixels farthest-to-nearest
   (`[get(-4), get(-3), get(-2), get(-1)]`), but every function in
   `loopfilter.rs` — via its `common_adjust`/`high_edge_variance` calls on
   `p[0]`/`p[1]` — was written expecting nearest-to-farthest. Both sides
   were internally consistent and neither panicked; the filter just read
   and adjusted the wrong two pixels near every filtered edge. This
   produced small, content-correlated errors (a few pixels off by a handful
   of levels) that were easy to mistake for rounding noise, concentrated
   wherever the loop filter actually had work to do — i.e. everywhere,
   which is why it wasn't obvious from which macroblocks were affected.

The common thread: table/formula transcription mistakes and index-convention
mismatches between two pieces of code that both compile, both run, and both
produce *plausible* pixel data. None of them panicked, none of them desynced
the bitstream, and unit tests written against synthetic input (flat fields,
hand-built arrays) did not catch any of them, because synthetic input tends
to avoid exactly the cases these bugs mishandle. **The only thing that
caught all four was differential testing against real encoder output**,
frame by frame, down to which macroblocks disagreed and by how much — see
Verification below.

### A fifth bug: the reconstruction filter is chosen by frame version

RFC 6386 §9.1's version field selects between three *reconstruction*
filters — 6-tap ("bicubic") for version 0, bilinear for versions 1-2, and
"none" (motion vectors truncated to full-pel) for version 3 — independently
of the loop filter, whose type is a separate, explicit per-frame header bit
(the RFC states outright that the loop-filter implication of the version
number "has no effect whatsoever on the decoding process"). An early version
of this decoder hardcoded the 6-tap filter for every frame regardless of
`version`, which is silently correct for the overwhelmingly common
version-0 streams and silently wrong for anything encoded at version 1-3.
`decode::reconstruction_filter` now derives `(bilinear, full_pel)` from
`FrameHeader::version` and both `mc_block` call sites (luma and chroma) pass
it through.

## How to change it

- **Adding threading / multiple token partitions** — the two-`BoolDecoder`
  split in `decode_macroblock` already keeps mode/MV records and coefficient
  tokens on separate streams; the remaining work is picking the right token
  partition per macroblock row (`header.rs` already parses
  `log2_nbr_of_dct_partitions` and the partition-size table, see
  `FrameHeader`) rather than hardcoding partition 0.
- **Touching Y2 DC folding** — there are two copies
  (`decode::predict_and_write_16` for intra, `decode::reconstruct_inter`'s Y
  loop for inter). Fix both, or better, factor them into one function; see
  bug #2 above for why leaving one unfixed is a real, easy mistake.
- **Touching any intra 4x4 (`B_*_PRED`) mode** — re-derive the RFC 6386
  §12.3 pixel formula by hand and compare index-by-index rather than
  trusting that "it looks like a diagonal, so it's probably right"; see bug
  #3.
- **Touching the loop filter** — `loopfilter.rs`'s functions take `p`/`q` as
  `[u8; 4]` ordered nearest-edge-pixel-first (`p = [p0,p1,p2,p3]`,
  `q = [q0,q1,q2,q3]`); `decode.rs`'s `filter_vertical_edge`/
  `filter_horizontal_edge` must build and write back arrays in that same
  order. This convention was the source of bug #4 — get it backwards again
  and every filtered edge is subtly wrong in a way that looks like noise.
- **Gotcha: `Vec::with_capacity`/`reserve` are workspace-disallowed** for
  header-derived sizes; this crate uses `vaco_limits::Budget::alloc`
  wherever a frame's dimensions drive an allocation (`Picture::new`, the
  segment map).
- **Known-unverified pieces** (documented in `decode.rs`'s own module doc,
  and not yet contradicted by any differential test run so far, but not
  independently confirmed against RFC 6386 prose either):
  1. The loop-filter *mode* delta's index mapping (`mode_delta_index`) — RFC
     6386 §9.4/§10 don't spell out which of the four mode-delta slots
     applies to which macroblock mode in the primary prose; this crate uses
     the widely-documented convention (0 = `B_PRED`, 1 = `ZEROMV`, 2 = other
     inter modes, 3 = `SPLITMV`).
  2. Chroma motion-vector rounding (`round_div8`) — the four covering luma
     (eighth-pel) MV components are summed and divided by 8 with a
     symmetric round; every differential test run so far (including
     SPLITMV content) has come back bit-exact with this formula, but the
     exact rounding RFC 6386 specifies was not independently located in the
     primary spec text during this crate's extraction pass.

## Configuration

None. No features, no environment variables. `Vp8Decoder::new` takes a
`vaco_limits::Limits` the same way every other decoder in the workspace
does, bounding allocation for header-derived buffer sizes.

## Dependencies

| Crate | For |
|---|---|
| `vaco-parse-vpx` | the uncompressed frame tag and key-frame dimension parse this crate builds decode on top of |
| `vaco-codec-msac` | `Vp8BoolDecoder`, the RFC 6386 §7.3 boolean entropy engine |
| `vaco-codec-core` | the `Decoder` trait and `DecoderDesc` registration |
| `vaco-frame` / `vaco-pixfmt` | output frame and pixel-format types |
| `vaco-packet` | input compressed packets |
| `vaco-pool` | pooled buffer allocation for output frames |
| `vaco-limits` | allocation budgets for header-derived sizes |
| `vaco-core` / `vaco-bitstream` | shared error taxonomy and bit access primitives |

Dev-only: `proptest`. No external runtime dependencies.

## Verification

- 29 unit tests across `predict`, `tokens`, `transform`, `loopfilter`,
  `interpolate` and `mv` — panic-freedom proptests over arbitrary input for
  every module that touches untrusted bitstream data, plus hand-worked
  examples (e.g. `dc_pred_falls_back_to_128_at_the_top_left_corner`,
  `wht_dc_only_matches_the_fast_path_formula`) traced from the RFC's own
  formulas.
- `cargo clippy -p vaco-codec-vp8 --all-targets -- -D warnings` clean,
  including `indexing_slicing`/`cast_possible_wrap`/`integer_division`
  denied workspace-wide.
- **Differential testing against `ffmpeg -c:v libvpx` reference output**
  (rawvideo YUV 4:2:0), comparing every plane of every decoded frame
  byte-for-byte, is what found and confirmed the fix for all five bugs
  above; a decoder that merely doesn't panic on fuzzed input says nothing
  about whether it reconstructs the right pixels. Fixtures exercised, all
  bit-exact end to end after the fixes in this doc:

  | Fixture | Resolution | Frames | Content | Result |
  |---|---|---|---|---|
  | `key1.ivf` | 64x48 | 1 (key) | flat/simple | bit-exact |
  | `multi.ivf` | 176x144 | 20 (1 key + 19 inter) | `testsrc`, SPLITMV present | bit-exact |
  | `crop.ivf` | 100x70 | 15 | `testsrc2`, non-multiple-of-16 dimensions | bit-exact |
  | `altref.ivf` | 160x120 | 20 | `mandelbrot`, `-auto-alt-ref 1 -lag-in-frames 10` (golden/altref exercised) | bit-exact |
  | `simple.ivf` | 128x96 | 23 | `-profile:v 1` (bilinear MC + simple loop filter) | bit-exact |
  | `seg.ivf` | 120x90 | 18 | `-aq-mode 1` (segmentation exercised) | bit-exact |
  | `fullpel.ivf` | 96x64 | 15 | `-profile:v 3` (full-pel-only MC) | bit-exact |

  112 frames total, 0 non-bit-exact, across all four RFC 6386 version
  profiles (0-3), both key and inter frames, SPLITMV, golden/altref
  reference usage, segmentation and non-16-multiple cropping.

## Specification

RFC 6386, "VP8 Data Format and Decoding Guide" (`rfc-6386`). RFC 6386's own
embedded reference C code ("Attachment One" and the per-section listings)
was used only for pulling pure numeric constants not otherwise given in
prose (D7/D15's merger-doctrine allowance for format-dictated tables); no
algorithmic code structure was read from it. Where the RFC's normative
behaviour is *expressed only* as C pseudocode with no separate prose
description — most of §16's mode/motion-vector census algorithm, for
example — that pseudocode is, unavoidably, the specification text itself for
this format; it was translated to idiomatic Rust rather than transcribed,
and cross-checked line-by-line against a from-scratch Python re-implementation
used as an independent oracle during debugging.
