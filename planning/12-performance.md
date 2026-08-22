# 12 — Performance Plan

How Vaco reaches (and in places exceeds) FFmpeg's throughput with **zero unsafe code**: no inline
assembly, no `core::arch` intrinsics, no `#[target_feature]` escape hatch in our own crates.

Binding inputs: `planning/00-decisions.md` (D2, D6, D8), `planning/10-architecture.md` §6 and §7,
`planning/research/08-performance-simd.md`, `planning/research/01-libavutil-swr-sws.md`.

Clean-room: nothing here is derived from reading FFmpeg source or assembly. Quantities about FFmpeg
come from the structural analysis in research §08 (file/line counts and public API names only).

---

## 0. The engineering problem, stated plainly

FFmpeg carries ~190,000 lines of hand-written assembly (x86 ~55.9k, AArch64 ~39.8k, ARM32 ~28.5k, and
a long tail). That assembly is the single reason FFmpeg is fast. We are forbidden it. The naive
conclusion — "we will be 2–4x slower" — is wrong, and the reason it is wrong is worth stating up
front because it determines everything below.

Hand-written asm beats a compiler in five specific ways:

1. **Instruction selection** for ops the source language cannot spell (`pmaddubsw`, `psadbw`,
   `pmulhrsw`, `packuswb`, `vpternlogd`, `tbl`, `udot`).
2. **Register allocation and blocking** across a whole 8x8 or 16x16 block, keeping everything
   resident and never spilling.
3. **Shuffle/permute sequences** chosen by hand, often 2–3 instructions where a naive lowering takes 6.
4. **Scheduling** for a specific microarchitecture's port pressure.
5. **Avoiding all bounds/overflow/aliasing checks** by construction.

Portable SIMD plus modern LLVM closes (1) more than people expect — LLVM has explicit DAG combines
that *reconstruct* `psadbw`, `pmaddwd`, `pmulhw`, `packuswb`, `packssdw`, `pavgb`, `blendv`, `uabdl`,
`sqdmulh`, `st3`/`ld3` from portable source patterns. It closes (5) completely, because Rust gives us
no-aliasing for free and `chunks_exact` gives us provable trip counts. It partially closes (2) via
`#[inline(always)]` + const generics. It does **not** reliably close (3) or (4).

So the honest shape of the answer is: **we are close to parity on straight-line arithmetic kernels,
behind on shuffle-dominated kernels, and ahead on everything that isn't SIMD at all** — because
FFmpeg uses no PGO, no BOLT, and only opt-in LTO (research §08 §7), and because we get to choose a
better architecture (ops-graph scaling, batched motion compensation, measured thread counts) rather
than inheriting twenty years of one.

The plan below is organised so that the places we lose are named, bounded, measured, and each has a
written mitigation — including, where honesty demands it, "escalate a scoped `unsafe` exception to
the user with a measured justification."

---

## 1. Realistic assessment: the hot paths

### 1.1 What the SIMD substrate can and cannot express

> **Rewritten 2026-08-21 per D12.** This section described `std::simd` (nightly `portable_simd`). The
> substrate is now **`fearless_simd`** on stable 1.89+. The table below is not an estimate — it was
> established by reading the v0.7.0 source, and the complete operation vocabulary is recorded in plan 11
> §5.6. The single largest change is at the bottom of the "not expressible" table: **runtime ISA
> dispatch has moved from impossible to free.**

Every kernel is a function generic over a **capability token** — `fn k<L: Lanes>(lanes: L, …)` — which is
monomorphised once per CPU level and selected at runtime. Vectors are native-width associated types
(`L::u8s`, `L::i16s`, `L::f32s`) or fixed-width (`u8x16<L>`, `f32x4<L>`), and `L::u8s::N` is the lane
count for the level actually running.

**Expressible, lowers 1:1 or near-1:1:**

| Capability | `fearless_simd` spelling | Lowers to |
|---|---|---|
| Elementwise int/float arithmetic | `+ - * / %` on `L::i32s` etc. | `padd*`/`pmull*`/`add`/`mul` |
| Fused multiply-add (float) | `a.mul_add(b, c)`, `mul_add_precise` | `vfmadd*`/`fmla` |
| Widening convert | `v.widen() -> (lo, hi)` | `pmovzx*`/`ushll`+`ushll2` |
| Narrowing with clamp | `lo.saturating_narrow(hi)` — **native, not a pattern match** | `packuswb`/`packssdw`/`sqxtun` |
| Narrowing, cheapest | `relaxed_narrow` (debug-asserts no overflow) | best available per backend |
| Min / max (int and float) | `a.min(b)`, `a.max(b)`, `_precise` variants | `pminub`/`pmaxsw`/`umin`/`smax` |
| Masked select / blend | `mask.select(if_true, if_false)` | `blendv`/`vpblendvb`/`bsl`/`k`-masks |
| Compare to mask | `simd_lt`, `simd_ge`, … | `pcmpgt`/`cmge`/`vpcmp` |
| Mask → bits, bits → mask | `to_bitmask`, `from_bitmask` | `pmovmskb`/`k`-regs |
| Mask reduction | `any_true`, `all_true`, `any_false`, `all_false` | `ptest`/`kortest`/`uminv` |
| 2-way interleave / de-interleave | `interleave`, `deinterleave`, `zip_low`/`zip_high`, `unzip_low`/`unzip_high` | `punpckl/h*`, `zip1/zip2`, `uzp1/uzp2` |
| 4-way interleaved load/store | `load_four_interleaved`, `store_four_interleaved` (128-bit vectors) | `vld4`/`vst4` on NEON; shuffle chain on x86 |
| **Dynamic** byte shuffle (table lookup) | `swizzle_dyn`, `swizzle_dyn_within_blocks`, `swizzle_dyn_precise` | `pshufb`/`tbl` (1 instr at 128-bit; lane-crossing above) |
| Lane slide / rotate / shift | `slide::<K>`, `rotate_elements_left/right::<K>`, `shift_elements_*::<K>` | `palignr`/`ext`/`vperm` |
| Uniform and per-lane shifts | `shl`/`shr`; `shlv`/`shrv` | `psll*`/`vpsllv*`/`ushl` |
| Reinterpret lane width | `bitcast::<T>()`, `to_bytes`/`from_bytes` — **safe, no `bytemuck`** | free |
| Vector concat / split | `combine`, `split` | free / `vextracti128` |
| Float↔int convert | `to_float`, `to_int`, `to_int_precise`, `cvt_*` | `cvtdq2ps`/`scvtf` |
| Rounding, sqrt, recip | `floor`/`ceil`/`trunc`/`round_ties_even`/`fract`/`sqrt`/`approximate_recip` | direct |
| **Runtime ISA dispatch** | `dispatch!(level, simd => …)` | a `match` + monomorphised call. **New — this is the whole point of D12.** |

**Not expressible, and this is the whole list that matters:**

| Missing | Consequence | What we do instead |
|---|---|---|
| **Saturating add/sub** | Every clamped accumulate | `min(!b)+b` (3 ops) unsigned, `max(b)-b` (2 ops); `widen`→add→`saturating_narrow` (~5) signed. Behind `ops::saturating_*`. Exposure is smaller than it looks — the dominant codec use is `clip(pred+residual)`, whose saturation lives in the narrowing step, which **is** native. |
| **Horizontal reduce** (sum/min/max) | SAD, SATD, dot products, energy measures | `log₂N` × (`rotate_elements_left` + `add`) ≈ 7 ops at N=8. Hoist it: keep a vector accumulator across the loop, reduce once per invocation. Only *mask* reductions are native. |
| **Average** (`pavgb`) | Bi-prediction, chroma MC, some filters | `(a\|b) - ((a^b)>>1)` — **4 ops, exact, in-width, cannot overflow.** Better than the widen route. |
| **Absolute difference / SAD** (`psadbw`) | Motion estimation | `max(a,b)-min(a,b)` (3 ops) plus a widening accumulate per row. ~2.4x on a 16×16 SAD. |
| **Integer `abs`** *(found during the D12 review; not in D12's list)* | SATD, residual metrics | `x.max(zero-x)` (2 ops). `abs` exists on floats only. |
| **Widening multiply / `pmaddwd`** | Pairwise dot against a coefficient *vector* | ~12 ops for 2 output vectors (~6x). **Restructure to avoid it** — see below. |
| **`pmaddubsw`** (u8×i8 → i16 pairwise dot) | 8-tap FIR on u8 pixels | Widen u8→i16 once per source vector, hoisted out of the tap loop; then `slide`+`mul`+`add` per tap. **~2.2–2.5x** the instruction count on the horizontal pass. Recovered partly by batching and const-generic tap unrolling (§1.4 Risk C). |
| **`pmulhrsw`** (rounded high multiply) | One extra add+shift per multiply | Fold the rounding constant into the coefficient table where the algorithm allows it. Unchanged by D12. |
| **`crc32`, AES-NI, VNNI, `gf2p8affine`, ARM `udot`/`i8mm`/`bf16`** | No access to fixed-function or newest widening-dot instructions | No composition exists for `crc32`. See Risk E; unchanged by D12. |
| **Raw `core::arch` intrinsics as an escape hatch** | We cannot reach through the substrate for anything above | `fearless_simd`'s `kernel!` macro would give exactly this, safely, under a token — **but its expansion contains `unsafe` in the calling crate, so `#![forbid(unsafe_code)]` rejects it.** This is the most important limitation of D12 and it is why the gap list above must be *composed* rather than bypassed. |
| SVE/RVV **scalable** vectors | We use a fixed native width per level | Same as x86. On SVE hardware the NEON path still uses the vector unit. Assume 0–8%. |
| Non-temporal stores (`movntdq`) | Large-frame writes pollute L3 | Tile so the working set fits L2. Real but small (~2% on 4K scale). |
| Prefetch hints | No `_mm_prefetch` | Hardware prefetchers handle unit-stride; batch MC by reference row. |

Three of the old table's entries have **moved from "missing" to "present"**, and they are worth calling
out because plan sections were built around their absence:

1. **Dynamic byte shuffle is now a first-class portable operation** (`swizzle_dyn`). Under `std::simd` it
   was listed as an outright gap, and §2.4 was built around `simd_swizzle!`'s requirement for a `const`
   index array — which is what forced the "macro-generated shuffles per concrete `N`" rule. That entire
   apparatus is gone (§2.0, §2.4).
2. **Saturating narrow is a named operation, not a hoped-for LLVM pattern match.** The old table's entry
   read "`simd_clamp` then `cast` → `packuswb` (LLVM `detectUSatPattern`)". It is now
   `lo.saturating_narrow(hi)`, guaranteed by the backend rather than by a peephole.
3. **Lane-width reinterpretation is safe and built in** (`bitcast`). The `bytemuck` dependency the old
   table required — and the D-P1 decision it forced — is no longer needed for this purpose.

**And one thing we gave up, stated plainly.** Several of the gaps above were expected to be closed *by
LLVM*: `combineAddToPMADDWD` recovers `pmaddwd` from a widen-mul-adjacent-add pattern,
`combineBasicSADPattern` recovers `psadbw`, `combineAVG` recovers `pavgb`. Those combines run over
generic IR. `fearless_simd` emits explicit `core::arch` intrinsics — which LLVM still models as shuffles
and arithmetic, so the combines *may* fire, but we can no longer assume it. **The trade is a set of
possible peephole wins for a guaranteed runtime-dispatch win.** For a distributed binary that is not a
close call: under the superseded plan the shipped artefact could not use AVX-512 at all. For a
`-C target-cpu=native` source build it is closer, and we do not know the answer until §11's checklist
item 1 is measured.

The single most important entry in the "missing" table is still `pmaddubsw`. It is the instruction that
makes FFmpeg's H.264/HEVC interpolation filters fast, there is no portable spelling, and D12 does not
change that — `std::simd` did not have it either. What D12 changes is the *size* of the estimate: the old
text put it at ~1.4x the ops, and the measured composition is ~2.2–2.5x. §1.3's bands are revised
accordingly.

### 1.2 Where we structurally *win*

These are not consolation prizes; they are quantified below and they are why the weighted result
lands near parity.

1. **PGO + BOLT + fat LTO.** FFmpeg ships with none of the first two and opt-in for the third
   (research §08 §7). Expected 3–8% end-to-end, 8–15% on entropy-decode-dominated work (§6).
2. **Ops-graph scaling** (architecture §7, research §01 §11). Fusing `unpack → linear → clamp → pack`
   into one pass over the pixel avoids the intermediate buffer round-trips the legacy per-format-pair
   kernels make. On a `yuv420p → scale → rgb24` chain this is one memory pass instead of three.
3. **Wider default vectors in filters — and now they actually run.** Research §08 §2d: much of
   libavfilter's x86 SIMD predates AVX-512 and some filters have no SIMD at all. A native-width
   implementation of bwdif/gblur/removegrain can plausibly exceed upstream by 10–60%. **Strengthened by
   D12:** under the superseded F5 this only helped users who installed the matching artefact, and the
   shipped default (v3) could never use AVX-512. With runtime dispatch, `L::u8s` is 64 bytes on
   AVX-512 hardware and 32 on AVX2 hardware *from the same binary*.
4. **Batched motion compensation.** We control the decoder *and* the kernel, so we can gather all
   4x4 chroma MC calls for a macroblock row into one batched kernel invocation, amortising the
   per-call overhead and filling wide lanes that a single 4-pixel-wide block wastes. FFmpeg's DSP
   contract (one call per block) forecloses this; ours does not.
5. **Generic specialisation instead of a combinatorial asm matrix.** `mc::<L, W, H, TAPS>()`
   monomorphises for free where FFmpeg hand-writes 6,476 lines of AArch64 qpel (research §08 §2c).
   We get every block size, at every bit depth, **at every ISA level**, from one source — the level is
   now just another axis of the same monomorphisation, which is exactly what FFmpeg pays for by hand.
   The cost is binary size (§1.4 Risk A), which is real and must be measured.
6. **Measured threading defaults** instead of `MAX_AUTO_THREADS = 16` (§7).
7. **No legacy ISA tiers, and no hand-written cascade.** FFmpeg maintains MMX→AVX-512 and NEON→SME2
   cascades by hand. Our floor is x86-64-v2 and ARMv8.0-A; everything below is deleted, not supported.
   **Revised by D12:** we now *do* have ISA tiers above the floor (SSE4.2 / AVX2 / AVX-512 on x86;
   a single NEON level on aarch64) — but they are generated from one generic source by
   monomorphisation, not written four times. That is the version of "no legacy tiers" that was always
   worth having: one implementation, every ISA, selected at runtime.

### 1.3 The hot paths, honestly banded

Bands are **Vaco / FFmpeg** wall-clock on the same machine, same input, single-threaded. `>1.0` means we
are faster. These are *predictions* to be falsified by §4's benchmark suite, not measurements; each is
falsifiable and each carries a named mitigation.

> **Revised 2026-08-21 per D12 — and yes, some bands move.** Two changes to the comparison itself:
>
> 1. **The comparison basis changed.** These used to be measured "both built for the same ISA baseline
>    (x86-64-v3 / ARMv8.2)", because under F5 our binary *was* its baseline. With runtime dispatch we
>    compare **as shipped**: one Vaco binary that dispatches, against `ffmpeg` as the distro ships it.
>    That is a fairer comparison and a harder one, and it is the one users experience.
> 2. **Five bands move, and four of them move down.** The `pmaddubsw` composition costs ~2.2–2.5x rather
>    than the ~1.4x the old §1.1 assumed, and two operations the old plan expected LLVM to synthesise
>    (`psadbw`, integer `abs`) must now be composed explicitly. Bands are marked **↓** / **↑** below with
>    the old value in parentheses. Nothing here is hidden: the weighted expectation in §1.5 is recomputed
>    and it comes out slightly *worse* before dispatch is accounted for, and better after.

| # | Path | Dominant ops | Portable-SIMD verdict | Band | Risk |
|---:|---|---|---|---|---|
| 1 | **Pixel-format / colour conversion** (yuv2rgb, rgb2rgb, range convert) | widen, 3x3 int matrix MAC, clamp, pack | Fully expressible. `saturating_narrow` is native, not a pattern match. Only weakness is the 3-byte packed store, and `swizzle_dyn` handles it portably. | **↑ 1.05–1.30x** *(was 1.00–1.25x)* | Low |
| 2 | **swscale h/v filtering** (bilinear/bicubic/lanczos) | precomputed coefficient walk, `pmaddwd`-shaped MAC, round, pack | Fully expressible if we keep FFmpeg's precomputed-coefficient layout (no hardware gather). | **0.90–1.10x** | Low |
| 3 | **Audio sample-format conversion + rematrix** | cast, scale, interleave/deinterleave, small matrix MAC | Fully expressible; upstream asm here is small (~1.8k lines total). We fuse convert+rematrix+resample into one pass. | **1.05–1.35x** | Low |
| 4 | **Audio resample (polyphase FIR)** | f32 FIR, multi-accumulator | Fully expressible. Needs explicit 4-accumulator unrolling (LLVM will not reassociate FP). | **0.95–1.15x** | Low |
| 5 | **Deinterlace (bwdif/yadif), blur, denoise, removegrain** | separable convolution, min/max networks, clamp | Fully expressible, and upstream is SSE2/AVX2-era or absent. | **1.10–1.60x** | Low |
| 6 | **H.264 luma qpel / chroma MC (8-bit)** | 6-tap and 2-tap FIR on u8, two-pass separable, saturating pack, 4x4–16x16 blocks | Expressible but **no `pmaddubsw`**; must widen u8→i16 first, at ~2.2–2.5x rather than the ~1.4x previously assumed. Small blocks waste lanes. `pavgb` composes at 4 ops, which protects the bi-prediction path. | **↓ 0.75–0.95x** *(was 0.80–1.00x)* | **High** |
| 7 | **HEVC/VVC epel/qpel (8-tap, 8/10-bit)** | same shape, wider taps, more sizes, 10-bit needs i32 | Same `pmaddubsw` gap at 8-bit, same revised cost; 10-bit is i16→i32 where we are competitive — and 10-bit is where runtime AVX-512 now pays. | **↔ 0.85–1.05x** *(unchanged: the 8-bit loss and the AVX-512 gain roughly cancel)* | **High** |
| 8 | **H.264/HEVC/VP9 in-loop deblocking** | per-edge strength decision, saturating clamp, 4x4/8x8 **byte transpose** for vertical edges | Branchiness is fine (upstream is also branchless-with-blends). Transposes are `interleave`/`zip` + `bitcast` chains, all native. | **0.80–1.00x** | Medium |
| 9 | **Integer IDCT 4x4 / 8x8 (H.264, HEVC, MPEG)** | butterfly MAC, shift, transpose | Arithmetic is trivial; transposes are 3 interleave rounds. Register pressure fine at 8x8. | **0.90–1.05x** | Low |
| 10 | **VP9/AV1 inverse transforms incl. 10/12-bit** | large butterfly networks, `pmulhrsw`-shaped rounding, heavy permute, i32 lanes at high bit depth | The largest shuffle-to-arithmetic ratio in the codebase. `pmulhrsw` gap unchanged. But this is the biggest single beneficiary of D12: 512-bit i32 lanes at high bit depth were previously only reachable by whoever installed the v4 artefact, and are now reachable by everyone. AVX-512 also more than doubles the vector register file, which is exactly what this kernel is short of. | **↑ 0.80–1.00x** *(was 0.75–0.95x)* | **High** |
| 11 | **Intra prediction (H.264/HEVC/VP9)** | mode-dependent averaging/interpolation along fixed angles | Simple math; our const-generic mode specialisation is arguably cleaner than upstream's table-driven dispatch. | **0.90–1.15x** | Low |
| 12 | **Motion estimation: SAD** | abs-diff, horizontal sum | **The band that moves furthest.** The old entry relied on LLVM's `combineBasicSADPattern` synthesising `psadbw` from `(a-b).abs()` + `reduce_sum`. Under explicit intrinsics we compose: `max-min-sub` (3 ops) plus a widening accumulate per row, ≈120 instructions on a 16×16 SAD against ≈50 for `psadbw`. | **↓↓ 0.70–0.90x** *(was 0.95–1.10x)* | Medium — **but see the containment note below** |
| 13 | **Motion estimation: SATD (Hadamard)** | butterfly + **transpose** + abs + reduce | Transpose-dominated (same as #10), plus integer `abs` must now be composed (2 ops) and the reduce is explicit. | **↓ 0.70–0.90x** *(was 0.80–1.00x)* | Medium — same containment |
| 14 | **FFT / MDCT / RDFT (`vaco-tx`)** | complex butterflies, twiddle multiply, bit-reversal / permute stages | Straight-line stages fine. The permute and twiddle-load stages are exactly what every real FFT library hand-tunes. | **0.70–0.95x** | **Highest** |
| 15 | **CABAC / CAVLC / bitstream entropy decode** | serial state machine, table lookups, unpredictable branches | **Not vectorizable at all — upstream has no CABAC asm either.** Pure scalar codegen contest, which is where PGO/BOLT pay. | **0.95–1.30x** | Medium |
| 16 | **AAC SBR / PS (QMF filterbank, envelope adjust)** | f32 FIR + FFT-adjacent | Mostly straight-line f32; inherits #14's risk for its transform core. | **0.85–1.05x** | Medium |
| 17 | **3D LUT / colour management / tonemap** | gather-heavy tetrahedral interpolation | Gather is slow on x86; but so is upstream's. Layout wins (SoA lattice) matter more than ISA. | **1.00–1.40x** | Low |
| 18 | *(non-SIMD, listed for honesty)* **CRC-32 / Adler-32 for framecrc & muxers** | `crc32` instruction on SSE4.2/ARMv8 CRC | **We cannot emit `crc32` without intrinsics.** Unchanged by D12 and worth being explicit about why: D12 gives us an `Sse4_2` capability token, but reaching `_mm_crc32_u64` needs `fearless_simd`'s `kernel!` macro, whose expansion contains `unsafe` in *our* crate. Slice-by-16 table CRC runs ~1.5 GB/s vs ~20 GB/s hardware. | **0.10–0.20x** | Contained |

**Containment note on bands 12 and 13 (motion estimation).** These are the two worst revisions in the
table and they matter much less to Vaco than the numbers suggest, for a reason specific to this project:
**SAD and SATD are encoder kernels.** Per D4 and D9, our default distributable build ships almost no
video encoders — HEVC, VVC and AAC encode are gated behind non-default features, H.264 encode is not
ours to ship, and AV1 encode goes through `rav1e`, which brings its own SIMD. Motion estimation is
therefore not on any default-build hot path, and it appears in none of the nine benchmark scenarios as a
dominant cost. It is recorded honestly here because it *is* a real regression against the superseded
plan, and because it will matter if the opt-in encoder features are ever exercised seriously. It is not
a reason to reconsider D12.

**Bands NOT changed by D12, and why** — recorded so the revision is auditable rather than selective:
#2 swscale filtering (broadcast-coefficient MAC, which is the shape D12 is *good* at), #3/#4 audio
convert and resample (f32 throughout; `mul_add` is native), #5 filters (min/max networks and separable
convolution, all native), #8 deblocking (clamps are `min`/`max`, which are native; byte transposes are
`zip`/`unzip`, native), #9 integer IDCT (i16/i32 butterflies and interleave transposes, all native),
#11 intra prediction (`avg` composes at 4 ops, which is the only operation at issue), #14 `vaco-tx`
(split-complex f32, no integer gaps in play), #15 entropy decode (scalar), #16 SBR/PS, #17 3D LUT.

### 1.4 The five genuine risks, and exactly what we do about each

#### ~~Risk A — Runtime ISA dispatch is structurally unavailable to us~~ → **RETIRED by D12**

**This risk no longer exists.** `fearless_simd` obtains runtime ISA dispatch without `unsafe` in our
crates, via capability tokens: zero-sized values that witness a CPU level, so the intrinsic call is safe
at the call site. One binary detects AVX-512 / AVX2 / SSE4.2 / NEON at startup and runs the matching
monomorphisation. Plan 11 §5.3 (F5′) argues the mechanism in full and verifies against the v0.7.0 source
that `dispatch!`'s expansion contains no `unsafe`.

**The multi-artifact build strategy is withdrawn.** Everything it required goes away:

| Withdrawn | Was |
|---|---|
| Per-ISA-level builds (`x86-64-v2`/`v3`/`v4`, `aarch64` baseline/`v82`) | 5 artefacts, 3x build time, 3x artefact size on x86 |
| The ~200-line `exec` launcher shim | ~0.3 ms on Unix, ~2 ms on Windows, and a permanent asterisk on scenario S7 |
| `VACO_ISA_LEVEL` env override, `--isa-level=` build flag, distro-packager special case | — |
| Phase 0.7 of the work order (§8) | 2 person-weeks |
| Decision D-P3 (§9) | — |
| The escalation candidate (a scoped `unsafe` dispatch shim in `vaco-simd`) | Withdrawn: it existed to buy AVX-512, which we now have for free |

Replaced by **one artefact per platform**, a compiled floor of x86-64-v2 / ARMv8.0-A (plan 11 §1.6), and
runtime selection above it. Startup cost is a cached CPUID, not a process launch. `VACO_TIER=sse4.2`
replaces `VACO_ISA_LEVEL` for testing and for `vaco-checkasm`'s cross-level comparison, and it is now a
*runtime* switch, which means checkasm can compare every level in one process on one machine — something
the multi-artifact design could only do by rebuilding.

**What it costs instead — the residual risk, which is real:**

1. **Binary size.** On x86, every level-generic function is compiled once per level. With the v2 floor
   that is three monomorphisations (SSE4.2 / AVX2 / AVX-512); the `Sse2` arm collapses into the ambient
   baseline. Across ~150 kernels plus their const-generic block-size and bit-depth axes, this compounds.
   Mitigations in order: `codegen-units = 1` and `lto = "fat"` (already set), then
   `--cfg disable_dispatch_avx512` in `RUSTFLAGS` to prune a level wholesale, then per-function level
   disabling. **This must be measured and reported in the release notes, not assumed away** — it is the
   one place where the superseded plan was cheaper. On aarch64 the cost is zero: there is exactly one
   level, so `dispatch!` is a single-arm match.
2. **Compile time.** 3x the codegen for DSP crates on x86. Partly why Cranelift dev builds (plan 11 §1.2)
   stay worth keeping even though they are now optional.
3. **`#[inline(always)]` becomes load-bearing for correctness of codegen.** The target-feature context
   reaches a kernel body only through inlining. A kernel that fails to inline is silently compiled at the
   baseline and loses its dispatch — it will still be *correct*, just slow, which is the worst kind of
   failure because no test catches it. This is precisely what the `forbid = ["call"]` assertion in
   §3.3(c) is for, and D12 promotes that assertion from "useful" to "mandatory on every kernel".

**Replacement risk, carried forward as Risk A′:** *a `fearless_simd` operation gap proves fatal on a hot
kernel.* Bounded by plan 11 §5.6's compositions (every gap has one, and the worst is ~6x on an operation
we can restructure away from), by the crate being forkable, and by `vaco-simd` being an adapter. Its
mitigation ladder is §11's adoption checklist.

#### Risk B — `vaco-tx` (FFT/MDCT) at 0.70–0.95x

This is the worst band in the table and it is load-bearing: AAC, Vorbis, Opus, MP3, AC-3 and every
SBR/QMF path go through it (research §01 §8, §08 §2b rank 10).

**Re-examined under D12: unchanged, and slightly helped.** `vaco-tx` is f32 throughout, and every
operation it needs is native in `fearless_simd` — `mul_add`/`mul_sub` (both with `_precise` variants,
which matters for the bit-exactness question in D-P7), `zip`/`unzip` for the split-complex layout,
`slide` for the permute stages. None of §1.1's gaps are in play. Two small gains: AVX-512's doubled
register file is now reachable at runtime on the machines that have it, which directly helps the
register-pressure problem in mitigation 1; and `bitcast` removes the `bytemuck` dependency the old plan
needed, which softens decision D-P1.

Mitigations, in order of application:

1. **Algorithmic, not instruction-level.** Use a **radix-4/radix-8 split-radix** decomposition with
   **pre-permuted twiddle tables**, so the permute work is done once at plan time and the inner loop
   is pure MAC. This trades table memory (a 4096-point f32 plan costs ~64 KiB) for eliminating the
   per-stage shuffles that portable SIMD lowers badly.
2. **Stockham auto-sort** formulation, which eliminates the bit-reversal pass entirely at the cost of
   ping-pong buffers. Frames are pooled (`vaco-pool`), so the extra buffer is free in steady state.
3. **Split-complex (SoA) layout** internally — real and imaginary in separate arrays — which turns
   every complex multiply into four elementwise multiplies and two adds with *zero* shuffles. Convert
   to/from interleaved at the API boundary only. This is the single biggest lever and it is a data
   layout choice, exactly the kind the constraint pushes us toward.
4. **Measure against a permissive external crate** (`rustfft`, MIT OR Apache-2.0) as an upper bound.
   If our safe implementation is >15% behind it, that is a bug in our implementation, not in the
   constraint. Note: `rustfft` contains `unsafe` internally; adopting it as a *dependency* is a
   separate decision the user must make, since D2's allowlist governs our crates and is silent on
   dependency internals. **Flagged for decision, not assumed.** *(D12 note: D10 and D12 have since
   settled the general principle — a dependency's internal `unsafe` is a measured, argued trade-off
   recorded at adoption, not an automatic disqualification. `rustfft` as a benchmark upper bound needs
   no decision at all; adopting it as a runtime dependency still does.)*
5. **If, after 1–4, MDCT is still <0.85x and AAC/Opus decode is measurably bottlenecked on it** —
   escalate a scoped `unsafe` exception covering `vaco-tx`'s butterfly kernels only, with the
   measured deficit attached. This is the second-most-likely escalation after Risk A.

#### Risk C — Motion compensation at 0.80–1.00x (the `pmaddubsw` gap)

H.264 MC is the single largest asm area upstream (39.6k lines) and a real 1080p H.264 decode spends
~30% of its time there.

**Re-examined under D12: this risk gets *worse*, and it is the main reason §1.5 is recomputed.** The old
§1.1 estimated the `pmaddubsw` gap at ~1.4x the ops. Measured against the real substrate vocabulary the
composition is **~2.2–2.5x** on the horizontal pass, because there is no widening multiply at all — not
merely no `pmaddubsw`. Band 6 moves from 0.80–1.00x to 0.75–0.95x.

Two things soften it. First, `pavgb` composes at **4 ops in-width** rather than the ~6 a widen-based
route would cost, so the bi-prediction and chroma-averaging paths are cheaper than feared. Second,
mitigation 2 below — the two-pass i16 intermediate — was already the plan, and it is *exactly* the
restructuring that makes the composition affordable: it hoists the u8→i16 widen out of the tap loop, so
widening is paid once per source vector instead of once per tap. What was a performance nicety is now
structural, and a kernel that skips it will be visibly ~2x off in the §3.3(d) `llvm-mca` numbers.

Mitigations:

1. **Batch the call.** Our `Decoder` owns both sides of the DSP boundary. Collect all MC requests for
   a macroblock row into a `SmallVec<McJob>` and dispatch one kernel call that processes 8–16 blocks.
   A 4x4 chroma block wastes 12 of 16 lanes; a batch of four 4x4 blocks wastes none. Expected recovery:
   **+15–25% on small-block-heavy content**, which is exactly the content where the gap is worst.
2. **Two-pass with an i16 intermediate plane.** Do the horizontal pass for a whole 16x21 region into
   an i16 scratch, then the vertical pass. This is what upstream does too, and it means the u8→i16
   widening is amortised over both passes instead of paid twice.
3. **Const-generic tap unrolling.** `fir_h::<TAPS>()` with the coefficient array as a const generic
   parameter, so the multiplies become immediate-operand and LLVM can strength-reduce
   `±1, ±5, ±20`-style H.264 taps into shifts and adds. Upstream's asm does exactly this by hand;
   const generics do it for free across every block size.
4. **Raise it upstream.** `fearless_simd` v1.0 is targeted for early September 2026, the project is
   actively taking feedback, and a widening multiply-add is a reasonable thing for a SIMD substrate to
   expose — we will not be the only consumer who wants it. Cost to us: one issue. This is the highest
   expected-value action on this risk and it should happen *before* we write MC kernels, not after.
5. **Accept the residual.** If after 1–4 we land at 0.90x on MC and 0.97x on whole-frame decode, we ship
   it. A 3% decode deficit is not worth an unsafe exception — and D12 has made that exception both less
   necessary (dispatch is free) and less available (`kernel!` is closed to us).

#### Risk D — VP9/AV1 inverse transforms at 0.75–0.95x

Shuffle-to-arithmetic ratio is the worst of any kernel. `vp9itxfm.asm` is 2,810 lines for a reason.

**Re-examined under D12: this risk gets *better*, and mitigation 3 below becomes free.** The band moves
up from 0.75–0.95x to 0.80–1.00x. Two reasons. (a) Mitigation 3 argued that 10/12-bit variants were "the
strongest argument for shipping the `x86-64-v4` artifact" — that artefact no longer exists, and AVX-512
is instead reachable at runtime on every machine that has it, at no packaging cost. AVX-512 also more
than doubles the vector register file at *all* widths, which is directly what a butterfly network short
of registers needs. (b) The transposes in mitigation 1 are `zip`/`unzip` chains plus `bitcast` between
lane widths, all native and all safe — the `bytemuck::cast` the old text relied on is no longer needed.
`pmulhrsw` remains a genuine gap and is unaffected.

Mitigations:

1. **The transposes are expressible**: an NxN byte/word transpose is `log2(N)` rounds of
   `interleave` with `bitcast` between lane widths (u8↔u16↔u32↔u64) — safe and built into the
   substrate, no `bytemuck`. This lowers to
   exactly the `punpck`/`zip` chain upstream writes by hand. Verified per-kernel in §3's asm check.
2. **Fuse transform with the reconstruct/add step**, so the final transpose's output is consumed
   immediately rather than stored and reloaded.
3. **10/12-bit variants are where wide lanes pay** — i32 lanes at 512 bits give 16-wide butterflies.
   *(Rewritten by D12: this used to be the strongest argument for shipping a separate `x86-64-v4`
   artefact. Runtime dispatch supplies it for free, from the same binary, to every user whose CPU
   supports it. Register a `Variant` at the AVX-512 level and it is selected automatically.)*
4. **AV1 is a fresh-start area anyway** (research §08 §2e: FFmpeg has *no* in-tree AV1 SIMD; upstream
   delegates to dav1d). For AV1 our comparison baseline is dav1d, which is a harder target than
   FFmpeg. We should say so publicly rather than quietly comparing against FFmpeg's non-existent
   native AV1 kernels. **Set the AV1 target at 0.70–0.90x of dav1d and treat anything above 0.80x as
   a win.**

#### Risk E — CRC-32 / hardware-instruction-only primitives

`crc32` (SSE4.2 / ARMv8 CRC) and AES-NI have no portable spelling. Slice-by-16 table CRC-32 runs
~1.5 GB/s vs ~20 GB/s for the instruction.

**Re-examined under D12: unchanged, and worth being precise about why.** It would be natural to assume
that a substrate which hands out an `Sse4_2` capability token also lets us call `_mm_crc32_u64`. It does
— through the `kernel!` macro — but `kernel!` expands `unsafe { … }` into the *calling* crate, and
`#![forbid(unsafe_code)]` rejects macro-expanded `unsafe` exactly as it rejected `multiversion`. So D12
buys nothing here. This is the clearest single illustration of D12's one real limitation: it gives us
safe *dispatch*, not safe *arbitrary intrinsics*.

**Why it is contained:** CRC in our default paths appears in (a) `framecrc`/`framemd5` conformance
output, which is a test-mode feature, and (b) Matroska/MPEG-TS integrity fields, which are per-packet
header-sized. Neither is a throughput path. Measured impact on any of the nine scenarios: expected
<0.5%. If `framecrc` becomes a bottleneck in the differential harness itself, the harness is a dev
tool and may depend on `crc32fast` (MIT/Apache, contains internal unsafe) — a tool-tree dependency,
not a shipped one. **Policy addition proposed:** the D2 allowlist governs shipped crates; the
`crates/tools/` tree is judged on a documented, looser bar since it never enters a distributed binary.

### 1.5 Weighted expectation

**Recomputed 2026-08-21 per D12.** Applying the revised bands to a measured time split for
**1080p H.264 decode** (entropy 30%, MC 30%, deblock 15%, IDCT 10%, intra 5%, other 10%). MC moves from
a 0.90 midpoint to 0.85; nothing else in this workload's mix changes.

```
0.30/1.10 + 0.30/0.85 + 0.15/0.90 + 0.10/0.98 + 0.05/1.00 + 0.10/1.05
  = 0.273 + 0.353 + 0.167 + 0.102 + 0.050 + 0.095 = 1.040 relative time
→ 0.96x before PGO/BOLT, 1.00–1.04x after.     (was 0.98x → 1.02–1.06x)
```

For a **transcode-shaped workload** (decode 35%, scale/convert 25%, filters 20%, encode 20%) the
scale/convert and filter bands dominate; band 1 moved *up*, and the encoder share is small in the default
build. Expect **1.05–1.22x** (was 1.05–1.20x).

For **audio-only decode** (AAC/Opus), `vaco-tx` dominates and it is untouched by D12: **0.85–1.00x**
until Risk B's mitigations land, then **0.95–1.10x**. Unchanged.

**These three numbers are the project's performance thesis. Benchmark scenarios 1, 2 and 5 exist to
confirm or refute them, and §4.7 defines what we do if they are refuted.**

#### Reading the change honestly

The headline decode number went **down** — 0.98x to 0.96x before PGO — and it would be dishonest to let
D12 be presented as a pure win. Three things need saying together:

1. **The loss is real and specific.** It is one kernel family (motion compensation), one missing operation
   family (widening multiply-add), and one estimate that was optimistic before anyone had read the
   substrate's actual operation list. ~2 points of end-to-end decode.
2. **The comparison basis got harder at the same time.** These bands used to assume our binary and
   `ffmpeg` were both built for x86-64-v3. Under D12 they compare *as shipped*. The superseded plan's
   numbers were achievable only by the subset of users who installed the artefact matching their CPU;
   a conservatively-packaged v2 build would have been materially worse than any number in the old table,
   and that case was never priced. **What the new numbers describe is what a user actually gets.**
3. **The variance narrowed enormously.** Under F5 our performance depended on packaging decisions made by
   third parties. Under D12 it depends on the CPU the user has. That is worth more than two points.

If the §11 checklist measures the `pmaddubsw` composition better than 2.2x — plausible, since LLVM's
DAG combines may still fire over the intrinsics — band 6 and this number both recover. If it measures
worse, Risk C mitigation 4 (raise it upstream, before writing MC kernels) is the response.

---

## 2. The kernel authoring standard

> **Rewritten 2026-08-21 per D12.** The substrate is `fearless_simd`, not `std::simd`. The authoring
> model changes from *const-generic over lane count* to *generic over a capability token*, and the
> worked example below is written against the **real v0.7.0 API** — every method named in it exists,
> with the signature shown. Contributors copy this section verbatim, so it has to be right rather than
> plausible.

Every DSP kernel in Vaco has exactly four artefacts, in this order, in this shape. This section is
the template contributors copy. The worked example is `yuv420p → rgb24`, chosen because it is
priority 1 in architecture §7.2 and because it exercises every part of the pattern: widening,
fixed-point MAC, clamping, narrowing, chroma upsampling, a shuffle-based packed store, and a tail.

### 2.0 The six rules

1. **Scalar reference first, and it is normative.** It defines the kernel's output bit-exactly. If a
   SIMD variant disagrees, the SIMD variant is wrong — never the other way round.
2. **The kernel is generic over the capability token `L: Lanes`, and uses native-width vectors.**
   *(Replaces: "arithmetic is generic over lane count `N`; shuffles are macro-generated per concrete
   `N`.")* Write `fn k<L: Lanes>(lanes: L, …)` and take widths from `L::u8s::N`, `L::i32s::N` and so on.
   **There is no lane-count generic parameter and no per-`N` macro.** The old rule existed because
   `simd_swizzle!` required a `const` index array, and a `const` item inside a generic function may not
   reference the function's generic parameters — a hard language boundary that forced shuffle-bearing
   steps behind a macro-generated trait. `fearless_simd`'s `swizzle_dyn` takes a **runtime** index
   vector, so that boundary does not exist and the whole apparatus is deleted (§2.4).

   Use a *fixed-width* type (`u8x16<L>`, `f32x4<L>`) only when the algorithm has a fixed shape — a 4x4
   transform, a 16-byte block transpose, or a shuffle whose cost you want identical at every level
   (§2.4). Both kinds interoperate; `L::u8s::Block` is the 128-bit block type for the level.
3. **The tail is scalar unless the kernel is elementwise-idempotent.** See §2.6. Unchanged.
4. **Every SIMD function is `#[inline(always)]`, all the way down to the `dispatch_kernel!` boundary.**
   *(New, and it is a correctness-of-codegen rule, not a tuning knob.)* The target-feature context of
   the dispatched level reaches a kernel body only through inlining. A body that fails to inline is
   silently compiled at the compiled baseline: still correct, just slow, and no correctness test will
   catch it. The `forbid = ["call"]` assertion in §3.3(c) is what catches it and is now mandatory on
   every kernel. Where forcing inlining is genuinely undesirable, use `Lanes::vectorize(lanes, || …)`
   to re-establish the context without inlining.
5. **No substrate type appears outside `vaco-simd`'s façade or a `kernel/` module, and no kernel
   open-codes a gap composition.** *(Replaces the old rule 4.)* Kernels import `vaco_simd::prelude::*`
   and nothing else. Anything in the plan-11 §5.6 gap table — saturating add/sub, average, abs-diff,
   integer abs, horizontal reduce, widening multiply-add — is called through `vaco_simd::ops`. One
   composition, one place to fix, one place to delete when the substrate grows the operation natively.
6. **A kernel without a `KernelSlot` registration and a `vaco-checkasm` differential test does not
   merge.** Enforced mechanically by `KernelSlot::assert_all_covered()` (§5.6). **And the differential
   test must cover every tier**, not just the one the CI machine happens to have — `VACO_TIER` forces
   each level in turn in a single process, which is a capability the superseded multi-artifact design
   did not have.

### 2.1 Shared types and the coefficient table

Coefficients are derived from ITU-T H.273 §8.3 by `vaco-color`, never transcribed from anywhere.
Q13 fixed point: the widest intermediate is `9539 × 219 + 13075 × 112 ≈ 3.55 × 10⁶`, comfortably
inside `i32`, so `i32` lanes are required and `i16` lanes are not usable for this kernel. That is a
real 4x fan-out from one `u8` vector and it is the reason path #1's band in §1.3 tops out where it does.

```rust
// crates/dsp/vaco-scale/src/kernel/yuv2rgb/mod.rs
#![forbid(unsafe_code)]

// The ONLY import a kernel module needs. Brings in `Lanes`, `Tier`, `ops`,
// `dispatch_kernel!`, and the substrate's vector traits (SimdBase, SimdInt, SimdFloat,
// SimdInto, Select, Bytes, SimdWiden, SimdNarrow, …). Per §2.0 rule 5.
use vaco_simd::prelude::*;

/// Q13 fixed-point YCbCr → RGB coefficients.
///
/// Provenance: ITU-T H.273 (2023-07) §8.3 matrix coefficients, combined with the
/// limited-range scaling of Rec. ITU-R BT.601-7 §2.5.3. Computed by
/// `vaco_color::matrix::rgb_from_ycbcr_q13`, never hand-transcribed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YuvCoeffs {
    pub y_off: i32,   // 16 (limited range) or 0 (full range)
    pub c_off: i32,   // 128 for 8-bit
    pub gy:    i32,   // luma gain
    pub r_cr:  i32,
    pub g_cb:  i32,   // negative
    pub g_cr:  i32,   // negative
    pub b_cb:  i32,
}

pub const SHIFT: u32 = 13;
pub const ROUND: i32 = 1 << (SHIFT - 1);

/// A borrowed 8-bit 4:2:0 planar source. Strides may exceed `width`.
pub struct Yuv420p<'a> {
    pub y: &'a [u8],  pub y_stride: usize,
    pub cb: &'a [u8], pub cr: &'a [u8], pub c_stride: usize,
    pub width: usize, pub height: usize,
}

impl<'a> Yuv420p<'a> {
    #[inline] fn luma_row(&self, r: usize) -> &'a [u8] { &self.y[r * self.y_stride..][..self.width] }
    #[inline] fn cb_row(&self, r: usize) -> &'a [u8] { &self.cb[r * self.c_stride..][..self.width.div_ceil(2)] }
    #[inline] fn cr_row(&self, r: usize) -> &'a [u8] { &self.cr[r * self.c_stride..][..self.width.div_ceil(2)] }
}

/// The kernel signature every variant conforms to. Plain safe `fn` — no `unsafe extern`,
/// and note that the capability token does NOT appear here: it is erased at the
/// `dispatch_kernel!` boundary (§2.5).
pub type Yuv420pToRgb24 = fn(&Yuv420p<'_>, dst: &mut [u8], dst_stride: usize, &YuvCoeffs);
```

### 2.2 The scalar reference — normative, readable, never optimised

Unchanged by D12. The reference has no SIMD in it and therefore no substrate dependency at all — which
is itself worth noticing: the oracle survives a substrate swap untouched, and it is the acceptance
criterion for any replacement.

```rust
pub mod scalar {
    use super::*;

    #[inline] fn clamp8(v: i32) -> u8 { v.clamp(0, 255) as u8 }

    /// Normative reference. Optimising this is forbidden: it exists to be obviously correct
    /// and to be the oracle `vaco-checkasm` compares every SIMD variant against.
    pub fn yuv420p_to_rgb24(src: &Yuv420p<'_>, dst: &mut [u8], dst_stride: usize, k: &YuvCoeffs) {
        for row in 0..src.height {
            let yr  = src.luma_row(row);
            let cbr = src.cb_row(row >> 1);
            let crr = src.cr_row(row >> 1);
            let out = &mut dst[row * dst_stride..][..src.width * 3];

            for (x, px) in out.chunks_exact_mut(3).enumerate() {
                let y  = (yr[x] as i32 - k.y_off) * k.gy + ROUND;
                let cb = cbr[x >> 1] as i32 - k.c_off;
                let cr = crr[x >> 1] as i32 - k.c_off;
                px[0] = clamp8((y + k.r_cr * cr) >> SHIFT);
                px[1] = clamp8((y + k.g_cb * cb + k.g_cr * cr) >> SHIFT);
                px[2] = clamp8((y + k.b_cb * cb) >> SHIFT);
            }
        }
    }
}
```

### 2.3 The SIMD variant — generic over the capability token

Two pieces are worth studying.

**Chroma upsampling.** 4:2:0 needs each chroma sample duplicated across two luma columns. The naive
answers are a gather (slow) or a masked load (slow). The right answer is `interleave(self, self)`, which
produces `[c0,c0,c1,c1,…]` across two full-width vectors and lowers to `punpcklbw`/`punpckhbw` on x86 and
`zip1`/`zip2` on AArch64. In `fearless_simd` this is `SimdBase::interleave(self, rhs) -> (Self, Self)`,
returning the low and high halves — the same shape `std::simd` had, so this part of the design survives
D12 intact. It needs no const tables, works at every level, and means we consume **`n` chroma samples and
produce `2n` pixels per iteration**, so every load is a full-width unmasked load.

**The widening chain.** `SimdWiden::widen(self) -> (Self::Widened, Self::Widened)` returns *two* vectors
of the same total bit width — so `L::u8s` (n bytes) widens to two `L::u16s` (n/2 lanes each), and each of
those widens to two `L::u32s` (n/4 lanes each). One u8 load therefore fans out to **four** i32 vectors.
That fan-out is behind `ops::widen_u8_to_i32` rather than open-coded, per §2.0 rule 5.

```rust
pub mod simd {
    use super::*;

    /// SIMD variant. `#[inline(always)]` is mandatory — see §2.0 rule 4.
    ///
    /// Note what is NOT here: no `const N: usize`, no `LaneCount<N>: SupportedLaneCount`
    /// bound, no `PackRgb24<N>` bound, no per-`N` trait impl. The level is a value.
    #[inline(always)]
    pub fn yuv420p_to_rgb24<L: Lanes>(
        lanes: L, src: &Yuv420p<'_>, dst: &mut [u8], dst_stride: usize, k: &YuvCoeffs,
    ) {
        let n = L::u8s::N;                    // 16 (NEON/SSE4.2), 32 (AVX2), 64 (AVX-512)
        let w = src.width;
        let body = (w / (2 * n)) * (2 * n);   // whole 2n-pixel groups; provably a multiple of 2n

        for row in 0..src.height {
            // Sub-slice to exact lengths ONCE, outside the inner loop. This is what lets
            // LLVM prove every subsequent access in-range and delete the bounds checks (§3.2).
            let (y_body,  y_tail)  = src.luma_row(row).split_at(body);
            let (cb_body, cb_tail) = src.cb_row(row >> 1).split_at(body / 2);
            let (cr_body, cr_tail) = src.cr_row(row >> 1).split_at(body / 2);
            let (o_body,  o_tail)  = dst[row * dst_stride..][..w * 3].split_at_mut(body * 3);

            for (((yc, cbc), crc), oc) in y_body
                .chunks_exact(2 * n)
                .zip(cb_body.chunks_exact(n))
                .zip(cr_body.chunks_exact(n))
                .zip(o_body.chunks_exact_mut(6 * n))
            {
                let cb = L::u8s::from_slice(lanes, cbc);
                let cr = L::u8s::from_slice(lanes, crc);

                // Nearest-neighbour 2x horizontal chroma upsample.
                // interleave(self, self) -> ([c0,c0,c1,c1,..], [c_{n/2},c_{n/2},..])
                let (cb0, cb1) = cb.interleave(cb);
                let (cr0, cr1) = cr.interleave(cr);

                let (o0, o1) = oc.split_at_mut(3 * n);
                block(lanes, &yc[..n], cb0, cr0, o0, k);
                block(lanes, &yc[n..], cb1, cr1, o1, k);
            }

            // Fewer than 2n pixels remain. Elementwise kernel, so an overlapping-vector tail
            // would also be correct; we use scalar because the tail is <1% of a 1080p row
            // and scalar is one code path fewer to verify. See §2.6.
            if !y_tail.is_empty() {
                scalar::row_tail(y_tail, cb_tail, cr_tail, o_tail, body, k);
            }
        }
    }

    /// One `n`-pixel block. `#[inline(always)]` so the caller's loop keeps everything in
    /// registers and LLVM can software-pipeline the two calls against each other.
    #[inline(always)]
    fn block<L: Lanes>(
        lanes: L, yc: &[u8], cb8: L::u8s, cr8: L::u8s, out: &mut [u8], k: &YuvCoeffs,
    ) {
        // u8 -> four i32 vectors. `widen` twice, then `bitcast` u32->i32 (safe, free,
        // and value-preserving because every input is 0..=255). Behind `ops` per rule 5.
        let y  = ops::widen_u8_to_i32::<L>(L::u8s::from_slice(lanes, yc));
        let cb = ops::widen_u8_to_i32::<L>(cb8);
        let cr = ops::widen_u8_to_i32::<L>(cr8);

        // Splats are hoisted out of the quarter-loop by LLVM; written here for clarity.
        let y_off = L::i32s::splat(lanes, k.y_off);
        let c_off = L::i32s::splat(lanes, k.c_off);
        let gy    = L::i32s::splat(lanes, k.gy);
        let round = L::i32s::splat(lanes, ROUND);

        let mac = |q: usize| {
            let yq  = (y[q] - y_off) * gy + round;
            let cbq = cb[q] - c_off;
            let crq = cr[q] - c_off;
            // Broadcast-coefficient MAC — the shape the substrate is good at. NOT a
            // pairwise dot (`ops::madd_i16_i32`), which would cost ~6x here. See §1.1.
            (
                yq + L::i32s::splat(lanes, k.r_cr) * crq,
                yq + L::i32s::splat(lanes, k.g_cb) * cbq
                   + L::i32s::splat(lanes, k.g_cr) * crq,
                yq + L::i32s::splat(lanes, k.b_cb) * cbq,
            )
        };
        let q: [_; 4] = core::array::from_fn(mac);

        // [i32; 4] -> u8, with the Q13 shift and saturation folded in. `saturating_narrow`
        // is NATIVE in the substrate — `packusdw` + `packuswb` / `sqxtun`, guaranteed by
        // the backend rather than hoped for from LLVM's `detectUSatPattern`. That is a real
        // improvement over the superseded `simd_clamp`-then-`cast` formulation, which was a
        // pattern match that could silently stop matching.
        let r = ops::pack_shift_u8::<L>(core::array::from_fn(|i| q[i].0), SHIFT);
        let g = ops::pack_shift_u8::<L>(core::array::from_fn(|i| q[i].1), SHIFT);
        let b = ops::pack_shift_u8::<L>(core::array::from_fn(|i| q[i].2), SHIFT);

        ops::store_rgb24(lanes, r, g, b, out);
    }
}
```

### 2.4 The packed store — `swizzle_dyn`, and why the per-`N` macro is gone

**This subsection used to be 90 lines of `const fn` index-table generation and a
`macro_rules!` impl for `N ∈ {8,16,32,64}`. All of it is deleted.**

The reason it existed: `simd_swizzle!` requires a `const` index array, and a `const` item inside a
generic function may not reference the function's generic parameters. That is a hard language boundary,
and it is what produced the old §2.0 rule 2 ("generic arithmetic, macro-generated shuffles").
`fearless_simd`'s `SimdBase::swizzle_dyn(self, indices: impl SimdInto<Self::Bytes, L>) -> Self` takes a
**runtime** index vector. The boundary is gone, so the apparatus is gone.

The replacement lives in `vaco_simd::ops::store_rgb24` and is ~15 lines:

```rust
// crates/core/vaco-simd/src/ops/pack.rs — the ONLY place this shuffle is written.
//
// Operates on 128-bit BLOCKS, not native width, deliberately: `swizzle_dyn` is exactly one
// `pshufb`/`tbl` on a 128-bit vector, but lane-crossing (and therefore several instructions)
// at 256 and 512 bits. Fixing the block size makes the cost of this step identical at every
// level, which is what we want for a step that is already the kernel's bottleneck. The
// arithmetic above it still runs at full native width. §2.0 rule 2's "fixed-width when the
// algorithm has a fixed shape" clause is exactly this case.
#[inline(always)]
pub fn store_rgb24_block<L: Lanes>(
    lanes: L, r: u8x16<L>, g: u8x16<L>, b: u8x16<L>, out: &mut [u8],   // out.len() == 48
) {
    // Output byte j of block s -> pixel p = (16*s + j)/3, component (16*s + j)%3.
    // Built once with `from_fn` and const-folded; no `const` item, no generic-parameter
    // restriction, no macro.
    for s in 0..3 {
        // Byte j of block s comes from pixel (16s + j)/3, which is always in 0..16
        // for s in 0..3 — so these indices are always in range for all three sources.
        let idx  = u8x16::from_fn(lanes, |i| ((16 * s + i) / 3) as u8);
        let comp = |c: usize| u8x16::from_fn(lanes, |i| u8::from((16 * s + i) % 3 == c));

        let rs = r.swizzle_dyn(idx);
        let gs = g.swizzle_dyn(idx);
        let bs = b.swizzle_dyn(idx);

        let v = comp(0).simd_eq(u8x16::splat(lanes, 1)).select(
            rs,
            comp(1).simd_eq(u8x16::splat(lanes, 1)).select(gs, bs),
        );
        v.store_slice(&mut out[16 * s..][..16]);
    }
}
```

**Cost, stated not assumed:** 3 `swizzle_dyn` + 2 `select` per 16-byte output block, so **15 operations
per 48 output bytes**. The superseded two-input `simd_swizzle!` formulation was 6 shuffles + 3 blends = 9
per 48 bytes. So the pack step is **~1.7x more expensive** than the old plan assumed — and it is the
kernel's bottleneck, which is why band #1's improvement in §1.3 (1.05–1.30x) comes from runtime AVX-512
reach and native `saturating_narrow`, not from the pack. Two mitigations, both already in the plan:

- **The index and mask vectors are loop-invariant.** Hoist them out of the row loop; LLVM does this
  reliably from `from_fn` with a closure over `i` only. Verify in the §3.3(c) assertion (`max_insns`
  catches it if it fails).
- **`swizzle_dyn` with an out-of-range index yields zero on every backend**, which permits replacing the
  two `select`s with two `or`s and per-channel index vectors. Same operation count on paper, one fewer
  dependency chain in practice. Left as a measured optimisation rather than the default, because the
  `select` form's semantics are documented and the `or` form's rely on a lowering detail.

**Design consequence for `vaco-scale` (strengthened by D12).** Because the 3-byte pack is the expensive
step, the ops graph (research §01 §11) should keep 4-byte intermediates internally and fuse the
`SWS_OP_PACK`-equivalent 3-byte packing into the terminal `WRITE` op only. **RGBA output needs no shuffle
at all** — `fearless_simd` has a native `SimdInterleaved::store_four_interleaved`, which is `vst4` on
NEON and a short shuffle chain on x86, wrapped as `ops::store_rgba`. RGB24 sits at the bottom of path
#1's band and RGBA at the top, and the gap between them is now *wider* than the old plan implied. Where
the pipeline can choose, it should choose RGBA.

### 2.5 KernelSet registration

Architecture §7.3: selection happens once at component construction into a struct of ordinary safe
`fn` pointers. **The consumer-facing shape is unchanged by D12** — which is the point plan 11 §5.3 makes
about why this revision was cheap. What changed is underneath: `Width` (a compile-time property) becomes
`Tier` (a runtime-detected level), and each variant is a *dispatching wrapper* rather than a directly
monomorphised function, because a `fn` pointer erases the token that `dispatch!` binds.

```rust
// crates/core/vaco-simd/src/kernelset.rs
#![forbid(unsafe_code)]

/// A CPU capability level. Newtype over the substrate's `Level`; see plan 11 §5.2.
///
/// Replaces the old `Width { Scalar, V128, V256, V512 }`. That enum described what the
/// *build* could emit; this describes what the *machine* can execute. Under F5 those were
/// the same thing and the distinction did not matter. Under D12 they are different and it
/// is the whole point.
pub use crate::Tier;

pub struct Variant<F: Copy + 'static> {
    pub name: &'static str,     // "scalar" | "sse4.2" | "avx2" | "avx512" | "neon"
    pub tier: Tier,             // the MINIMUM tier this variant requires
    pub func: F,
}

pub struct KernelSlot<F: Copy + 'static> {
    pub id: &'static str,
    pub reference: F,
    pub variants: &'static [Variant<F>],
}

impl<F: Copy + 'static> KernelSlot<F> {
    /// Highest-tier variant the running CPU satisfies, subject to the per-microarchitecture
    /// preference table (e.g. prefer AVX2 over AVX-512 on parts that downclock under 512-bit
    /// load — a real effect on Skylake-SP-era Xeons and exactly the kind of thing that is
    /// now measurable in one process instead of requiring two build artefacts).
    pub fn select(&self, cpu: &CpuProfile) -> F {
        self.variants
            .iter()
            .filter(|v| v.tier.rank() <= cpu.max_useful_tier.rank())
            .max_by_key(|v| v.tier.rank())
            .map(|v| v.func)
            .unwrap_or(self.reference)
    }
    /// Force a named variant. Used by `vaco-checkasm`, by `-cpuflags` CLI compatibility, and
    /// by `VACO_KERNEL_OVERRIDE=scale/yuv2rgb/*=scalar` for bisecting a mismatch.
    pub fn select_named(&self, name: &str) -> Option<F> { /* … */ }
}
```

The bridge from a level-generic body to a `fn` pointer is one generated wrapper per kernel:

```rust
// crates/dsp/vaco-scale/src/kernel/yuv2rgb/registry.rs
use vaco_simd::prelude::*;
use super::{Yuv420pToRgb24, scalar, simd};

/// The dispatching wrapper. `dispatch_kernel!` matches the detected level, binds the
/// capability token, and calls the monomorphisation for that level. Generated by
/// `vaco_simd::kernel_variants!` so it is never hand-written and never gets out of step.
///
/// COST MODEL, unchanged from the pre-D12 fn-pointer table: one indirect call plus one
/// jump-table branch on a cached enum, paid per *row*, never per pixel.
fn dispatched(src: &Yuv420p<'_>, dst: &mut [u8], stride: usize, k: &YuvCoeffs) {
    dispatch_kernel!(Tier::detect(), lanes => simd::yuv420p_to_rgb24(lanes, src, dst, stride, k))
}

pub static YUV420P_TO_RGB24: KernelSlot<Yuv420pToRgb24> = KernelSlot {
    id: "scale/yuv2rgb/yuv420p_to_rgb24",
    reference: scalar::yuv420p_to_rgb24,
    // One entry per level, not one per lane count. `simd::yuv420p_to_rgb24` is a SINGLE
    // source function; the substrate monomorphises it per level and `dispatched` selects.
    variants: &[Variant { name: "dispatched", tier: Tier::baseline(), func: dispatched }],
};

/// Resolved once, at `SwsGraph` construction — never per row, never per pixel.
pub struct Yuv2RgbKernels { pub yuv420p_to_rgb24: Yuv420pToRgb24, /* … */ }

impl Yuv2RgbKernels {
    pub fn select(cpu: &vaco_simd::CpuProfile) -> Self {
        Self { yuv420p_to_rgb24: YUV420P_TO_RGB24.select(cpu) }
    }
}
```

**What happened to the four `x8`/`x16`/`x32`/`x64` variants.** They are gone, and their disappearance is
the clearest single illustration of what D12 changed. Under F5 they were four separately-monomorphised
functions registered as four `Variant`s, and `KernelSlot::select` chose between them using a
per-microarchitecture width-preference table. Under D12 there is **one** source function; the substrate
produces the per-level monomorphisations, and the width follows the level. Registering multiple
`Variant`s is still supported and is still the right tool when two *algorithms* compete (a `swizzle_dyn`
pack versus an `interleave`-tree pack, say) — but not for width, which is no longer a choice we make.

### 2.6 Tail handling — the three options and when to use each

| Strategy | Cost | Correct when | Used by |
|---|---|---|---|
| **Scalar remainder** (`chunks_exact` + `.remainder()`) | ~`n` scalar iterations per row | Always | Default. Everything unless a rule below applies. |
| **Overlapping last vector** — process pixels `[w-n .. w)` with a full vector, recomputing up to `n-1` already-written outputs | One extra vector op per row, zero scalar code | Kernel is a **pure elementwise function of input** (recomputation yields identical bytes) **and** input/output do not alias | Colour conversion, format conversion, LUT application, audio format conversion |
| ~~**Masked load/store**~~ | — | — | **Withdrawn by D12.** `fearless_simd` exposes no masked load or store — `SimdBase` has `from_slice`/`store_slice` and nothing predicated — and there is no composition for one that is cheaper than the two rows above. The old entry also depended on a separate `x86-64-v4` artefact, which no longer exists. If a masked tail is ever wanted it is an upstream request, not a local workaround. |

**Consequence of the withdrawal, stated so nobody rediscovers it the hard way:** the tail is now always
one of the first two rows. Since the overlapping-vector trick was already the fast path for exactly the
elementwise kernels where masking would have helped, the practical loss is close to zero — the third row
was only ever going to be selected on AVX-512 and SVE hardware, and those are precisely the machines
where `n` is largest and the tail is proportionally smallest.

Rules that forbid the overlap trick: in-place kernels; kernels with per-output-position state
(dithering with error diffusion, IIR filters); kernels whose write is wider than their logical output
(the RGB24 packer writes `3N` bytes and would overrun the row into the next row's stride padding).
`vaco-checkasm`'s stride-guard check (§5.4) is what catches a violation.

### 2.7 The differential test

Two artefacts: an inline `#[test]` for fast local feedback, and the `vaco-checkasm` registration for
the exhaustive/edge-case/benchmark sweep.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use vaco_checkasm::{Differential, EdgeCase};

    #[test]
    fn yuv420p_to_rgb24_matches_reference() {
        Differential::new("scale/yuv2rgb/yuv420p_to_rgb24")
            // Shapes deliberately include: minimum legal (2x2), exactly one vector,
            // one vector plus one pixel (tail), odd width (chroma div_ceil), and real frames.
            .shapes(&[(2, 2), (16, 2), (17, 3), (31, 5), (64, 64), (352, 288), (1920, 1080), (1921, 1081)])
            // Strides deliberately exceed width, so an over-wide store is detectable.
            .stride_padding(&[0, 1, 64])
            .edge_cases(&[
                EdgeCase::Zeros,
                EdgeCase::Max,                       // Y=255, C=255 -> clamps high on all three
                EdgeCase::AlternatingMinMax,
                EdgeCase::LimitedRangeBoundaries,    // Y ∈ {15,16,235,236}, C ∈ {15,16,240,241}
                EdgeCase::ClipInducing,              // Y=235, Cr=240 -> R clamps; Y=16, Cb=240 -> G clamps
                EdgeCase::Random { seeds: 64 },
            ])
            .coeffs(&[BT601_LIMITED_8, BT709_LIMITED_8, BT601_FULL_8, BT2020_NCL_LIMITED_8])
            .reference(scalar::yuv420p_to_rgb24)
            // D12: sweep TIERS, not lane counts. `all_tiers()` forces each level the running
            // CPU supports in turn, in one process, via `VACO_TIER` — and asserts they agree
            // bit-exactly with each other and with the reference. This is a capability the
            // superseded multi-artifact design did not have: it could only compare levels by
            // rebuilding, so cross-level agreement was never actually tested on one machine.
            .all_tiers(simd::yuv420p_to_rgb24)
            // Tiers ABOVE what this CPU supports still get correctness coverage from the
            // `Fallback` level via `force_support_fallback`; timing coverage needs the metal.
            .guard_stride_padding(true)   // assert padding bytes are byte-identical after the call
            .run();
    }
}

/// Registered into the generated checkasm inventory (architecture §6: explicit registration,
/// no linker tricks). One line per kernel in `crates/tools/vaco-checkasm/inventory.toml`.
pub fn register(reg: &mut vaco_checkasm::Registry) {
    reg.add(YUV420P_TO_RGB24.id, /* the same Differential builder, as a closure */);
}
```

### 2.8 Checklist a kernel PR must satisfy

- [ ] Scalar reference exists, is unoptimised, and carries a provenance comment naming the spec.
- [ ] SIMD body is `fn k<L: Lanes>(lanes: L, …)` and uses native-width types (`L::u8s`, `L::i32s`),
      or a fixed-width type with a stated reason (§2.0 rule 2).
- [ ] **Every** SIMD function on the path is `#[inline(always)]`, and the §3.3(c) assertion for this
      kernel includes `forbid = ["call"]` (§2.0 rule 4 — this is a codegen-correctness check, not a
      style one).
- [ ] No plan-11 §5.6 gap operation is open-coded; each goes through `vaco_simd::ops` (§2.0 rule 5).
- [ ] If the kernel uses `ops::madd_i16_i32`, the PR description says why `ops::wmla_u8_i16` does not
      fit. Pairwise dot costs ~6x; broadcast-coefficient MAC does not.
- [ ] `swizzle_dyn` calls state whether they need full-width or `_within_blocks` semantics, and
      loop-invariant index vectors are hoisted out of the row loop.
- [ ] `KernelSlot` registered. Multiple `Variant`s only where two *algorithms* compete — not for
      width, which the substrate now chooses (§2.5).
- [ ] `Differential` test covers: min shape, exactly-one-vector, one-vector-plus-one, odd dimensions,
      ≥2 real resolutions, ≥3 stride paddings, all listed edge cases, ≥64 random seeds — **and
      `.all_tiers()`, so every level the machine supports is proven bit-identical to the reference and
      to each other.**
- [ ] Stride-padding guard enabled unless the kernel documents why it writes padding.
- [ ] `#[vaco::must_vectorize]` on the inner loop, and the §3.4 asm assertion names the expected
      instruction class.
- [ ] `divan` micro-benchmark registered (§4.2) with a throughput counter.
- [ ] `docs/` entry updated per the repository documentation standard.

---

## 3. Autovectorization discipline

Explicit vector code covers the ~150 kernels we hand-write. The other ~90% of the codebase — bitstream
parsing, header construction, buffer copies, per-row loops in filters we have not hand-vectorized yet
— relies on LLVM's loop vectorizer. LLVM will vectorize reliably *if the source removes every reason
not to*, and will silently stop when someone adds one back. This section is the ruleset plus the
mechanism that makes the silent stop loud.

> **D12 makes this section stronger, in a way worth stating explicitly.** Under `std::simd` the
> autovectorized 90% was compiled at the **compiled baseline** and could never do better, no matter what
> CPU it ran on. `fearless_simd`'s `dispatch!` monomorphises *any* body generic over `L: Lanes` per
> level, including bodies containing nothing but plain scalar loops — the token is unused, but the
> `#[target_feature]` context it carries applies to the whole function, so **LLVM autovectorizes that
> loop at the dispatched level**. This is the substrate's documented "automatic vectorization" mode.
>
> Practical consequence: a hot scalar loop that is not worth hand-vectorizing can be wrapped in
> `fn f<L: Lanes>(_: L, …)` + `dispatch_kernel!` and get runtime-dispatched autovectorization for the
> cost of two lines. Use it for the loops that `#[vaco::must_vectorize]` already marks and that sit on a
> measured hot path — not indiscriminately, because each one costs three monomorphisations on x86
> (§1.4 Risk A cost 1).

### 3.1 The rules

**R1 — `chunks_exact` / `chunks_exact_mut`, never a manual index loop.**
`chunks_exact(K)` gives LLVM a provably-`K`-length inner slice and a trip count that is a multiple of
`K`. `for i in 0..n { a[i] = b[i] + c[i] }` gives it neither: it must prove `a.len() >= n` and
`b.len() >= n` independently, and if it cannot, the bounds check stays in the loop and vectorization
dies.

**R2 — Sub-slice to exact lengths *before* the loop, never inside it.**
This is the single highest-value rule and the one violated most often in media code, because strides
exceed widths everywhere.

```rust
// BAD — `row_stride` is not `width`, so every access needs its own check
for y in 0..h { for x in 0..w { dst[y*ds + x] = f(src[y*ss + x]); } }

// GOOD — one check per row, none in the inner loop
for y in 0..h {
    let s = &src[y * ss..][..w];
    let d = &mut dst[y * ds..][..w];
    for (d, &s) in d.iter_mut().zip(s) { *d = f(s); }
}
```

**R3 — Iterator + `zip`, not index arithmetic.** `zip` gives LLVM a single fused trip count and
proves both sides in range. Three-way and four-way zips are fine; beyond that, prefer
`izip!`-style destructuring (or nest `zip` as in §2.3) rather than reintroducing indices.

**R4 — No panic path inside the loop.** Bounds checks, integer-overflow checks (debug), slice-index
panics, `unwrap`, `?`, and `assert!` all create a loop-carried control-flow edge to a landing pad.
LLVM's vectorizer refuses loops with more than one exit. Hoist every check above the loop.

**R5 — Hint with `assert!`, deliberately and sparingly.** The legitimate uses:

```rust
// Teach LLVM the two slices are the same length -> deletes the second bounds check chain
assert_eq!(a.len(), b.len());
// Teach LLVM the trip count is a multiple of the vector width -> deletes the scalar epilogue
assert!(n % 16 == 0);
// Teach LLVM a divisor is non-zero -> deletes the div-by-zero branch
assert!(stride != 0);
```

An `assert!` that LLVM cannot use is dead weight in a hot loop; every one we add gets a comment
naming which check it removes, and the §3.4 asm assertion is what proves it worked.

**R6 — Integer reductions reassociate; floating-point reductions do not.**
`iter().sum::<u32>()` vectorizes. `iter().sum::<f32>()` does **not**, because reassociating FP
addition changes the result and Rust has no `-ffast-math`. Every FP reduction in a hot path is
written with explicit multiple accumulators:

```rust
// Audio FIR dot product — 4 accumulators, explicitly. Result differs from the naive
// scalar sum in the last ulp; the scalar reference uses the SAME 4-accumulator schedule
// so the differential test is exact.
let mut acc = [Simd::<f32, 8>::splat(0.0); 4];
for (xs, cs) in x.chunks_exact(32).zip(c.chunks_exact(32)) {
    for j in 0..4 {
        acc[j] += Simd::from_slice(&xs[j*8..]) * Simd::from_slice(&cs[j*8..]);
    }
}
```
This has a knock-on rule: **for float kernels, the scalar reference must use the identical
accumulation order as the SIMD variant**, or the differential test can only assert a ULP bound
instead of bit-exactness. We prefer bit-exactness; the reference is written to the SIMD schedule
(and says so in a comment) rather than the SIMD being written to a naive reference.

**R7 — `#[inline(always)]` on the kernel body, `#[inline(never)]` on the per-plane wrapper.**
The body must inline so the loop is visible to the vectorizer with concrete `N`. The wrapper must not,
so that `perf`, PGO profiles and BOLT see a named symbol per kernel instead of one inlined blob.

**R8 — No `dyn Trait`, no closure captured across the loop boundary, no `Box<dyn Fn>` per pixel.**
Architecture §1.4 already forbids this; it is restated because it is the most common way a
well-meaning refactor destroys a kernel.

**R9 — Prefer `u32`/`i32` loop counters materialised as `usize` slice ops.** Avoid mixed-width index
arithmetic that forces sign/zero-extension inside the loop.

**R10 — Structure of arrays at every seam that a kernel touches.** Interleaved data forces
de-interleave shuffles. `vaco-frame`'s planar storage already does this for pixels; `vaco-tx` must do
it for complex numbers (§1.4 Risk B); `vaco-resample` must do it for channels.

**R11 — Loop over rows outer, columns inner, and tile when the working set exceeds L2.**
For 4K frames a full-row pass over three planes is ~24 MB of traffic. `vaco-scale`'s graph executor
processes in horizontal bands sized to keep `input band + intermediates + output band` under
~50% of L2 (queried at startup, defaulting to 512 KiB). Measured separately in benchmark scenario 3.

### 3.2 What to do about bounds checks specifically

Rust's bounds checks are not a tax we accept; they are a tax we structure away. In order of preference:

1. Sub-slice to exact length (R2) — removes the check entirely.
2. `chunks_exact` (R1) — removes the check entirely.
3. `zip` (R3) — removes the redundant side.
4. `assert!` at the top (R5) — moves one check out of the loop.
5. `get_unchecked` — **forbidden.** This is `unsafe`. If a kernel appears to need it, the loop is
   structured wrongly; rewrite it, or escalate.

CI runs a `bounds-check-audit` job that builds the DSP crates with
`-Cllvm-args=-pass-remarks-analysis=loop-vectorize` and greps for
`loop not vectorized: could not determine number of loop iterations` and
`loop not vectorized: cannot prove it is safe to reorder memory operations`, both of which are
bounds-check symptoms in practice.

### 3.3 Verifying vectorization happened

Four mechanisms, in increasing cost:

**(a) Optimization remarks — cheap, runs on every build of the DSP crates.**
`rustc -Cremark=loop-vectorize -Zremark-dir=target/remarks` emits YAML per function. A small tool
(`crates/tools/vaco-vecheck`) parses them and cross-references against `vecheck.toml`.

**(b) `#[vaco::must_vectorize]` — the declarative contract.**
A proc-macro attribute in `vaco-simd` that does nothing to codegen but records the function's mangled
symbol into a build-time manifest. `vaco-vecheck` then requires that a `loop-vectorize` **Passed**
remark exists for that symbol, and fails the build if it finds a **Missed** remark instead. The remark
text is printed verbatim so the author sees *why*.

**(c) `cargo-show-asm` instruction assertions — the ground truth for hand-written kernels.**
Every `KernelSlot` variant gets an entry in `vecheck.toml`:

```toml
[[kernel]]
id      = "scale/yuv2rgb/yuv420p_to_rgb24"
variant = "x16"

[kernel.expect.x86_64-v3]
require     = ["vpmulld|vpmaddwd", "vpackusdw", "vpackuswb", "vpunpcklbw", "vpshufb"]
forbid      = ["vpgatherdd", "call", "\\bud2\\b"]     # no gather, no outlined call, no panic
max_insns   = 96                                       # per 16-pixel block body

[kernel.expect.aarch64-v82]
require     = ["smull|smlal", "sqxtun", "zip1", "tbl"]
forbid      = ["\\bbl\\b", "brk"]
max_insns   = 88
```

`just vecheck` runs `cargo asm --target <t> --simplify <sym>` for each entry and matches the
regexes. `forbid = ["call"]` is the strongest single assertion in the file: an outlined call inside a
kernel body means either the panic path was not hoisted or `#[inline(always)]` failed. `max_insns`
catches gradual bloat that the require/forbid lists miss.

**(d) `llvm-mca` throughput modelling — for the kernels that matter most.**
For the top-20 kernels, `vecheck` extracts the loop body and runs `llvm-mca -mcpu=<uarch>
-iterations=100`, recording `Block RThroughput` and port pressure into the results store alongside
the divan numbers. This catches "still vectorized, but now port-5 bound because someone added a
shuffle" — a class of regression wall-clock benchmarks on a noisy machine can miss.

### 3.4 What to do when vectorization silently regresses

The failure is always caught by one of (a)–(d); the response is a fixed ladder:

1. **Read the remark.** `vaco-vecheck` prints the LLVM reason string. In practice ~80% of regressions
   are R2 or R4 violations reintroduced by a refactor, and the fix is mechanical.
2. **Bisect the source change** with `just vecheck --kernel <id>` (runs in ~2 s for one kernel).
3. **If the source is unchanged and the toolchain moved:** this is a rustc/LLVM regression. Reproduce
   on Godbolt, file upstream, and record a **waiver** in `vecheck.toml`:
   ```toml
   [[waiver]]
   kernel = "scale/yuv2rgb/yuv420p_to_rgb24"
   variant = "x32"
   reason = "rustc 1.9x: vpermd not folded, +6% on this kernel"
   upstream = "https://github.com/rust-lang/rust/issues/NNNNN"
   expires = "2026-12-01"     # CI fails once this passes, forcing a re-check
   cost_pct = 6.0             # measured, and summed into the release perf report
   ```
   Waivers expire. The sum of `cost_pct` across live waivers is published in the release notes so the
   debt is visible rather than accumulating silently.
4. **If it cannot be recovered by restructuring:** promote the loop from autovectorized to explicit
   `fearless_simd` (which is not subject to the vectorizer's whims at all). This is always available
   and is the reason the strategy is robust: autovectorization is an *optimisation*, explicit vector
   code is the *guarantee*. Note the intermediate step D12 adds: before hand-writing vectors, try
   wrapping the loop in `fn f<L: Lanes>(_: L, …)` + `dispatch_kernel!`, which often fixes a "missed"
   remark outright because the loop is now compiled with a richer target-feature set (§3 preamble).
5. **Only if explicit vector code also cannot express it** — i.e. it needs an operation in the plan-11
   §5.6 gap table and the composition is too slow — escalate per §1.4 Risk A′ and §11: restructure,
   then raise it upstream. Reaching for the intrinsic is *not* an available step: `kernel!` is closed
   to us under `#![forbid(unsafe_code)]`.

### 3.5 Toolchain pinning as a performance decision

> **Revised 2026-08-21 per D12.** D8 pinned *nightly*, for `portable_simd`. `fearless_simd` builds on
> stable 1.89+, so **the pin is now a pinned stable release** (plan 11 §1.2 works through the full
> toolchain consequences).

The pin's *performance* rationale is unchanged and is now its **only** rationale: LLVM version changes
move vectorization and scheduling decisions, and our kernels carry instruction-level assertions. So the
channel names an exact stable release rather than `"stable"` — a floating channel would let a CI runner
silently change codegen under a benchmark.

What changes in practice:

- **Cadence: per stable release (~6 weeks), not monthly-nightly.** Stable releases are far less likely to
  break us, and staying close to the head keeps each bump's diff small.
- **The bump PR's obligation is unchanged and non-negotiable:** a full `vecheck` run and a full benchmark
  comparison against the previous pin on the reference machine. A bump costing >1% on any of the nine
  scenarios is reverted or lands with an explicit, quantified justification.
- **One mechanism moves to a non-gating nightly lane.** §3.3(a)'s optimisation-remark parsing uses
  `-Zremark-dir`, which is nightly-only (`-Cremark=` itself is stable). The *gating* vectorization check
  is §3.3(c), the `cargo-show-asm` instruction assertions, which is stable and was already described
  there as the ground truth. Nothing that gates a merge requires nightly.
- **A new class of regression to watch for.** With multiversioning, a toolchain bump can change codegen
  differently *per level* — an AVX-512 monomorphisation can regress while the AVX2 one improves.
  `vecheck` entries therefore key on `(kernel, tier)` and the bump PR must run the matrix, not one
  representative build. This replaces the superseded cross-artefact check in §5.7.

---

## 4. The benchmark suite

Three tiers, three tools, one results store.

| Tier | Tool | Question it answers | Frequency |
|---|---|---|---|
| **Micro** (per kernel, cycles) | `vaco-checkasm --bench` (§5) | Is this kernel variant faster than the scalar reference and than last week's build? | Every PR (correctness), nightly (timing) |
| **Meso** (per kernel, throughput) | `divan` in `crates/*/benches/` | What is this kernel's MB/s or Mpixel/s, across lane widths and shapes? | Every PR touching the crate |
| **Macro** (whole pipeline, vs ffmpeg) | `vaco-bench` (own harness) | Are we faster than FFmpeg at the thing users actually run? | Nightly + pre-release |

### 4.1 Why `divan` rather than `criterion`

D8 permits either. We choose **divan** (MIT OR Apache-2.0) for the meso tier:

- **Argument matrices are first-class.** `#[divan::bench(consts = [8, 16, 32, 64], args = SHAPES)]`
  expresses "this kernel across four lane widths and six frame shapes" as one function. In criterion
  the same thing is a hand-rolled `BenchmarkGroup` loop. Given we register ~150 kernels each with
  4 widths, this is the difference between a maintainable suite and an unmaintainable one.
- **Per-sample overhead is roughly an order of magnitude lower**, so it can resolve kernels in the
  50–500 ns range without forcing every author to hand-write an inner repetition loop. Many of our
  kernels are a single 16-pixel block.
- **`divan::counter::BytesCount` / `ItemsCount`** give throughput directly, which is the number we
  actually compare against FFmpeg, rather than an opaque time-per-iteration.
- **Dependency weight**: divan pulls a handful of crates; criterion pulls `plotters`, `rayon`,
  `regex`, `serde_json` and more into the dev tree, which slows every `cargo test` in the workspace.

What we give up: criterion's bootstrap statistics, its HTML reports, and `critcmp`. We do not need
them, because (a) cycle-accurate regression detection lives in `vaco-checkasm`, which has a stronger
statistical model for the thing it measures, and (b) the results store (§4.5) renders our own report
across all three tiers uniformly. Criterion's plots would only cover one tier.

Macro benchmarks use **neither**: they are whole-process runs of `vaco` and `ffmpeg`, where an
in-process harness adds nothing and cannot measure startup latency or peak RSS. `vaco-bench` drives
them directly.

### 4.2 Micro/meso: the per-kernel benchmark

Every `KernelSlot` gets a divan benchmark, generated by a macro so registration is one line:

```rust
// crates/dsp/vaco-scale/benches/yuv2rgb.rs
use vaco_bench_support::{frame_shapes, Yuv420pFixture};

#[divan::bench(consts = [8, 16, 32, 64], args = frame_shapes::VIDEO)]
fn yuv420p_to_rgb24<const N: usize>(b: divan::Bencher, shape: Shape) {
    let src = Yuv420pFixture::deterministic(shape, /* seed */ 0xC0FFEE);
    let mut dst = vaco_pool::aligned_vec(shape.rgb24_len());
    b.counter(divan::counter::ItemsCount::new(shape.pixels()))
     .counter(divan::counter::BytesCount::new(shape.yuv420p_bytes() + shape.rgb24_len()))
     .bench_local(|| simd::yuv420p_to_rgb24::<N>(
         divan::black_box(&src.view()), divan::black_box(&mut dst), shape.rgb_stride(), &BT709_LIMITED_8));
}

#[divan::bench(args = frame_shapes::VIDEO)]
fn yuv420p_to_rgb24_scalar(b: divan::Bencher, shape: Shape) { /* same, scalar reference */ }
```

`frame_shapes::VIDEO` = `[(352,288), (640,360), (1280,720), (1920,1080), (3840,2160), (1921,1081)]`
— the last one deliberately odd to keep the tail path in the measurement.

Fixtures are **deterministic and synthetic** (seeded `Xoshiro256++`, not `rand`'s thread RNG), so
micro-benchmarks require no corpus download and no licence considerations at all.

### 4.3 The nine reproducible scenarios

These are the macro tier, run by `vaco-bench`, and they are the scenarios research §08 §5 proposes,
specified to the point of being executable. Every scenario runs **both** `vaco` and the reference
`ffmpeg` on the same machine in the same session, interleaved (A/B/A/B) so drift affects both equally.

| # | Scenario | Inputs | Metrics | Notes |
|---:|---|---|---|---|
| **S1** | **Decode throughput per codec** | H.264, HEVC, VP9, AV1, plus ProRes/FFV1 for intra-only; each at 480p/1080p/2160p; threads ∈ {1, 4, 16, ncpu} | fps, cycles/frame, instructions/frame, IPC, cache-miss rate | `perf stat -e cycles,instructions,cache-misses,branch-misses`. Decode to `-f null` so muxing is excluded. AV1 compares against **dav1d**, not FFmpeg's native path (§1.4 Risk D). |
| **S2** | **Encode speed/quality curves** | AV1 (rav1e/SVT-class settings), VP9, plus opt-in HEVC/AAC builds; 3 rate points per codec; 1 and N threads | fps at fixed CQ; VMAF/SSIM/PSNR at fixed bitrate; bits at fixed quality | Curves, not points. Report the whole (fps, VMAF) frontier; a codec that is 20% faster at 0.5 VMAF lower has not won. VMAF via a pinned `libvmaf` build used only as a measuring instrument. |
| **S3** | **Scale / pixel-format conversion throughput** | yuv420p→rgb24, yuv420p→rgba, yuv420p→nv12, yuv420p10→yuv420p, bicubic 4K→1080p, bilinear 1080p→4K, lanczos 1080p→720p | Mpixel/s, MB/s, and L2/L3 miss rate | Single-threaded (upstream rarely threads this — a fair single-core comparison). Also run with `--tile-off` to quantify R11's banding win. |
| **S4** | **Resample throughput** | 44.1k→48k, 48k→44.1k, 48k→16k, 96k→48k; s16↔fltp; mono/stereo/5.1/7.1; each filter type (cubic, blackman-nuttall, kaiser) | Msample/s per channel; and SNR vs a high-precision reference resampler | SNR matters: a fast resampler that is 6 dB worse is not faster. |
| **S5** | **Filter throughput** | `scale`, `bwdif`, `yadif`, `gblur`, `hqdn3d`, `unsharp`, `overlay`, `format`, plus `atempo`/`loudnorm` for audio; 1080p and 4K | fps per filter, and fps for a 5-filter chain | The chain number is the one that exposes whether our filter-graph scheduler adds overhead FFmpeg's does not. |
| **S6** | **Seek latency** | mp4/H.264, mkv/HEVC, ts/H.264, webm/VP9, mp4/AV1; 200 pseudo-random timestamps per file, fixed seed | p50 / p95 / p99 / max ms from seek call to first decoded frame; also seeks/s | Separately reported for keyframe-accurate and exact seeks, and for local file vs `http` protocol. |
| **S7** | **Startup / cold-start latency** | `vaco -version`, `vaco-probe` on a 10 MB mp4, decode-1-frame, transcode-1-second | p50/p95 ms wall clock, process-start to first output byte | **D12: the launcher is gone.** This used to carry the §1.4 multi-artifact launcher's `exec` cost (~0.3 ms Unix, ~2 ms Windows) as a permanent asterisk. Startup now pays one cached CPUID instead. What S7 must still report honestly is the **binary size** and its page-in cost, since multiversioning makes the image larger (§1.4 Risk A cost 1). Run with cold and warm page cache separately. |
| **S8** | **Memory high-water mark** | The S1 and S2 matrices, plus a 4K HDR transcode | peak RSS, peak virtual, allocation count, bytes allocated | Via `/proc/self/status` VmHWM on Linux, `mach_task_basic_info` on macOS. Allocation count via a counting global allocator in a dedicated build (a counting allocator is safe Rust). Frame-thread count vs RSS is the key curve (§7.2). |
| **S9** | **Environment control (applies to S1–S8)** | — | — | See §4.4. Every result carries the environment fingerprint; results from an uncontrolled machine are recorded but never used for regression gating. |

Additional scenario we add beyond research §08's nine, because our architecture makes it a risk:

| **S10** | **Determinism across thread counts** | S1's matrix | framemd5 equality between `-threads 1` and `-threads N` | Not a performance metric, but it runs on the same harness and it is what stops us "winning" S1 by breaking output. Gates merges. |

### 4.4 Machine control

The reference runner is a dedicated, self-hosted, physically-owned machine — CI cloud runners are
unusable for performance gating (noisy neighbours, opaque frequency behaviour). Two are provisioned:
one x86-64 (AMD Zen 4 or Intel Raptor Lake class) and one AArch64 (Ampere or Apple Silicon).

`vaco-bench` refuses to produce gating results unless it verifies, and records:

```
governor            = performance             # cpupower frequency-set -g performance
turbo               = disabled                # intel_pstate/no_turbo=1 or cpb=0 on AMD
smt                 = disabled                # or: sibling of the pinned core is offline
affinity            = pinned                  # sched_setaffinity to an isolcpus-reserved core set
nohz_full           = on the reserved cores
aslr                = disabled for the run    # setarch -R, for cycle-count stability
thp                 = madvise (recorded, not forced)
irq affinity        = steered away from reserved cores
ambient temp / fan  = recorded where readable; a thermal-throttle event invalidates the run
```

Plus the fingerprint: CPU model + stepping + microcode, kernel version, glibc version, rustc commit
hash, LLVM version, `RUSTFLAGS`, target-cpu, PGO profile hash, git SHA, and the reference `ffmpeg`
version + configure line.

**Repetitions and statistics.** Each scenario runs `max(11, until_cv_below_1pct)` repetitions,
interleaved A/B with the reference. We report **median and median absolute deviation**, plus **min**
(the least-noise-contaminated estimate). Comparisons use the **Mann–Whitney U test** on the paired
samples; a difference is called only when `p < 0.01` **and** the median shift exceeds the noise floor
measured for that scenario (established by running the same binary against itself, typically 0.3–0.8%).

### 4.5 The sample corpus

Four tiers, with licence status governing what may be committed, cached and redistributed.
`vaco-corpus` (architecture §3, Tools) fetches by manifest with SHA-256 verification.

| Tier | Content | Source | Licence | Committed? | Redistributed? |
|---|---|---|---|---|---|
| **T0 — Synthetic** | Deterministic PRNG planes, gradients, edge patterns, sine sweeps, white/pink noise, silence, clipping signals | Generated by `vaco-corpus gen` from a seed | None needed — we author it | Yes (generator, not output) | Yes |
| **T1 — Permissive real media** | Big Buck Bunny, Sintel, Tears of Steel, Cosmos Laundromat, Spring, Sprite Fright (Blender Foundation, CC-BY); Netflix Open Content — El Fuente, Chimera, Meridian, Sol Levante (CC BY 4.0) | Fetched over HTTPS from the publishers | CC-BY / CC BY 4.0 | No (fetched + cached) | Attribution required; we ship the attribution file, not the media |
| **T2 — Conformance bitstreams** | ITU-T H.264/H.265 JCT-VC conformance streams, AOM AV1 Argon coverage streams, Opus/Vorbis/FLAC test vectors | Standards bodies / AOM / Xiph | Free-to-use for conformance; individually recorded per file | No | No — fetch-only |
| **T3 — Restricted benchmark sets** | Ultra Video Group (UVG) 4K sequences, Xiph derf collection items with unclear provenance | Publisher sites | **CC BY-NC or unclear** | No | **No, and flagged NC in the manifest.** Usable for local measurement; results from T3 are published as numbers only, never with the media. |

Manifest entry shape:

```toml
[[sample]]
id       = "netflix/el_fuente_1080p"
url      = "https://…"
sha256   = "…"
bytes    = 812_437_120
licence  = "CC-BY-4.0"
attribution = "Netflix, Inc. — Netflix Open Content"
redistributable = false
tier     = "T1"
uses     = ["S1", "S2", "S5", "S8"]
```

**Rule: every gating benchmark (the ones that can fail CI) uses only T0 and T1.** T2 and T3 feed the
published comparison numbers and the fuzzing/conformance corpora, but a licence problem in T3 can
never break the build. `cargo-deny`'s licence policy (D3) is mirrored by a `corpus-licence-check`
job that fails if a manifest entry lacks an explicit licence field.

We additionally derive **short clips** (5 s, 300 frames) from T1 sources at build time for the
fast-feedback benchmark set, and keep the full-length versions for the nightly run. Encoded variants
(the same T1 source encoded to each codec at each resolution) are generated once by a pinned
reference `ffmpeg`, hashed, and cached — so the *decode* benchmark inputs are byte-identical for
everyone and are not themselves a source of variance.

### 4.6 Harness, storage, and regression detection

**Harness.** `crates/tools/vaco-bench`, invoked as `just bench`, `just bench-macro`,
`just bench-compare <baseline>`. It:

1. Verifies the machine control preconditions (§4.4) and aborts with a diagnostic if any fail.
2. Resolves the corpus manifest, fetching and verifying anything missing.
3. Locates the reference `ffmpeg` — **two** of them: `ffmpeg-distro` (whatever the OS ships, i.e.
   what users compare against in practice) and `ffmpeg-tuned` (a pinned source build with
   `--enable-lto --cpu=native`, i.e. the strongest fair opponent). Both versions are recorded.
   Running the GPL binary as a black box is already sanctioned by D6/D7.
4. Runs each scenario interleaved, collecting wall clock, `perf` counters, RSS, and output checksums.
5. Verifies the output checksum matches the conformance expectation — **a benchmark run that produces
   wrong output is a failure, not a fast result.**
6. Emits one JSON Lines record per (scenario, configuration, implementation, repetition).

**Storage.** Results land in an orphan `bench-results` branch of the repository:
`results/<machine-id>/<yyyy-mm>/<git-sha>.jsonl`. JSONL is ~50–500 KB per full run, so a year of
nightlies is under 200 MB — acceptable in git without LFS. `just bench-report` builds a static HTML
dashboard from the branch (time series per metric, per machine, with the fingerprint diffed between
adjacent points so a regression caused by a kernel bump is visibly attributable).

**Regression detection in CI.**

| Gate | Runs on | Threshold | Action on trip |
|---|---|---|---|
| `checkasm --bench` per-kernel | Nightly, reference runner | median cycles >3% worse than rolling median of last 7 green runs, `p<0.01` | Open an issue, tag the kernel owner, mark the nightly amber |
| `divan` per-kernel | PR, if the PR touches that crate | >5% worse than the merge-base measurement on the same runner | **Block the PR** unless labelled `perf-accepted` with a written justification |
| `vaco-bench` macro | Nightly | >2% worse on any of S1–S8 vs rolling median | Block the next release; bisect job auto-triggers |
| `vecheck` asm assertions | PR | any `require` missing or `forbid` present | **Block the PR** |
| Ratio vs `ffmpeg-tuned` | Nightly | any scenario drops below its §1.3 predicted band | Amber; requires a band revision PR with measurement, not a silent adjustment |

PR-level gating uses divan (fast, in-process, no corpus) rather than the macro suite (slow, needs the
dedicated runner); the macro suite gates releases, not commits. This keeps PR latency under ~10
minutes while still catching the regressions that matter before they ship.

### 4.7 What we do if the §1.5 thesis is refuted

If, after the priority 1–4 work in §8 lands, S1 shows H.264 decode below **0.85x** of `ffmpeg-tuned`:

1. Attribute the deficit with `perf record` down to the kernel, and compare against the per-kernel
   `checkasm --bench` ratios — the macro deficit must be explainable by the micro numbers, and if it
   is not, the problem is architectural (threading, allocation, dispatch overhead), not SIMD.
2. Apply the named mitigation for that path from §1.4.
3. Re-measure. If still below 0.85x and the mitigation is exhausted, **escalate to the user with**:
   the measured deficit, the specific kernel, the specific instruction or shuffle sequence portable
   SIMD cannot express, the estimated recovery, and the minimal `unsafe` surface required (function
   count and line count). The user decides. We do not silently reach for `unsafe`, and we do not
   silently ship a 0.6x decoder while calling the constraint a success.

---

## 5. `vaco-checkasm`

FFmpeg's `checkasm` is GPL and therefore unusable to us (D3), and its shared-lineage upstream core is
likewise off limits. We build our own from the behavioural description in research §08 §1f — what it
does, not how it does it. It is our clean-room equivalent, and it is the mechanism that makes
architecture §7.3's rule ("a kernel without a scalar reference and a differential test is not
merged") enforceable rather than aspirational.

Its two jobs:

- **Verify** every SIMD variant of every kernel against its scalar reference, over randomised and
  edge-case input, on every shape and every parameter combination.
- **Benchmark** every variant in cycles, with a nop-overhead baseline subtracted, so a contributor
  can justify a kernel change with numbers.

Crucially it is *not* a fuzzer (that is `cargo-fuzz`, D6) and *not* a throughput benchmark (that is
divan, §4.2). It is the differential oracle for kernels, and its unit of measurement is the cycle.

### 5.1 Why the C design does not port directly

C `checkasm` relies on varargs, `setjmp`, stack-clobber detection, and register-clobber checks —
all of which exist because C asm can violate the ABI. Safe Rust cannot. What we keep, what we drop,
what we add:

| C checkasm feature | Our disposition |
|---|---|
| Randomised + edge-case input, byte-compare against C reference | **Keep** — the core value |
| Cycle benchmarking with nop baseline | **Keep** |
| `--test` / `--function` filters, `--bench` mode | **Keep**, same CLI spelling for muscle memory |
| Buffer-overwrite detection via guard bytes | **Reframe**: Rust cannot overrun a slice, but a kernel *can* write into stride padding or beyond `width` within a legitimately-sized buffer. We guard exactly that. |
| Register/stack clobber checks | **Drop** — impossible in safe Rust |
| *(none — new under D12)* | **Add**: forced-tier sweep. `--tier` re-runs a kernel at each CPU level in one process and asserts bit-identical output across all of them. This is the check that catches a monomorphisation diverging at one level only — the characteristic failure mode of runtime dispatch, and one the superseded build-time design could not produce. |
| `setjmp` crash recovery | **Drop** — a panic is a test failure; `catch_unwind` reports which variant and which seed |
| — | **Add**: parameter-space sweeps (bit depth, coefficient set, block size) as first-class axes |
| — | **Add**: coverage assertion — every registered `KernelSlot` variant must have a test (§5.6) |
| — | **Add**: cross-*tier* comparison. Under the superseded plan our variants lived in different build artefacts and could only be compared by rebuilding; D12 makes every level reachable in one process, so this becomes a plain `--tier` sweep (§5.1). |

### 5.2 API

The registration API is a builder, typed on a `Kernel` trait that decouples "what inputs does this
kernel take" from "how do we generate and compare them".

```rust
// crates/tools/vaco-checkasm/src/lib.rs
#![forbid(unsafe_code)]

/// A kernel family under test. One impl per kernel signature shape, shared by all
/// kernels with that shape (all 3-plane→packed converters share one impl, etc.).
pub trait Kernel: 'static {
    /// Immutable inputs, owned by the harness.
    type Input;
    /// The mutable output buffer(s). Must be `Clone` so the harness can hand each
    /// variant an identical, independently-poisoned copy.
    type Output: Clone + PartialEq + Debug;
    /// Non-buffer parameters swept independently of the data (coeffs, bit depth, mode).
    type Params: Clone + Debug;

    /// The function-pointer type every variant conforms to.
    type Func: Copy + 'static;

    fn shapes() -> &'static [Shape];
    fn make_input(shape: Shape, pattern: Pattern, rng: &mut Rng) -> Self::Input;
    fn make_output(shape: Shape) -> Self::Output;
    fn call(f: Self::Func, input: &Self::Input, out: &mut Self::Output, p: &Self::Params);

    /// Bytes the kernel is *permitted* to write. Everything else in `Output`'s backing
    /// storage is poisoned with 0xA5 and asserted unchanged. This is our replacement for
    /// C checkasm's guard bytes and it catches the real bug class: writing into stride
    /// padding, or past `width` on the last row.
    fn writable_region(shape: Shape) -> WritableRegion;

    /// How to explain a mismatch to a human: which pixel/sample, expected vs got,
    /// with a small neighbourhood dump.
    fn diff(expected: &Self::Output, got: &Self::Output, shape: Shape) -> Mismatch;
}

/// The builder every kernel module uses (as seen in §2.7).
pub struct Differential<K: Kernel> { /* … */ }

impl<K: Kernel> Differential<K> {
    pub fn new(id: &'static str) -> Self;
    pub fn shapes(self, s: &[Shape]) -> Self;
    pub fn stride_padding(self, pads: &[usize]) -> Self;
    pub fn edge_cases(self, e: &[EdgeCase]) -> Self;
    pub fn params(self, p: &[K::Params]) -> Self;
    pub fn reference(self, f: K::Func) -> Self;
    pub fn variant(self, name: &'static str, f: K::Func) -> Self;
    /// D12: register a level-generic kernel body once, and sweep every CPU level the machine
    /// supports (plus `fallback`), asserting all agree with the reference and each other.
    /// This is the normal case; `variant` remains for competing *algorithms* (§2.5).
    pub fn all_tiers(self, f: impl LevelGeneric<K>) -> Self;
    pub fn guard_stride_padding(self, on: bool) -> Self;
    /// Float kernels only: assert within N ULP instead of bit-exact. Requires a written
    /// justification string that appears in the report; bit-exact is the default and the
    /// preference (see §3.1 R6).
    pub fn ulp_tolerance(self, ulps: u32, why: &'static str) -> Self;

    /// Correctness sweep. Panics with a full reproduction command on mismatch.
    pub fn run(self);
    /// Cycle benchmark. Used by `--bench`; not run by `#[test]`.
    pub fn bench(self, cfg: &BenchConfig) -> Vec<BenchResult>;
}
```

**Input patterns.** `Pattern` is the axis that makes the sweep exhaustive rather than hopeful:

```rust
pub enum EdgeCase {
    Zeros,                     // all-zero planes
    Max,                       // all-max for the element type
    Ones,                      // all 1 — catches sign/shift errors
    AlternatingMinMax,         // 0,255,0,255… — worst case for saturation and for filters
    Checkerboard,              // 2D alternation — catches row/column confusion
    Ramp { step: i32 },        // monotone — catches off-by-one in interpolation
    LimitedRangeBoundaries,    // exactly at 16/235/240 and one either side
    ClipInducing,              // constructed to force output clamping on every channel
    SignBoundary,              // for signed kernels: INT_MIN, INT_MIN+1, -1, 0, 1, INT_MAX
    FloatSpecials,             // ±0.0, ±denormal, ±1e-38, ±1e38, ±inf, NaN (quiet + signalling)
    Impulse { at: (usize, usize) }, // single non-zero sample — exposes filter tap ordering
    Random { seeds: u32 },     // the bulk of the coverage; seed printed on failure
    RandomSparse { density: f32 }, // mostly-zero random — exercises different branch mixes
}
```

`FloatSpecials` matters more than it looks: our resampler and `vaco-tx` are f32, and a NaN
propagation difference between scalar and SIMD is exactly the class of bug that survives to
production. Note it is applied only where the kernel's contract admits those values — a kernel
documented as requiring finite input gets a `#[deny]`-style annotation excluding it.

### 5.3 Cycle measurement without `unsafe`

`rdtsc` is `core::arch::x86_64::_rdtsc` and is `unsafe`. We do better than reach for it:

| Platform | Source | Quality |
|---|---|---|
| Linux (any arch) | `perf_event_open` → `PERF_COUNT_HW_CPU_CYCLES`, thread-scoped, via the `perf-event` crate (MIT/Apache) | **Best available.** Real core cycles, immune to frequency scaling, and gives instructions/branch-misses/cache-misses in the same read group. Strictly better than `rdtsc`, which counts reference cycles. |
| macOS | `std::time::Instant` (`mach_continuous_time`), ~41 ns resolution | Adequate with amortisation (§5.5). Reported as **ns**, plus derived cycles at the recorded nominal frequency, clearly labelled as derived. |
| Windows | `Instant` (QPC) | Same as macOS. |
| Any, fallback | `Instant` | Same. |

`perf-event` contains internal `unsafe` (it is an `ioctl` wrapper). Per §1.4 Risk E's proposed policy,
`crates/tools/` is judged on a looser dependency bar than shipped crates because nothing there enters
a distributed binary. `vaco-checkasm` itself carries `#![forbid(unsafe_code)]`.

**`vaco-checkasm` reports cycles on Linux and nanoseconds everywhere else, and never silently
conflates them.** JSON records carry `unit: "cycles" | "ns"` and the regression gate compares like
with like.

### 5.4 Measurement protocol

Mirrors what research §08 §1f describes checkasm doing, adapted to our measurement sources.

1. **Nop baseline.** Time an empty function with an identical signature and identical argument
   marshalling, `R` times. The median is the per-call overhead and is subtracted from every variant's
   result. Reported alongside, so a suspiciously large baseline (a sign of a mis-set-up harness) is
   visible.
2. **Warm-up.** Run each variant `max(64, 2% of R)` times untimed, to fault in pages, warm caches and
   train the branch predictor. Buffers are re-poisoned after warm-up.
3. **Amortisation.** Each timed sample is `K` back-to-back calls where `K` is auto-chosen so one
   sample takes ≥ 20 µs (making the timer's resolution contribute < 0.2% on macOS). `K` is recorded.
4. **Sampling.** Collect ≥ 30 samples, or until the coefficient of variation of the median falls
   below 1%, whichever is later, capped at a cycle budget so a slow kernel cannot hang CI.
5. **Statistics.** Report **median**, **MAD**, **min**, and **p95**. `min` is the headline for
   kernel work (it is the least noise-contaminated estimate of the kernel's true cost); `median` is
   what the regression gate uses.
6. **Ratios.** Every variant is reported as a ratio against (a) the scalar reference on the same
   machine, and (b) the stored baseline for the same variant.
7. **Cache-state axis.** Each kernel is measured twice: **hot** (same buffers reused, everything in
   L1/L2 — measures the kernel's arithmetic) and **cold** (buffers cycled through a working set
   larger than L3 — measures what actually happens in a real decode). Both are reported. Hot-only
   numbers are how SIMD work gets over-claimed, and we refuse to publish only those.
8. **Stride-guard verification** runs in correctness mode only; benchmark mode disables poisoning so
   the poison write does not pollute the cache measurement.

### 5.5 CLI

```
vaco-checkasm [OPTIONS]

  -t, --test <GLOB>        Restrict to kernel ids matching a glob   [default: *]
                           e.g. --test 'scale/*'  --test 'codec/h264/mc/*'
  -f, --function <GLOB>    Restrict to variant names                [default: *]
      --list               List registered kernels and variants, then exit
      --tier <TIER>        fallback|sse4.2|avx2|avx512|neon — force one level
                           (D12: replaces `--isa-level scalar|v128|v256|v512`, which capped a
                           build-time width. This forces a runtime level, and unlike the old
                           flag it can select a level ABOVE the compiled baseline.)

  -b, --bench              Benchmark instead of verify
      --bench-cache <M>    hot|cold|both                            [default: both]
      --min-samples <N>                                             [default: 30]
      --budget <MS>        Per-variant wall-clock budget            [default: 250]

      --seed <HEX>         PRNG seed; a fresh random one is used and PRINTED if absent
      --repeat <N>         Random-pattern repetitions per shape     [default: 64]
      --exhaustive         Every shape x every pad x every edge case x every param
                           (slow: the nightly job's mode)
      --shapes <SPEC>      Override shape list, e.g. '16x16,1920x1080'

      --json <PATH>        Write JSONL results
      --baseline <PATH>    Compare against a previous --json run
      --fail-under <R>     Fail if any variant's median/baseline ratio < R  [default: off]
      --fail-slower-than-reference
                           Fail if any SIMD variant is slower than scalar (a real bug class:
                           a narrow kernel where the shuffle overhead exceeds the arithmetic win)

      --machine-check      Verify the §4.4 environment preconditions; exit 2 if unmet
```

Behavioural details that matter:

- **Default mode is verify, and it is deterministic** when `--seed` is given. When it is not, the
  chosen seed is printed on the first line so a CI failure is always reproducible with one
  copy-pasteable command. On mismatch the harness prints exactly that command.
- **A mismatch report** names: kernel id, variant, shape, stride padding, params, seed, pattern, the
  first differing coordinate, and a 5x5 (or 8-sample) neighbourhood of expected vs got. This is the
  difference between a 10-minute fix and a 2-hour one.
- `--list` output is machine-readable (`--list --json`) and is what §5.6's coverage check consumes.

### 5.6 Coverage enforcement

The architecture §7.3 rule is enforced by a test, not by review:

```rust
// crates/tools/vaco-checkasm/tests/coverage.rs
#[test]
fn every_registered_kernel_variant_has_a_differential_test() {
    let declared = vaco_simd::registry::all_slots();       // from the generated registry module
    let tested   = vaco_checkasm::Registry::built();       // from inventory.toml registration
    let missing: Vec<_> = declared
        .flat_map(|s| s.variants.iter().map(move |v| (s.id, v.name)))
        .filter(|k| !tested.contains(k))
        .collect();
    assert!(missing.is_empty(), "kernel variants without a differential test: {missing:#?}");
}
```

A second test asserts the inverse (no test registered for a kernel that no longer exists), so
deleting a kernel forces deleting its test.

### 5.7 CI integration

| Job | Trigger | Command | Gating |
|---|---|---|---|
| `checkasm-verify` | Every PR, every platform in the matrix | `vaco-checkasm --seed 0` then `vaco-checkasm` (random seed, printed) | **Required.** Two runs: one reproducible, one exploratory. |
| `checkasm-coverage` | Every PR | `cargo test -p vaco-checkasm --test coverage` | **Required.** |
| `checkasm-exhaustive` | Nightly | `vaco-checkasm --exhaustive --repeat 4096` | Failure opens an issue and blocks the next release |
| `checkasm-bench` | Nightly, reference runner only | `vaco-checkasm -b --bench-cache both --json out.jsonl --baseline prev.jsonl` | Amber on >3% regression; also `--fail-slower-than-reference` is **hard-failing** |
| `checkasm-tier-matrix` | Nightly | **One process, every tier**, forced via `VACO_TIER` (`sse4.2`/`avx2`/`avx512` on x86; `neon` on aarch64; plus `fallback` via the substrate's `force_support_fallback` feature) | *(Renamed from `checkasm-isa-matrix` by D12.)* Verifies every level's monomorphisation is bit-identical to the scalar reference and to every other level, and produces the per-microarchitecture preference data `KernelSlot::select` uses. **Strictly better than the superseded per-artefact job**: it compares levels on one machine in one session, so a cross-level divergence is attributable rather than confounded with a rebuild. |
| `checkasm-cross` | Weekly | Under `qemu-user` for aarch64/riscv64 from an x86 host | Correctness only (timing is meaningless under emulation); catches endianness and lane-order mistakes early on architectures we do not have hardware for |

Total nightly cost estimate: ~150 kernels x ~4 variants x (verify exhaustive + bench hot/cold) is
roughly 40–70 minutes on the reference runner — acceptable for a nightly, which is why the PR job
runs the fast sweep instead.

---

## 6. PGO, LTO and BOLT

Research §08 §7 establishes the opportunity precisely: FFmpeg's `configure` has an opt-in
`--enable-lto`, no PGO workflow of any kind, and no BOLT references anywhere. Distro builds ship
without LTO. This is not a small gap — it is the compensating advantage that offsets the assembly
we cannot write, and it lands disproportionately on the branchy scalar code (CABAC, bitstream
parsing, container demuxing, mode decision) where SIMD is irrelevant and where §1.3 path #15 lives.

### 6.1 The release pipeline, in order

```
1. cargo build --release                     lto="fat", codegen-units=1, panic="abort"
                                             -Cprofile-generate=<dir>
2. vaco-profile run <workload-manifest>      exercise the instrumented binary (§6.2)
3. llvm-profdata merge -o vaco.profdata <dir>/*.profraw
4. cargo build --release                     -Cprofile-use=vaco.profdata
                                             -Cllvm-args=-pgo-warn-missing-function
5. llvm-bolt vaco -o vaco.bolt               Linux ELF only (§6.5)
   ├─ perf record -e cycles:u -j any,u       collect LBR traces on the same workload
   └─ perf2bolt                              convert to BOLT's profile format
6. verify                                    full conformance + differential + checkasm suite
7. measure                                   the nine scenarios, vs the non-PGO build
```

`just pgo-build` runs 1–4. `just bolt-build` adds 5. `just release` runs all seven and refuses to
produce an artifact if step 6 fails or step 7 shows a regression against the previous release.

Fat LTO plus `codegen-units=1` is the baseline (architecture §8) and is *not* optional: PGO's
cross-module inlining decisions are worth much less without it, and our ~150-crate workspace has an
unusually high proportion of cross-crate hot calls (every kernel call crosses a crate boundary).

### 6.2 The profile workload

The profile must be **representative without being overfit**. An overfit profile is worse than none:
it will lay out the binary for the exact codecs and resolutions in the training set and pessimise
everything else.

`vaco-profile`'s workload manifest, `profile/workload.toml`, is deliberately *broader and shallower*
than the benchmark corpus:

| Group | Content | Why |
|---|---|---|
| **Decode** | Every default-feature codec, ≥2 resolutions each, ≥300 frames, both 8-bit and 10-bit where the codec supports it | The bulk of the weight. Entropy decode is the payoff target. |
| **Demux/probe** | Every default container, including malformed and truncated files from the fuzz corpus | Probing and error paths are branchy and are what `vaco-probe`'s startup latency depends on |
| **Transcode** | 6 representative pipelines: remux, decode→scale→encode, decode→filter chain→encode, audio-only, multi-output, complex filtergraph | Exercises the scheduler, the graph builder, and cross-component paths |
| **Scale/convert** | The S3 conversion matrix, one frame each | Keeps the ops-graph dispatch paths warm in the profile without dominating it |
| **Resample** | The S4 matrix, 1 s each | Same |
| **Encode** | Every default-feature encoder, ≥60 frames, ≥2 rate-control modes | Mode-decision code is extremely branchy and benefits most |
| **Seek** | 50 seeks across 5 container/codec pairs | Seek paths are cold-code-heavy; PGO's value here is mostly in *not* inlining them |
| **CLI** | `-version`, `-formats`, `-codecs`, `-filters`, `-h filter=…`, argument-parse errors | Startup latency (S7) is dominated by this code |

Constraints on the manifest:

- **Uses only T0 and T1 corpus tiers** (§4.5) so any contributor can regenerate a profile locally.
- **Total instrumented runtime ≤ 25 minutes** on the reference runner, so the nightly refresh fits.
- **Every default-feature component appears at least once.** A `profile-coverage` check compares
  `llvm-profdata show --all-functions` against the registry's component list and **fails the release
  build** if any registered decoder/demuxer/filter has zero profile counts — because that component
  would otherwise get the "cold" layout treatment and could regress badly.
- **No component may exceed 15% of total profile weight.** Enforced by the same check. This is the
  anti-overfit guard.

### 6.3 Profile storage and refresh

Merged `.profdata` for a workspace this size runs 20–80 MB — too large for the main repository's
history.

- Profiles are published as **release-channel artifacts** (GitHub release assets / an OCI artifact),
  named `vaco-<target>-<profile-manifest-hash>.profdata`, with a SHA-256 recorded in
  `profile/lockfile.toml` in the main repo.
- `just pgo-build` fetches the pinned profile by hash; no network access means the build falls back
  to non-PGO with a loud warning rather than failing.
- **Refresh cadence:** a nightly job regenerates the profile from the current `main` and uploads it.
  The pin in `profile/lockfile.toml` is advanced by an automated PR **weekly**, and that PR must show
  the benchmark comparison between old and new profile. This keeps the profile fresh without making
  every commit's build non-reproducible.
- **Staleness detection:** `-Cllvm-args=-pgo-warn-missing-function` emits a warning per function
  present in the source but absent from the profile. CI counts them; if missing-function warnings
  exceed **5% of functions in the hot list** (the top-200 symbols by nightly `perf` samples), the
  build is marked stale and the refresh PR is escalated to blocking. A profile is also hard-rejected
  if `llvm-profdata show` reports a different hash for a function that PGO would otherwise apply
  stale counts to — LLVM already detects this; we surface it as an error rather than a warning.
- **Reproducibility:** because the profile is content-addressed and pinned, a given git SHA plus a
  given lockfile produces a bit-identical binary. That property is worth protecting; a floating
  "latest profile" would destroy it.

### 6.4 Validating the PGO build

PGO cannot change program semantics, but it changes inlining, layout and register allocation
aggressively enough that it *exposes* latent bugs (particularly around stack depth and around code
that accidentally depended on an inlining boundary). So:

1. **The full test suite runs against the PGO binary**, not just the plain release binary: unit tests,
   the D6 differential conformance harness, `vaco-checkasm --exhaustive`, and the S10 determinism
   check. This is a separate CI job (`release-verify-pgo`).
2. **Bit-exactness is asserted**: every conformance output from the PGO build must be byte-identical
   to the non-PGO build. Any difference is a bug, full stop — PGO does not enable FP reassociation and
   must not change a single output byte.
3. **A fuzzing smoke run** (30 minutes per target) against the PGO binary, because stack-depth
   changes are the one class of behaviour difference PGO can legitimately produce.
4. **Benchmarked**: the nine scenarios, PGO vs non-PGO, recorded in the results store. If PGO shows a
   *regression* on any scenario, the profile is suspect (usually overfit) and the release blocks.

### 6.5 BOLT: applicability and limits

BOLT (LLVM's binary optimizer) rewrites the final binary for better code layout using LBR samples. It
composes with PGO rather than replacing it: PGO informs inlining and branch weights, BOLT informs
basic-block and function placement, i-cache and iTLB behaviour.

| Target | BOLT status | Our disposition |
|---|---|---|
| `x86_64-unknown-linux-gnu` | Mature | **Ship BOLTed.** Default for Linux x86-64 releases. |
| `aarch64-unknown-linux-gnu` | Functional since LLVM 16, less battle-tested | **Evaluate per release.** Ship BOLTed only if the release-verify job is green and the measured gain exceeds 1%. |
| `*-apple-darwin` (Mach-O) | Not supported | Not applicable. macOS ships PGO+LTO only. |
| `*-pc-windows-msvc` (PE/COFF) | Not supported | Not applicable. Windows ships PGO+LTO only. |
| `riscv64`, others | Not supported / immature | Not applicable. |

Requirements we must satisfy to BOLT at all: build with `-C link-arg=-Wl,--emit-relocs`, keep the
symbol table (no full strip before BOLT), and ensure `panic="abort"` does not confuse exception-table
reconstruction (it does not; there are no landing pads to reconstruct).

BOLT is applied **after** step 6's verification of the PGO binary, and the BOLTed binary is then
re-verified independently — binary rewriting is the one step in the pipeline that can, in principle,
produce a broken artifact, so it never ships unverified.

### 6.6 Expected gains, with sources

We state the numbers we expect and where the expectation comes from, so that a measured shortfall is
a falsified prediction rather than a moved goalpost.

| Stage | Expected gain | Basis |
|---|---|---|
| **Fat LTO + codegen-units=1** vs thin LTO / CGU=16 | **2–6%** overall | Standard Rust release-profile deltas; larger here than typical because our hot calls cross crate boundaries constantly (every kernel dispatch, every trait seam). Our own S1 measurement replaces this estimate immediately. |
| **PGO**, decode-heavy workloads | **3–8%** end-to-end | The Rust project's own PGO documentation and `cargo-pgo` report ~5–15% on typical application workloads; media decode is *less* branchy than a compiler or database, so we take the bottom half of that range. |
| **PGO**, entropy-decode-dominated portion (CABAC, CAVLC, bitstream parse) | **8–15%** on that portion | The branch-density argument: this is code with a high proportion of poorly-predicted branches and indirect calls, which is exactly PGO's best case. Compiler self-build PGO results (Clang building itself, ~10–20%) are the closest published analogue for branch-dense code. |
| **PGO**, DSP-dominated workloads (scale, resample, filters) | **0–2%** | Straight-line vectorized loops have almost nothing for PGO to improve. We should expect near-zero here and not be disappointed. |
| **BOLT** on top of PGO+LTO, x86-64 Linux | **1–4%** | The BOLT paper (Panchenko et al., *BOLT: A Practical Binary Optimizer for Data Centers and Beyond*, CGO 2019) reports up to 8.0% on top of PGO+LTO for large server binaries (HHVM, Clang). Those are much larger binaries with much worse i-cache behaviour than a media tool; we take a quarter to a half of their headline. |
| **Combined vs a distro `ffmpeg`** (no LTO, no PGO, no BOLT) | **5–12%** of the total pipeline, concentrated in non-SIMD code | Sum of the above, weighted by the §1.5 time splits. |

Applied to §1.5's H.264 decode arithmetic: the pre-PGO estimate of 0.98x becomes **1.02–1.06x**, and
the entropy-decode band in §1.3 (path #15, 0.95–1.30x) is where most of that comes from.

**How we avoid fooling ourselves.** The comparison must be like-for-like on the *build*, not just the
machine. `vaco-bench` reports three ratios for every scenario:

- `vaco(PGO+BOLT+LTO)` vs `ffmpeg-distro` — what a user actually experiences.
- `vaco(PGO+BOLT+LTO)` vs `ffmpeg-tuned` (`--enable-lto --cpu=native`) — the fair fight.
- `vaco(plain release)` vs `ffmpeg-tuned` — **the honest measure of the SIMD story alone**, with the
  compiler-optimization advantage removed.

The third number is the one that tells us whether portable SIMD is working. We publish it. A plan
that only reported the first would be marketing.

---

## 7. Threading performance

Architecture §6 defines three orthogonal axes. This section says how each is tuned from data and how
we detect the pathologies.

### 7.1 Axis 1 — Pipeline parallelism (`vaco-sched`)

Every component (demux, decode, filter, encode, mux) is a task connected by bounded channels;
backpressure is the channel.

**What we measure.** Every channel is instrumented with a cheap, always-on counter set (relaxed
atomics, `CachePadded`): `send_blocked_ns`, `recv_blocked_ns`, `occupancy_histogram` (16 buckets).
`vaco -benchmark_all` (our `-benchmark` equivalent) dumps them at exit.

**What the numbers mean, and the tuning rule.** A pipeline's throughput equals its slowest stage.
The instrumentation makes the bottleneck unambiguous:

| Observation | Diagnosis | Action |
|---|---|---|
| Stage N's `send_blocked_ns` is high, occupancy pinned at max | Downstream is the bottleneck | Give the downstream stage more of axis 2/3 parallelism |
| Stage N's `recv_blocked_ns` is high, occupancy pinned at 0 | Upstream is the bottleneck | Same, upstream |
| Occupancy oscillates 0↔max with high blocked time on both ends | Queue too shallow; stages are lock-stepping | Increase depth |
| Occupancy pinned at max everywhere, RSS high | Queues too deep; we are buffering, not pipelining | Decrease depth |

**Queue depth is chosen from data, not guessed.** Default depth is derived from the frame size and a
memory budget rather than being a constant: `depth = clamp(target_bytes / frame_bytes, 2, 64)` with
`target_bytes` defaulting to 64 MiB per link and settable by `-thread_queue_size`-equivalent. A
nightly job sweeps depth ∈ {1,2,4,8,16,32,64} across the S1/S5 matrices and publishes the throughput
and RSS surfaces; the defaults table in `vaco-sched` is regenerated from that sweep, reviewed, and
committed. Same mechanism as §7.2's thread-count table.

### 7.2 Axis 2 — Frame parallelism (decoders), and killing the 16-thread constant

Architecture §6 states we do not inherit FFmpeg's `MAX_AUTO_THREADS = 16`. Here is what replaces it.

**The measurement.** A nightly job runs S1's matrix at every thread count from 1 to `ncpu`, recording
fps, cycles, peak RSS, and decode latency (time from packet-in to frame-out). For each
(codec, resolution) it computes:

- the **knee**: the smallest `t` where `fps(t+1) / fps(t) < 1.05` (i.e. the next thread buys <5%);
- the **peak**: `argmax fps(t)`;
- the **efficiency point**: the largest `t` where `fps(t) / (t · fps(1)) > 0.6`;
- the **RSS slope**: MiB per additional frame thread.

**The rule.** `default_threads(codec, pixels, mode)` returns:

```
mode = Throughput  →  min(ncpu, knee)                       # batch transcoding
mode = Latency     →  min(ncpu, efficiency_point, 4)        # playback, low-delay
mode = Memory      →  min(ncpu, knee, memory_budget / rss_slope)
```

with the per-(codec, resolution-bucket) constants baked into a generated table
(`crates/codec/vaco-codec-core/src/thread_defaults.rs`) produced by the nightly sweep and committed
after review. Resolution buckets: `<0.2 MP`, `0.2–1 MP`, `1–2.5 MP`, `2.5–9 MP`, `>9 MP`.

This is expected to produce *different* answers from 16 in both directions — likely below 16 for
low-resolution content (where reference-frame dependencies bound parallelism regardless of core
count) and above 16 for 4K/8K on high-core-count servers, which is exactly the case FFmpeg's constant
serves badly. We will know rather than assume.

**The progress primitive.** Cross-frame dependency signalling (architecture §6) is a per-frame
`AtomicU32` row counter plus a `Notify`/condvar. The performance risks are (a) waking too often and
(b) the counter sharing a cache line with something written by another thread. Both are addressed in
§7.4. Row-progress updates are batched to once per `max(1, height/64)` rows to bound wake frequency,
and the batch size is itself a swept parameter.

### 7.3 Axis 3 — Data parallelism (filters, slices, scaling)

Safe by construction via `split_at_mut` / `chunks_mut` over disjoint plane bands.

**The tuning question is slice granularity**, and it has a single governing rule: a slice must be
large enough to amortise the task-dispatch overhead and small enough that all workers finish
together. We set:

```
slice_rows = clamp(height / (threads * OVERSUBSCRIBE), MIN_ROWS, height)
```

with `OVERSUBSCRIBE = 4` (four slices per worker, so a straggler costs 1/4 of a worker's time rather
than a whole worker's) and `MIN_ROWS` chosen so each slice does at least ~256 KiB of work — measured
per filter, since a `format` filter and a `gblur` filter differ by 50x in work per row.

**The measurement**: a nightly sweep of `OVERSUBSCRIBE ∈ {1,2,4,8}` and `MIN_ROWS ∈ {4,8,16,32,64}`
over the S5 filter matrix, producing the same kind of committed defaults table as §7.2. Filters with
a vertical dependency (bwdif, deinterlace, vertical blur) need overlap rows; their effective
`MIN_ROWS` floor is `taps` and the sweep respects it.

**Interaction with §7.1 and §7.2.** The three axes multiply, and oversubscribing the machine is the
most common way a media tool loses to a simpler one. `vaco-sched` owns a single global `rayon`-style
pool sized to `ncpu` and hands axis-2 and axis-3 work into it; axes do not each spawn their own pool.
A nightly `oversubscription` check asserts that total runnable threads at steady state stays within
`ncpu ± 25%` for each of the S1/S5 configurations.

### 7.4 Detecting false sharing and contention

**False sharing.** The pathology to fear is two hot atomics on one cache line — the frame-progress
counters of adjacent frames, the per-channel queue counters, or the per-worker statistics.

- **Structurally prevented**: `vaco-core` provides `CachePadded<T>` (`#[repr(align(128))]` — 128 not
  64, because Apple Silicon and some x86 prefetchers operate on 128-byte pairs). Every atomic that is
  written by more than one thread is wrapped. A clippy lint (`vaco::unpadded_shared_atomic`,
  custom via `dylint`) flags `AtomicU*` fields in `Sync` structs that are not `CachePadded` and are
  not explicitly annotated `#[vaco::single_writer]`.
- **Detected empirically**: a nightly `perf c2c record` run over the S1 (16-thread) and S5
  configurations. `perf c2c report` gives HITM (cache-line transfer) counts attributed to source
  line. The job fails amber if remote-HITM exceeds **0.5% of total loads**, and the report names the
  offending line, which is normally enough to fix it in minutes.
- **Cross-checked**: `perf stat -e machine_clears.memory_ordering` (x86) — a rising count is the
  other signature of a sharing problem.

**Lock contention.** Our design has very few locks (channels + atomics + `Arc`), which is deliberate,
but the remaining ones must be visible:

- All `Mutex`/`RwLock` acquisitions on a hot path go through a `vaco-core` wrapper that records wait
  time into a `tracing` span when the `perf-trace` feature is on.
- `perf lock record` nightly on the same configurations.
- A structural rule: **no lock may be held across a call into a codec, filter, or kernel.** Enforced
  by review plus a `dylint` check on the wrapper's guard lifetime.

**`Arc` refcount contention** is the subtle one, because `vaco-frame`'s zero-copy model
(architecture §7.4) means a hot frame's `Arc` is cloned and dropped by every stage. Atomic
increment/decrement on a shared line is exactly the false-sharing pattern, and it does not show up as
a lock. Mitigations: pass `&Frame` where a stage does not retain it; batch `Arc` clones at stage
boundaries rather than per-plane; and measure — the `perf c2c` job will surface an `Arc` counter as a
top HITM line immediately if it becomes one.

**Allocation contention.** Steady-state decode must not allocate (`vaco-pool`, architecture §7.4).
Verified rather than assumed: a `no-steady-state-alloc` test wraps the global allocator with a
counting shim (safe Rust), runs a 300-frame decode, and asserts zero allocations after frame 30.
This is a correctness-shaped test for a performance property, which is the right way to keep it true.

### 7.5 Reporting

`vaco -benchmark_all` prints, at exit: per-stage CPU time and wall time, per-link queue statistics,
per-axis thread counts actually used, peak RSS, allocation count, and — when built with
`--features perf-trace` — a Chrome-trace JSON of the whole pipeline. The last is the tool that makes
a "why is this only 3x on 16 cores" question answerable in one look rather than one afternoon.

---

## 8. Prioritised work order

Effort is in **engineer-weeks (ew)** for one competent Rust engineer who has read §2 and §3, including
the scalar reference, the SIMD variants, the checkasm registration, the divan benchmark, and the
`docs/` entry. It does *not* include designing the surrounding component (the decoder, the filter),
only the kernels.

Ordering is by **(expected speedup contribution) / (effort)**, adjusted so that infrastructure lands
before the work that depends on it and so that the parallelisable tracks are unblocked early.

### Phase 0 — Infrastructure (blocking; must land first)

| # | Item | ew | Depends on | Parallelisable? |
|---:|---|---:|---|---|
| 0.1 | `vaco-simd` adapter (D12): `Tier`, `Variant`, `KernelSlot`, `CpuProfile`, `dispatch_kernel!`, and the `ops` module — **including all nine §5.6 gap compositions**, each with an exhaustive test and an instruction-count assertion. `bytemuck` is no longer needed (`bitcast` is native). | 2.5 | — | No — everything blocks on it |
| **0.0** | **`fearless_simd` adoption checklist (§11).** Micro-benchmark the gap compositions and the dispatch overhead against the §1.3 bands *before* 0.1's API is frozen. | **0.5** | — | **No — this gates 0.1** |
| 0.2 | `vaco-checkasm` core: `Kernel` trait, `Differential` builder, edge-case generators, mismatch reporting, CLI verify mode | 3 | 0.1 | No |
| 0.3 | `vaco-checkasm` bench mode: `perf-event` backend, `Instant` fallback, nop baseline, hot/cold protocol, JSONL + baseline compare | 2 | 0.2 | Yes, alongside 0.4 |
| 0.4 | `vaco-vecheck`: remark parsing, `#[vaco::must_vectorize]`, `vecheck.toml`, `cargo-show-asm` assertions, waiver expiry | 2 | 0.1 | Yes, alongside 0.3 |
| 0.5 | `vaco-bench` macro harness + `vaco-corpus` manifest/fetch/verify + machine-control preconditions | 3 | — | Yes, fully independent |
| 0.6 | Results store: JSONL schema, `bench-results` branch tooling, HTML report, CI regression gates | 2 | 0.3, 0.5 | Yes |
| ~~0.7~~ | ~~Multi-artifact ISA build + launcher, `VACO_ISA_LEVEL`, packaging~~ **DELETED by D12** — Risk A is retired, dispatch is runtime. Replaced by a much smaller item: `VACO_TIER` override + a binary-size budget check in CI (§1.4 Risk A cost 1). | 0.5 *(was 2)* | 0.1 | Yes |
| 0.8 | PGO pipeline: `just pgo-build`, `vaco-profile`, workload manifest, coverage/anti-overfit checks, profile lockfile + refresh job | 3 | 0.5 | Yes |
| 0.9 | BOLT pipeline (Linux x86-64), `--emit-relocs` plumbing, re-verification job | 1 | 0.8 | Yes |
| | **Phase 0 total** | **19 ew** *(was 20: −1.5 from deleting the multi-artifact work, +0.5 for the gap compositions, +0.5 for the adoption checklist)* | | ~4 tracks → **~6 calendar weeks** with 4 engineers |

Phase 0 is not overhead. Without 0.2 and 0.4 every subsequent kernel is unverifiable and every
performance claim in this document is unfalsifiable. Do not start Phase 1 before 0.1–0.4 are green.

**And do not start 0.1 before 0.0 is green.** The adoption checklist is half a week and it is the only
point at which switching substrates is still cheap. See §11.

### Phase 1 — Highest reward per unit effort

Matches architecture §7.2's priority order. These are the paths where §1.3 predicts we are at or
above parity, so they convert directly into a defensible headline number.

| # | Area | Kernels | ew | Band (§1.3) | Track |
|---:|---|---|---:|---|---|
| 1.1 | **Colour / pixel-format conversion** — the ops-graph primitives: `READ`/`WRITE`, `SWIZZLE`, `UNPACK`/`PACK`, `SHIFT`, `CONVERT`, `MIN`/`MAX`, `SCALE`, `LINEAR`, `DITHER` | ~35 | 6 | 1.00–1.25x | A |
| 1.2 | **Packed↔planar and subsampling converters** — yuv420/422/444 ↔ nv12/nv21, rgb24/rgba/bgra, 10/12/16-bit variants, endianness | ~25 | 4 | 1.00–1.25x | A |
| 1.3 | **swscale horizontal/vertical filters** — bilinear, bicubic, lanczos, spline, area, point; 8/10/12/16-bit; precomputed coefficient layout | ~14 | 5 | 0.90–1.10x | A |
| 1.4 | **Audio sample-format conversion + rematrix** — the full `AVSampleFormat` matrix, interleave/deinterleave, mix matrix apply | ~30 (mostly generated) | 3 | 1.05–1.35x | B |
| 1.5 | **Audio polyphase resampler** — FIR apply, phase advance, linear interp between phases, all three filter types | ~8 | 3 | 0.95–1.15x | B |
| 1.6 | **Dither + noise shaping** | ~8 | 1.5 | 1.0x | B |
| | **Phase 1 total** | | **22.5 ew** | | 2 tracks → **~6 calendar weeks** with 4 engineers |

Phase 1 alone delivers the transcode-shaped **1.05–1.20x** prediction from §1.5, and it does so before
any codec DSP exists. This is the right first milestone to publish numbers from.

### Phase 2 — Filters (where we expect to exceed upstream)

Research §08 §2d: upstream's filter SIMD is comparatively small and largely pre-AVX-512. This is our
best opportunity to be visibly, unambiguously faster.

| # | Area | ew | Band | Track |
|---:|---|---:|---|---|
| 2.1 | Deinterlace: `bwdif`, `yadif`, `w3fdif`, `estdif` | 4 | 1.10–1.60x | C |
| 2.2 | Blur/sharpen: `gblur`, `boxblur`, `unsharp`, `smartblur` | 3 | 1.10–1.60x | C |
| 2.3 | Denoise: `hqdn3d`, `atadenoise`, `removegrain`, `nlmeans` | 5 | 1.10–1.60x | C |
| 2.4 | Compositing/blend: `overlay`, `blend`, `alphamerge`, `colorkey` | 3 | 1.0–1.3x | C |
| 2.5 | Analysis: `ssim`, `psnr`, `colordetect`, `signalstats` | 3 | 1.0–1.3x | C |
| 2.6 | `lut3d` / colour management / tonemap (SoA lattice, tetrahedral) | 4 | 1.00–1.40x | C |
| 2.7 | Audio filters: `atempo`, `loudnorm`, `firequalizer`, `aresample` glue | 3 | 1.0–1.2x | D |
| | **Phase 2 total** | **25 ew** | | 2 tracks, fully independent of Phase 3 |

### Phase 3 — Codec DSP (the hard, high-volume work)

| # | Area | Kernels | ew | Band | Risk | Track |
|---:|---|---|---:|---|---|---|
| 3.1 | **Shared `vaco-codec-dsp-*` families**: `idct`, `hpel`, `videodsp` (edge emulation), `blockdsp`, `fmtconvert` | ~40 | 6 | 0.90–1.05x | Low | E |
| 3.2 | **H.264 MC** (qpel luma, chroma, weighted pred, batched dispatch per §1.4 Risk C) | ~60 | 8 | 0.75–0.95x *(revised down by D12)* | **High** | E |
| 3.3 | **H.264 deblock + intra pred** | ~35 | 6 | 0.80–1.15x | Medium | F |
| 3.4 | **`vaco-tx`**: FFT/MDCT/RDFT/DCT, split-complex SoA, Stockham, pre-permuted twiddles (§1.4 Risk B) | ~20 plans | 8 | 0.70–0.95x | **Highest** | G |
| 3.5 | **HEVC/VVC epel/qpel + SAO + deblock + ALF** | ~90 | 12 | 0.85–1.05x | High | F |
| 3.6 | **VP9/AV1 inverse transforms** incl. 10/12-bit | ~50 | 10 | 0.75–0.95x | **High** | H |
| 3.7 | **VP8/VP9 MC, intra pred, loop filter** | ~55 | 8 | 0.85–1.05x | Medium | H |
| 3.8 | **AV1 reconstruction** (MC, CDEF, loop restoration, film grain) | ~70 | 12 | 0.70–0.90x *of dav1d* | High | H |
| 3.9 | **`me_cmp`**: SAD, SATD, SSD, variance — encoder-side and error concealment | ~30 | 4 | 0.80–1.10x | Medium | I |
| 3.10 | **Encoder DSP**: forward transforms, quantisation, RDO helpers, `lpc` | ~35 | 6 | 0.85–1.05x | Medium | I |
| 3.11 | **AAC SBR/PS QMF**, `sinewin`, `ac3dsp` | ~25 | 5 | 0.85–1.05x | Medium | G |
| | **Phase 3 total** | | **85 ew** | | 5 tracks (E–I) → **~20 calendar weeks** with 5 engineers |

### Phase 4 — Scalar performance (the part SIMD cannot reach)

Runs **in parallel with Phase 3**, on a separate track, because it is a different skill and touches
different code.

| # | Item | ew | Expected gain |
|---:|---|---:|---|
| 4.1 | Bit reader design: 64-bit refill, branchless renormalisation, checked-body/unchecked-tail split (architecture §7.4), `#[inline(always)]` discipline | 3 | Foundational — everything below depends on it |
| 4.2 | CABAC engine: state table layout (cache-line-aware), renormalisation without unpredictable branches, bypass-decode batching (decode `k` bypass bins at once arithmetically) | 4 | 5–15% of H.264/HEVC decode |
| 4.3 | CAVLC / Exp-Golomb table layout and lookup-width tuning | 2 | 2–5% of H.264 decode |
| 4.4 | PGO profile tuning specifically for entropy paths; BOLT layout verification | 2 | Realises §6.6's 8–15% band |
| 4.5 | Container demux hot paths: MPEG-TS packet loop, Matroska EBML parse, MP4 sample-table walk | 3 | Dominates S6/S7 |
| 4.6 | Startup latency: lazy registry construction, avoiding eager table generation, and **binary-size / page-in cost from multiversioning** *(D12: replaces "launcher cost", which no longer exists)* | 2 | S7 |
| | **Phase 4 total** | **16 ew** | one track → ~16 calendar weeks for 1 engineer, or 8 for 2 |

### Summary and parallelisation

| Phase | ew | Tracks | Notes |
|---|---:|---:|---|
| 0 — Infrastructure | 20 | 4 | **Blocking.** Nothing else starts until 0.1–0.4 land. |
| 1 — Convert/scale/resample | 22.5 | 2 | Delivers the first publishable number |
| 2 — Filters | 25 | 2 | Independent of Phase 3; best "we are faster" story |
| 3 — Codec DSP | 85 | 5 | The bulk. Tracks E–I are genuinely independent. |
| 4 — Scalar/entropy | 16 | 1 | Parallel with Phase 3 |
| **Total** | **168.5 ew** | up to 8 concurrent | ≈ **8–10 calendar months with 6–8 engineers**, or ≈ 3.5 years solo |

**Track independence** (this is the parallelisation plan): after Phase 0, tracks A/B (convert,
audio), C/D (filters), E/F (H.264, HEVC), G (transforms), H (VP9/AV1), I (encoder DSP) and the
Phase 4 scalar track share only `vaco-simd` and `vaco-checkasm`. A contributor picks a track, copies
§2's template, and never blocks on another track. That is the whole reason §2 exists as a template
rather than as prose.

**Ordering advice within a track:** always do the *scalar reference plus checkasm registration for
the whole family first*, then vectorize one kernel at a time. This means the codec is correct and
shippable (if slow) early, and every subsequent SIMD commit is a pure, independently-revertable
performance change with a number attached.

**The three items to start early despite their position**, because they are the long poles and carry
the most uncertainty:

1. **3.4 `vaco-tx`** — highest risk band, and audio decode is blocked on it functionally as well as
   for performance. Start a spike in Phase 1 to de-risk §1.4 Risk B before committing to the plan.
2. **3.2 H.264 MC batching** — the batched-dispatch design (§1.4 Risk C) changes the
   `Decoder`↔`KernelSet` contract, so it must be settled before `vaco-codec-core`'s DSP traits
   freeze.
3. ~~**0.7 multi-artifact builds**~~ — **deleted by D12.** Replaced as a sequencing concern by
   **0.0, the adoption checklist**: it affects the `vaco-simd` API, every kernel written after it, and
   the §1.3 bands. It is half a week and it must come first.
   Late discovery here invalidates earlier measurements.

---

## 9. Open items requiring a decision

These are not hedges; they are choices this plan cannot make unilaterally, each with a
recommendation and a trigger.

| # | Question | Recommendation | Trigger for escalation |
|---:|---|---|---|
| D-P1 | **Does D2's `unsafe` prohibition extend to dependency internals?** ~~`bytemuck` for lane-width reinterpretation~~ (**no longer needed — D12's substrate has a native safe `bitcast`**); `perf-event` for cycle counts; possibly `rustfft` for `vaco-tx`; and now `fearless_simd` itself, which is the load-bearing case. | D2 governs *our* crates. Dependencies are governed by D3 (licence) plus a documented review. **Largely settled by D10 and D12**: internal `unsafe` is a measured, argued trade-off recorded at adoption, not a disqualification. Still add the explicit clause. | Needed before Phase 0.1 lands. |
| D-P2 | **Looser dependency bar for `crates/tools/`?** Nothing there ships in a binary. | Yes — document it, and have CI assert that no `crates/tools/` crate is in the dependency closure of any shipped binary. | Needed before Phase 0.2. |
| ~~D-P3~~ | ~~**Multi-artifact ISA builds vs a 30-line scoped `unsafe` dispatch shim**~~ | **RESOLVED by D12: neither.** `fearless_simd`'s capability tokens give runtime dispatch with no `unsafe` in our crates and no multi-artifact packaging. Both options are withdrawn. | — |
| **D-P3′** | **Binary-size budget.** Multiversioning compiles every level-generic function three times on x86 (§1.4 Risk A cost 1). What size is acceptable, and do we prune AVX-512 by default? | Measure in §11 item 4 before deciding. Provisional: ship all three levels; `lto="fat"` + `codegen-units=1` are already set; treat >2.5x the single-level size as the trigger to reconsider. | When the §11 measurement lands, or if a packager objects. |
| D-P4 | **`vaco-tx`: our own safe FFT, or a permissive external crate?** | Build our own (it must be bit-exact with our decoders and support the full `AVTXType` matrix per research §01 §8, which no external crate does). Use `rustfft` as a benchmark upper bound only. | If our implementation is >15% behind `rustfft` after the §1.4 Risk B mitigations. |
| D-P5 | **AV1 comparison baseline.** FFmpeg has no native AV1 SIMD (research §08 §2e). | Benchmark against **dav1d**, state it publicly, and target 0.70–0.90x of dav1d rather than a meaningless ratio against FFmpeg's native path. | Now — it changes what S1 reports. |
| D-P6 | **T3 (NC-licensed) corpus usage.** UVG and parts of the derf collection are CC BY-NC or unclear. | Fetch-only, never redistributed, never used for gating; publish derived numbers only. Prefer T1 replacements where they exist. | If a headline claim depends on a T3 sample, replace the sample. |
| D-P7 | **Bit-exactness vs ULP tolerance for float kernels** (§3.1 R6). | Bit-exact, with the scalar reference written to the SIMD accumulation schedule. | If a codec spec mandates an accumulation order we cannot vectorize, take a documented ULP tolerance for that kernel only. |
| D-P8 | **Dedicated benchmark hardware.** Two physical machines (x86-64 + AArch64) are a hard prerequisite for §4's gating. | Provision before Phase 0.5. Cloud runners record results but never gate. | Now — it is a purchasing decision with lead time. |

---

## 10. What "done" looks like

The performance work is complete for v1 when all of the following hold on the reference machines,
measured by the harness in §4, published in the results store:

1. **S1 (decode):** H.264 and HEVC within **0.93–1.10x** of `ffmpeg-tuned` at 1080p *(lower bound
   relaxed from 0.95 by D12's §1.5 recomputation — the `pmaddubsw` composition costs more than the
   superseded plan assumed. If §11 item 1 measures better than 2.2x, restore 0.95.)*, single-threaded
   and at the measured default thread count. VP9 within 0.90–1.10x. AV1 within 0.75x of dav1d.
2. **S3 (scale/convert):** **≥1.00x** of `ffmpeg-tuned` on every conversion in the matrix, and
   ≥1.10x on at least half of them.
3. **S4 (resample):** ≥0.95x throughput at equal or better SNR.
4. **S5 (filters):** ≥1.10x on the deinterlace, blur and denoise sets.
5. **S6 (seek), S7 (startup):** p95 within 1.25x of the reference. *(D12: there is no launcher cost to
   disclose any more. What must be disclosed instead is the **binary size** relative to a single-level
   build, per D-P3′.)*
6. **S8 (memory):** peak RSS ≤ 1.1x of the reference at equal thread counts.
7. **S10 (determinism):** byte-identical output across all thread counts, always.
8. **`vaco-checkasm`:** 100% of registered kernel variants covered; zero variants slower than their
   scalar reference; exhaustive nightly green.
9. **`vaco-vecheck`:** zero unwaived assertion failures; total live waiver `cost_pct` under 3%.
10. **Every band in §1.3 either confirmed by measurement, or revised by a PR that carries the
    measurement.** No band is allowed to remain a prediction at v1.

And the honest disclosure that goes with it: a published table of every place where we are slower
than FFmpeg, by how much, and why — because the project's credibility rests on the numbers being
trustworthy, not on their all being above 1.0.

---

## 11. `fearless_simd` adoption checklist (D12)

**Status: this is Phase 0 item 0.0 and it gates 0.1.** Half a person-week. It exists because D12 was
taken on a documentation review, and the point of maximum leverage is *before* `vaco-simd`'s API is
frozen and before a single production kernel is written against it.

Two things were already settled during the D12 revision and are recorded here as **closed**, not as
work items:

- ✅ **aarch64 NEON exists.** `Level::Neon(Neon)` under `#[cfg(target_arch = "aarch64")]`, with
  `Level::as_neon() -> Option<Neon>` and a fully generated backend. Confirmed against the v0.7.0 source
  and the `aarch64-apple-darwin` documentation build. D12's open risk 2 is closed. Bonus: aarch64 has
  exactly one level, so `dispatch!` is a single-arm match there and the binary-size cost is zero.
- ✅ **`dispatch!` expands to no `unsafe`.** Verified against the v0.7.0 macro source: it is a `match`
  over `Level` that binds a token and calls `Simd::vectorize(token, || body)`, a safe trait method.
  `#![forbid(unsafe_code)]` survives across the entire workspace. **`kernel!` is a different story and
  is closed to us** — its expansion *does* contain `unsafe` in the calling crate.

### What must be measured before we write production kernels

| # | Measurement | Why it is on the list | Pass condition |
|---:|---|---|---|
| **1** | **The `pmaddubsw` composition.** Build the 8-tap u8 horizontal FIR both ways — widen-hoisted-out-of-the-tap-loop (plan 11 §5.6) and a `pmaddubsw` reference written in C or asm purely as a yardstick — and count instructions with `llvm-mca` at each level. | This is the single largest named performance risk in the plan and the one whose estimate moved most (~1.4x → ~2.2–2.5x). §1.3 band 6, §1.5's headline number and §10's S1 criterion all hang off it. Also tests whether LLVM's DAG combines still fire over explicit intrinsics. | ≤2.5x the instruction count. **>3x ⇒ stop and escalate** (Risk C mitigation 4: raise it upstream before writing MC). |
| **2** | **Every gap composition in plan 11 §5.6**, benchmarked against its native instruction on both x86 and aarch64: `saturating_add`/`sub`, `avg_round`, `abs_diff`, `abs_int`, `hsum_i32`, `madd_i16_i32`. | These are stated as instruction counts derived by reading the source. Counted instructions are not measured cycles, and `pmulld`-class multiplies in particular have latencies that instruction counts hide. | Each within the count stated in §5.6, ±1. Any composition >1.3x its stated cost gets a written note and an upstream issue. |
| **3** | **Dispatch overhead.** Time `dispatch_kernel!` on a trivial body at 1, 10 and 100 calls per frame. | The cost model in §2.5 claims "per row, never per pixel". If dispatch is more expensive than an indirect call we need to know before the `KernelSet` shape is frozen. | <5 ns per dispatch, and no measurable difference against a plain `fn` pointer at 1 call/row. |
| **4** | **Binary size.** Build `vaco-scale` with all levels, then with `--cfg disable_dispatch_avx512`, then at a single level. | §1.4 Risk A cost 1 and decision D-P3′. Multiversioning is the one place the superseded plan was cheaper, and packagers will ask. | Record the number. Provisional trigger: >2.5x single-level ⇒ reconsider shipping AVX-512 by default. |
| **5** | **Inlining actually happens.** Take one real kernel through `dispatch_kernel!` and assert with `cargo-show-asm` that the AVX2 monomorphisation contains `vpmulld`-class 256-bit instructions and **no `call`**. | §2.0 rule 4. A body that fails to inline is compiled at the baseline: correct, silently slow, and invisible to every correctness test. This is the failure mode most likely to bite us and least likely to be noticed. | Zero `call` in the kernel body at every level; instruction widths match the level. |
| **6** | **`interleave` and `swizzle_dyn` costs at 256/512 bits.** Both have `_within_blocks` siblings, which implies the unsuffixed forms are lane-crossing. | §2.3's chroma upsample uses `interleave`; §2.4's pack uses `swizzle_dyn`. Both are in the inner loop of path #1, and if the full-width forms cost an extra `vperm2i128` the block-granularity design in §2.4 should extend to the upsample too. | Document the per-level cost of each. No pass/fail — this feeds a design choice. |
| **7** | **Cross-tier bit-exactness.** Run the `checkasm-tier-matrix` (§5.7) over the `ops` module and two real kernels at every level including `fallback`. | Integer kernels must be bit-identical across levels. A backend that rounds or saturates differently at one level is a correctness bug we must find now, not after 150 kernels exist. | Bit-identical at every level. Any divergence is a blocking upstream bug report. |
| **8** | **Gate 3 re-check at v1.0.** `fearless_simd` v1.0 is due early September 2026, days after this plan is written. | We would be adopting 0.7 and upgrading almost immediately. | Re-run the D10 Gate 3 assessment and the `cargo-geiger` count against 1.0; record both in `docs/dependencies.md`. |

Items 1, 5 and 7 are blocking. Items 2, 3, 4, 6 and 8 are recorded and reported but do not stop 0.1.

### Upstream engagement, as a deliberate act

v1.0 is targeted for early September 2026 and the project is taking feedback in the open. **File the gap
list as issues before v1.0 lands**, not after. The asks, in priority order: widening multiply-add
(`pmaddwd`/`pmaddubsw` shape), saturating add/sub, horizontal reductions, integer `abs`, rounded average,
and absolute difference. None of these is exotic — a SIMD substrate aimed at graphics and media will
acquire them eventually, and we are unlikely to be the only consumer asking. Cost to us: an afternoon.
Expected value: high, because a native operation deletes a composition rather than optimising it.

This is also the honest reciprocal of depending on a small crate: if we are going to rely on it, we
should contribute to it.

### If a gap proves fatal — the fallback

**The blast radius is one crate.** `vaco-simd` is an adapter, per D11 and plan 11 §5.3:
`fearless_simd` appears in exactly one `Cargo.toml` under `crates/`, CI-enforced; the substrate's
`Level`, its `dispatch!` and its `kernel!` are named only there; and every operation whose semantics we
care about is behind `vaco_simd::ops` under our own name.

The escalation ladder, in order, and none of these steps is speculative:

1. **Restructure the algorithm to avoid the operation.** `wmla_u8_i16` over `madd_i16_i32` is the model:
   a broadcast-coefficient MAC needs no widening multiply at all, and the two-pass i16-intermediate
   structure that makes it affordable was already in the plan (Risk C mitigation 2). Most gaps yield to
   this; it should always be tried first because it costs nothing and is often faster than the
   instruction it replaces would have been.
2. **Raise it upstream** and, if it is small, send the patch. See above.
3. **Swap the substrate.** `pulp` is the natural candidate — the closest comparable design,
   `fearless_simd`'s own acknowledged inspiration, same capability-token idea, permissively licensed.
   The work is a rewrite of `vaco-simd` (design) plus a mechanical rename pass across kernel bodies
   (`sed`), because plan 11 §5.3's "honest boundary" section is explicit that `Lanes` is a re-export
   rather than a newtype. **Not zero changes outside one crate** — one crate of *thought*, and
   mechanical edits elsewhere. Say so plainly rather than overselling the adapter.
4. **Fall back to `std::simd` + the superseded F5 multi-artifact design.** It is fully written down in
   plan 11 §5.3's "superseded reasoning" and in this document's §1.4 Risk A, deliberately preserved
   rather than deleted, precisely so this is a retreat to a known position rather than a redesign. We
   would be trading runtime dispatch back for LLVM's peephole combines, plus a nightly pin. It is a
   worse place to be, but it is a *place*, and the cost of getting back to it is bounded.
5. **A scoped `unsafe` exception** is the last resort and it has become *less* attractive, not more:
   what it would buy is now only the small set of operations `kernel!` would give us, not dispatch,
   which we already have for free. If it is ever proposed it must be scoped to one named operation in
   `vaco_simd::ops`, with the measured deficit attached, per D2's escalation rule.

### One thing this checklist cannot settle

Whether the substrate's maintainers stay interested. `fearless_simd` is a small crate from a small
project, and our whole DSP layer would sit on it. That risk is genuinely mitigated — zero dependencies,
Apache-2.0 OR MIT, ~5k lines of hand-written code over generated backends, small enough for us to fork
and maintain — but it is not eliminated, and "we could fork it" is a cost we would actually have to pay.
Record it in `docs/dependencies.md` as an accepted risk with a named owner, and re-check it at every
release per D10 Gate 3. It is the right trade, and it is a trade.

---

## Amendment — PF-0.0 measured (2026-08-21)

See `planning/00-decisions.md` D12 second addendum for the full table. What
changes in this document:

1. **The widening-MAC risk is downgraded on aarch64 and remains open on x86.**
   Measured 0.79x for the `pmaddwd` shape and 1.12x for the 8-tap u8 FIR, against
   the ~6x and 2.2–2.5x this plan assumed. LLVM reconstructs `smull`/`smull2`/
   `addp.4s` from our composition. The performance bands in §2 that were driven
   down by this gap should be re-derived for aarch64; **they stand unrevised for
   x86 until an x86-64 run exists**, since that target was not measured at all.

2. **§5.6's FIR structure recommendation is withdrawn.** "Hoist the widen, `slide`
   per tap" measures 1.63x versus 1.12x for the naive per-tap reload it was meant
   to improve — twelve `ext.16b` contending for one shuffle port. Prefer the naive
   form and measure before restructuring.

3. **Two authoring rules are added to §5, and they outrank instruction selection.**
   Both are invisible to correctness tests and each is worth up to 4x:
   - *Batch until you spill.* Batching the FIR made it worse — one stack spill
     became six. There is an optimum and it is found by measuring, not by reasoning.
   - *Never carry a single vector accumulator; use four.* Both the horizontal
     reduction (3.90x → 0.99x) and the rounded average (1.55x → 1.00x) were
     latency chains misdiagnosed as missing instructions.

4. **A missing operation is not automatically a cost.** The methodology lesson: of
   seven gaps this plan treated as deficits, six measure free once LLVM has seen
   the composition. Measure the composition before designing around its absence.

5. **§11's "count instructions with `llvm-mca`" is superseded.** Timing
   `#[inline(never)]` symbols and disassembling them gives both the instruction
   count and the cycles, and it caught a 0.45x reading on byte-identical machine
   code that an instruction count would have accepted as equal.

6. **New CI requirement.** These results depend on LLVM 22's combiner; a toolchain
   bump could take a 1.00x row to 3x with no test failing. `xtask` needs an
   instruction-selection assertion driven by the `probes` module in `vaco-simd`,
   so silent codegen regressions fail CI.

7. **Benchmark harness note.** `divan` was dropped for this checklist in favour of
   a hand-rolled interleaved A/B harness, because the measurement needs a ~300 ms
   core-promotion warmup: macOS parks new processes on efficiency cores, and
   without it an unchanged binary reported 45 ns and 132 ns for the same row on
   consecutive runs. Any benchmark on Apple silicon needs this.
