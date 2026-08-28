# `vaco-codec-vp9`

Layer 4. VP9 video decode (VP9 Bitstream & Decoding Process Specification
v0.6) — key and inter frames, single and compound reference prediction,
switchable sub-pel interpolation, and §8.8's in-loop deblocking filter, so
lossy content decodes bit-exactly rather than within a filter-shaped
tolerance. Builds on `vaco-codec-msac` (the shared VP8/VP9 boolean entropy
engine) and `vaco-codec-dsp-idct` (the shared DCT/ADST/WHT transform math).
Profiles 1-3 and threading remain out of scope — see "What is deliberately
not here" below.

## What it is

A from-scratch VP9 decoder covering C-29/C-30 (headers, superframes, the
boolean decoder, the probability model, intra prediction and transforms) and
C-31 (inter prediction): the uncompressed header (§6.2), the compressed
header's forward probability update (§6.3), Annex B superframe splitting,
the partition/mode-info bitstream walk (§6.4) for both intra and inter
frames, motion-vector prediction and the candidate scan (§6.5), coefficient
token decode (§6.4.24-26, §9.3.2's Pareto-table probability expansion),
dequantization and the inverse transform (§8.6-8.7: DCT4/8/16/32, ADST4/8/16,
the lossless Walsh-Hadamard transform), all ten §8.5.1 intra prediction
modes, §8.5.2's inter prediction process (reference-frame selection and
scaling, motion-vector clamping, the two-pass 8-tap sub-pel filter,
compound-reference averaging), and §8.8's in-loop deblocking filter (level
and sharpness derivation, segment and reference-frame level deltas, all
three filter widths, and the vertical-then-horizontal, superblock-raster
filter ordering the spec's own NOTE says later edges' correctness depends
on). It registers a `Decoder` for codec id `vp9` via `VP9_DECODER`.

| Module | Contents |
|---|---|
| `header` | `FrameHeader`, `ColorConfig`, `LoopFilterParams`, `QuantParams`, `Segmentation`, `TileInfo`, `EntropyContext`, `parse_uncompressed_header`/`parse_compressed_header` (now covering the inter-frame compressed-header fields: `reference_mode`, `inter_mode_probs`, `interp_filter_probs`, `is_inter_prob`, `comp_mode`/`comp_ref`/`single_ref_prob`, `mv_probs`) and the `diff_update_prob`/`decode_term_subexp`/`inv_remap_prob` forward-update machinery |
| `tables` | The spec's constants, trees and large data tables (`kf_*_probs`, `default_coef_probs`, `pareto_table`, the scan tables, `dc_qlookup`/`ac_qlookup`, `mv_ref_blocks`, `subpel_filters`, the inter-mode/interp-filter/mv trees, the 18 new `default_*` inter probability tables) — the large ones via `include!` from `tables/*.in`, extracted from the spec PDF and shape/count-validated separately from this crate |
| `tokens` | §6.4.24's `tokens()`/`read_coef`, §9.3.2's `pareto`/neighbour-context derivation, indexed by `is_inter` as well as tx size/plane/band/context |
| `predict` | §8.5.1's ten intra prediction modes, one implementation per mode generic over block size (unlike VP8, which has separate 16x16/8x8/4x4 formulas) |
| `loopfilter` | §8.8's in-loop deblocking filter: `frame_init`'s per-segment/ref/mode level lookup, the superblock/edge iteration, filter-size selection, the adaptive-strength (`limit`/`blimit`/`thresh`) derivation, and the mask/narrow/wide sample-filtering processes |
| `mvpred` | §6.5's motion-vector prediction: `find_mv_refs`'s spatial+temporal candidate scan, `find_best_ref_mvs`, `append_sub8x8_mvs`, `read_mv`/`read_mv_component` |
| `interpredict` | §8.5.2's inter prediction process: motion-vector selection (with chroma sub-block averaging), clamping, reference-frame scaling, and the two-pass 8-tap sub-pel convolution with compound averaging |
| `refframe` | The 8-slot reference-frame store (`RefFrameStore`/`RefSlot`), `Arc<Picture>`-shared since a typical GOP refreshes only one or two slots per frame |
| `transform` | §8.6's dequantization and reconstruct process, built on `vaco_codec_dsp_idct::vp9` |
| `superframe` | Annex B's superframe index parsing, returning every sub-frame (not just the display-relevant last one — see below) |
| `framebuf` | `Plane` (`u16`-backed, holds 8/10/12-bit samples), `Picture` |
| `decode` | The per-superblock orchestrator (`decode_partition`/`decode_block`/`intra_frame_mode_info`/`inter_frame_mode_info`/`residual`) plus the `Decoder` impl, reference-frame refresh, and hidden-frame (non-shown alt-ref) decode |

The inverse transform math itself (`TxType`, `inverse_transform_2d`, the
butterfly network) lives in `vaco_codec_dsp_idct::vp9`, not in this crate —
D19's shared-kernel convention: a pure function of already-dequantised
coefficients with no VP9-specific context belongs in the shared signal
crate once, alongside the existing h264/hevc/mpeg2 transform modules, rather
than being re-derived independently by a future AV1 decoder that shares
VP9's DCT/ADST family almost exactly.

### What is deliberately not here

**Profiles 1-3, threading (the rest of epic #32).** §8.8's in-loop
deblocking filter is now applied (C-32a) — see "How it works" and
"Verification" below. Profiles 1/3 (independent chroma subsampling, more
pixel format combinations) and 2/3 (10/12-bit) are parsed for totality but
not exercised by any fixture this crate's scope can reach; multi-tile-column
decode has a known `AvailL` simplification (see `planning/TECH-DEBT.md`).

**A pre-existing, unrelated decode bug in lossy content with extreme or
smoothly-varying coefficient values (§8.6's dequantization / §6.4.24's
token decode / diagonal intra prediction modes — not yet root-caused).**
Found while building C-32a's verification corpus, not by this phase's own
work: a lossy-encoded `mandelbrot`-sourced key frame (fractal content,
smooth continuous gradients, `ffmpeg -f lavfi -i "mandelbrot=size=64x64"
-c:v libvpx-vp9 -crf 15"`) decodes catastrophically wrong (mean absolute
deviation ~50 out of 255, not a small tolerance) even though the identical
content losslessly encoded has been bit-exact since Phase B. Confirmed by
direct bisection to be **unrelated to the loop filter and to inter
prediction**: it reproduces identically with the loop filter call removed
entirely, and it reproduces on a single 64x64 key frame with no inter
prediction involved at all. The same failure mode (smaller in magnitude,
but still a real, non-tolerance divergence) also appears in AQ-mode
(`-aq-mode 1`/`-aq-mode 2`, i.e. per-segment quantizer deltas) content after
a few frames, which points at dequantization/coefficient handling under an
unusual quantizer value or magnitude range rather than at mandelbrot's
pixel content specifically. Flagged as a separate task rather than fixed
here, since it sits in C-29/C-30's scope (intra decode), not C-32a's — see
`planning/TECH-DEBT.md`.

**Backward probability adaptation (§8.3/8.4).** Not implemented. Unlike
Phase B (key frames only, where `setup_past_independence()` resets
`EntropyContext` before every frame's own forward update, making backward
adaptation provably inert), this is now a real, live gap: `refresh_probs()`
should fold this frame's own observed symbol counts into the loaded context
before saving it (`if (!error_resilient_mode && !frame_parallel_decoding_mode)
{ load_probs(...); adapt_coef_probs(); ...adapt_noncoef_probs(); }`), and
this crate instead saves the forward-updated `entropy` back to
`frame_contexts[frame_context_idx]` verbatim, with no counting/adaptation
step. Every fixture verified below still decodes bit-exact because forward
updates alone are enough to track the tested content's actual probabilities
— but a longer real-world GOP whose encoder relies on backward adaptation to
converge (rather than repeating full forward updates every frame) is exactly
the case this gap would surface in. See `planning/TECH-DEBT.md`.

## How it works

### Phase B: the `inv_remap_prob` bug, and why it looked like a bitstream desync

The one real, subtle bug found while bringing this decoder to bit-exact:
§6.3.5's `inv_remap_prob` decrements its `prob` argument (`m--`) immediately
after reading it, *before* the `(m<<1) <= 255` branch test and both
`inv_recenter_nonneg` calls that follow. The first transcription read `prob`
directly into `m` and used it unchanged — `prob` and the correct `m` differ
by exactly one, easy to lose sight of when the two look interchangeable in
the surrounding arithmetic.

`inv_remap_prob` is the last step of `diff_update_prob`, the single shared
mechanism behind every forward-updated probability table
(`coef_probs`/`skip_prob`/`tx_probs`). Its bug is invisible for any
probability the compressed header never actually updates (`update_prob ==
0` skips the function entirely) and for content whose only update happens
to land on a value the off-by-one doesn't move across the branch boundary.
That is exactly why a trivial or flat test clip decoded perfectly while
real content did not: a flat 64x64 frame's compressed header updates few or
no coefficient probabilities, so every block reads defaults; real
(`testsrc`-style) content updates dozens of table entries, and every one of
them came out wrong. Once a wrong probability feeds a boolean/range decode,
the decoder's internal range and value state desyncs from what the true
encoder assumed — every symbol read after that point is consistent-looking
noise, not an obviously-corrupt value — which is why the visible symptom
was "block zero decodes bit-exact, block one onward is unrecoverable
garbage" and looked exactly like a context-derivation or bitstream-position
bug for most of the investigation. Isolating it took cross-checking every
other candidate (partition context, mode-info context, the token/neighbour
derivation, the full DCT/ADST/WHT butterfly network, dequantization) line
by line against the spec text and finding all of them correct, then
hand-deriving the expected Walsh-Hadamard reconstruction of a lossless
fixture's actual transmitted coefficients and confirming the *decoder's own
arithmetic* was internally consistent with its output — which meant the
coefficients themselves, not the transform, had to be wrong, which meant
the entropy decode had desynced, which meant a probability was wrong, which
meant a *forward update* was wrong, which is the one piece of machinery
every subsequent block depends on and block zero (mostly untouched
defaults) does not.

A second, smaller bug in the same investigation: `get_uv_tx_size` hardcoded
`subsampling_x`/`subsampling_y` as `(false, false)` when computing the
chroma plane's block size, instead of using the frame's actual color
config. For 4:2:0 content this selected the wrong (too-large) chroma
transform size, corrupting the "more_coefs" probability context for every
chroma block from the very first frame onward — caught the same way, by
comparing traced `tx_sz` values against a hand-computation of
`get_plane_block_size`.

### Phase C: four real bugs, all invisible to a first-block-only check

Bringing inter prediction to bit-exact needed four independent fixes, each
with the same shape as the Phase B bug above: individually-correct-looking
code that only misbehaves once a *second* real block, superblock, or
reference type is actually exercised — so a fixture with only one inter
block, or only one interpolation filter, or only single-reference content,
would have shipped every one of these silently.

1. **`tokens::coef_row` always read the `is_inter = 0` (intra) half of
   `coef_probs`.** §8.6.2/9.3.2's coefficient probabilities are indexed by
   `is_inter` (`REF_TYPES`, 2 entries) as well as tx size/plane/band/context
   — an inter block reads an entirely different probability table from an
   intra block at the same position. The Phase B code hardcoded index 0 with
   a comment noting "is_inter always 0 for a key frame", true at the time
   and silently wrong the moment a real inter block called it. Symptom:
   every inter block's residual decoded to plausible-looking but wrong
   coefficients — not a crash, not an obviously-corrupt value, just the
   wrong picture.
2. **`decode_partition` always read `KF_PARTITION_PROBS`.** §9.3.2's own
   prose has a well-known erratum here — "If FrameIsIntra is equal to 0, the
   probability is given by `kf_partition_probs`" is inverted from the actual
   (libvpx-matching) behaviour, which is "a key frame reads the fixed
   `kf_partition_probs`; every other frame reads the adaptive,
   forward-updatable `partition_probs`". The Phase B code, correctly
   decoding only key frames, never had reason to notice the inversion.
   Symptom: a frame's first superblock (whose partition context, `pctx`, is
   often the all-zero fallback where the two tables happen to be close
   enough to still decode the right shape) looked fine; the second
   superblock in a row — the first one whose context differs — read the
   wrong probability and desynced everything after it.
3. **`parse_compressed_header` called `read_interp_filter_probs`
   unconditionally.** §6.3's `compressed_header()` syntax guards this call
   with `if (interpolation_filter == SWITCHABLE)` — plenty of real encoder
   output fixes one filter for a frame's whole duration and never writes
   this table's update bits at all. Reading it unconditionally consumes
   bits the encoder never wrote, which does not corrupt *this* read (the
   probabilities it produces are simply garbage and never used) but shifts
   every subsequent read in the *same* compressed header — `is_inter_prob`,
   `frame_reference_mode`, `frame_reference_mode_probs`, `y_mode_probs`,
   `partition_probs`, `mv_probs` — off by however many bits the phantom read
   consumed. This was the deepest of the four: every one of those tables'
   own context-derivation formulas and default values checked out correct
   against the spec text in isolation, because the bug was never in what
   they computed, only in where in the bitstream they started reading from.
4. **`clamp_mv`/`clamp_mv_row`/`clamp_mv_col` clamped `MiRows - bh - MiRow`
   (and the column equivalent) to zero before it could go negative.**
   §6.5.4/6.5.5 and §8.5.2.2 compute this as a genuinely signed quantity —
   negative for any block whose nominal size overhangs the frame edge, which
   is the common case whenever a frame's height or width is not an exact
   multiple of the block-size grid (this crate's own 176x144 fixtures:
   `mi_rows = 18` is not a multiple of 8). A `saturating_sub` chain
   (`mi_rows.saturating_sub(bh).saturating_sub(mi_row)`) silently narrows
   the legal motion-vector clamp range for every boundary block, which only
   ever showed up as a handful of wrong pixels in the last superblock row or
   column of a frame — easy to miss next to the far larger errors bugs 1-3
   were producing at the time, and the last of the four to be isolated once
   the rest of a frame was already bit-exact.

Isolating each one used the same method as the `inv_remap_prob` bug: a
localized, per-8x8-block deviation map (`VP9CHECK_LOCALIZE`) plus a
per-pixel window dump (`VP9CHECK_WINDOW`) against the real reference YUV,
narrowing "which block, which plane, which pixel" until the pattern (a
sharp all-or-nothing boundary for bugs 1-3; a small, edge-confined deviation
for bug 4) pointed at a specific syntax element, then checking that
element's exact spec text word-for-word rather than trusting the existing
transcription.

### Phase C-32a: a spec erratum worth remembering beyond this crate

The loop filter itself needed no bug fixes once transcribed carefully
against the spec text — every fixture in the Verification table below
passed on the corpus's first real run. The one thing worth recording here
is a **second confirmed erratum in the primary spec text**, found and
worked around the same way as C-31's `kf_partition_probs` inversion:

§8.8.1's own ordered steps read, verbatim:
> 3. If `loop_filter_delta_update` is equal to 0, then `LvlLookup[...]` is
>    set equal to `lvlSeg` for every `ref`/`mode`.
> 4. If `loop_filter_delta_enabled` is equal to 1, then \[the per-ref/mode
>    delta formula runs\].

Read literally and independently, these two conditions look like they
could both fire in the same frame (deltas enabled, but not refreshed this
frame — `loop_filter_ref_deltas`/`loop_filter_mode_deltas` persisting from
an earlier frame is completely ordinary). It turns out this does not
matter: step 4 runs *after* step 3 and unconditionally overwrites every
entry step 3 just wrote whenever `delta_enabled` is 1, so the two steps are
only ever *observably* different when `delta_enabled` is 0 — and
`delta_update` is only ever read from the bitstream (hence only ever
nonzero) when `delta_enabled` is 1. A plain `if delta_enabled { step 4 }
else { step 3 }` is therefore provably equivalent to transcribing both
ordered steps literally; `loopfilter::frame_init` implements exactly that,
with the derivation spelled out in its own doc comment so the next person
reading §8.8.1 does not have to re-derive it (or worse, "fix" what looks
like a redundant condition into something subtly wrong).

This is not the §9.3.2 partition-probs erratum from C-31 (that one really
was inverted — the spec's condition and the correct behaviour are
opposites). This one is not wrong, just written in a way that reads as
ambiguous or contradictory until worked through by hand. **The general
lesson for this project holds regardless: this spec's own algorithmic
pseudocode is the authority by convention, and it is still worth
independently verifying any place two of its ordered steps look like they
could conflict, because the primary text has already been wrong once and
confusing at least twice.**

### Superframes: every sub-frame, not just the last

`vaco-parse-vpx::superframe::last_subframe` (used by the format layer for
container-visible metadata) only needs the *last* sub-frame of a
superframe. A decoder cannot do that: a superframe's leading entries are
typically hidden alt-ref frames that become reference frames for the
visible frame that follows, so `crate::superframe::split` returns every
sub-frame's byte range in bitstream order. Layering also rules out reusing
`vaco-bsf-vpx::superframe_split` directly (a codec-layer crate cannot depend
on a format-layer one), so this module reimplements the index-parsing logic
independently rather than sharing it.

## How to change it

- **Adding backward probability adaptation (§8.3/8.4):** `refresh_probs()`'s
  gap (see "What is deliberately not here") is the natural next piece —
  `decode_one_frame` would need to count symbol occurrences during
  `decode_frame_tiles` (a `Counts` struct paralleling `EntropyContext`'s own
  shape) and fold them into the *loaded* context, not the forward-updated
  one, before `save_probs`.
- **Touching any inter-frame compressed-header field:** re-read §6.3's exact
  `compressed_header()` call order before adding or reordering anything —
  bug 3 above (an unconditional `read_interp_filter_probs`) shows how one
  wrongly-guarded call corrupts every *later* read in the same header while
  leaving every earlier one, and every later one's own formula, looking
  correct in isolation.
- **Touching partition or motion-vector-clamp arithmetic:** bugs 2 and 4
  above are both "this looks like a saturating/simplifying transformation of
  the spec's formula, and it is not" — `MiRows - bh - MiRow` (and the
  column equivalent) must stay signed, and `kf_partition_probs` vs.
  `partition_probs` selection must key off `FrameIsIntra`, not be hardcoded.
- **Touching the loop filter:** `loopfilter::filter_frame` runs at the end
  of `decode::decode_frame_tiles`, after every superblock's tile decode has
  finished and before the picture is returned (spec-mandated: reference
  frames must store the *filtered* picture). It takes a `loopfilter::Grid`
  built from `decode.rs`'s own `MiCell` array via a small
  `loopfilter::MiInfo` per cell (deliberately not `MiCell` itself, which
  also carries motion vectors/interpolation filter this process never
  reads) — extend that mapping, not `MiCell`'s shape, if a future syntax
  element needs a new field here. The superblock/plane/pass iteration order
  in `filter_frame` is spec-mandated (see the module doc's NOTE about later
  edges depending on earlier ones); do not reorder it for tidiness.
- **Adding backward-adapted (not just per-frame-forward) loop filter
  behaviour:** none exists — `LvlLookup` is recomputed from scratch every
  frame via `frame_init`, which is correct per spec (§8.8.1 has no
  persistence across frames of its own; only `loop_filter_ref_deltas`/
  `loop_filter_mode_deltas` persist, and those already flow through
  `LoopFilterParams`, which the frame header machinery threads
  frame-to-frame).
- **Extending to profiles 1-3:** `header::color_config` already parses
  profile-dependent subsampling and 10/12-bit `bit_depth`; `pic_to_frame`'s
  pixel-format match already has stub arms for `yuv422p`/`yuv444p`/`yuv440p`
  and their `Nle` variants. The gap is entirely in `predict`/`transform`
  never having been exercised at those bit depths or subsampling ratios,
  not in missing syntax.
- **Gotcha:** any new forward-updated probability table must go through
  `header::diff_update_prob`/`inv_remap_prob` — see the bug above before
  touching either.

## Configuration

No env vars or flags. `Vp9Decoder::new(limits: vaco_limits::Limits)` bounds
all allocation (picture planes, per-superblock context arrays) through
`vaco_limits::Budget`.

## Dependencies

| Crate | For |
|---|---|
| `vaco-codec-msac` | `Vp9BoolDecoder`, shared with `vaco-codec-vp8` |
| `vaco-codec-dsp-idct` | `vp9::{TxType, inverse_transform_2d}`, the shared transform math |
| `vaco-parse-vpx` | investigated for reuse (superframe/header parsing) but not depended on for decode logic — see "Relationship to other VP9 work" below |
| `vaco-codec-core` | the `Decoder` trait, `DecoderDesc` registration, the `Machine<Frame>` send/receive state machine |
| `vaco-frame` / `vaco-pixfmt` | output frame and pixel-format types |
| `vaco-packet` | input compressed packets |
| `vaco-pool` | pooled buffer allocation |
| `vaco-limits` | allocation budgets for header-derived sizes |
| `vaco-core` / `vaco-bitstream` | shared error taxonomy and bit access primitives |

Dev-only: `proptest`. No external runtime dependencies.

## Relationship to other VP9 work

- **`vaco-parse-vpx::vp9`** parses VP9's uncompressed header only as far as
  the format layer needs it (container-level dimensions, profile,
  show-frame flags) and stops before loop filter/quant/segmentation/tile
  info. This crate's `header.rs` implements the *full* uncompressed and
  compressed header syntax independently rather than extending that
  module, since the two live in different layers (format vs. codec) and
  this crate needs fields `vaco-parse-vpx` has no reason to carry.
- **#265 (D-21a, `vaco-cbs-vp9`)** targets a from-scratch crate for
  byte-identical VP9 bitstream round-trip read/write (a "CBS"-style parser
  for muxer/bitstream-filter use — inspect and rewrite a stream without
  fully decoding it). That is a different problem from this crate's:
  decoding to pixels never needs to re-serialize what it read, and a
  round-trip parser never needs prediction, dequantization, or a transform.
  **This crate does not subsume #265 and sits beside it, not on top of
  it** — the two would only plausibly share the low-level bit/bool-reading
  primitives (`vaco-codec-msac`), which already exist independently of
  both.

## Verification

- 16 unit tests across `predict`, `superframe`, `header` and `loopfilter` —
  panic-freedom checks for every intra mode/size combination, superframe
  index parsing (including malformed input), a hand-computed regression
  test for the `inv_remap_prob` bug above
  (`inv_remap_prob_decrements_m_before_use`), and 7 hand-derived tests for
  `loopfilter::frame_init`/`adaptive_strength`/`filter_size` covering the
  `SEG_LVL_ALT_L` absolute/delta/clamp cases, the `delta_enabled`
  true/false split (see "Phase C-32a" above), the `sharpness` clip branch,
  and the chroma-right-edge `filterSize` demotion.
- 6 unit tests in `vaco_codec_dsp_idct::vp9` for the shared transform math
  (DC-only DCT flatness, all-zero-in/all-zero-out, `cos64`/`brev` known
  values, panic-freedom at `i32::MIN`/`MAX` magnitude coefficients).
- `cargo clippy -p vaco-codec-vp9 --all-targets -- -D warnings` and
  `cargo clippy -p vaco-codec-dsp-idct --all-targets -- -D warnings` both
  clean, including `indexing_slicing`/`cast_possible_wrap`/
  `integer_division`/`unwrap_used`/`expect_used` denied workspace-wide
  (the last two apply to test code too — every hand-derived test above
  reads its expected values through `.get()`, not `[]` or `.unwrap()`).
- Fuzzed via `fuzz_targets/vp9_decode.rs` (`cargo +nightly fuzz run
  vp9_decode --no-default-features --features codec-vp9 -- -max_total_time=30`),
  no crashes; the loop filter runs on essentially every parseable frame the
  fuzzer's inherited corpus produces (any 6-bit `loop_filter_level` field
  other than exactly 0), so it is exercised on attacker-controlled
  levels/deltas by construction, not just by the differential fixtures
  below.
- **Differential testing against `ffmpeg -c:v libvpx-vp9` reference
  output** (rawvideo YUV 4:2:0), comparing every plane of every decoded
  frame byte-for-byte. Fixtures generated with `-bitexact` (encoder side)
  and decoded with `ffmpeg -bitexact` (reference side) to keep both the
  codec's own encoder metadata and the comparison itself byte-deterministic.
  **Every row below is now bit-exact** — C-32a's loop filter closes the gap
  every lossy row in this table used to carry ("MAD 0.0x, max deviation N,
  loop filter, not this crate's scope"):

  | Fixture | Resolution | Frames | Content | Loop filter | Result |
  |---|---|---|---|---|---|
  | flat, lossless | 64x64 | 1 (key) | solid color | level 0 | bit-exact |
  | flat, lossy | 64x64 | 1 (key) | solid color | level 0 | bit-exact |
  | gradient, lossless | 64x64 | 1 (key) | `gradients` | level 0 | bit-exact |
  | gradient, lossy | 64x64 | 1 (key) | `gradients` | level 8 | **bit-exact** (was: MAD 0.03/max dev 3, loop-filter tolerance) |
  | `testsrc`, lossless | 64x64 | 1 (key) | `testsrc` | level 0 | bit-exact |
  | `testsrc`, lossy | 64x64 | 1 (key) | `testsrc` | level 6 | **bit-exact** (was: MAD 0.02/max dev 3) |
  | non-multiple-of-8/64 | 130x98 | 1 (key) | `testsrc`, lossless | level 0 | bit-exact |
  | larger frame | 256x256 | 1 (key) | `testsrc2`, lossless | level 0 | bit-exact |
  | complex content | 192x192 | 1 (key) | `mandelbrot`, lossless | level 0 | bit-exact |
  | noisy content | 96x96 | 1 (key) | `testsrc` + heavy noise, lossless | level 0 | bit-exact |
  | multi-frame GOP (all key) | 176x144 | 5 (all key) | `testsrc`, lossless | level 0 | 5/5 bit-exact |
  | real inter GOP, lossless | 176x144 | 15 (1 key + 14 inter) | `testsrc2`, `-lossless 1` | level 0 | 15/15 bit-exact |
  | mixed key/inter, lossy | 176x144 | 8 (1 key + 7 inter) | `testsrc`, `-auto-alt-ref` | level >0, growing 3→12 | **bit-exact** (was: loop-filter tolerance) |
  | compound prediction, lossy | 176x144 | 30 | `testsrc2`, `-auto-alt-ref 1 -lag-in-frames 25` (2-pass), `-crf 20` | level >0, switchable filter, `REFERENCE_MODE_SELECT` w/ real compound blocks (`comp_fixed_ref=ALTREF_FRAME`) | **bit-exact** (was: MAD ≤0.08, max dev 2-7) |
  | switchable interpolation filter | 176x144 | 10-15 (various) | `testsrc2`, several `-speed`/`-crf` combinations, `interpolation_filter == SWITCHABLE` confirmed via header trace | level >0 | **bit-exact** (was: MAD ≤0.2, max dev 2-24) |
  | slow pan, real motion | 176x144 | 15 | `testsrc2` cropped with a per-frame moving origin | level >0 | **bit-exact** (was: MAD ≤0.14, max dev 2-8) |
  | busy content, non-64-multiple, lossy inter GOP | 130x98 | 15 (1 key + 14 inter) | `testsrc2`, `-crf 24` | level >0 | bit-exact |
  | noisy content, lossy inter GOP | 176x144 | 15 (1 key + 14 inter) | `testsrc2` + heavy noise, `-crf 10` | level >0 | bit-exact |

  **The C-32a-specific corpus** the coordinator's dispatch asked for by
  name — busy content, non-multiple-of-64 dimensions, segmentation — and
  what each one actually found:

  - **Busy content, non-64-multiple dimensions:** the "busy content,
    non-64-multiple" and "noisy content, lossy inter GOP" rows above. Both
    bit-exact.
  - **Segmentation enabled (`-aq-mode 1`/`-aq-mode 2`):** the loop filter's
    own segment-dependent behaviour (`LvlLookup`'s segment dimension) is
    confirmed correct for as long as the underlying decode stays correct —
    frames 0 and 1 of an `-aq-mode 1`, 15-frame `testsrc2` inter GOP are
    bit-exact, matching every other lossy inter GOP in this table. Frames 2
    onward diverge (MAD up to 0.33, max deviation up to 60) — **but this
    reproduces identically with the loop filter call removed entirely**,
    so it is the pre-existing, unrelated dequantization bug described in
    "What is deliberately not here" above (AQ's per-segment quantizer
    deltas are the second, independent way found this phase of triggering
    it — the first was lossy `mandelbrot` content), not a defect in this
    package's own segment-handling. `SEG_LVL_ALT_L` (the loop filter's own
    per-segment *level* delta, as opposed to AQ's per-segment *quantizer*
    delta) was not reachable through any real encoder setting found;
    covered instead by the hand-derived unit tests above.
  - **Filter-order and level-derivation hypotheses the coordinator flagged
    as likely bug locations:** neither needed a fix. Both were transcribed
    directly from §8.8.1/§8.8.2's ordered steps (see "Phase C-32a" above
    for the one genuine ambiguity found in the level-derivation text, which
    did not require a behavioural change) and the resulting corpus passed
    without any filter-order or level-derivation defect surfacing.

  Fifteen consecutive frames of a genuine key-then-inter GOP — 14 real
  motion-compensated frames — reconstruct byte-for-byte identical to
  `ffmpeg -c:v libvpx-vp9`'s own output, and now so does every lossy
  fixture in this corpus, loop filter included. **The "within loop-filter
  tolerance" caveat that accompanied every lossy result in this table
  before this phase is gone**, except where explicitly noted as the
  separate, flagged, pre-existing dequantization bug above — which is not
  a loop-filter caveat at all, and reproduces whether or not the loop
  filter runs.

## Specification

VP9 Bitstream & Decoding Process Specification, version 0.6, 8 December
2016 (`vp9-bitstream-spec-v0.6`). Several sections express normative
behaviour only as algorithmic pseudocode with no separate prose description
— the inverse transform butterfly network (§8.7) and the token/neighbour
context derivation (§9.3.2) most notably — and that pseudocode is,
unavoidably, the specification text itself for this format (RFC 6386's
identical situation, documented in `vaco-codec-vp8`'s doc, for VP8). It was
translated to idiomatic Rust rather than transcribed, and every table
extracted from the spec PDF was independently shape- and
element-count-validated against the spec's own declared dimensions before
being trusted (this caught two real silent-truncation bugs in the
extraction tooling itself, in `kf_y_mode_probs` and `pareto_table`, before
either could reach decoded pixels).
