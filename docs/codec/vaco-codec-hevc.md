# `vaco-codec-hevc`

Layer 4. HEVC/H.265 video decode (ITU-T H.265 (08/2021)) — I-, P- and
B-slices: NAL/VPS/SPS/PPS handling, the CTU quadtree, coding units, intra
prediction (planar/DC/33 angular modes, MPM derivation, reference sample
smoothing and strong intra smoothing), inter prediction (`prediction_unit()`
syntax, merge/AMVP and combined bi-predictive merge candidate derivation,
§8.5.3.3 uni- and bi-predictive motion compensation, the inter CABAC
context tables, `src/dpb.rs`'s reference picture management — see "Stage 2:
P-slices, byte-exact — landed" and "B-slices (...), landed" below), the
transform tree, residual coding, dequantisation, reconstruction, in-loop
deblocking (§8.7.2, see "Deblocking (§8.7.2), landed" below), SAO
(§7.3.8.3/§8.7.3, see "SAO (§7.3.8.3 / §8.7.3), landed" below), wavefront
parallel processing (§9.3.2.3, see "WPP (`entropy_coding_sync_enabled_flag`),
landed" below), per-CU adaptive QP (`cu_qp_delta`, §7.3.8.11/§8.6.1, see
"Per-CU QP delta (`cu_qp_delta`), landed" below) and uni- and bi-predictive
weighted prediction (§8.5.3.3.4.3, see "Weighted prediction
(§8.5.3.3.4.3), landed" and "B-slices (...), landed" below). Filter
suppression for protected I_PCM and transquant-bypass CUs shares one per-CU
mask. Filtering-disabled tiles without per-CU QP changes are decoded; other tile shapes and
every range-extension feature remain out of scope — see "What was cut" below. I_PCM is implemented both
with and without per-CU loop-filter suppression.

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
| `ctu.rs` | `coding_quadtree`/`coding_unit`/`transform_tree`/`transform_unit` (§7.3.8.4/.5/.8/.10), including §7.3.8.7 I_PCM sample decode/reconstruction, for I-slices; `coding_unit_p`/`decode_skip_cu`/`decode_inter_cu`/`transform_tree_inter` and `prediction_unit()` parsing (§7.3.8.5/.6/.9) for P-slices — see "Stage 2: P-slices, byte-exact — landed" below |
| `motion.rs` | Merge candidate derivation (§8.5.3.2.2/.3, spatial + temporal + zero-fill) and AMVP candidate derivation (§8.5.3.2.6/.7), plus the shared motion-vector scaling arithmetic (`dist_scale_factor`/`scale_mv`, §8.5.3.2.8) both use |
| `mc.rs` | Motion compensation: 8-tap luma / 4-tap chroma separable interpolation filters at quarter/eighth-sample precision (§8.5.3.3), exact HM fixed-point arithmetic; also `predict_block_intermediate` (the unweighted intermediate `predSampleLX`) and `apply_weight` (§8.5.3.3.4.3's explicit weighted formula), which `weight.rs` resolves weights for |
| `weight.rs` | Explicit weighted sample prediction (§8.5.3.3.4.3): resolves a slice's `pred_weight_table()` into a per-`ref_idx` `LumaWeightL0`/`ChromaWeightL0`/offset table — see "Weighted prediction (§8.5.3.3.4.3), landed" below |
| `residual.rs` | `residual_coding()` (§7.3.8.11) and its context derivations (`sig_ctx_inc`, `pattern_sig_ctx`, `sig_group_ctx_inc`, `context_set_index`, `read_coeff_remain`) — shared unchanged between I- and P-slices |
| `scan.rs` | `generate`/`generate_grouped` — up-right-diagonal/horizontal/vertical scan generation, including HM's `SCAN_GROUPED_4x4` sub-block-then-within-sub-block order |
| `cabac_ctx.rs` | `ContextBank` — CABAC context-init tables and `ContextBank::new(slice_qp)` (I-slice row) / `ContextBank::new_p_slice(slice_qp, cabac_init_flag)` (P-slice P/B rows, §9.3.2.2) |
| `dpb.rs` | `Dpb`/`ReferencePicSets`/`CollocatedMotionField` — reference-picture-set derivation (§8.3.2), reference-picture-list construction (§8.3.4), DPB output-reordering "bumping" (Annex C.3.2/C.5.2), and the compressed 16x16 motion field TMVP reads (§8.5.3.2.8/.9) |
| `intra_mode.rs` | MPM derivation (§8.4.2), `rem_intra_luma_pred_mode` resolution, chroma derived-mode (Table 8-2/8-3), mode-dependent scan-order selection |
| `intra_pred.rs` | Reference-sample line construction/substitution/smoothing, planar/DC/angular prediction (hand-rolled — see the module doc for why `vaco-codec-dsp-intrapred`'s `angular_project` does not fit HEVC's negative-angle extension) |
| `transform.rs` | Scaling-list resolution (§7.4.5), scaling/dequantisation (§8.6.3), the inverse-transform hand-off to `vaco-codec-dsp-idct::hevc` plus §8.6.5's caller-side `bdShift` |
| `framebuf.rs` | `Picture`/`Plane` (`ready: Vec<bool>` plus the current segment's CTB-range gate substitutes for z-scan availability — see its module doc for why that is exact, not approximate) |

## How it works

`HevcDecoder::send_packet` treats each packet as one container access unit
and hands it to the embedded `HevcParser::push_access_unit`, which walks
the packet's NAL units in whichever framing `set_extradata` resolved
(Annex-B or `hvcC`-length-prefixed), ingesting VPS/SPS/PPS into its own
`ParameterSets` and parsing the primary-coded-picture slice headers.
The decoder then re-scans the same NAL units (via `vaco_format_nalu::units`,
not a second parser) only to reach each segment's raw bits, which
`push_access_unit` does not hand back; resolves its PPS/SPS via
`parameter_sets().sps_for_pps`; runs `check_scope`; and walks each declared
CTB range in raster order via `ctu::decode_ctu` — `coding_quadtree` down to
`coding_unit`, which reads `part_mode`/`prev_intra_luma_pred_flag`/mpm-or-
rem/`intra_chroma_pred_mode` and then `transform_tree` (recursive,
carrying inherited `cbf_cb`/`cbf_cr` and the luma cbf context's
`trafoDepth == 0` special case) down to `transform_unit`, which predicts,
decodes residual coefficients, dequantises, inverse-transforms and adds. An
eligible `pcm_flag` takes the alternate §7.3.8.5 branch: byte-align, read raw
luma/Cb/Cr samples at the SPS-declared precision, scale them to picture bit
depth, and initialize only the arithmetic engine again before the next CU.

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
`separate_colour_plane`, SPS/PPS range extensions, SCC
extensions and all tile shapes outside the bounded filtering-disabled,
no-`cu_qp_delta` tiles-only path. Neither I_PCM (including
`pcm_loop_filter_disabled_flag`), transform skip, deblocking, SAO,
`cu_qp_delta_enabled`, P-slices, B-slices, nor
weighted (uni- or bi-predictive) prediction are on this list any more.
`transquant_bypass_enabled_flag` is also accepted and its per-CU syntax is
implemented — see the I_PCM/transquant-bypass section below.
`decode_packet`'s own former `SliceKind::B` refusal (it had no footprint
visible before the slice header is parsed, so it could not live in
`check_scope` either way — the same was once true of weighted prediction's
own `weighted_pred_flag` check, also removed) is lifted; see "B-slices
(...), landed" below. Both Annex-B and length-prefixed (`hvcC`) framing are
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
- **Extending scope** (tile shapes beyond the bounded no-`cu_qp_delta` path — inter prediction (P- and B-slices),
  deblocking, SAO, WPP, `cu_qp_delta` and weighted (uni- and bi-predictive)
  prediction are done, see their own sections above): the corresponding
  SPS/PPS fields already correctly return `Error::Unsupported` by name in
  `check_scope` when a real stream exercises them — implement behind that
  same call site rather than adding a new refusal path. Reference picture management
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

## The JCT-VC `HEVC_v1` subset, measured (46 streams)

Every "byte-exact" claim above is against `libx265` output. That is a real
reference and a narrow one. Read back out of the encodes themselves
(`ffmpeg 9.0.1`'s `libx265`, no `-x265-params`), a stock invocation writes
`amp_enabled_flag = 0`, `max_transform_hierarchy_depth_inter = 0`,
`max_transform_hierarchy_depth_intra = 0`, `cabac_init_present_flag = 0`,
`scaling_list_enabled_flag = 0`, `pcm_enabled_flag = 0` and
`transquant_bypass_enabled_flag = 0`, and leaves `--rect` off. So no fixture
built that way contains a non-`PART_2Nx2N` inter CU, a transform tree deeper
than one split, an 8x4/4x8 prediction unit, a scaling list, an I_PCM block or
a lossless CU. The 46-stream JCT-VC subset in `vaco-corpus`'s
`vaco-media.lock` (`jctvc` section) contains all of them.

Measured 2026-09-04 on `ffmpeg 9.0.1`, partitioning each raw Annex-B stream
with the production `HevcParser`/`ParserDriver`, feeding those access units to
`HevcDecoder`, and comparing every byte of every plane against
`ffmpeg -i <stream>.bin -f rawvideo -pix_fmt yuv420p`:

| result | streams |
| --- | ---: |
| byte-exact against `ffmpeg` on every frame | **39** |
| exact against the archive-published checksum | **1** |
| refused by name (`Unsupported`) | 6 |
| CABAC desync mid-stream | **0** |
| wrong pixels at the right length | **0** |
| wrong frame count | 0 |

Byte-exact: `amp-a`, `amvp-a`, `amvp-b`, `cip-a`, `cip-b`, `confwin-a`,
`entp-c`, `filler-a`, `initqp-a`, `ipred-c`, `merge-a`..`merge-e`,
`mvclip-a`, `mvedge-a`, `picsize-d`, `pmerge-a`, `pmerge-b`, `poc-a`, `ps-b`,
`rplm-a` (300 frames), `rps-a`, `rqt-a`, `sao-a`, `sao-g`, `slpplp-a`,
`struct-a`, `tscl-a`, `vpsid-a`, `nut-a`, `nooutprior-a`, `slist-c`, `ipcm-a`,
`ipcm-b`, `ipcm-c`, `ipcm-d`, `ipcm-e`.

Archive-exact: `vpsspspps-a`, whose six-picture published checksum is the
authoritative oracle because this session's `ffmpeg 9.0.1` emits only two of
its six independently parameterised pictures, as detailed below.

An earlier scratch-only raw harness reported that `initqp-a-sony-1`
desynchronized after 47 of 60 frames. That harness waited for the next first
VCL NAL before closing an access unit, so it attached the following picture's
AUD, PPS and prefix SEI to the preceding picture. `HevcDecoder` discovers
parameter sets across the supplied packet before decoding its slice, which
made the next picture's varying PPS active one picture early. This harness was
never committed and is not the raw demux path: the production parser applies
the non-VCL access-unit boundaries in H.265 §7.4.2.4.4. Through that path,
all 60 frames and all 8,985,600 output bytes are exact. A sibling scan found
post-VCL PPS NALs in `cip-a`, `initqp-a`, `nooutprior-a`, `nut-a`, and
`slist-c`; the corrected full sweep covers all five.

There are no remaining non-refusal frame-count failures in this subset.

### EOS-triggered CRA output discard

`NoOutPrior_A_Qualcomm_1` puts an end-of-sequence NAL unit immediately before
a CRA. Although this CRA's coded `no_output_of_prior_pics_flag` is `0`, the
EOS makes it a `NoRaslOutputFlag` random-access point. Its stored decoded
pictures are removed without output, and its seven associated RASL pictures
may be ignored because they can reference pictures no longer present.

The decoder previously did neither: its independent POC state never saw the
non-VCL EOS access unit, so the CRA was treated as ordinary, then all 50
input pictures were emitted. It now records EOS/EOB after parser processing,
resets the decoder-side POC/output state, discards the pre-CRA DPB output on
the next no-RASL IRAP, and skips its RASLs before reference lookup. Measured
2026-09-04 with the lockfile's SHA-256-verified vector: this decoder and
`ffmpeg 9.0.1` both output 40 832x480 yuv420p frames (23,961,600 bytes), with
every byte exact.

### BLA RASL output suppression

`NUT_A_ericsson_5` exercises the Table 7-1 access-point and leading-picture
NAL types in one stream. Before the §8.1/Annex C fix, the decoder stored every
`pic_output_flag` picture as displayable. That emitted two extra pictures: the
`RASL_R` and `RASL_N` pictures following `BLA_W_LP`. They must still be
decoded, and may remain usable as references, but a RASL whose POC precedes
the current IRAP with `NoRaslOutputFlag` set is not output.

`HevcDecoder` therefore remembers that IRAP POC and passes the derived output
eligibility, rather than raw `pic_output_flag`, to `Dpb::store`. The narrow
unit regression keeps the type/POC boundary explicit: RASL below the BLA POC
is suppressed; RADL and later trailing pictures remain output. The full vector
is corpus-owned rather than vendored into the crate. Measured 2026-09-04 with
the lockfile's SHA-256-verified `NUT_A_ericsson_5.bit`: this decoder and
`ffmpeg 9.0.1` both output 34 416x240 yuv420p frames (5,091,840 bytes), and
every byte matches.

### Independent and dependent slice-segment assembly

An access unit may contain independent slice segments and the dependent
segments belonging to them. The decoder parses every header before
reconstructing and validates strictly increasing CTB ranges. Each independent
segment starts a fresh CABAC context/QP state and defines a logical slice range;
a dependent segment inherits that active header, creates a new arithmetic
decoder for its own coded data, and carries the CABAC context, QP predictor,
and neighbour availability across its boundary. This follows §6.3.1,
§7.4.7.1, and §9.3.2.1/.5: a dependent segment is part of the same slice, not
a new unavailable-neighbour range.

The supported shape requires matching picture-wide decoding fields for every
independent segment and `slice_loop_filter_across_slices_enabled_flag` when
there is more than one independent non-WPP slice. Segment-local slice type,
QP, and motion-list state are refreshed at each independent boundary; filter
state remains picture-invariant because filtering is applied after the
complete walk. This is what permits the mixed I/P slices in the real fixture without
reusing an I-slice CABAC context for a P-slice. Independent WPP segments are
also supported when every segment begins and ends on a complete CTU-row
boundary and both SAO and deblocking are disabled. WPP segments that inherit a
dependent header, split a CTU row, or need in-loop filtering across their
boundary remain named refusals, as do tile pictures and independent headers
requiring distinct picture-wide context.

JCT-VC `HRD_A_Fujitsu_3` exercises four raster-contiguous independent row
segments per 416x240 picture; its vendored stream has 96 yuv420p frames and
the package's published visible-byte MD5
`f6d04dba2ef09bcadbea7b8ab5c8c917`. `DSLICE_A_HHI_5` exercises both kinds of
segment over 50 1920x1080 yuv420p frames. The checked-in 373,328-byte Annex-B
stream (SHA-256
`8398fb23c814a197bba497ad2c6103f81ca8003434fa40ec347d4d0a07c9468a`)
decodes to 155,520,000 visible bytes with MD5
`c7caf3164b0a316549ac7244f66f1294`, identical to both the package's `.md5`
and a local black-box `ffmpeg 9.0.1` decode measured on 2026-09-05.

`SLICES_A_Rovi_3` is the regression for per-segment state: each 640x480
picture has twenty independent four-CTB segments, alternating I and P slice
types. Its vendored stream is 65,943 bytes (SHA-256
`7440908beaa68768ee66b7af5823a28ce0716d90aa1b49de630d2c5aa555d955`) and
produces exactly nine frames / 4,147,200 visible bytes. The published and
black-box `ffmpeg` MD5 is `c2d63a4d145a5713afecd822032ec271`, and Vaco matches
the complete Y, U, and V byte stream.

### Picture-level short-term RPS across independent segments

Section 8.3.2 derives one RPS for a picture before any slice's reference-list
construction. Independent headers may select an SPS candidate or carry an
inline `st_ref_pic_set()` spelling; the decoder compares the resulting negative
and positive POC deltas and used-by-current flags, rather than rejecting an
equivalent syntax spelling. A different derived RPS remains a named refusal:
one `Ctx` and DPB-marking pass cannot safely represent per-slice reference
state yet. Long-term references remain separately refused.

JCT-VC `RPS_A_docomo_5` exercises short-term slice-header RPS
inter-prediction in the final three pictures of its 44-picture 416x240 stream.
The checked-in 64,948-byte Annex-B input has SHA-256
`e7a90335952dc5718d931adb461d90049eb558b4d08c90ff1706612f8bca4439`.
On 2026-09-05, this decoder and black-box `ffmpeg 9.0.1` each produced
6,589,440 visible yuv420p bytes with the archive-published MD5
`7f4ad6c6b3de54558b0db59629b87db9`.

### I_PCM and transquant-bypass decoding

`ipcm_A_NEC_3` isolated a scope refusal rather than a pixel bug: the decoder
rejected any SPS with `pcm_enabled_flag`, before §7.3.8.5 could decode a
single `pcm_flag`. The accepted subset now follows the specification's
distinct entropy path. `pcm_flag` uses the termination process
(§9.3.4.3.5); on `1`, the decoder verifies `pcm_alignment_zero_bit`, reads
§7.3.8.7's raster-order luma then Cb/Cr samples at `PcmBitDepthY/C`, applies
§8.4.1 equations 8-12/8-15/8-16, and initializes the arithmetic engine per
§9.3.2.6 without resetting the context models.

Measured 2026-09-04 against the SHA-256-verified JCT-VC archives and
`ffmpeg 9.0.1` direct Annex-B decode:

- `ipcm_A_NEC_3` (8-bit Y/C), `ipcm_B_NEC_3` (6-bit Y/C),
  `ipcm_C_NEC_3` (8-bit Y/C with per-PCM-CU loop-filter suppression), and
  `ipcm_E_NEC_2` (6-bit Y, 8-bit C) each produce exactly one 416x240 yuv420p
  frame, 149,760 bytes, byte-exact in every plane. The oracle MD5 values
  `8049988c383486e076ea2494edda3831`,
  `23a3b7024fd9bc64b946b9961ab0f51e`,
  `c3e74c399b73a5ab2dbd20523f583464`, and
  `e1cd7a16d3f6a342855044ccba3e41f5` respectively match the checksums shipped
  in the four JCT-VC archives.
- Before I_PCM decode landed, A/B/E stopped at the blanket SPS scope check and
  emitted no frame. Before per-CU filter suppression landed, C alone stopped at
  `pcm_loop_filter_disabled_flag` and emitted no frame. `tests/ipcm.rs` keeps A
  and C's bitstreams plus their 149,760-byte published-checksum references as
  durable regressions; the same test keeps D's exact transquant-bypass oracle.
- C's decoded `pcm_flag` footprints are painted once into `CuGrid` at 4x4 luma
  granularity. Deblocking applies §8.7.2.5.7/.8's P- and Q-side substitutions
  independently, so a protected side stays transmitted-value exact while its
  neighbour may still be filtered. SAO consults the same mask and leaves
  protected samples unchanged. The mask is the single source of truth for both
  I_PCM CUs protected by `pcm_loop_filter_disabled_flag` and CUs whose decoded
  `cu_transquant_bypass_flag` is `1`; prediction and entropy-neighbour queries
  remain unaffected. Narrow unit tests discriminate protected P, Q,
  both/neither sides and masked versus unmasked SAO samples.
- `ipcm_D_NEC_3` enables `transquant_bypass_enabled_flag` while leaving
  `pcm_loop_filter_disabled_flag` clear. The decoder reads each
  `cu_transquant_bypass_flag` before the CU's skip or prediction syntax
  (§7.3.8.5), suppresses `transform_skip_flag` and sign-data hiding, and maps
  the decoded coefficient levels directly to residual samples without scaling
  or inverse transform (§7.3.8.11, §7.4.9.5 and §8.6.4.1 equation 8-297).
  Range-extension residual rotation remains unavailable because range
  extensions are refused at the SPS.
- Measured 2026-09-04, the SHA-256-verified D vector and `ffmpeg 9.0.1` each
  produce one 416x240 yuv420p frame (149,760 bytes), byte-exact in every plane;
  MD5 `aa64a16240064bc2a90fadf979a62a7b` matches the archive's published value.
  `tests/ipcm.rs` vendors the bitstream and reference as the durable oracle.

### Scaling-list resolution and dequantisation

`ScalingMatrices::from_parameter_sets` in `transform.rs` is the single source
of truth for an active SPS/PPS pair. It applies §7.4.3.3.1's precedence (PPS
data, otherwise SPS data, otherwise defaults), resolves §7.4.5's default and
copy modes in matrix-id order, and maps `ScalingList` entries into raster
factors with the existing up-right diagonal scan generator. The 16x16 and
32x32 forms retain one 8x8 base plus their separately signalled DC value;
`factor` performs the 2x2 or 4x4 replication from equations 7-46 through 7-49
at lookup time. Because this value is built once with `CtxShared`, the four
intra/inter and luma/chroma residual paths cannot disagree about list
precedence, scan orientation, or defaults.

The implementation deliberately stores Table 7-6 in `ScalingList` order,
then uses the same `scan::generate(8, ScanOrder::Diag)` mapping used for
explicit lists. Do not replace those arrays with visually row-major matrices
without also removing that mapping. To add a supported chroma format, extend
the matrix selection for that format before lifting its scope check: 4:4:4
32x32 chroma uses §7.4.5 equations 7-50 and 7-51, which differ from this
crate's current 4:2:0-only path. Every new constant table of 32 or more values
also needs an entry in `provenance/vaco-codec-hevc.toml`.

`tests/scaling_list.rs` vendors JCT-VC `SLIST_C_Sony_4` as the durable
black-box regression. The SPS enables scaling lists but carries no list data;
picture 0's PPS also has no data and therefore selects the defaults, while
the next coded picture replaces the same PPS id with explicit custom
matrices. Their POCs are 0 and 8 in this hierarchical-B stream. The direct
`ffmpeg 9.0.1` yuv420p output frames 0 and 8 are 1,198,080 bytes with SHA-256
`bef0aacb39148117f740fa53af9714c32092196dfef6bb126e7b45dea827f535`.
The complete reference decode has 65 832x480 frames (38,937,600 bytes),
SHA-256 `9056adddfacd0fa6fb14014ea4a8dc4920650144c04888005804b35ab2c7fa8e`,
and the archive-published MD5 `61024c25cbd60e9bf86dbe3bc5b9b48b`.

The second scaling-list vector, `VPSSPSPPS_A_MainConcept_1`, needs a split
oracle recorded rather than false ffmpeg-byte-exactness. Its archive declares
six I pictures with distinct VPS/SPS/PPS ids and luma geometries 176x144,
320x240, 352x288, 640x480, 704x576, and 1280x720; their concatenated yuv420p
reference is 2,756,736 bytes with published MD5
`1ddf74263cb4953cfdfcf99c563d88ea`. On this stream `ffmpeg 9.0.1` reports
missing parameter sets and emits only two 352x288 frames (304,128 bytes),
SHA-256 `db4c4554fdd3eb11325d9c6de6db4a508da666c739e3c59bf7b37fa3bde3d4a1`,
not the six-picture reference. That is an oracle limitation, not a target
behaviour: the decoder is checked against the archive geometry/checksum and
permitted HM reference behaviour,
while `SLIST_C` remains the ffmpeg byte-for-byte scaling-list oracle.

### AMVP motion-vector sum wrapping

ITU-T H.265 (08/2021) §8.5.3.2.1 equations 8-94–8-97 do not clamp
`mvpLX + mvdLX`: each component is reduced modulo `2^16`, then interpreted
in the signed range `-32768..=32767`. `Mv::add_mvd` is the single source of
that operation, and both list branches in `ctu::decode_inter_cu` use it.
The focused unit test crosses both signed boundaries; replacing the reduction
with plain addition makes it fail with `(32776, -32776)` instead of
`(-32760, 32760)`.

The discriminating real stream is JCT-VC `MVCLIP_A_qualcomm_3`. Without the
wrap it still decodes five 416x240 4:2:0 frames and exits normally, but
443,454 of 748,800 output samples differ from `ffmpeg 9.0.1`, beginning at
frame 1. With the specified wrap, all five frames and all 748,800 bytes are
exact. A crate-wide search found exactly two `mvp + mvd` construction sites,
the L0 and L1 branches now routed through `Mv::add_mvd`; merge candidates and
scaled temporal candidates do not perform that addition.

The arithmetic regression is covered without external data by the focused
`Mv::add_mvd` unit test. A durable end-to-end `MVCLIP_A_qualcomm_3` regression
would require vendoring its 100,306-byte bitstream or making the crate test
network-dependent; the corpus conformance case is `tier = "full"` and is not
available to ordinary offline crate tests. A skip-when-missing test would not
protect CI, so none is added.

### Why the `jctvc-conformance` suite measures less than this

`tests/conformance/transcode/hevc-jctvc-conformance.toml` wraps each raw
bitstream in MP4 with the reference's own `-fflags +genpts -i <stream> -c
copy -f mp4`, because `vaco`'s raw-elementary-stream demux assigns no packet
timestamps (the manifest header says so). That wrap is **not lossless**:
running the manifest's exact command and then decoding both the wrap and the
original with `ffmpeg 9.0.1` itself, **35 of the 46 wraps decode to a
different number of frames than the bitstream they came from** — `rqt-a`
2 -> 1, `poc-a` 5 -> 2, `rplm-a` 300 -> 95, and `cip-a-panasonic-3` to zero
frames. `-f nut`, `-f matroska` and `-f mpegts` were tried and are no
better (35, 46 and 46 lossy respectively; the last two refuse the copy
outright). So a case that agrees there agrees on a *subset* of its stream,
sometimes an empty one, and this table -- taken from the bitstreams directly
-- is the number to trust.

The suite itself does not read green: with the fixes above,

```sh
VACO_BIN_VACO=<your build> VACO_CORPUS_NETWORK=1 \
  vaco-conformance run --suite jctvc-conformance --tier full
# 46 cases: 4 agreed, 0 allowed, 42 diverged, 0 failed, 0 skipped
```

Of those 42, 13 are the honest `Unsupported` refusals above and 23 exit
183. At least **21 of them never reach a pixel comparison at all**, dying in
`vaco-format-core::time::check_monotonic` ("non-monotonic dts") because the
pictures the wrap dropped leave `dpb`'s bumping emitting POCs out of order,
while the previous text described the other two as real CABAC desyncs. The
corrected raw sweep has zero CABAC desyncs, and the committed MP4 harness cannot
validate that earlier classification: on the current build its lossy
`initqp-a` wrap writes two frames (299,520 bytes) before failing the
monotonic-DTS check, while the same raw stream is exact for all 60 frames
through the production parser. Only 6 cases get as far as comparing bytes.
What does hide the result is the tier: the suite is `tier = "full"`,
so CI's `--tier core` job never selects it, and nothing in CI reports these
42.

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
- **At registration, SAO was refused instead of merely left unapplied.**
  `sample_adaptive_offset_enabled_flag` was not checked in `check_scope`, so
  a stream that turned SAO on (the common case — most encoders leave it on)
  hit exactly the same full-decode CABAC desync as the bug above, from the
  first CTU that merged or set an offset, because the crate then parsed none
  of §7.3.8.3's per-CTU `sao()` syntax. The registration-time fix refused it
  by name (`Error::Unsupported`) at the SPS. That interim refusal was removed
  when SAO parsing and filtering landed; current behavior is documented in
  "SAO (§7.3.8.3 / §8.7.3), landed" below.

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

`wpp=0`/`no-sao=1`/`no-deblock=1`/constant `qp=` (CQP, not CRF) were the
registration-time fixture's isolation settings, not the decoder's current
restrictions. At that checkpoint WPP, SAO and `cu_qp_delta` (which CRF rate
control implies) were refused cleanly, while deblocking was disabled at the
encoder because it has no bitstream footprint that the decoder could refuse.
All four subsequently landed; their sections below record the current scope
and measurements.

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

## `transform_skip_flag` (§7.3.8.11 / §8.6.4.2), landed

The flag was already parsed at all four residual call sites and then refused
by name whenever it decoded to `1`. Measured against the 46-stream JCT-VC
`HEVC_v1` subset `vaco-corpus`'s `vaco-media.lock` registers, that one
refusal was what **26 of 46** streams hit first — more than every other
refusal in that corpus put together.

`transform::inverse_transform` now takes a `TransformKind`
(`Dct`/`Dst4`/`Skip`) instead of a `use_dst: bool`, so "DST-VII *and*
transform-skip" is not a state a caller can construct. The `Skip` arm is
§8.6.4.2's `r[x][y] = d[x][y] << tsShift` with `tsShift = 5 + Log2(nTbS)`,
falling into the same §8.6.5 `bdShift` tail every other branch uses. At
8-bit 4x4 the pair collapses to `(d + 16) >> 5`, which is HM 18.0's own
`TComTrQuant::xITransformSkip` shift (`MAX_TR_DYNAMIC_RANGE - bitDepth -
log2TrSize`) — that identity is how the arithmetic was checked before it was
measured. The `extended_precision_processing_flag`-dependent
`Min(5, bdShift - 2)` alternative is unreachable: that flag is an SPS range
extension `check_scope` refuses.

Verified byte-exact against plain `ffmpeg` on real `libx265 --tskip=1`
output: `tests/tskip.rs` (64x64, 2 IDR frames, in-loop filters off at the
encoder so a residual regression cannot hide behind a filtering one) and,
out of tree, a 192x128 all-intra encode and a 192x128 I/P/B encode with
`bframes=3`, both byte-identical on every sample of every plane of every
frame. That the fixture genuinely reaches the path was itself measured, not
assumed: making `read_transform_skip_flag` refuse on a decoded `1` and
re-running makes `tests/tskip.rs` refuse on its first access unit while
every other fixture in the crate decodes unchanged.

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
is a performance-only difference, not an algorithmic one). Its supported
multi-slice shape requires loop filtering across slice boundaries, so the
picture-wide filtering pass may use a neighbour in another segment; the
earlier CABAC `sao()` merge syntax is separately gated to the current
segment.

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
rather than reset. A dependent segment that begins part-way through a row also
inherits the preceding segment's final context and `qPY_PREV` state under
§9.3.2.5/§8.6.1; the predictor still resets at the first CTB of every later
WPP row.

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

### Independent WPP slice segments — bounded row-aligned shape

Independent WPP segments carry their own `entry_point_offsets` table. Those
offsets count only the segment's coded row substreams, so two otherwise
matching headers may legitimately contain different offset values or even a
different number of entries. The decoder now compares the shared picture state
while ignoring that per-segment table, then decodes each segment's local row
window at its actual raster-row origin. Each segment restarts CABAC from its
own header, while rows within that segment retain the existing §9.3.2.3
second-CTU context handoff.

The checked-in `tests/wpp_multislice.rs` fixture is a real two-frame 256x256
`libx265` stream with `wpp=1:slices=2:no-sao=1:no-deblock=1`. Each picture has
two independent segments covering two complete CTU rows each. The stream is
11,927 bytes with SHA-256
`75dbd6e7e7659e26de62c96ff03b8e219ea2fc8107e69e5895a7df5adaba9354`; black-box
`ffmpeg` decoding yields exactly 196,608 visible bytes with MD5
`138d30492cca3f85709c514b8b4d9bac`, and Vaco matches both the frame count and
the digest. Non-row-aligned WPP remains refused by name because its neighbour
state needs a separate proof.

### Dependent WPP slice segments — one-CTU-wide conformance shape

The registered corpus entry `jctvc-hevc-wpp-d-ericsson-main-2`
(`WPP_D_ericsson_MAIN_2`) exercises WPP in a one-CTU-wide
picture, including independent and dependent segments, variable one- and
two-row segment lengths, per-row entry-point substreams, and slice-header
extension bytes. Dependent headers inherit the active slice syntax; their
entry-point and extension syntax is still parsed, and each WPP row starts the
CABAC context prescribed by §9.3.2.3. A one-CTU-wide segment with no entry
points is one continuous substream across its rows, so its CABAC state remains
continuous until the segment terminator.

The checked-in 22,474-byte Annex-B stream (SHA-256
`30eec63f2324aa982fb91bd4c1c551c833c253ba711ee131aa9cc4d322398caf`) decodes
to 48 64x240 yuv420p frames, exactly 1,105,920 visible bytes. The archive's
published and Vaco/black-box reference MD5 is
`f710612103f386c415be3e6300693451`, and the complete Y, U, and V byte stream
matches. WPP segments that are not row-aligned remain named refusals except
for the bounded two-CTU-wide partial-row shape exercised below; filtered
boundaries without cross-slice filtering remain refused.

### Dependent WPP slice segments — bounded partial-row shape

The registered corpus entry `jctvc-hevc-wpp-e-ericsson-main-2`
(`WPP_E_ericsson_MAIN_2`) exercises the next bounded shape: a two-CTU-wide
picture whose dependent segments may split the first row at CTU address one,
then continue through complete rows. At each dependent boundary the decoder
preserves both the CABAC context bank and the running QP predictor; a new row
still uses the WPP context saved after the preceding row's second CTU and
resets `qPY_PREV` to `SliceQpY`.

The checked-in 29,642-byte Annex-B stream (SHA-256
`c8fe49762a13e1cc2b033308bb22aeb35da0c96f94ca8ef7d87d95bdadaeaa2c`) decodes
to 48 128x240 yuv420p frames, exactly 2,211,840 visible bytes. The archive's
published and Vaco/black-box reference MD5 is
`485798dbf95ad61232075df2f294aa3f`, and the complete Y, U, and V byte stream
matches. Other non-row-aligned WPP boundaries and tile pictures remain named
refusals.

The companion `jctvc-hevc-wpp-f-ericsson-main-2` (`WPP_F_ericsson_MAIN_2`)
fixture proves the same state split at three CTUs per row: its dependent
segment continues from CTU zero through CTUs one and two, then the next
dependent segment begins at the following row and loads the saved second-CTU
context. The checked-in 31,461-byte stream (SHA-256
`e8566e0e48509592dfaf7d314b7e292f51cede045559e3c32b447a9822a8b949`) decodes
to 48 192x240 frames and 3,317,760 exact yuv420p bytes, MD5
`2aaf16274fe8e799d72fa08a4963850d`.

The `jctvc-hevc-wpp-a-ericsson-main-2` (`WPP_A_ericsson_MAIN_2`)
fixture extends that proof to seven CTUs per row (416x240). Its 48 pictures
use a finite set of arbitrary partial-row boundaries, mixing dependent
segments with independent segments that restart CABAC at those boundaries.
The checked-in 67,554-byte stream has SHA-256
`54d896d9fbdfa0aae15629001105c6ee132c8459e152abb06efc62cead4324ae` and
decodes to exactly 7,188,480 yuv420p bytes. Vaco matches the archive's
`cd7e815eb47e8138fec2185d4de84304` MD5 and every Y, U, and V byte. Other
non-row-aligned WPP boundaries and tile pictures remain named refusals.

The `jctvc-hevc-wpp-b-ericsson-main-2` (`WPP_B_ericsson_MAIN_2`) fixture
extends the same bounded proof to thirteen CTUs per row (416x240). Its 48
pictures use the measured mixed dependent/independent boundary patterns,
including segments that begin and end part-way through a row. The checked-in
68,895-byte stream has SHA-256
`f1d18f737a9380f6ce58b6aeae158af458c5b557e06dd24977b7a5c6e095b9b2` and
decodes to exactly 7,188,480 yuv420p bytes. Vaco matches the archive's
`e37c7e561a1226640a7bf98e81df78b1` MD5 and every Y, U, and V byte. WPP-C and
other unproven non-row-aligned boundaries remain named refusals.

The `jctvc-hevc-wpp-c-ericsson-main-2` (`WPP_C_ericsson_MAIN_2`) fixture
extends the same bounded proof to twenty-six CTUs per row (416x240). Its 48
pictures use the measured mixed dependent/independent boundary patterns,
including row-spanning segments and regularly row-aligned segments. The
checked-in 71,856-byte stream has SHA-256
`21cf0a7c5f6fba5a76c7132a2714313d120d1443a0130400fb55ab1e454d5bda` and
decodes to exactly 7,188,480 yuv420p bytes. Vaco matches the archive's
`e067aa3a6a12cd5743849ded793c8d3f` MD5 and every Y, U, and V byte. Other
unproven non-row-aligned boundaries remain named refusals.

## Multi-row tile reconstruction

`TileLayout` validates the PPS geometry and maps tile IDs to half-open CTB
rectangles. The decoder accepts one independent, full-picture tiles-only slice
with SAO and `cu_qp_delta` disabled. It partitions the escaped
slice payload at §7.4.7.1 entry points, de-escapes each tile range
independently, and initializes fresh arithmetic and CABAC context state for
every tile. CABAC states are suspended between tile rows so CTUs reconstruct in
picture-raster order; that preserves the existing reconstruction, edge, CU and
SAO row-publication contract while `Ctx` narrows syntax-neighbour availability
to the active tile. The no-`cu_qp_delta` boundary makes the slice QP predictor
constant while the tile states are interleaved.

The fixture in `tests/tiles.rs` is a 1,813-byte 512x64 HM 18.0 IDR with two
uniform tile columns. It validates the geometry and CABAC boundaries, then
decodes the whole access unit and compares the 49,152 visible yuv420p bytes
byte-for-byte with an independently generated ffmpeg reference (MD5
`6ccc33b0cd92240a275d30a05de031cc`).

The regular deblock pass derives each filtered edge's two CTBs and suppresses
only a cross-tile edge when PPS
`loop_filter_across_tiles_enabled_flag` is clear; edges within a tile and all
cross-tile edges when it is set keep the ordinary filtering path. Tile SAO is
still a named refusal because its snapshot/filter neighbourhood rules have not
yet been made tile-aware.

With WPP, the decoder accepts the same one independent full-picture tile slice.
It derives one escaped substream per picture-row/tile-column, de-escapes every
range separately, restarts arithmetic decoding for each, and carries a tile's
CABAC context from its local second CTU to its next row. The outer loop still
publishes the reconstruction state once per picture row. Multiple or dependent
tile slices, tile pictures with `cu_qp_delta`, and tile pictures requiring SAO
remain named refusals.

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
- **Weighted prediction (`weightp=1`) is now implemented** — see "Weighted
  prediction (§8.5.3.3.4.3), landed" below. At the time this bullet was
  written it still refused cleanly instead
  (`pps.weighted_pred && hdr.kind == SliceKind::P` in `decoder.rs`), the
  same posture H.264's own history warns is dangerous to leave
  unimplemented (neutral weights collapse to a plain copy, hiding the gap
  on most content) — kept here as the historical record of what a fully
  stock `libx265` invocation used to hit next, once P-slices themselves
  stopped being the blocker.

**`check_scope` no longer refuses P-slices.** `decoder.rs::decode_packet`'s
former "P-slices are not supported yet (TMVP-for-AMVP defect, see docs)"
refusal is gone; a P-slice now decodes through the same path this section
describes. B-slices (bi-prediction, combined bi-predictive merge
candidates) remain **not implemented at all** — `decode_packet`'s
"B-slices are not supported" refusal predates this pass and is unrelated.
Weighted prediction no longer refuses either — see the next section.

## Weighted prediction (§8.5.3.3.4.3), landed

The last thing standing between this crate and a fully stock `libx265`
invocation staying P-only (`-x265-params log-level=none:bframes=0` — B-slices
are separately, pre-existingly out of scope, see above): `libx265` turns
`weighted_pred_flag` on by default, so any P-slice stream decoded through the
default (non-weighted) §8.5.3.3.4.2 path whenever an encoder happened to pick
neutral weights, and refused outright the moment one didn't.

**`pred_weight_table()` (§7.3.6.3) was already parsed, and thrown away** —
exactly the shape of bug this crate's own docs warn about from H.264's
history (see "A specific warning" in the brief that started this pass, and
the "Re-enabling the disabled encoder features" bullet above): a stream
description has to be able to describe the table, so `vaco-parse-hevc`
parsed it in full (`vaco_parse_hevc::slice::PredWeightTable`,
`SliceHeader::pred_weight_table`) well before anything in this crate applied
it. `weight_table_exact` (that struct's own field) is always `true` in this
crate's scope: the one condition that can make the parse inexact
(`pps_curr_pic_ref_enabled_flag`, a screen-content-coding flag) lives behind
the SCC extension, which `check_scope` already refuses outright.

**What was added**: `src/weight.rs` resolves a slice's `PredWeightTable` once
per slice into a flat per-`ref_idx` table of `LumaWeightL0`/`ChromaWeightL0`
and `luma_offset_l0`/`ChromaOffsetL0` (§8.5.3.3.4.3's own derivation,
including the `ChromaOffsetL0` clip formula involving `WpOffsetHalfRangeC`);
`src/mc.rs` gained `predict_block_intermediate` (§8.5.3.3.3's own
`predSampleLX`, stopping one clause earlier than `predict_block`'s existing
folded shift/clip — see that module's own doc for why `predict_block`
itself could not be reused: its single-pass branches fold the *default*
clause's final shift into the interpolation, which is only valid when no
weighting follows it) and `apply_weight` (§8.5.3.3.4.3's uni-predictive
formula itself); `ctu::build_cu_prediction` branches per PU on whether
`InterSliceParams::weights` is `Some` (resolved once in `decoder.rs`,
exactly when `weighted_pred_flag && slice_type == P`) and, when it is, looks
up the reference's own weight via a new `InterSliceParams::ref_idx_for_poc`
before calling the intermediate/weighted path instead of `predict_block`.
The default (non-weighted) path is untouched: `predict_block` itself was not
modified, and every non-weighted call site still calls it exactly as before.

**One known, narrow approximation, inherited from an existing one**:
`MotionInfo` carries a resolved POC rather than a `ref_idx` (a deliberate
choice recorded in `motion.rs`'s own doc, since within one slice
`RefPicList0` is shared and fixed, so the two normally carry the same
information for every comparison this crate's merge/AMVP clauses actually
need). Weighted prediction is the one place that equivalence has a gap:
§8.3.4's `RefPicListTemp0` cycling can place the same POC at more than one
list position when fewer distinct reference pictures exist than
`num_ref_idx_l0_active_minus1 + 1` requests, and `LumaWeightL0`/
`ChromaWeightL0` are addressed by `ref_idx`, not POC. `ref_idx_for_poc`
resolves to the *first* matching position — the same convention
`plane_for_poc` already uses for picture lookup — exact whenever a POC
appears once in the list, and a narrow approximation (picking one of several
equally valid list positions, all naming the same picture) in the cycling
case. No real fixture measured for this pass exercises that case.

**Verified against real `libx265` output with genuinely non-neutral
weights, confirmed by inspecting the parsed table before trusting any byte
comparison** — the exact discipline this pass's own brief demanded, after
H.264's own history of an entirely unimplemented weighted-prediction path
staying invisible because most content happens to get neutral weights.
Temporary instrumentation (an `eprintln!` of the parsed `PredWeightTable`,
gated on an env var, removed before committing) confirmed real deltas on
every fixture below before the byte comparison ran — for example
`luma_log2_weight_denom = 5, delta_luma_weight_l0[0] = -3` (`w = 29`) on a
`life` fixture, and `luma_log2_weight_denom = 5, delta_luma_weight_l0[0] =
15, luma_offset_l0[0] = -7` (`w = 47`) on a `testsrc2`-with-`fade` one — both
far from the neutral `w == 1 << denom, o == 0` case that a formula bug could
hide behind. Measured with `weightp=1` forced explicitly (`life` is not
guaranteed to pick non-neutral weights on every encode, so this was checked
per-run, not assumed) and, separately, at `libx265`'s own default
`weightp=1` on `testsrc2` (which turned out to carry only neutral weights —
a fixture worth naming as a *negative* result: it passing proves nothing
about the weighted arithmetic, only that the neutral path still collapses
to the default one, the same algebraic identity `weight::tests::
a_neutral_weight_collapses_to_the_default_shift_and_offset` checks directly):

- `life` (320x240, `mold=10`), weighted prediction alone
  (`bframes=0:weightp=1:no-sao=1:no-deblock=1:wpp=0:qp=30:keyint=25`,
  isolating weighted prediction from every other loop-filter/QP feature):
  **2,880,000/2,880,000 bytes exact** (100%), every plane, every frame.
- The same `life` fixture with SAO, deblocking and WPP restored to their own
  defaults on top: still 100% byte-exact.
- The same again with CRF rate control restored too (implying
  `cu_qp_delta`, `-x265-params "log-level=none:bframes=0:weightp=1:
  keyint=25"`, no explicit `qp=` at all — weighted prediction, SAO,
  deblocking, WPP and `cu_qp_delta` all simultaneously at their own
  defaults): 100% byte-exact, and confirmed to still carry genuinely
  non-neutral weights (23 of 24 P-slice `pred_weight_table`s had at least
  one non-neutral entry).
- The same CRF+weighted-prediction fixture at 352x288 and at 300x500 (a
  partial last CTU row *and* column): 100% byte-exact at both.
- A `testsrc2`-with-`fade=t=in` fixture (a second, independently-generated
  source of non-neutral weights, with larger deltas than `life` produced —
  `delta_luma_weight_l0[0]` up to `+15`): 100% byte-exact.
- `life` at 640x480 (a regular, larger CTU grid): 100% byte-exact.
- Regression check, non-weighted path: 320x240, 416x240, 640x480, 352x288
  and 300x500 with `weightp=0` (everything else at its own default) are all
  still 100% byte-exact — `predict_block`'s own arithmetic was not touched,
  and this confirms nothing broke it.

`check_scope` no longer refuses weighted prediction; the check itself never
lived at `check_scope` (it was a `decode_packet` slice-kind/PPS-flag check,
alongside the B-slice refusal, since `pred_weight_table`'s presence has no
SPS/PPS-level footprint before the slice header is parsed) and is simply
gone now, not replaced.

A fully stock, completely unmodified `libx265` invocation
(`-x265-params log-level=none`, no other flags) still does not decode
end-to-end in this environment — not because of weighted prediction, but
because this `ffmpeg`/`libx265` build's own default GOP structure inserts a
B-slice as the very first non-IDR picture after the anchor P-frame in
decode order (`I, P, B, B, B, P, ...` in decode order for a 320x240
`testsrc2` clip), and B-slices remain refused, unrelated to this pass. Before
this pass, that same file hit the weighted-prediction refusal at the anchor
P-frame (decode order position 1) before ever reaching the B-slice check;
after this pass, that P-frame decodes correctly and the pipeline reaches the
B-slice refusal one picture later instead — real progress, though it does
not change the end-to-end frame count for a literally-unmodified invocation,
since `vaco-cli`'s pipeline does not flush any already-decoded picture once
a later one errors and the reorder buffer has not yet bumped anything by
that point. `-x265-params log-level=none:bframes=0` (the same "stock except
B-slices, a pre-existing and unrelated gap" framing every earlier stage in
this document uses) decodes 100% byte-exact, 2,880,000/2,880,000 bytes,
though — as measured above — that particular invocation's own default
weights turn out to be neutral on `testsrc2`, so the `life`/`fade` fixtures
above are what actually exercise the new arithmetic.

## B-slices (§7.3.8.6/§8.5.3.2.4/.9/§8.5.3.3.4.2-.3), landed

The last blocker to a fully stock `libx265` invocation (no `-x265-params`
restrictions at all) decoding end-to-end: `libx265`'s default GOP inserts a
B-slice as the very first non-IDR picture after the anchor P-frame, and
`decode_packet` refused every B-slice outright (see "A fully stock,
completely unmodified `libx265` invocation" at the end of the weighted
prediction section above for what that refusal used to hit).

**What was added, on top of everything the P-slice/weighted-prediction
passes already built**: `inter_pred_idc` parsing (§7.3.8.6,
`ctu::parse_inter_pred_idc`, its own three-context CABAC binarization) and
the rest of a B-slice's `prediction_unit()` — `ref_idx_l1`,
`mvd_coding(x, y, 1)`, `mvp_l1_flag` — in `decode_inter_cu`'s AMVP branch;
`RefPicList1` construction, already generic over list index in
`dpb::build_ref_pic_lists` (an `is_b` parameter it already accepted, unused
until now); combined bi-predictive merge candidates (§8.5.3.2.4,
`motion::derive_merge_candidates`'s `is_b` branch, with the priority tables
`PRIORITY0`/`PRIORITY1` HM's own `getInterMergeCandidates` uses) and a
corrected zero-candidate fill (§8.5.3.2.5's `zeroIdx` clamps at `0` once it
passes `numRefIdx`, it does not wrap modulo — a bug this pass found and
fixed in the existing P-slice zero-fill code too, see
`a_b_slice_zero_fill_clamps_at_zero_rather_than_wrapping` and its P-slice
sibling test); `collocated_from_l0_flag`-aware temporal motion vector
prediction (§8.5.3.2.9's `colList` selection —
`is_low_delay ? targetList : (collocated_from_l0 ? L1 : L0)`, falling back
to the other list when the primary one has no motion at that position,
`ctu::col_mvp`); AMVP's own-list-then-other-list neighbour search (matched
by POC, not list identity — `RefList::pick`/`other`); default
(unweighted) and explicit-weighted bi-predictive motion compensation
(§8.5.3.3.4.2/.3, `mc::default_biprediction`/`apply_weight_bi`, extending
`mc.rs`/`weight.rs`'s existing uni-predictive implementation rather than
duplicating it — `weight::resolve_list` replaces the old L0-only
`resolve_l0`); B-slice CABAC context initialization (§9.3.2.2,
`cabac_ctx::ContextBank::new_b_slice` — a P-slice's default `cabac_init_flag`
row and a B-slice's are *opposite*: `new_p_slice` uses
`usize::from(cabac_init_flag)`, `new_b_slice` uses
`usize::from(!cabac_init_flag)`); and a full §8.7.2.4 Table 8-12
boundary-strength derivation for deblocking across bi-predicted edges
(`deblock::boundary_strength`, porting HM's `xGetBoundaryStrengthSingle`'s
same-ref-set check and its "different L0/L1" vs "same L0/L1" comparison
branches, rather than the uni-prediction-only formula the P-slice pass
shipped).

**Two real, independent defects surfaced only by genuine hierarchical-B
content** (invisible under every existing P-slice/intra fixture), both
found via an instrumented repro against a real `bframes=3` `libx265` file
that came out of the decoder with non-monotonic presentation timestamps:

1. `Dpb`'s latency bound (`SpsMaxLatencyPictures`, §C.5.2.1) omitted the
   `sps_max_num_reorder_pics` term, computing it as bare
   `max_latency_increase_plus1 - 1`. Fixed in `Dpb::new`.
2. Bumping itself was a single unified reorder/latency/capacity check, but
   Annex C specifies two distinct phases: C.5.2.2 pre-decode bumping
   (reorder, latency, *and* capacity, evaluated on the DPB state *before*
   storing the current picture) and C.5.2.3 post-decode "additional
   bumping" (reorder and latency only, evaluated *after* storing, with
   `PicLatencyCount` incremented only for pending pictures whose POC
   follows the just-stored picture's in output order). `bump_before_storing`
   is now `bump_pre_decode` (called before `store`) plus
   `bump_post_decode(current_poc)` (called after), both delegating to a
   shared `bump_while`.

Combining L1 motion arrays into `CuGrid` for B-slices also exposed a
pre-existing, crate-wide gap: `vaco_limits::Budget` charges allocations but
this crate never released them anywhere, so a structurally larger B-slice
DPB footprint (and, transiently, even doubling every P/I slice's `CuGrid`
before a `has_l1` gate was added) pushed a 640x480 stock fixture over
`max_alloc_total`. Fixed by threading `&mut Budget` through
`Dpb::apply_reference_picture_set`/`clear_all`/`reap_unused`, releasing
each dropped DPB entry's `Picture::budget_bytes()` on removal, and gating
`CuGrid`'s L1 arrays behind `has_l1` so P/I slices keep their original
footprint exactly.

**Verified against real `libx265` output, whole-sequence, byte-for-byte,
per plane, per frame**, reusing `verify_hevc_deblock.sh`:

- A genuinely bare `ffmpeg -c:v libx265` encode (**zero** `-x265-params` of
  any kind) at 320x240 and at 640x480, `testsrc2`, 25 frames: **byte-exact,
  whole file** (`cmp` reports no difference) — the number this pass was
  asked to report. `verify_hevc_deblock.sh`'s own harness requires a non-empty
  `x265-params` argument for its `log-level=none` wrapper, so the resolution
  sweep below passes `bframes=4` explicitly (`libx265`'s own default value,
  confirmed via `x265 --help`) as a functional no-op standing in for "no
  flag at all"; the two direct, wrapper-free `cmp` runs above remove any
  doubt that this is equivalent to a truly unmodified invocation.
- The same, effectively-stock sweep (`bframes=4`, i.e. every encoder
  feature — SAO, deblocking, WPP, `cu_qp_delta`, weighted prediction — at
  its own default) at 320x240, 416x240, 352x288, 640x480 and 300x500:
  100% byte-exact at every resolution.
- `mandelbrot` (continuous zoom, real non-block-aligned motion) and `life`
  (cellular-automaton content) at 320x240, same stock settings: both 100%
  byte-exact.
- A deep hierarchical-B GOP forced explicitly
  (`bframes=6:b-adapt=2:weightp=1:keyint=25` on `life`): 100% byte-exact.
- **Weighted bi-prediction specifically** (`apply_weight_bi`, the
  `(Some, Some)` branch with both `w0`/`w1` resolved): the `weightp=1`
  fixture above never exercised it — `libx265` gates weighted bi-prediction
  behind its own separate `--weightb` flag (default off; confirmed via
  `x265 --help`, distinct from `--weightp`), so a fixture that only forces
  `weightp=1` cannot express this path at all, the same "a fixture that
  cannot express the bug proves nothing" trap this crate's own history
  warns about. Re-run with `weightb=1` added
  (`bframes=6:b-adapt=2:weightp=1:weightb=1:keyint=25` on `life`): every
  bi-predicted PU in the stream took the weighted branch (confirmed by
  temporary instrumentation counting `apply_weight_bi` vs
  `default_biprediction` calls before trusting the result, then removed),
  with genuinely non-neutral, distinct weight/offset pairs observed on both
  lists (for example `w0 = Weight { log2_wd: 13, w: 113, o: 0 }`,
  `w1 = Weight { log2_wd: 13, w: 123, o: 1 }`) — and the decode is still
  100% byte-exact.
- Regression check: every existing all-intra/P-slice/weighted-prediction
  fixture at 320x240, 416x240, 352x288, 640x480 and 300x500 remains 100%
  byte-exact, and all 61 crate unit tests, `tests/flat.rs` and
  `tests/oracle.rs::dense_content_is_byte_exact` still pass.

`decode_packet`'s "B-slices are not supported" refusal is **lifted**.
`check_scope` itself never refused B-slices (it is an SPS/PPS-level check;
slice type is not known that early) and is unchanged. What `check_scope`
still refuses is unrelated to this pass: non-4:2:0 chroma, non-8-bit
samples, `separate_colour_plane_flag`, SPS/PPS range extensions,
screen-content-coding extensions, and tiles. Long-term reference pictures
(refused by `derive_reference_pic_sets`, not `check_scope`) remain refused.
Dependent and independent multi-segment pictures are assembled by the
constraints described above; only multi-segment WPP, tile pictures, and
independent segments with a genuinely different picture-level RPS or other
unsupported decoding state remain named refusals. The supported WPP
multi-segment subset is limited to row-aligned independent segments, plus the
bounded one-CTU-wide dependent shape documented above; filtered boundaries are
accepted only when cross-slice filtering is enabled. Non-row-aligned
boundaries and filtered boundaries without cross-slice filtering remain
refused.

## The `Budget::release` leak past 640x480, found and fixed

The B-slice pass above fixed *one* `Budget::release` gap (`Dpb` never
releasing an evicted `Picture`'s charge, plus `CuGrid`'s `l1` arrays
doubling every P/I slice's footprint unconditionally) and, on the strength
of that fix, reported a stock `libx265` encode byte-exact at 320x240 and
640x480. Both fixtures happened to sit below the point where the *next*
leak crossed `Limits::strict`'s 64 MiB `max_alloc_total` cap inside a
25-frame clip — a bare `ffmpeg -c:v libx265` encode (no `-x265-params` at
all) failed at every resolution from 854x480 up, each time with a
`requested` byte count within a fraction of a percent of the 67,108,864-byte
cap regardless of the picture's own real size, the signature of a
cumulative counter creeping to a ceiling rather than a single allocation
that is genuinely too large.

**Confirmed as a leak, not a footprint problem, before touching any code**:
the previous pass's own 640x480/25-frame fixture, re-encoded to 10 seconds
(250 frames) instead of 1, failed with the same `max_alloc_total limit
exceeded` error a *leaked* budget predicts and a *too-large-per-frame*
budget does not — `max_alloc_total` measures a running total, so a
resolution that fits comfortably in 25 frames must eventually cross the
same fixed ceiling once given enough additional frames, if and only if
something is charged once per frame and never released. (A genuinely
too-large single picture would instead fail on frame 1, every time,
independent of stream length — the opposite of what was measured.)

**Root cause: three independent per-slice or per-picture allocations were
charged to `Budget` and never released**, on top of the one the previous
pass already fixed. Every one of them is a pure working buffer whose real
lifetime ends well before the function that allocated it returns, which is
exactly the shape `vaco-limits`' own "gotcha: releasing" warns about
("nothing releases automatically except a dropped `Reservation`"):

1. **`ctu::Ctx`'s own `cu_grid: CuGrid` and `sao_params: Vec<CtuSao>`**,
   allocated fresh once per slice (`decoder.rs`'s `CuGrid::new`/`Ctx::new`)
   and previously just dropped — not released — at the `drop(walk)` call
   after a slice's CTU walk, deblocking and SAO passes finish with them.
   Both are pure per-slice scratch space: `cu_grid`'s own per-4x4-block
   neighbour metadata and motion have no reader once the picture they
   describe is fully reconstructed (`CollocatedMotionField::build` already
   copies out, at its own smaller and deliberately un-tracked footprint,
   everything a *later* picture's TMVP needs), and `sao_params` is
   consumed in full by `sao::filter_picture` in the same function. Fixed by
   `CuGrid::budget_bytes` (framebuf.rs) and `Ctx::working_budget_bytes`
   (ctu.rs, `cu_grid` plus `sao_params`'s own `total_ctbs *
   size_of::<CtuSao>()`), released in `decoder.rs` immediately before
   `drop(walk)`. This was the largest of the three: `CuGrid` alone charges
   nine per-4x4-block arrays (`depth`/`mode`/`qp`/`mv0_x`/`mv0_y`/
   `ref_poc0`, plus `mv1_x`/`mv1_y`/`ref_poc1` for a B slice), 15 bytes per
   4x4 block for a P/I slice and 27 for a B slice — for a 1920x1080 frame,
   roughly 1.94 MB (P/I) to 3.5 MB (B) per slice, charged and kept forever
   under the old code.
2. **`sao::filter_picture`'s three `Snapshot`s** (`snap_y`/`snap_cb`/
   `snap_cr`, one read-only pre-SAO copy per plane, each the same pixel
   count as one of `Picture`'s own three planes) — built on every slice
   that has *any* SAO syntax to parse at all
   (`slice_sao_luma_flag || slice_sao_chroma_flag`, `libx265`'s own
   default), previously dropped uncollected at the end of the function.
   Fixed by `Snapshot::byte_len` (sao.rs), released in `filter_picture`
   right after the per-CTU offset loop that is their only reader. This one
   is roughly as large as `CuGrid`'s own charge per slice (it is
   `Picture`-plane-sized), so together the two alone were doubling the
   real per-slice leak the previous pass measured.
3. **The `Frame` `pic_to_frame` builds for `machine.emit`** — charged by
   `vaco_frame::Frame::alloc_video` and, unlike every other charge in this
   list, *never freed even conceptually*: it is not scratch space consumed
   within one function, it is the actual output handed to the caller, one
   per emitted picture for the entire lifetime of the decoder (I, P and B
   pictures alike). `vaco-codec-h264`'s own `decoder.rs` had already found
   and fixed the identical shape (`#421`, its own `build_frame`/
   `frame_bytes`/`release` sequence): once a frame is handed to
   `machine.emit`, it is the caller's memory to account for, not this
   decoder's own working set — nothing about a `Frame`'s `Drop` calls
   `Budget::release`, so the charge has to be given back explicitly, at the
   point the frame is built, not carried as if this decoder still owned it.
   Fixed the same way H.264 did: measure the `committed()` delta around
   `Frame::alloc_video`, then `release` it before returning. This was the
   single largest contributor of the three, being unbounded by `Dpb`
   occupancy the way a leaked `Picture` or `CuGrid` charge at least
   eventually is (bounded reference-picture-set size caps how many of
   those can be live at once; nothing bounds how many frames a long stream
   emits).

A fourth, much smaller instance of the same shape was fixed alongside these
for completeness (`AGENT-CONSTRAINTS.md`'s "find every path that charges
`Budget` and check each has a matching release", not because it moved any
measured number): `decoder::wpp_row_ranges`'s returned `Vec<(usize,
usize)>` (one entry per CTU row, only allocated when
`entropy_coding_sync_enabled_flag` is set) was released unconditionally by
splitting `decode_wpp_rows`'s row-decode loop into its own function
(`decode_wpp_row_ranges`) so the caller can release `row_ranges`'s charge
once, after that call returns on *any* path — success or the row loop's own
early-return error cases — rather than depending on every current and
future early return inside the loop to remember it individually.

**No cap in `vaco-limits` changed.** All four charges above were pure
`Budget::release` omissions, not a case where the real footprint exceeds
`Limits::strict`'s 64 MiB for a legitimate reason — see the next paragraph
for the one case (4K) where the arithmetic says otherwise, and why that
one is a different crate's issue, not this one's.

**Verified against real `libx265` output, byte-for-byte, per plane, per
frame, with the actual `vaco` CLI binary**
(`--features vaco-registry/patent-encumbered-hevc-decode`), a bare `ffmpeg
-c:v libx265` encode with **no** `-x265-params` at all:

- The exact confirmation fixture, re-measured after the fix: 640x480, 250
  frames (10 seconds) — byte-exact, whole file, where it used to fail
  partway through (leaking budget until frame ~32 of 250 tripped the cap).
- The full resolution sweep this pass was asked to close: 640x480,
  854x480, 1024x576, 1280x720 and 1920x1080, 25 frames each — all
  byte-exact, whole file, where every one above 640x480 used to fail.
- 1280x720 re-measured at 250 frames (10 seconds) instead of 25 — still
  byte-exact, confirming duration no longer matters at a resolution that
  used to fail well inside 25 frames.
- Every pre-existing regression fixture — 320x240, 416x240, 352x288,
  322x242 and 300x500 (a partial last CTU row *and* column) — still 100%
  byte-exact, plus the 61 crate unit tests, `tests/flat.rs` and
  `tests/oracle.rs::dense_content_is_byte_exact`.

**3840x2160 is a genuine, separate, out-of-scope gap, not this leak** —
confirmed by driving `HevcDecoder` directly with `Limits::permissive()`
(bypassing the CLI entirely): the same bare-`libx265` 3840x2160, 25-frame
fixture decodes **100% byte-exact** once given a `Limits` preset sized for
it, proving this crate's own decode logic is correct at 4K and the leak
fix is complete. The real `vaco` CLI still refuses a 4K file, but for a
reason that belongs to two other crates, neither owned by this pass:
`vaco-cli`'s own decoder construction (`exec.rs`) builds every decoder with
`vaco_limits::Limits::default()` (`== strict()`, 16 MiB `max_frame_bytes`)
rather than `Limits::permissive()` — the CLI default `vaco-limits`' own
docs describe — and `vaco_parse_hevc::Sps::checked` rejects any SPS whose
`width.max(coded_width) * height.max(coded_height) * 4` exceeds
`max_frame_bytes` (`sps.rs`'s `budget.check_frame(w, h, 4)`). 3840x2160
needs 33,177,600 bytes there against `strict`'s 16,777,216-byte cap —
genuinely over, by design, for that preset — while 1920x1080's 8,294,400
needs only 8,847,360, comfortably under. Raising `vaco-limits`' own
`strict` preset would not be the right fix even if this pass owned that
decision: `strict` is deliberately conservative for an untrusted-input
embedder, and the CLI's own wiring not using `permissive()` (its
documented "CLI default") looks like the real, separate defect. Reported
rather than worked around, per this pass's own scope (`vaco-codec-hevc`
and `vaco-limits` only).

**Which fixture would have caught this originally**: any real fixture
whose `CuGrid`/`Snapshot`/emitted-`Frame` charges, accumulated across
however many frames it contains, cross `Limits::strict`'s 64 MiB before the
fixture ends — a fixed frame count at a large-enough resolution (this
pass's 854x480-and-up sweep) or a fixed, smaller resolution run long enough
(the 640x480-at-250-frames confirmation). 320x240 and 640x480 at 25 frames
each cannot express this bug at all: both stay under the ceiling for the
whole clip, which is exactly why the previous pass's real, correct fix for
the *other* `Budget::release` gap read as complete once those two passed.

## Row-wise data movement (`PERF-PROGRAMME.md` B1), landed

The first profile ever taken of this decoder found 31.3% of decode spent
moving samples one bounds-checked `u16` (or `i32`) at a time, in five
functions with no arithmetic in them at all: `write_inter_cu_no_residual`'s
`Plane::set` (9.32%), `sao::Snapshot::capture`'s `Plane::get` (8.08%),
`sao::offset_block` (5.35%), `decoder::emit_pocs`'s `u16 -> u8` blit
(5.11%), `ctu::build_cu_prediction`'s `i32` blit (3.48%). `framebuf::Plane`
now exposes `row`/`row_mut`/`mark_row_ready`/`clone_samples`, and each of
those five call sites moves a whole row (or, for `Snapshot::capture`, the
whole plane in one `copy_from_slice`) instead of one sample per bounds
check — see `planning/E2E-GAPS.md` §24 for the measured before/after
profile share, the interleaved A/B numbers (~1.22–1.29x on
`hevc_{sd,720p,1080p,4k}.mp4`, CPU-seconds primary under heavy shared
load), and a live account of a concurrent-agent commit mistake that briefly
reverted and then correctly restored this change.

`sao::offset_block` is the one function in the five that did not collapse
to memcpy-class after this change: its per-sample cost is genuine
arithmetic (the band or edge-offset computation §8.7.3 specifies), and the
row-wise rewrite only amortises the *bounds check* across a row rather than
eliminating the per-sample work itself. `Plane::set_i32` and
`sao::Snapshot::get` are now dead (every caller reads/writes whole rows
instead) and have been removed; if a future SAO change needs single-sample
random access again, re-add them rather than reaching back to `get`/`set`
in a loop that could be row-wise instead.

## `Plane` stores `u8` (`PERF-PROGRAMME.md` B2), landed

`Plane::data` is `Vec<u8>`, not `Vec<u16>`, for a crate whose whole scope
is 8-bit (`check_scope` refuses anything else). Availability
(`Plane::is_ready`) is tracked on a `(width/4) x (height/4)` grid — every
write this crate makes is at least a 4x4 transform block, and
`pic_width`/`pic_height_in_luma_samples` are themselves always CTB-grid-
aligned, so a 4x4 grid answers "has this pixel's block been written" as
exactly as the old per-pixel bitmap did, at 1/16 the memory.
`Plane::get`/`set` kept their `u16` call signature (thin wrappers over the
real `u8` storage) specifically so `deblock.rs`/`intra_pred.rs`/`mc.rs`
needed no changes; only `Plane::row`/`row_mut`/`clone_samples` (B1)
changed element type, which let `decoder::blit` collapse to a plain
`copy_from_slice` (both sides are `u8` now) and let `ctu::write_block`
(intra reconstruction, previously untouched by B1) move to the same
row-wise shape as `write_pred_block`. See `planning/E2E-GAPS.md` §29 for
the measured numbers (a 1.00–1.13x bonus on top of this item's own
correctness-only stop condition) and why an I-only stress fixture could
not be produced cleanly in that pass's environment.

**B3 (PU-level separable motion compensation) was attempted twice on top
of this and reverted both times** — see `planning/E2E-GAPS.md` §29 for the
full account. Building an edge-replicated source block once per PU (either
via `vaco-codec-dsp-mc`'s `edge::extend_edges`, or a hand-rolled `i32`
clamp matching `clamped_sample`'s own shape) measured flat-to-negative
under clean load both times, losing the large majority of interleaved
rounds — `clamped_sample`'s original per-tap clamp was apparently already
cheap and cache-friendly enough that building it into a buffer first only
added a write-then-read pass without removing any bounds check
(`tap_sum_row`/`tap_sum_col` still index the extended buffer once per tap,
exactly as many times as `clamped_sample` was called before). `mc.rs` is
therefore unchanged from B1/B2's own state; a future attempt at B3 should
read §29's "why, on reflection" before trying another block-extension
design, since two of that shape are now ruled out with real numbers behind
them.

## Specification

`itu-t-h265-202108` (ITU-T Rec. H.265 (08/2021)) and
`hm-reference-software` (the HM reference decoder, BSD-3-Clause, Tier A
per `planning/AGENT-CONSTRAINTS.md`'s clean-room section — unlike
FFmpeg/x265, which stay Tier B), both recorded in
`provenance/sources.toml`. Mechanically-thresholded tables recorded in
`provenance/vaco-codec-hevc.toml`.
