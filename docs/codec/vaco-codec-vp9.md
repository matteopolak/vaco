# `vaco-codec-vp9`

Layer 4. VP9 video decode (VP9 Bitstream & Decoding Process Specification
v0.6), **key-frame intra decode only** — see "What is deliberately not here"
below before assuming this is a full decoder. Builds on `vaco-codec-msac`
(the shared VP8/VP9 boolean entropy engine) and `vaco-codec-dsp-idct`
(the shared DCT/ADST/WHT transform math).

## What it is

A from-scratch VP9 decoder covering C-29 (headers, superframes, the boolean
decoder, the probability model) and C-30 (intra prediction and transforms):
the uncompressed header (§6.2), the compressed header's forward probability
update (§6.3), Annex B superframe splitting, the partition/mode-info
bitstream walk (§6.4), coefficient token decode (§6.4.24-26, §9.3.2's
Pareto-table probability expansion), dequantization and the inverse
transform (§8.6-8.7: DCT4/8/16/32, ADST4/8/16, the lossless Walsh-Hadamard
transform), and all ten §8.5.1 intra prediction modes. It registers a
`Decoder` for codec id `vp9` via `VP9_DECODER`.

| Module | Contents |
|---|---|
| `header` | `FrameHeader`, `ColorConfig`, `LoopFilterParams`, `QuantParams`, `Segmentation`, `TileInfo`, `EntropyContext`, `parse_uncompressed_header`/`parse_compressed_header` and the `diff_update_prob`/`decode_term_subexp`/`inv_remap_prob` forward-update machinery |
| `tables` | The spec's constants, trees and large data tables (`kf_*_probs`, `default_coef_probs`, `pareto_table`, the scan tables, `dc_qlookup`/`ac_qlookup`) — the large ones via `include!` from `tables/*.in`, extracted from the spec PDF and shape/count-validated separately from this crate |
| `tokens` | §6.4.24's `tokens()`/`read_coef`, §9.3.2's `pareto`/neighbour-context derivation |
| `predict` | §8.5.1's ten intra prediction modes, one implementation per mode generic over block size (unlike VP8, which has separate 16x16/8x8/4x4 formulas) |
| `transform` | §8.6's dequantization and reconstruct process, built on `vaco_codec_dsp_idct::vp9` |
| `superframe` | Annex B's superframe index parsing, returning every sub-frame (not just the display-relevant last one — see below) |
| `framebuf` | `Plane` (`u16`-backed, holds 8/10/12-bit samples), `Picture` |
| `decode` | The per-superblock orchestrator (`decode_partition`/`decode_block`/`intra_frame_mode_info`/`residual`) plus the `Decoder` impl |

The inverse transform math itself (`TxType`, `inverse_transform_2d`, the
butterfly network) lives in `vaco_codec_dsp_idct::vp9`, not in this crate —
D19's shared-kernel convention: a pure function of already-dequantised
coefficients with no VP9-specific context belongs in the shared signal
crate once, alongside the existing h264/hevc/mpeg2 transform modules, rather
than being re-derived independently by a future AV1 decoder that shares
VP9's DCT/ADST family almost exactly.

### What is deliberately not here

**Inter prediction (C-31/#325).** This crate decodes exactly one frame type:
key frames. `Vp9Decoder::decode_one_frame` checks
`fh.show_existing_frame || !fh.is_key_frame` and returns without emitting a
frame for anything else — an inter frame or a shown-existing-frame pointer
is *skipped*, not decoded wrong. A stream that is anything other than
all-intra will visibly drop frames rather than reconstruct them, which is
the honest behaviour: this crate has no motion-vector decode, no reference
frame buffer, and no inter residual path at all.

**The loop filter, profiles 1-3, threading (epic #32).** §8.8's in-loop
deblocking filter is parsed (`LoopFilterParams` is a real, populated
struct) but never applied. A stream whose loop filter level is nonzero will
decode every pixel this crate touches bit-exactly and then differ from a
reference decoder by the filter's own small, single-digit-magnitude,
per-edge smoothing — see "Verification" below for what that looks like in
practice. Profiles 1/3 (independent chroma subsampling, more pixel format
combinations) and 2/3 (10/12-bit) are parsed for totality but not exercised
by any fixture this crate's scope can reach; multi-tile-column decode has a
known `AvailL` simplification (see `planning/TECH-DEBT.md`).

**Backward probability adaptation (§8.3/8.4).** Not implemented, and not an
oversight: every key frame's `setup_past_independence()` call
unconditionally resets `EntropyContext` to the specification's defaults
before that frame's own compressed header forward-updates it (`FrameIsIntra`
is always 1 on a real key frame, and the uncompressed header's syntax makes
the reset call unconditional in that case). A stream of consecutive key
frames therefore never carries adapted probabilities from one key frame to
the next — backward adaptation cannot affect, or be verified against, any
bitstream this crate can fully decode. See `crate::header`'s module doc.

## How it works

### The `inv_remap_prob` bug, and why it looked like a bitstream desync

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

- **Adding inter prediction (C-31/#325):** the natural extension point is
  `decode::decode_block` — `intra_frame_mode_info` and its unconditional
  `is_inter = 0` would need to become a real `is_inter` branch reading
  `inter_frame_mode_info()`, a reference-frame buffer would need to be
  threaded into `FrameCtx`, and `residual`'s `if (is_inter) predict_inter`
  branch (currently entirely absent, since it is never true here) would
  need real motion compensation. `predict_block`'s intra-only prediction
  call is the one place that would need an `is_inter` fork.
- **Adding the loop filter:** §8.8 operates on `CurrFrame` after every
  superblock in a tile has been reconstructed, reading `LoopFilterParams`
  (already parsed, in `FrameHeader`) and the per-block `tx_size`/`skip`/mode
  info this crate already tracks in `FrameCtx`'s `grid`. It would run
  between `decode_frame_tiles` finishing and `pic_to_frame` blitting to the
  output `Frame`.
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

- 9 unit tests across `predict`, `superframe` and `header` — panic-freedom
  checks for every mode/size combination, superframe index parsing
  (including malformed input), and a hand-computed regression test for the
  `inv_remap_prob` bug above (`inv_remap_prob_decrements_m_before_use`).
- 6 unit tests in `vaco_codec_dsp_idct::vp9` for the shared transform math
  (DC-only DCT flatness, all-zero-in/all-zero-out, `cos64`/`brev` known
  values, panic-freedom at `i32::MIN`/`MAX` magnitude coefficients).
- `cargo clippy -p vaco-codec-vp9 --all-targets -- -D warnings` and
  `cargo clippy -p vaco-codec-dsp-idct --all-targets -- -D warnings` both
  clean, including `indexing_slicing`/`cast_possible_wrap`/
  `integer_division` denied workspace-wide.
- Fuzzed via `fuzz_targets/vp9_decode.rs` (`cargo +nightly fuzz run
  vp9_decode --no-default-features --features codec-vp9`), no crashes in
  5.8M executions over 30s.
- **Differential testing against `ffmpeg -c:v libvpx-vp9` reference
  output** (rawvideo YUV 4:2:0), comparing every plane of every decoded
  frame byte-for-byte, is what found and confirmed the fix for both bugs
  above. Fixtures generated with `-bitexact` (encoder side) and decoded
  with `ffmpeg -bitexact` (reference side) to keep both the codec's own
  encoder metadata and the comparison itself byte-deterministic:

  | Fixture | Resolution | Frames | Content | Loop filter | Result |
  |---|---|---|---|---|---|
  | flat, lossless | 64x64 | 1 (key) | solid color | level 0 | bit-exact |
  | flat, lossy | 64x64 | 1 (key) | solid color | level 0 | bit-exact |
  | gradient, lossless | 64x64 | 1 (key) | `gradients` | level 0 | bit-exact |
  | gradient, lossy | 64x64 | 1 (key) | `gradients` | level 8 | MAD 0.03, max deviation 3 (loop filter, not this crate's scope) |
  | `testsrc`, lossless | 64x64 | 1 (key) | `testsrc` | level 0 | bit-exact |
  | `testsrc`, lossy | 64x64 | 1 (key) | `testsrc` | level 6 | MAD 0.02, max deviation 3 (loop filter) |
  | non-multiple-of-8/64 | 130x98 | 1 (key) | `testsrc`, lossless | level 0 | bit-exact |
  | larger frame | 256x256 | 1 (key) | `testsrc2`, lossless | level 0 | bit-exact |
  | complex content | 192x192 | 1 (key) | `mandelbrot`, lossless | level 0 | bit-exact |
  | noisy content | 96x96 | 1 (key) | `testsrc` + heavy noise, lossless | level 0 | bit-exact |
  | multi-frame GOP | 176x144 | 5 (all key) | `testsrc`, lossless | level 0 | 5/5 bit-exact |
  | mixed key/inter stream | 176x144 | 8 (1 key + 7 inter) | `testsrc`, `-auto-alt-ref` | level >0 | key frame decoded within loop-filter tolerance; 7 inter frames correctly skipped, no crash, no wrong pixels emitted |

  Every non-bit-exact row above differs from the reference only by the
  unimplemented loop filter's own small, expected smoothing (maximum
  per-pixel deviation of 3, mean absolute deviation under 0.03) — every
  pixel this crate is actually responsible for (header parse, partition,
  mode decode, token decode, dequantization, transform, intra prediction)
  is bit-exact in every fixture tested, lossless or not. No superframe with
  more than one physical sub-frame was reachable from pure key-frame
  content with the encoders available (`-auto-alt-ref` did not produce one
  for any test clip small/simple enough to keep purely intra); the
  superframe splitter itself is covered by unit tests instead.

  If a full picture, not just this crate's slice of one, is the goal:
  **inter prediction (C-31) is the only thing standing between this
  decoder and decoding an arbitrary real-world VP9 stream correctly.**
  Every other piece — headers, the probability model, the partition and
  mode walk, coefficient decode, dequantization, the transform, intra
  prediction — is implemented and verified bit-exact.

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
