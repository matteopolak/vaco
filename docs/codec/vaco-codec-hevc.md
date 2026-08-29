# `vaco-codec-hevc`

Layer 4. Intra-only (I-slice) HEVC/H.265 video decode (ITU-T H.265
(08/2021)): NAL/VPS/SPS/PPS handling, the CTU quadtree, coding units,
intra prediction (planar/DC/33 angular modes, MPM derivation, reference
sample smoothing and strong intra smoothing), the transform tree, residual
coding, dequantisation and reconstruction. Deblocking, SAO, inter
prediction, B/P-slices, tiles, WPP, `cu_qp_delta`, I_PCM, transform-skip
residual coding, custom scaling lists and every range-extension feature
are explicitly out of scope — see "What was cut" below.

**Not registered.** No `vaco-component.toml` exists for this crate yet —
it is built and tested exactly like `vaco-codec-av1`/`vaco-codec-opus`
before it, both intentionally left unregistered pending further
byte-exactness work. See "Known gap" below for exactly what remains.

## What it is

Builds on `vaco-parse-hevc` (VPS/SPS/PPS and slice-segment-header syntax)
per the D14 layering split, `vaco-codec-cabac` (the CABAC engine and
`ContextModel::init_hevc`/`init_contexts_hevc`), and
`vaco-codec-dsp-idct::hevc` (the already-implemented IDCT/IDST) —
this crate owns only what those three do not: the CTU/CU/PU/TU walk,
residual-coding's context derivations, and intra prediction.

| Module | Contents |
|---|---|
| `decoder.rs` | `HevcDecoder`, the `Decoder` trait impl, `check_scope` (out-of-scope SPS/PPS features refused by name), Annex-B NAL splitting |
| `ctu.rs` | `coding_quadtree`/`coding_unit`/`transform_tree`/`transform_unit` (§7.3.8.4/.5/.8/.10), tying entropy decode, prediction and reconstruction together |
| `residual.rs` | `residual_coding()` (§7.3.8.11) and its context derivations (`sig_ctx_inc`, `pattern_sig_ctx`, `sig_group_ctx_inc`, `context_set_index`, `read_coeff_remain`) |
| `scan.rs` | `generate`/`generate_grouped` — up-right-diagonal/horizontal/vertical scan generation, including HM's `SCAN_GROUPED_4x4` sub-block-then-within-sub-block order |
| `cabac_ctx.rs` | `ContextBank` — I-slice-only CABAC context-init tables and `ContextBank::new(slice_qp)` |
| `intra_mode.rs` | MPM derivation (§8.4.2), `rem_intra_luma_pred_mode` resolution, chroma derived-mode (Table 8-2/8-3), mode-dependent scan-order selection |
| `intra_pred.rs` | Reference-sample line construction/substitution/smoothing, planar/DC/angular prediction (hand-rolled — see the module doc for why `vaco-codec-dsp-intrapred`'s `angular_project` does not fit HEVC's negative-angle extension) |
| `transform.rs` | Flat-scaling-list dequantisation (§8.6.3), the inverse-transform hand-off to `vaco-codec-dsp-idct::hevc` plus §8.6.5's caller-side `bdShift` |
| `framebuf.rs` | `Picture`/`Plane` (`ready: Vec<bool>` substitutes for z-scan availability — see its module doc for why that is exact, not approximate, given this crate's single-slice-segment/no-tiles scope) |

## How it works

`HevcDecoder::send_packet` walks a packet's Annex-B NAL units. `SPS_NUT`/
`PPS_NUT` replace the held parameter sets; a VCL NAL resolves its PPS/SPS,
runs `check_scope`, parses the slice segment header, and walks every CTB
in raster order via `ctu::decode_ctu` — `coding_quadtree` down to
`coding_unit`, which reads `part_mode`/`prev_intra_luma_pred_flag`/mpm-or-
rem/`intra_chroma_pred_mode` and then `transform_tree` (recursive,
carrying inherited `cbf_cb`/`cbf_cr` and the luma cbf context's
`trafoDepth == 0` special case) down to `transform_unit`, which predicts,
decodes residual coefficients, dequantises, inverse-transforms and adds.

Decode and reconstruction are interleaved leaf by leaf (unlike HM's own
two-pass `decodeCtu`/`decompressCtu` split) because no CABAC context in
this crate's scope depends on a reconstructed pixel *value* — only on
already-parsed syntax (depth, cbf, mode) — so there is nothing a
two-pass split would buy here.

### Two real bugs found and fixed against real `libx265` output

- **Missing §8.6.5 `bdShift`.** `vaco_codec_dsp_idct::hevc`'s own module
  doc is explicit that its two-pass engine stops at the transform
  clause's last step and applies no further shift — that final
  `bdShift = 20 - BitDepth` is a residual-*reconstruction* concern one
  clause up from "the inverse transform" itself, and this crate was not
  applying it at all. A single DC coefficient decoded to a uniform
  picture value of 0 instead of the correct constant, caught by the
  flat-grey fixture in `tests/flat.rs`. Fixed in `transform::inverse_transform`.
- **Flat scan used for blocks above 4x4.** HEVC's residual coding needs
  HM's `SCAN_GROUPED_4x4` order — 4x4 sub-blocks visited in scan order,
  then each sub-block's own 16 positions visited in scan order again —
  which is a genuinely different sequence from a flat diagonal/
  horizontal/vertical scan run once at the full block size for anything
  above 4x4 (a flat 8x8 diagonal scan interleaves positions from
  different 4x4 sub-blocks along a shared anti-diagonal). Every
  `coded_sub_block_flag`/`patternSigCtx`/subset-boundary computation in
  `residual_coding()` silently desynchronised for 8x8+ blocks. Fixed by
  `scan::generate_grouped`, with a property test
  (`grouped_scan_is_a_permutation_and_completes_each_sub_block_before_the_next`)
  that would fail on the old flat-scan mistake.
- A third, smaller bug in the same area: `residual::sig_base` used one
  shared context-offset base for every `log2TrafoSize == 3` (8x8) luma
  block regardless of scan order, and let 32x32 luma fall through to
  `CONTEXT_TYPE_SINGLE`'s reserved offset instead of sharing
  `CONTEXT_TYPE_NxN` with 16x16 — both confirmed against
  `ContextTables.h`'s `significanceMapContextSetStart` table and its own
  column comments. A real, confirmed fix, independent of the gap below.

### Known gap

A real `libx265`-encoded fixture (`tests/fixtures/qp32_64x64.hevc`,
encoded with `no-deblock=1:no-sao=1` so there is nothing for either side's
loop filter to disagree about, verified redundantly against
`-skip_loop_filter all` too) still shows real, structured pixel error
against `ffmpeg`'s reference decode — not the diffuse, small deviation
this project's own shipping bar treats as acceptable (see
`AGENT-CONSTRAINTS.md`'s "byte-exactness is a check, not the bar"
section). What is verified correct, by hand, against this exact fixture:
every 4x4 (single-coefficient-group) transform block across the first
16x16 region — three full 8x8 `PART_NxN` CU quadrants, including negative-
angle intra modes and sign-data-hiding — decodes byte-exact, and the
flat-grey fixture (`tests/flat.rs`) decodes byte-exact end to end. The
first CU with a genuinely multi-coefficient-group residual (an
un-split 8x8 `PART_2Nx2N` transform block) diverges, and the defect
survived a full line-by-line re-comparison of every `residual_coding()`
context derivation, the transform-tree split/cbf logic, and the QP/scan
derivations against the HM reference decoder — not yet root-caused.
Named in `tests/oracle.rs` as `known_gap_dense_content_still_diverges`,
`#[ignore]`d rather than deleted, mirroring `vaco-codec-av1`'s own
precedent for exactly this shape of gap. `tests/oracle.rs` also has an
always-green smoke test (`cabac_intra_frame_decodes_without_error`) that
decodes the same fixture and reports per-plane Y/U/V agreement without
asserting a threshold, so the crate's tests stay a useful signal even
while the ignored test names the real target.

### What was cut

`check_scope` in `decoder.rs` refuses (`Error::Unsupported`, by name, at
the SPS/PPS) rather than approximates: non-4:2:0 chroma, non-8-bit depth,
`separate_colour_plane`, custom scaling lists, I_PCM, SPS/PPS range
extensions, SCC extensions, tiles, `entropy_coding_sync` (WPP),
`cu_qp_delta_enabled`, `transquant_bypass_enabled`. Deblocking and SAO are
never applied (there is no code path for either, not even a disabled
one) — this is why the test fixtures are deliberately encoded with both
disabled at the encoder, rather than relying solely on
`-skip_loop_filter all` to paper over the gap. B/P-slices are not parsed
at all; only NAL types this crate recognises as I-slice VCL data are
decoded. Only Annex-B framing is handled — length-prefixed `hvcC` samples
are not (`decoder.rs`'s own module doc).

## How to change it

- **Root-causing the known gap**: start from `residual.rs`'s
  `residual_coding()` and the transform-tree split/cbf logic in `ctu.rs`
  — both were re-verified against HM's `TDecSbac.cpp`/`TDecEntropy.cpp`
  this pass and matched, so the next increment should probably instrument
  a byte-for-byte CABAC bin trace against a from-scratch reference decode
  of the same fixture rather than re-deriving formulas from spec text
  again.
- **Extending scope** (deblocking, SAO, inter prediction, tiles, WPP):
  the corresponding SPS/PPS fields already correctly return
  `Error::Unsupported` by name in `check_scope` when a real stream
  exercises them — implement behind that same call site rather than
  adding a new refusal path.
- **New CABAC context table**: transcribe from HM's `ContextTables.h`
  (Tier A, BSD-3-Clause — see "Specification" below), not from memory or
  from the spec's clause text alone; add the table to `cabac_ctx.rs` and
  a `provenance/vaco-codec-hevc.toml` entry if it has 32+ elements.
- Do not remove `tests/oracle.rs`'s `#[ignore]`d test without either
  fixing the gap it names or replacing it with an equally specific one —
  see that file's own module doc.

## Configuration

No env vars or flags. `HevcDecoder::new(limits: vaco_limits::Limits)`
bounds all allocation (picture planes, per-CTU grids) through
`vaco_limits::Budget`.

## Dependencies

| Crate | For |
|---|---|
| `vaco-parse-hevc` | VPS/SPS/PPS, slice segment header (per D14, this crate must not duplicate any of it) |
| `vaco-codec-cabac` | the CABAC engine, `ContextModel::init_hevc`/`init_contexts_hevc` |
| `vaco-codec-dsp-idct` (`hevc` module) | the 2-D inverse DCT/DST transform |
| `vaco-codec-dsp-intrapred` | not used for angular prediction (see `intra_pred.rs`'s module doc) but available for future reuse |
| `vaco-codec-core` | the `Decoder` trait, `Machine<Frame>` send/receive state machine |
| `vaco-frame` / `vaco-pixfmt` | output frame and pixel-format types |
| `vaco-packet` | input compressed packets |
| `vaco-limits` | allocation budgets for header-derived sizes |
| `vaco-core` / `vaco-bitstream` | shared error taxonomy and bit access primitives |

Dev-only: `proptest`. No external runtime dependencies.

## Verification

`tests/flat.rs`: a real `libx265`-encoded flat-grey fixture decodes to a
byte-exact constant against `ffmpeg`'s decode. `tests/oracle.rs`: a
busier real fixture is checked two ways — an always-green smoke test that
decodes without error/panic and reports Y/U/V agreement, and an
`#[ignore]`d test naming the real byte-exactness target (see "Known gap").
`fuzz/fuzz_targets/hevc_decode.rs` runs arbitrary bytes through
`send_packet`/`receive_frame` twice (the second call exercising the
VPS/SPS/PPS maps persisted across NAL units) — run with:

```sh
cargo +nightly fuzz run hevc_decode --no-default-features --features codec-hevc -- -max_total_time=30
```

## Specification

`itu-t-h265-202108` (ITU-T Rec. H.265 (08/2021)) and
`hm-reference-software` (the HM reference decoder, BSD-3-Clause, Tier A
per `planning/AGENT-CONSTRAINTS.md`'s clean-room section — unlike
FFmpeg/x265, which stay Tier B), both recorded in
`provenance/sources.toml`. Mechanically-thresholded tables recorded in
`provenance/vaco-codec-hevc.toml`.
