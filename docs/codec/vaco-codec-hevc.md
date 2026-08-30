# `vaco-codec-hevc`

Layer 4. HEVC/H.265 video decode (ITU-T H.265 (08/2021)) — I-slices and
P-slices: NAL/VPS/SPS/PPS handling, the CTU quadtree, coding units, intra
prediction (planar/DC/33 angular modes, MPM derivation, reference sample
smoothing and strong intra smoothing), inter prediction (`prediction_unit()`
syntax, merge/AMVP candidate derivation, §8.5.3.3 motion compensation, the
inter CABAC context tables, `src/dpb.rs`'s reference picture management —
see "Stage 2: P-slices, byte-exact — landed" below), the transform tree,
residual coding, dequantisation, reconstruction, in-loop deblocking (§8.7.2,
see "Deblocking (§8.7.2), landed" below), SAO (§7.3.8.3/§8.7.3, see "SAO
(§7.3.8.3 / §8.7.3), landed" below), wavefront parallel processing
(§9.3.2.3, see "WPP (`entropy_coding_sync_enabled_flag`), landed" below) and
per-CU adaptive QP (`cu_qp_delta`, §7.3.8.11/§8.6.1, see "Per-CU QP delta
(`cu_qp_delta`), landed" below).
B-slices, weighted prediction, tiles, I_PCM, transform-skip residual
coding, custom scaling lists and every range-extension feature are
explicitly out of scope — see "What was cut" below.

**Registered, patent-encumbered-gated.** `vaco-component.toml` declares
this decoder with `encumbered = true` / `default = false` behind the
`patent-encumbered-hevc-decode` feature, the same D4/D4.1 posture as
`vaco-codec-h264` (HEVC decode is covered by multiple patent pools — see
`planning/research/07-legal-patents-licensing.md`). It was held back from
registration while a real `libx265` fixture showed a genuine defect on
multi-coefficient-group residual blocks; see "The multi-coefficient-group
residual defect, found and fixed" below for the root cause and how it was
found, and "Registration" for what changed to flip the switch.

## What it is

Builds on `vaco-parse-hevc` (VPS/SPS/PPS and slice-segment-header syntax)
per the D14 layering split, `vaco-codec-cabac` (the CABAC engine and
`ContextModel::init_hevc`/`init_contexts_hevc`), and
`vaco-codec-dsp-idct::hevc` (the already-implemented IDCT/IDST) —
this crate owns only what those three do not: the CTU/CU/PU/TU walk,
residual-coding's context derivations, and intra prediction.

| Module | Contents |
|---|---|
| `decoder.rs` | `HevcDecoder`, the `Decoder` trait impl, `check_scope` (out-of-scope SPS/PPS features refused by name); embeds `vaco_parse_hevc::HevcParser` for parameter-set bookkeeping and `hvcC`-vs-Annex-B framing, mirroring `vaco-codec-h264` |
| `ctu.rs` | `coding_quadtree`/`coding_unit`/`transform_tree`/`transform_unit` (§7.3.8.4/.5/.8/.10) for I-slices; `coding_unit_p`/`decode_skip_cu`/`decode_inter_cu`/`transform_tree_inter` and `prediction_unit()` parsing (§7.3.8.5/.6/.9) for P-slices — see "Stage 2: P-slices, byte-exact — landed" below |
| `motion.rs` | Merge candidate derivation (§8.5.3.2.2/.3, spatial + temporal + zero-fill) and AMVP candidate derivation (§8.5.3.2.6/.7), plus the shared motion-vector scaling arithmetic (`dist_scale_factor`/`scale_mv`, §8.5.3.2.8) both use |
| `mc.rs` | Motion compensation: 8-tap luma / 4-tap chroma separable interpolation filters at quarter/eighth-sample precision (§8.5.3.3), exact HM fixed-point arithmetic |
| `residual.rs` | `residual_coding()` (§7.3.8.11) and its context derivations (`sig_ctx_inc`, `pattern_sig_ctx`, `sig_group_ctx_inc`, `context_set_index`, `read_coeff_remain`) — shared unchanged between I- and P-slices |
| `scan.rs` | `generate`/`generate_grouped` — up-right-diagonal/horizontal/vertical scan generation, including HM's `SCAN_GROUPED_4x4` sub-block-then-within-sub-block order |
| `cabac_ctx.rs` | `ContextBank` — CABAC context-init tables and `ContextBank::new(slice_qp)` (I-slice row) / `ContextBank::new_p_slice(slice_qp, cabac_init_flag)` (P-slice P/B rows, §9.3.2.2) |
| `dpb.rs` | `Dpb`/`ReferencePicSets`/`CollocatedMotionField` — reference-picture-set derivation (§8.3.2), reference-picture-list construction (§8.3.4), DPB output-reordering "bumping" (Annex C.3.2/C.5.2), and the compressed 16x16 motion field TMVP reads (§8.5.3.2.8/.9) |
| `intra_mode.rs` | MPM derivation (§8.4.2), `rem_intra_luma_pred_mode` resolution, chroma derived-mode (Table 8-2/8-3), mode-dependent scan-order selection |
| `intra_pred.rs` | Reference-sample line construction/substitution/smoothing, planar/DC/angular prediction (hand-rolled — see the module doc for why `vaco-codec-dsp-intrapred`'s `angular_project` does not fit HEVC's negative-angle extension) |
| `transform.rs` | Flat-scaling-list dequantisation (§8.6.3), the inverse-transform hand-off to `vaco-codec-dsp-idct::hevc` plus §8.6.5's caller-side `bdShift` |
| `framebuf.rs` | `Picture`/`Plane` (`ready: Vec<bool>` substitutes for z-scan availability — see its module doc for why that is exact, not approximate, given this crate's single-slice-segment/no-tiles scope) |

## How it works

`HevcDecoder::send_packet` treats each packet as one container access unit
and hands it to the embedded `HevcParser::push_access_unit`, which walks
the packet's NAL units in whichever framing `set_extradata` resolved
(Annex-B or `hvcC`-length-prefixed), ingesting VPS/SPS/PPS into its own
`ParameterSets` and parsing the one primary-coded-picture slice header.
The decoder then re-scans the same NAL units (via `vaco_format_nalu::units`,
not a second parser) only to reach that slice's raw bits, which
`push_access_unit` does not hand back; resolves its PPS/SPS via
`parameter_sets().sps_for_pps`; runs `check_scope`; and walks every CTB in
raster order via `ctu::decode_ctu` — `coding_quadtree` down to
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

### The multi-coefficient-group residual defect, found and fixed

A real `libx265`-encoded fixture (`tests/fixtures/qp32_64x64.hevc`, encoded
with `no-deblock=1:no-sao=1` so there is nothing for either side's loop
filter to disagree about, verified redundantly against `-skip_loop_filter
all` too) used to show structured pixel error against `ffmpeg`'s reference
decode, confined to multi-coefficient-group (8x8-and-larger) residual
blocks: every 4x4 (single-coefficient-group) transform block decoded
byte-exact, including negative-angle intra modes and sign-data-hiding, while
the first CU with a genuinely multi-group residual (an un-split 8x8
`PART_2Nx2N` transform block) diverged. A full line-by-line re-comparison of
`residual_coding()`'s context derivations, the transform-tree split/cbf
logic and the QP/scan derivations against the HM reference decoder found
nothing further — formula-level re-derivation was exhausted without finding
the cause.

**Method that found it**: a byte-for-byte CABAC bin trace (context array,
index, `pStateIdx`, `valMPS`, decoded value, at every context-coded bin) was
added temporarily to both this crate and a from-source, locally-built HM
18.0 (BSD-3-Clause, Tier A — cloned from `vcgit.hhi.fraunhofer.de`, built
natively with the `-msse4.1` compile flag guarded off non-x86 targets since
HM's own SIMD gate is x86-only), decoding the same fixture, and the two
traces diffed bin-for-bin. The first divergence landed exactly on the
`sig_coeff_flag` bin at the DC position of the first multi-coefficient-group
(8x8, 4-subset) luma transform block.

**Root cause**: HM's `TComTrQuant::getSigCtxInc` returns a literal `0` for
the DC position (`(posX + posY) == 0`) — a special case that *bypasses*
`firstSignificanceMapContext` (this crate's `sig_base`/`sig_class_base`)
entirely, rather than feeding `0` into it as one more `ctxInc` term. DC is
one context shared by every transform size within a component, addressed at
the component's own base index alone (`comp_base`), never
`comp_base + sig_class_base`. `residual_coding()` was computing
`comp_base + sig_class_base + inc` unconditionally, so at the DC position it
read the wrong context slot — one belonging to a *different, wrongly
sized* significance-map class — for every TU size where `sig_base != 0`.
That distinction is invisible at 4x4 (`sig_base` is `0` there by
definition), which is exactly why every 4x4 residual block decoded
byte-exact while every 8x8-and-larger block desynchronised the entire rest
of the CABAC bitstream from that one bin onward. Fixed in
`residual::residual_coding` by special-casing `x == 0 && y == 0` to
`comp_base` before computing the size-class-offset path at all; see that
function's inline comment and `tests/oracle.rs`'s
`dense_content_is_byte_exact` for the fixed regression test.

With the fix, the fixture decodes **byte-exact on every sample of all three
planes** (Y, U and V) against `ffmpeg`'s decode, and the real `vaco` binary
(`vaco -i <libx265.mp4> -c:v rawvideo -f rawvideo out.raw`, built with
`--features vaco-registry/patent-encumbered-hevc-decode`) matches `ffmpeg`'s
own decode of the same file byte-for-byte, per plane, end to end.

### Earlier bugs found and fixed against real `libx265` output

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
  column comments. A real, confirmed fix, independent of the DC-context bug
  above (that fix does not move this fixture's own numbers, since it never
  reaches a horizontal/vertical-scan 8x8 luma block or a 32x32 block, but it
  is genuine and kept).

### What was cut

`check_scope` in `decoder.rs` refuses (`Error::Unsupported`, by name, at
the SPS/PPS) rather than approximates: non-4:2:0 chroma, non-8-bit depth,
`separate_colour_plane`, custom scaling lists, I_PCM, SPS/PPS range
extensions, SCC extensions, tiles, `transquant_bypass_enabled`. Neither
deblocking, SAO, `entropy_coding_sync` (WPP), `cu_qp_delta_enabled`, nor
P-slices are on this list any more — see their own "landed" sections
below. B-slices and weighted prediction (`weighted_pred_flag`, a P-slice
feature — see "Stage 2: P-slices, byte-exact — landed") are refused by
slice kind/PPS flag in `decode_packet` itself rather than at the SPS/PPS
`check_scope` call, since neither has a footprint visible before the slice
header is parsed. Both Annex-B and length-prefixed (`hvcC`) framing are
handled, via the embedded `vaco_parse_hevc::HevcParser` (`decoder.rs`'s own
module doc).

## How to change it

- **If a future fixture shows a similar structured, size-correlated
  divergence**: do not trust "formula re-derivation found nothing" as the
  final word — it wasn't, for the DC-context bug above. Instrument a
  byte-for-byte CABAC bin trace (context array + index, `pStateIdx`,
  `valMPS`, decoded value) in both this crate and a from-source HM build
  decoding the same fixture, and diff bin-for-bin; the first divergence
  names the exact syntax element and context, which a spec re-read cannot
  do as precisely. HM 18.0 builds natively outside Linux/x86 once
  `-msse4.1` is guarded to x86 targets only (its own SIMD code is already
  gated on `__SSE2__`/`__AVX__`, so removing the *compiler flag* on a
  non-x86 host is enough — no source changes needed beyond the build
  script). Clean-room rule: HM is Tier A (BSD-3-Clause) and may be read,
  built and instrumented directly; `ffmpeg`/`x265` stay Tier B — run only,
  never opened.
- **Extending scope** (B-slices, weighted prediction, tiles — inter
  prediction (P-slices), deblocking, SAO, WPP and `cu_qp_delta` are done,
  see their own sections above): the corresponding SPS/PPS fields already
  correctly return `Error::Unsupported` by name in `check_scope` when a
  real stream exercises them — implement behind that same call site rather
  than adding a new refusal path. Reference picture management
  (`src/dpb.rs`) is already landed for inter prediction — see "Reference
  picture management (§8.3.2 / §8.3.4), landed" below for what it covers,
  and "Stage 2: P-slices, byte-exact — landed" for how `prediction_unit()`/
  merge/AMVP/motion compensation build on it. Deblocking was the
  one exception to the "extend behind the existing refusal" pattern, since
  it has no bitstream footprint to refuse in the first place (a silent
  pixel-only deviation, not a parse error) — its own call site is
  `decoder::decode_packet`'s post-CTU-loop `deblock::filter_picture` call,
  not `check_scope`.
- **If a future scope extension touches per-CU state** (the way
  `cu_qp_delta` did): check whether `deblock.rs` is still assuming that
  state is constant across the picture before trusting its own module doc.
  `cu_qp_delta` is the second time this crate's own "collapse this general
  clause-8.7.2 term to a constant because nothing else in scope can make it
  vary" reasoning stopped being true the moment a new feature landed — see
  "Per-CU QP delta (`cu_qp_delta`), landed" below for what changed and how
  it was caught (the crate's own module doc said so, in so many words,
  before this pass started).
- **New CABAC context table**: transcribe from HM's `ContextTables.h`
  (Tier A, BSD-3-Clause — see "Specification" below), not from memory or
  from the spec's clause text alone; add the table to `cabac_ctx.rs` and
  a `provenance/vaco-codec-hevc.toml` entry if it has 32+ elements.
- Do not remove `tests/oracle.rs`'s `dense_content_is_byte_exact` test
  without either preserving its byte-exact assertion against a real,
  busy fixture or replacing it with an equally specific one.

## Configuration

No env vars or flags. `HevcDecoder::new(limits: vaco_limits::Limits)`
bounds all allocation (picture planes, per-CTU grids) through
`vaco_limits::Budget`.

## Dependencies

| Crate | For |
|---|---|
| `vaco-parse-hevc` | VPS/SPS/PPS, slice segment header, and `HevcParser` (access-unit assembly, `hvcC`/Annex-B framing, parameter-set bookkeeping — per D14, this crate must not duplicate any of it) |
| `vaco-format-nalu` | the `Framing`-aware NAL-unit iterator (`units`) and `RbspBuf`, the same low-level pieces `HevcParser` itself is built on |
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
decodes without error/panic and reports Y/U/V agreement, and
`dense_content_is_byte_exact`, which asserts full byte-exactness per plane
(see "The multi-coefficient-group residual defect, found and fixed" above).
`fuzz/fuzz_targets/hevc_decode.rs` runs arbitrary bytes through
`send_packet`/`receive_frame` twice (the second call exercising the
embedded `HevcParser`'s parameter sets persisted across NAL units) — run
with:

```sh
cargo +nightly fuzz run hevc_decode --no-default-features --features codec-hevc -- -max_total_time=30
```

**End-to-end, via the real binary**: with the CLI built including
`vaco-registry/patent-encumbered-hevc-decode`, a real `libx265`-encoded
sequence decodes with the same per-plane agreement as the in-crate fixture
— see "Registration" below for the measured command and result.

## Registration

Registering this decoder needed two more real bugs closed first, both
found only once the real `vaco` binary — not this crate's own tests — was
the thing being measured (`AGENT-CONSTRAINTS.md`'s own point about a
component with no production caller):

- **`hvcC`/AVCC-equivalent extradata was accepted and discarded.**
  `HevcDecoder::set_extradata` was the trait's no-op default; MP4 never
  carries in-band VPS/SPS/PPS, so `vaco -i real.mp4 -c:v hevc` decoded
  nothing at all — the identical `vaco-codec-h264` history (`E2E-GAPS.md`
  §1) repeating one crate later. Fixed the same way `vaco-codec-h264`
  fixed it: `HevcDecoder` now embeds `vaco_parse_hevc::HevcParser`
  (`set_extradata`, `framing`, `push_access_unit`, `parameter_sets`)
  instead of its own ad-hoc VPS/SPS/PPS `HashMap`s and a hardcoded
  Annex-B-only NAL walk — see `decoder.rs`'s own module doc. **No
  `hevc_mp4toannexb` bitstream filter is used or needed**: that filter's
  job is muxer-side re-framing on *output*; `vaco_format_nalu::units`
  already walks either framing identically on input.
- **The luma MPM "above" candidate ignored a CTB-row-boundary rule.**
  §8.4.2 forces `candIntraPredModeB` to `INTRA_DC` whenever the neighbour
  one pixel above would sit in a *different* CTB row than the current
  block — regardless of whether that neighbour has already been decoded
  (confirmed directly against HM's `TComDataCU::getPUAbove(...,
  planarAtCtuBoundary = true)`, which returns `NULL` at exactly that
  condition). This crate special-cased only the picture's own top edge
  (`pu.y == 0`), so any PU at the top row of a CTU below the first read a
  real, already-decoded neighbour's mode instead — silently picking the
  wrong MPM list entry for `mpm_idx`, which decodes to the wrong intra
  mode, which selects the wrong residual scan order and `sig_coeff_flag`
  context class, cascading into a full CABAC desync
  (`CABAC decode ran past the slice segment data`) from the second CTB row
  onward. **No single-CTU fixture can expose this** — it needs a second
  row to exist at all. Fixed in `ctu.rs`'s per-PU mode loop: `above` is
  forced to `DC_IDX` whenever `pu.y % ctb_size == 0`, which subsumes the
  old top-edge-only check.
- **SAO is now refused, not merely unapplied.** `sample_adaptive_offset_enabled_flag`
  was not checked in `check_scope`, so a stream that turns SAO on (the
  common case — most encoders leave it on) hit exactly the same
  full-decode CABAC desync as the bug above, from the first CTU that
  merges or sets an offset, since this crate parses none of §7.3.8.3's
  per-CTU `sao()` syntax. Now refused by name (`Error::Unsupported`) at
  the SPS, the same posture every other cut in `check_scope` already has
  — turning a confusing internal error into an honest one, not adding new
  scope.

With all three fixed:

```sh
cargo build --release -p vaco-cli --features vaco-registry/patent-encumbered-hevc-decode
ffmpeg -y -f lavfi -i "testsrc2=size=320x240:rate=25" -t 1 -pix_fmt yuv420p -c:v libx265 \
       -x265-params "log-level=none:wpp=0:no-sao=1:no-deblock=1:qp=32:keyint=1" out.mp4
vaco -i out.mp4 -c:v rawvideo -f rawvideo out.raw
```

Measured against `ffmpeg`'s own decode of the same file (Y, U and V
compared separately per frame, per `AGENT-CONSTRAINTS.md`): full-length
(25 frames), **byte-exact on every sample of every plane of every frame** —
320x240 is 5x4 CTUs at the default 64-pixel CTB size, so both the
multi-coefficient-group residual fix and the CTB-row-boundary MPM fix are
exercised for real, not just in a single-CTU fixture.

`wpp=0`/`no-sao=1`/`no-deblock=1`/constant `qp=` (CQP, not CRF) are not new
restrictions this registration invented — they are this crate's
already-documented, pre-existing scope ("What was cut" above), made to
apply consistently: WPP and `cu_qp_delta` (which CRF rate control implies)
were already refused cleanly before this pass; SAO now is too. The
task-setter's own unmodified example command (no `-x265-params` at all)
encodes with WPP and adaptive per-CU QP by default on a typical `libx265`
build at this resolution, and gets a clean, named refusal rather than
either of those — not a silent misdecode, and not the internal-error crash
SAO used to cause.

Registered as `encumbered = true` / `default = false` behind
`patent-encumbered-hevc-decode` — see `vaco-component.toml` and
`DECODER_HEVC` in `src/lib.rs`, mirroring `vaco-codec-h264`'s own D4/D4.1
posture exactly.

## Deblocking (§8.7.2), landed

`no-deblock=1` above was the interim posture, not the final one:
`libx265`'s own default turns deblocking on, so no ordinary HEVC file
decoded byte-exact against a plain `ffmpeg` decode until this landed.
`src/deblock.rs` implements clause 8.7.2 directly (cross-checked against
HM 18.0's `TComLoopFilter.cpp`, Tier A) rather than reusing
`vaco-codec-h264`'s `vaco-codec-dsp-deblock` — see that module's own doc
for why the two codecs' filters, despite the shared name, are different
algorithms (different tables, a per-4-line-group decision instead of
per-line, no chroma activity gate, and more) that a shared primitive would
have to parameterise away entirely.

This crate's own scope (I-slice-only, no `cu_qp_delta`, one slice segment,
no tiles) collapses HM's general boundary-strength/QP derivation
substantially: boundary strength is always 2 (every CU is intra), and
`qP_P == qP_Q` on every edge (one constant slice QP). Both are consequences
of scope already refused elsewhere in `check_scope`, not new
approximations.

Verified with `verify_hevc_deblock.sh` (a parameterised descendant of the
previous agent's `verify_hevc_cli.sh`, same "prove the harness can report a
failure before trusting a pass" discipline — checked here by diffing a
plain decode against an `-skip_loop_filter all` decode of the same file and
confirming the harness reports a real, non-trivial mismatch): a real
`libx265` stream (`wpp=0:no-sao=1`, deblocking **on**, `libx265`'s own
default) decodes **byte-exact on every sample of every plane of every
frame** against plain `ffmpeg` (no `-skip_loop_filter`), at 320x240 (5x4
CTUs) and 640x480, at multiple QPs. The pre-existing
`wpp=0:no-sao=1:no-deblock=1` byte-exact regression
(`tests/oracle.rs::dense_content_is_byte_exact`) is unchanged, since
`deblock::filter_picture` returns immediately when
`slice_deblocking_filter_disabled_flag` is set.

## SAO (§7.3.8.3 / §8.7.3), landed

`no-sao=1` above was also the interim posture, not the final one:
`libx265` turns SAO on by default, so — combined with deblocking landing
first — no *ordinary* `libx265` file decoded byte-exact until this closed
the gap. Unlike deblocking, SAO genuinely has bitstream footprint this
crate used to parse none of at all (`sao()`, once per CTU, gated by
`slice_sao_luma_flag`/`slice_sao_chroma_flag`); `src/sao.rs` now parses it
(merge-left/merge-up against one shared CABAC context, then per-component
`sao_type_idx`/band-or-edge-offset/band-position/`sao_eo_class`, following
HM 18.0's `TDecSbac::parseSAOBlkParam` field order and Cb/Cr sharing rule —
Cr copies Cb's type/class but reads its own offsets and, for band offset,
its own band position) and applies clause 8.7.3's filtering process
(`TComSampleAdaptiveOffset::offsetBlock`) after deblocking, reading from a
picture-wide snapshot so a CTU's own SAO output never reads a neighbour's
already-SAO'd samples.

The filtering process itself is written per-pixel rather than porting HM's
row-buffer-reuse optimisation (see `sao.rs`'s own module doc for why that
is a performance-only difference, not an algorithmic one) and leans on this
crate's single-slice/no-tile scope for neighbour availability exactly the
way `framebuf`'s own module doc already justifies for intra prediction: a
neighbouring sample is unavailable if and only if it is outside the
picture.

Verified the same way deblocking was: a real `libx265` stream with SAO left
at its own (on) default (`wpp=0`, deblocking also at its default-on)
decodes byte-exact on every sample of every plane of every frame against
plain `ffmpeg`, at 320x240 and 640x480, at multiple QPs.

## WPP (`entropy_coding_sync_enabled_flag`), landed

A previous pass got most of the mechanism right and still reverted rather
than ship it — this section used to be titled "attempted and reverted" and
recorded exactly that measurement. It is kept below (renamed to "root
cause, found and fixed") because the fix depended entirely on trusting that
record and re-deriving from it, not from a fresh attempt.

**The mechanism**: `decode_wpp_rows` (`decoder.rs`) splits
`slice_segment_data()` into one CABAC substream per CTU row and performs
§9.3.2.3's context save/restore at each row's second CTU (or leaves a
narrower-than-two row's context untouched — no restore point exists there):
mirroring `TDecSlice.cpp`'s own `m_entropyCodingSyncContextState` exactly,
a whole `ContextBank` snapshot (it is `Copy`) is taken once, right after
the CTU at column index 1 finishes, and used to initialise the *next* row's
starting context — never carried forward from that row's own last CTU.
Each row's arithmetic-decoding engine (`CabacDecoder`) always reinitialises
fresh at that row's own substream's first byte, matching clause 9.3.1.2's
own slice-start init; only the context state is conditionally inherited
rather than reset.

### Root cause, found and fixed

The previous pass's own measurement, kept verbatim because it is what the
fix was found from:

> Measured against a real `libx265` WPP stream (25 real IDR frames, 320x240,
> 4 CTU rows, `no-sao=1` to isolate WPP from the SAO work landing in the same
> pass): with the full mechanism active, frame 0 decoded byte-exact against
> `ffmpeg`, full 4-row picture, multi-row and multi-column — proof the
> substream-splitting and sync-restore *mechanism* is structurally sound.
> Across all 25 frames, 7 failed with a genuine `CABAC decode ran past the
> slice segment data` desync — always in the third or fourth CTU row, never
> the first two. Disabling *only* the context restore raised the failure
> rate to 23 of 25 frames — strong evidence the restore mechanism is doing
> most of the right thing. HM 18.0 (Tier A) decodes the identical bitstream
> with zero errors, confirming the defect is in this crate, not the
> encoder's output.

**Method that found it**: the same bin-for-bin CABAC trace this crate's own
history keeps reaching for (see "The multi-coefficient-group residual
defect" above) — temporary instrumentation added to both this crate and a
from-source HM 18.0 build (removed before committing), diffed bin-for-bin
against the same real, failing WPP fixture. The first divergence was not a
wrong bin *value* at all: two `split_cu_flag` decisions at the start of a
failing frame's third CTU row decoded identically to HM (same context
index, same `pStateIdx`/`valMPS`, same outcome), and the very next bin
(`prev_intra_luma_pred`) started from the *same* context state in both
decoders yet decoded a *different* value — possible only if the underlying
CABAC engine's `range`/`offset`, not any context, had already diverged.
That pointed away from the context save/restore mechanism (which a
whole-`ContextBank` dump at the row's own save point confirmed was
byte-for-byte identical to HM's) and toward the *byte range* each row's
substream was being read from.

**The actual bug**: §7.4.7.1 defines `entry_point_offset_minus1[i] + 1` as
a byte count over the *coded* slice segment data — the specification's own
words are explicit that "emulation prevention bytes ... are counted as
part of the slice segment data for purposes of subset identification".
`decode_wpp_rows` was instead slicing those offsets directly out of
`cabac_data`, which `RbspBuf::fill` had already de-escaped (stripped every
`00 00 03` emulation-prevention sequence out of). The two byte-position
spaces only agree when no such escape occurs before the row boundary in
question — which is genuinely content-dependent (whether a run of CABAC
output happens to contain two zero bytes followed by a byte `<= 3`), rare
enough that most short clips' frames never trigger it and common enough
that a real fraction do, exactly matching the previous pass's "7 of 25
frames, never the first two rows" measurement: row 0 has no accumulated
byte-range error to be wrong yet, and a wrong `range`/`offset` does not
necessarily flip a bin's *value* in the very row it first goes wrong,
only compounds until it eventually does.

**Fixed** by never slicing `cabac_data` for WPP: `ebsp_offset_for_rbsp_len`
maps the RBSP-relative position where `slice_segment_data()` begins back to
its position in the original, still-escaped NAL bytes; row byte ranges are
computed there, directly from the entry-point offsets; and each row's own
byte range is de-escaped independently (`vaco_bitstream::annexb::to_rbsp`)
before that row's `CabacDecoder` reads it. This is exact, not an
approximation: WPP row boundaries are always byte-aligned by construction
(`end_of_subset_one_bit` + `byte_alignment()`, §7.3.8.1), so no escape
sequence can straddle one, and de-escaping each row's own coded bytes in
isolation reproduces exactly what de-escaping the whole substream and
slicing the result would give.

With the fix, the identical 320x240 WPP fixture decodes byte-exact on every
sample of every plane of all 25 frames, and the same holds at 640x480
(10x8 CTUs) and at 300x500 (5x8 CTUs — both a partial last CTU column and a
partial last CTU row, deliberately a second, differently-sized clip rather
than trusting the first), with WPP left at its own default-on setting
alongside deblocking and SAO also at their own defaults. The pre-existing
non-WPP byte-exact paths (`tests/oracle.rs::dense_content_is_byte_exact`,
and the same real-binary check with `wpp=0`) are unchanged.

`check_scope` no longer refuses `entropy_coding_sync_enabled_flag`.

## Per-CU QP delta (`cu_qp_delta`), landed

The one restriction still standing after deblocking, SAO and WPP all
landed at their own defaults: `libx265`'s own default rate control is CRF,
which implies `cu_qp_delta_enabled_flag` (adaptive per-CU QP) — a genuinely
stock invocation, `-c:v libx265 -x265-params log-level=none` with nothing
else, hit exactly this refusal and nothing else. Closing it makes an
ordinary, completely unmodified `libx265` file decode end to end (still
subject to this crate's other pre-existing, unrelated scope limits — I-slice
only, no tiles, and so on).

**The mechanism**: `ctu.rs`'s `coding_quadtree()` resets the current
quantisation group's own `CuQpDeltaVal`/`IsCuQpDeltaCoded` (mirrored as
`cu_qp_delta_val`/`is_cu_qp_delta_coded`) and re-derives `qPY_PRED`
(`qp_y_pred`, cached as `qg_qp_pred`) every time it is entered at or above
`Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth` — which,
since that threshold never exceeds the CTB size, fires at least once per
CTU and is the *only* reset a CU larger than the nominal QG size ever gets
(§7.3.8.4's own syntax table does this unconditionally, split or not).
`cu_qp_delta_abs`/`cu_qp_delta_sign_flag` (§7.3.8.11) are read by
`maybe_parse_cu_qp_delta`, called from `transform_unit()` — the first leaf
in decoding order whose luma-or-chroma `cbf` is set, in the same quantisation
group, reads it; every later leaf in that QG (with or without its own
residual) reuses the same `cu_qp_delta_val`. `derive_qp_y` applies §8.6.1's
final wraparound (`(qPY_PRED + CuQpDeltaVal) % 52`, `QpBdOffsetY == 0`
throughout this crate's 8-bit-only scope). Every coding unit — even one with
no residual anywhere in it — gets its own finalised `QpY` written to
`CuGrid`'s new per-4x4 `qp`/`qp_written` grid once its whole transform tree
has been walked (`coding_unit()`'s own tail, mirroring HM's
`TDecCu::xFinishDecodeCU`), because a later quantisation group's own
`qPY_A`/`qPY_B` neighbour derivation, and deblocking's own `qP_P`/`qP_Q`,
both need it regardless of whether that particular CU coded any residual.

`qPY_A`/`qPY_B` (`qp_y_pred`) fall back to `qPY_PREV` (`qp_y_prev`) whenever
the corresponding neighbour would sit outside the *current quantisation
group's own CTB* — confirmed directly against HM's `getQpMinCuLeft`/
`getQpMinCuAbove` (`TComDataCU.cpp`), which return `NULL` exactly at that
condition, always addressing `m_pcPic->getCtu(getCtuRsAddr())` and never
crossing a CTB boundary regardless of whether the neighbour has already been
decoded. This subsumes the picture-edge case for free (a QG at `x == 0`/
`y == 0` is trivially CTB-aligned). `qPY_PREV` itself resets to `SliceQpY`
at the very start of the slice and, since HM's own
`CUIsFromSameSliceTileAndWavefrontRow` requires equal CTU-row `y` when WPP
is active, at the start of every CTB row when `entropy_coding_sync_enabled_flag`
is set — mirrored in `decoder::decode_wpp_rows` by resetting `walk.qp_y_prev`
before each row's own CTU loop (the per-CTU QG reset above then re-derives
everything else fresh from that seed).

**`cu_qp_delta_abs`'s binarisation** is a context-coded truncated-unary
prefix (cMax = 5; bin 0 uses one context, every further bin shares a second)
followed, only on saturation, by a bypass-coded EGk(k = 0) suffix — HM's
`CU_DQP_TU_CMAX = 5`/`CU_DQP_EG_k = 0`, `TDecSbac::parseDeltaQP`. The prefix
is a direct, bin-for-bin port of HM's own `xReadUnaryMaxSymbol` (`ctu.rs`'s
`read_unary_max`) rather than a generic truncated-unary reading, because its
"always consume up to `max_symbol - 1` further bins, and only afterward
decide whether the last one means +1" shape does not fall out of the more
obvious early-return-on-cap reading. The suffix reuses
`vaco_codec_cabac::CabacDecoder::decode_bypass_egk`, already shared
CABAC-engine machinery, unmodified.

**Deblocking's own constant-QP assumption had to go.** `deblock.rs`'s
own module doc said outright that `qP_P == qP_Q` on every edge "since there
is no `cu_qp_delta`" — true when written, false the moment this landed, and
exactly the kind of stale-blocker doc `AGENT-CONSTRAINTS.md`'s "check a
recorded blocker before you accept it" section warns about, except here the
record was accurate *at the time* and simply needed updating alongside the
feature that invalidated it, which this pass did rather than leaving it to
rot. `qP_P`/`qP_Q` are now looked up per edge from `CuGrid::qp_at` (the same
grid `qPY_A`/`qPY_B` read) and averaged (`deblock::qp_avg`) exactly where HM
does — `iQP = (iQP_P + iQP_Q + 1) >> 1` — before, not after, the chroma QP
mapping/offset (`xEdgeFilterChroma`'s own `iQP = ((iQP_P + iQP_Q + 1) >> 1) +
chromaQPOffset`), confirmed against that same HM source. Boundary strength
is still always 2 (an unrelated consequence of this crate's intra-only
scope).

Verified against a **fully stock** `libx265` invocation (`-c:v libx265
-x265-params log-level=none`, keyed as all-intra only because inter
prediction is separately, pre-existingly out of scope — orthogonal to this
change): byte-exact on every sample of every plane of every frame at
640x480 and 320x240 (both regular CTU grids) and 300x500 (a partial last
CTU row *and* column), at the default CRF and at explicit `crf=15`/`crf=40`
(low- and high-QP extremes, to vary how often a quantisation group actually
codes a non-zero delta — `cu_qp_delta` is content-dependent the same way
the WPP entry-point bug above was, so a fixture that never exercises a real
delta would pass without proving anything), with WPP, SAO and deblocking
all left at their own defaults throughout. The pre-existing fixed-QP
regressions (`tests/oracle.rs::dense_content_is_byte_exact`, and the
320x240/640x480 real-binary checks with explicit `qp=`) are unchanged,
since a stream with `cu_qp_delta_enabled_flag` clear never resets
`cu_qp_delta_val` away from its `Ctx::new` initial `0`, and `derive_qp_y`
of a constant `qPY_PRED` with a `0` delta is that same constant.

`check_scope` no longer refuses `cu_qp_delta_enabled`.

## Reference picture management (§8.3.2 / §8.3.4), landed

Inter prediction needs the crate to know what a reference picture *is*
before it can decode a single motion vector against one, and until this pass
the crate had no such notion at all — no decoded picture buffer, no
reference picture set, no reference picture list. `src/dpb.rs` now
implements that layer on its own, ahead of `prediction_unit()`/merge/AMVP/
motion compensation (the rest of the P-slice stage — see "What remains, and
why this pass stopped here" below): §8.3.2's short-term reference-picture-set
derivation (`StCurrBefore`/`StCurrAfter`/`StFoll`, from a slice header's
already-parsed `ShortTermRps` — `vaco-parse-hevc` owns the syntax and the
delta-POC arithmetic of §7.4.8 already; this module only applies §8.3.2's
picture-order-count offsets), §8.3.4's reference picture list construction
(`RefPicListTemp0`/`RefPicListTemp1` cycling, `ref_pic_lists_modification()`
applied on top), and a real decoded picture buffer implementing Annex C's
informal "bumping" output-reordering process (smallest-POC-first, gated on
`sps_max_num_reorder_pics`, `sps_max_dec_pic_buffering` and, when indicated,
`sps_max_latency_increase_plus1`), including the IRAP
`no_output_of_prior_pics_flag` special case and end-of-stream flush.

**Long-term reference pictures are refused by name**, not approximated:
`derive_reference_pic_sets` returns `Error::Unsupported` the moment a slice
header names any. §7.4.7.1's `DeltaPocMsbCycleLt[i]` cumulative sum resets
not only at `i == 0` but also at `i == num_long_term_sps` — the boundary
between an SPS-predefined long-term entry and one a slice codes inline — and
`vaco_parse_hevc::SliceHeader::long_term_refs` merges both sources into one
`Vec` without recording where that second boundary falls (a deliberate
choice in that crate — see its own module doc). Guessing at the missing
boundary would be exactly the class of bug this crate's own history warns
about most directly: a formula that is right whenever a stream uses only one
source of long-term entries (the boundary is never reached) and silently
wrong the moment one mixes both — invisible until a fixture happens to
exercise the mixed case, the same shape as the DC-context and CTB-row-MPM
bugs above. Long-term references are also genuinely rarer in practice than
short-term ones; every fixture this crate has ever measured against, stock
`libx265` output included, uses short-term references exclusively.

Verified by unit test directly against the specification's own derivation
(`dpb.rs`'s own test module), the same standard this crate's `poc.rs`/`rps.rs`
sibling modules in `vaco-parse-hevc` are held to: reference-picture-set
splitting by used/not-used and before/after POC, list construction with and
without `ref_pic_lists_modification()`, temp-list cycling when more entries
are requested than exist, DPB marking (a picture dropped from every derived
set is removed; one kept only in `StFoll` survives), bumping triggered by
reorder count, by DPB fullness and by `max_latency_increase` independently,
IRAP `no_output_of_prior_pics_flag` both ways, and end-of-stream flush. This
is a different kind of verification than this crate's usual "byte-exact
against a real `libx265` decode" bar, deliberately: none of the above touches
a sample or a CABAC bin, so a real-fixture comparison would not exercise the
code any harder than a hand-derived scenario does. It also has no real-file
verification yet for the reason the next section states.

### Stage 2: P-slices, byte-exact — landed

A later pass built HM 18.0 from source natively (arm64 Darwin — the
prebuilt HM this environment ships is an x86-64 Linux ELF, unusable here;
building from source at `vcgit.hhi.fraunhofer.de` was the "future pass"
this section used to ask for) and used it as a bin-for-bin CABAC oracle to
implement `prediction_unit()` syntax (skip/merge/AMVP), merge and AMVP
candidate derivation (§8.5.3.2), motion compensation (8-tap luma / 4-tap
chroma separable filtering, §8.5.3.3), the inter CABAC context tables
(§9.3.2.2's P/B rows), and the `decoder.rs` wiring — POC (via
`vaco-parse-hevc::PocState`), `dpb.rs`'s reference-picture-set derivation
and reference-picture-list construction, and DPB storage/bumping — a P
slice needs that `dpb.rs` alone (landed in the previous pass) did not yet
have a caller for.

Two real, independently-committed bugs were found and fixed on the way,
both only visible by comparing *values* against HM rather than any
parse-level check:

- **`cbf_luma` parsed unconditionally for inter CUs.** §7.3.8.8 only
  parses it when `trafoDepth != 0 || cbf_cb || cbf_cr`; a root-level,
  all-chroma-zero leaf must infer it `1` instead. This desynced CABAC by
  one bin the first time a CTU's only-luma-residual case occurred — an
  intra CU's own leaf has no such condition, so Stage 1's I-slice-only
  decode never exercised it.
- **An inverted `Log2ParallelMergeLevel` MER-exclusion check** in
  `derive_merge_candidates`, which at the specification's own default
  (a 4x4 region — what almost every real encoder leaves it at) discarded
  the A1/B1/B0/A0/B2 spatial merge candidates for nearly every PU in the
  stream. This one kept CABAC entirely in sync (`merge_idx`/`mvp_idx`/`mvd`
  all parse identically to HM) and corrupted only the candidate *values*
  those parsed indices resolved to — invisible to any bin-count or
  bin-value check, visible only as wrong motion vectors and pixel drift
  compounding frame over frame.

Fixing both took a real `libx265` P-only fixture (320x240, testsrc2,
`no-sao=1:no-deblock=1:wpp=0:bframes=0:weightp=0:qp=32:keyint=25`) from
1,867,405/2,880,000 bytes exact (64.8% — wrong from frame 1 onward) to
**2,829,043/2,880,000 (98.2%) — exact through frame 1**, per plane, per
frame, whole-sequence.

**The TMVP-for-AMVP defect that used to stand between 98.2% and byte-exact
is fixed.** The repro that cornered it, kept verbatim because it is what
the fix was found from:

> CU (208, 24), frame 2 (POC 2) of the fixture above. AMVP path,
> `ref_idx_l0 = 1`, `mvd = (0, 0)`, `mvp_idx = 1` — every one of those
> bins matches HM exactly. The resolved predictor at slot 1 is `(0, 0)`
> in `vaco`; HM's is `(-32, 0)`. The spatial neighbours at this position
> (A1/A0 left group, B0/B1/B2 above group) are confirmed intra
> (unavailable) or exactly zero-motion on *both* sides, which rules out a
> plain spatial-candidate bug and points at the TMVP-for-AMVP path
> (`ctu::temporal_candidate` / `dpb::CollocatedMotionField`) — a predictor
> of `(0, 0)` where HM has a real, POC-distance-scaled `(-32, 0)` reads
> less like a miscomputed candidate than like a *dropped* one, falling
> through to the zero-candidate fallback.

**The leading hypothesis — that `temporal_candidate`'s naive `xPb +
nPbW`/`yPb + nPbH` bottom-right pixel arithmetic needed HM's z-scan-index
correction instead — is refuted.** A from-source HM 18.0 build (arm64
Darwin, Tier A) was instrumented directly in `TComDataCU::fillMvpCand`/
`xGetColMVP` (temporary, removed before this landed) to report the exact
pixel position it resolves for the bottom-right candidate and why
`xGetColMVP` succeeds or fails there. Control-tested first against the
already-byte-exact all-intra fixture (identical output, traced or not) and
hand-checked against `deriveRightBottomIdx`'s own Z-scan arithmetic by
hand before trusting the trace. On the documented repro, HM's own
bottom-right pixel position is `(216, 32)` — *exactly* `pu_x + pu_w`,
`pu_y + pu_h`, the same naive arithmetic this crate already used. The
positions were never wrong.

**The real bug**: HM's `fillMvpCand` always tries the bottom-right
position first when it is geometrically available (in the picture, same
CTB row as the PU) and falls back to the centre position whenever that
attempt *fails for any reason* — its own `if (ctuRsAddr >= 0 &&
xGetColMVP(...)) { bottom-right } else { centre }` does not distinguish
"geometrically unavailable" from "geometrically fine, but the position
names an intra block". `temporal_candidate` only ever tried the centre
position in the first case, so a bottom-right position that was in-bounds
but intra returned `None` outright — dropping the candidate instead of
falling back. The trace confirmed exactly this at the repro: HM's own
bottom-right lookup at `(216, 32)` in POC 1 (the collocated picture) fails
with `reason=notInter` (genuinely intra there), and its centre fallback at
`(212, 28)` succeeds, resolving `mv=(-16, 0)` scaled by `dist_scale_factor`
to the documented `(-32, 0)`. Fixed in `ctu::temporal_candidate` by always
falling back to the centre position whenever the bottom-right lookup — for
any reason — did not produce a candidate, matching HM's own
undifferentiated `else` branch (see that function's own doc for the fuller
account).

With that fix alone, the same P-only fixture decodes **2,880,000/2,880,000
(100%) byte-exact**, every plane, every frame.

**A second, independent defect surfaced immediately once deblocking was
re-enabled** (see "Re-enabling the disabled encoder features" below):
`deblock.rs`'s boundary-strength derivation was still the I-slice-only
constant `bS = 2` this module's own doc used to describe as permanent, and
— separately — a skip coding unit, and a merged (non-skip) coding unit
whose `rqt_root_cbf` parses to `0`, never called *any* edge-marking
function at all, since both bypass the transform tree entirely (the only
code that used to mark a deblocking edge). Found the same way as the TMVP
bug: an HM trace (this time of `TComLoopFilter::xGetBoundaryStrengthSingle`,
control-tested the same way) showed a real `bS == 1` edge at a position
this crate's own trace showed as unmarked altogether — not a
misclassification, an edge silently absent from consideration. Fixed by:

- `deblock::boundary_strength`, a real Table 8-12 derivation restricted to
  this crate's uni-prediction-only scope (either side intra → `2`; the
  edge is also a transform-block edge and either side's luma transform
  coded a non-zero coefficient → `1`; different reference pictures or a
  motion-vector component differing by 4 or more quarter-samples → `1`;
  otherwise `0`, and a `bS == 0` edge is now skipped entirely rather than
  filtered).
- `CuGrid::cbf_luma`/`fill_cbf_luma`/`cbf_luma_at`, recording each inter
  luma transform-unit leaf's own `cbf_luma` for that derivation's
  non-zero-coefficient term (only the inter path ever writes it — an
  intra edge's `bS` is already decided by the "either side intra" term
  before this would matter).
- `EdgeMarks::mark_tu_vert`/`mark_tu_horiz`, distinguishing a genuine
  transform-block edge from a plain prediction-unit-only one (the
  non-zero-coefficient term must not fire on a PU-only boundary,
  regardless of what either side's own, necessarily larger, unsplit
  transform block coded — confirmed directly against HM's own
  `xSetEdgefilterTU`/`xSetEdgefilterPU`, which only the former ever
  touches `m_aapucBS` for). `decode_skip_cu` and `decode_inter_cu`'s
  `rqt_root_cbf == 0` branch now call these directly on their own
  whole-CU extent (matching `TComLoopFilter::xDeblockCU`, which marks
  every coding unit's own transform-block edge unconditionally,
  regardless of skip status), closing the "never marked at all" gap.
- `decode_inter_cu`'s own PU loop also now marks each PU's own left/top
  edge as a plain (non-transform) filterable boundary, closing a separate,
  narrower gap: a CU whose `part_mode` splits into more than one PU but
  whose transform tree stays unsplit at that depth has an internal PU
  boundary with no transform-unit leaf of its own to mark it.

With both defects fixed, the same P-only fixture stays byte-exact with
deblocking re-enabled, and stays byte-exact through every other encoder
feature re-enabled on top of it (below).

### Re-enabling the disabled encoder features

The P-only fixture's `no-sao=1:no-deblock=1:wpp=0:weightp=0` flags were
the limit of what it could find, not evidence those paths were fine — each
was re-enabled in turn and re-verified against plain `ffmpeg`, 320x240,
25 real `testsrc2` frames, byte-for-byte, per plane, per the reusable
`verify_hevc_deblock.sh` harness:

- SAO alone (`no-deblock=1`, SAO at its own default-on): byte-exact.
- SAO + deblocking together (both at their own defaults): byte-exact —
  this combination is what surfaced the deblocking defect above; once
  fixed, exact.
- WPP added on top of both (`entropy_coding_sync_enabled_flag` at its own
  default-on, nothing else disabled): byte-exact.
- `cu_qp_delta` (a genuinely stock invocation implying CRF rate control —
  `-x265-params "log-level=none:bframes=0:weightp=0"`, no explicit `qp=`
  at all) alongside WPP/SAO/deblocking all at their own defaults:
  byte-exact at 320x240, 640x480 and 300x500 (a partial last CTU row *and*
  column).
- Widened content, since flat synthetic clips have hidden two H.264 bugs
  in this project's own history: an `mandelbrot` source (continuous
  zoom — real, non-block-aligned motion and detail throughout) and a
  scene-cut fixture (`testsrc2` concatenated with `smptebars` mid-GOP, so
  the cut itself is coded as a P-slice, not a new IDR) both decode
  byte-exact end to end at 320x240.
- **Weighted prediction (`weightp=1`) still refuses cleanly** —
  `pps.weighted_pred && hdr.kind == SliceKind::P` in `decoder.rs` — rather
  than silently misdecoding: HM's H.264 history already has one precedent
  for weighted prediction being entirely unimplemented and invisible on
  most content (neutral weights collapse to a plain copy), and this crate
  has never implemented §8.5.3.3.4's actual weighted-MC arithmetic at all
  (Stage 4, tracked separately — see "What was cut" below). A fully stock
  `libx265` invocation (`-x265-params log-level=none`, `bframes=0` aside)
  turns `weighted_pred_flag` on by default, so this refusal is what a
  literally-unmodified encode still hits — an honest, named gap, not a new
  one this pass introduced.

**`check_scope` no longer refuses P-slices.** `decoder.rs::decode_packet`'s
former "P-slices are not supported yet (TMVP-for-AMVP defect, see docs)"
refusal is gone; a P-slice now decodes through the same path this section
describes. B-slices (bi-prediction, combined bi-predictive merge
candidates) remain **not implemented at all** — `decode_packet`'s
"B-slices are not supported" refusal predates this pass and is unrelated.
Weighted prediction (Stage 4) remains refused by name, as above.

## Specification

`itu-t-h265-202108` (ITU-T Rec. H.265 (08/2021)) and
`hm-reference-software` (the HM reference decoder, BSD-3-Clause, Tier A
per `planning/AGENT-CONSTRAINTS.md`'s clean-room section — unlike
FFmpeg/x265, which stay Tier B), both recorded in
`provenance/sources.toml`. Mechanically-thresholded tables recorded in
`provenance/vaco-codec-hevc.toml`.
