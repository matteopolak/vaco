I'm a read-only search/analysis agent and have no file-write capability (Write/Edit are disabled in this session), so I cannot create `/Users/matthew/projects/vaco/planning/research/08-performance-simd.md` myself. Below is the complete report content — please have the orchestrating agent write this to that path. All findings are structural/quantitative descriptions derived from repo layout, file sizes, header/API names, and build-config flags; no FFmpeg source or assembly is reproduced.

Repo analyzed: `~/repos/FFmpeg`, commit `564f92cce23ae95399476617b8a1dc357f002a47` (2026-08-19).

---

```markdown
# FFmpeg Performance & SIMD Architecture — Analysis for Rust Reimplementation

Source: ~/repos/FFmpeg @ 564f92cce23ae95399476617b8a1dc357f002a47 (2026-08-19)
Method: structural analysis only (file layout, line counts, header/API names,
build-config flags). No FFmpeg source or assembly text is reproduced anywhere
in this document, per clean-room requirements.

## 1. DSP Dispatch Architecture (conceptual)

FFmpeg's performance model is built on a small number of recurring patterns,
repeated identically across every codec/filter/scaler library:

**a) "DSP context" struct-of-function-pointers.**
Each subsystem that has performance-critical inner loops (h264dsp, hevcdsp,
vp8dsp, vp9dsp, aacdsp/sbrdsp, ac3dsp, mpegvideodsp, videodsp, hpeldsp,
me_cmp, swscale's SwsContext internals, swresample's resample/mix contexts,
many libavfilter vf_*/af_* filters) declares a plain C struct of function
pointers ("XxxDSPContext"). Generic C implementations are the default
values. There is no vtable/polymorphism machinery — it's a flat struct
assigned once at init time and then called through directly, so the
call is a single indirect branch with no dynamic dispatch overhead beyond
that.

**b) Per-architecture init cascade.**
A top-level `ff_xxxdsp_init()` (in the library's generic .c file) first
installs the C defaults, then — gated by `ARCH_X86`, `ARCH_AARCH64`,
`ARCH_ARM`, `ARCH_PPC`, `ARCH_MIPS`, `ARCH_LOONGARCH`, `ARCH_RISCV`
preprocessor conditionals set by `configure` — calls exactly one
`ff_xxxdsp_init_<arch>()` function compiled from that architecture's
subdirectory (`libavcodec/x86/xxxdsp_init.c`, `libavcodec/aarch64/...`,
etc.). This is a compile-time fork: only the matching architecture's init
object is even built into the binary; there's no runtime branching between
architectures.

**c) Runtime ISA-tier overrides inside the arch init function.**
Within e.g. `libavcodec/x86/h264dsp_init.c`, the arch-specific init reads a
process-wide CPU feature bitmask (`av_get_cpu_flags()`, from
`libavutil/cpu.c`) once, then does a cascading sequence of `if` blocks —
MMX → MMXEXT → SSE → SSE2 → SSSE3 → SSE4 → AVX → AVX2 → AVX-512 (x86); or
NEON → dotprod/i8mm → SVE/SVE2 → SME/SME2 (ARM/AArch64) — where each tier
unconditionally overwrites the function-pointer slots set by the previous
(weaker) tier. The result is "highest satisfied tier wins," expressed as
sequential overwrites rather than a switch. Feature gating goes through
`EXTERNAL_<ISA>(flags)` / `EXTERNAL_<ISA>_SLOW/_FAST(flags)` macros
(`libavutil/x86/cpu.h`) that also encode known-microarchitecture "this ISA
is present but slower than the previous tier" exceptions (e.g.
SSE2SLOW/SSSE3SLOW/AVXSLOW/ATOM), which the C code must know to skip.
`av_get_cpu_flags()` itself does CPUID/getauxval/HWCAP-style detection once
at process start and caches the result; it can be overridden via the
`AV_CPU_FLAG_FORCE`-style env/API for testing.

**d) Two build-time gates layer on top of the runtime one.** `configure`
decides (i) whether a given ISA extension's *code* is compiled in at all
(`--disable-avx2`-style flags, and whether the toolchain/assembler
supports it), and (ii) whether inline/external assembler (`nasm`/`yasm`
for x86, native `.S` for ARM/AArch64/RISC-V) is available at all
(`HAVE_X86ASM`, etc.). So there are three independent layers: compile-time
"is this ISA supported by this build," compile-time "which architecture
directory is linked in," and runtime "which ISA tier does this CPU
actually have." A Rust equivalent needs to reproduce all three
independently rather than collapsing them.

**e) Bit-depth / chroma-format specialization multiplies the matrix.**
Many DSP inits take extra parameters (bit depth, chroma subsampling idc)
and select among multiple pointer sets for 8/10/12/14-bit or 420/422/444,
so the "DSP context" is really indexed by {arch tier} × {bit depth} ×
{format}, all resolved once at decoder/filter init, never per-call.

**f) checkasm as the correctness+perf harness.** `tests/checkasm/` (99
per-subsystem test files) is a standalone test/bench executable that, for
each DSP function, generates randomized/edge-case inputs, calls every
available implementation (C reference plus every enabled asm/intrinsics
variant the runtime CPU supports) on identical input, and byte-compares
outputs; on mismatch it reports which variant failed. Since FFmpeg 8-ish,
checkasm's core (`tests/checkasm/ext/`) is a *vendored external* library
(shared lineage with dav1d's checkasm) with its own CLI: `--test=<pattern>`
and `--function=<pattern>` restrict which DSP areas run, and `--bench`/`-b`
switches from correctness-checking to timing mode, where it uses a
platform performance counter (rdtsc-class on x86, cycle counter reads on
ARM/AArch64) with an estimated per-call fixed-overhead ("nop cycles")
subtracted out, running each variant until a target cycle-count budget or
minimum sample count (30 samples) is reached, then reporting median/stddev
cycles and a ratio against a reference implementation. This is wired into
`make fate-checkasm` (`tests/fate/checkasm.mak` enumerates one fate target
per DSP area) so asm-vs-C parity is part of CI, and into ad hoc
`./checkasm --bench` runs used by contributors to justify asm patches with
before/after cycle counts.

**Implication for a Rust architect** — three viable equivalents, roughly in
FFmpeg's own preference order:
- **fn-pointer struct** (closest analogue): a plain struct of `fn` pointers
  or `unsafe extern "C"`-ABI function pointers per DSP area, populated once
  at decoder/filter construction by an arch-specific init path chosen via
  `#[cfg(target_arch = ...)]` plus a runtime `is_x86_feature_detected!`/
  `std::arch::is_aarch64_feature_detected!`-style cascade mirroring (c).
  Zero dynamic-dispatch cost beyond one indirect call, and it reproduces
  FFmpeg's "resolve once, call many times" hot-path property exactly.
- **trait objects**: more idiomatic Rust but adds a vtable indirection and
  (for `dyn Trait`) makes multiple-return-value / SIMD-width-specific
  signatures awkward; better suited to coarse-grained strategy selection
  (e.g. "which whole scaler") than per-DSP-primitive dispatch.
- **function multiversioning** (`#[target_feature]` + a dispatch shim,
  or nightly multiversion-style crates): closest to zero-overhead but
  currently less mature/portable in stable Rust across architectures than
  x86; would need per-arch feature-detection glue anyway.
A hybrid — fn-pointer contexts for codec/filter DSP tables (matching
FFmpeg's structure and easing side-by-side benchmarking), with
`#[target_feature]` multiversioning for the leaf kernels the pointers
resolve to — most directly ports FFmpeg's model while staying idiomatic.
A Rust checkasm-equivalent (property-test each kernel variant against a
scalar reference over randomized/edge inputs, plus a criterion-based cycle
harness with a nop-overhead baseline) is worth building early, mirroring
(f), since it's what let FFmpeg accept hundreds of contributor asm patches
without regressions.

## 2. Quantified Map of Where the Assembly Is

### 2a. Per-library, per-architecture volume (asm/.S files only; headers/.c excluded from the asm counts, included in "all files")

| Library | Arch dir | All files (.c/.h/.asm/.S) | Total lines | Asm/.S files | Asm/.S lines |
|---|---|---:|---:|---:|---:|
| libavcodec | x86 | 212 | 69,040 | 113 | 55,916 |
| libavcodec | aarch64 | 87 | 44,207 | 46 | 39,800 |
| libavcodec | arm | 131 | 33,403 | 63 | 28,517 |
| libavcodec | mips | 110 | 98,044 | 0 | 0 (C/MSA intrinsics, no .S) |
| libavcodec | loongarch | 54 | 39,730 | 9 | 11,995 |
| libavcodec | riscv | 107 | 13,395 | 60 | 10,213 |
| libavcodec | ppc | 30 | 6,609 | 1 | 147 |
| libavcodec | wasm | 5 | 1,267 | 0 | 0 |
| libavutil | x86 | 28 | 8,469 | 12 | 6,676 |
| libavutil | aarch64 | 16 | 3,576 | 7 | 2,697 |
| libavutil | arm | 15 | 1,940 | 4 | 1,175 |
| libavutil | mips | 10 | 4,049 | 0 | 0 |
| libavutil | riscv | 15 | 1,801 | 6 | 952 |
| libavutil | loongarch | 4 | 2,088 | 0 | 0 |
| libavutil | ppc | 10 | 923 | 0 | 0 |
| libswscale | x86 | 21 | 11,941 | 13 | 6,014 |
| libswscale | aarch64 | 22 | 10,717 | 8 | 5,473 |
| libswscale | loongarch | 14 | 9,649 | 3 | 3,290 |
| libswscale | ppc | 6 | 3,867 | 0 | 0 |
| libswscale | arm | 8 | 1,157 | 6 | 921 |
| libswscale | riscv | 7 | 849 | 4 | 649 |
| libswresample | x86 | 7 | 1,940 | 3 | 1,557 |
| libswresample | arm/aarch64 | 5 each | 659 each | 2 each | 442 each |
| libavfilter | x86 | 83 | 13,385 | 42 | 10,698 |
| libavfilter | aarch64 | 6 | 1,709 | 3 | 1,474 |
| libavfilter | riscv | 5 | 216 | 2 | 126 |

Note: `libavcodec/mips` and `libavutil/mips`/`loongarch` (partially) carry
their SIMD as C intrinsics (MSA/MMI/LSX/LASX) rather than standalone `.asm`/
`.S`, which is why their file-line totals are large but asm-file counts are
0 — a reminder that "assembly volume" undercounts total SIMD-code volume on
those targets; x86 and ARM/AArch64 are the ones that use real hand-written
asm (nasm syntax / GNU `.S`) almost exclusively, which is also why they are
FFmpeg's primary optimization targets and should be a Rust project's primary
targets too.

Total addressable "genuine assembly" across libav* (x86+arm+aarch64+riscv+
loongarch+ppc, excluding mips/wasm which are intrinsics-based): **~190,000
lines**, x86 and AArch64 alone accounting for ~95,700 of it.

### 2b. Ranked DSP-area table (lines of asm/.S summed across all architectures; ~60 areas, categorized by grep over path/filename)

| Rank | Area | Asm lines (all arches) | Notes |
|---:|---|---:|---|
| 1 | H.264 decode (mc/qpel/chroma, IDCT, intra pred, in-loop deblock, cabac-adjacent) | 39,647 | Largest single area; split across `h264*`/`h26x/` dirs in x86, arm, aarch64, riscv, loongarch |
| 2 | VP9 decode (itxfm incl. 16bpp, intra pred, mc, loop filter) | 32,802 | x86 has 16bpp itxfm/intrapred/AVX-512 variants; ARM/AArch64 near-parity |
| 3 | HEVC/VVC decode (mc/epel/qpel, idct, sao, deblock, pred, ALF for VVC) | 22,254 | AArch64 h26x epel/qpel + VVC inter/ALF are individually huge files (see 2c) |
| 4 | swscale scaling/resize core (`swscale.S`/`.asm`, hscale/vscale) | 16,347 | loongarch's `swscale.S` alone is 2,236 lines |
| 5 | libavfilter vf_/af_ SIMD filters (aggregate) | 12,298 | Long tail of many filters; see 2d breakdown |
| 6 | VP8 decode (mc, idct, intra pred, loop filter) | 8,510 | |
| 7 | mpegvideo/generic IDCT/block/pixblock dsp | 5,696 | Shared by mpeg1/2/4, simple_idct, idctdsp, blockdsp |
| 8 | VC-1 decode | 4,806 | |
| 9 | motion estimation / me_cmp (SAD/SATD family) | 3,450 | Used by every encoder and by decoders' error concealment |
| 10 | av_tx (float FFT/MDCT, replaces legacy fft.c) | 3,224 | Shared transform backend for many audio codecs |
| 11 | AAC decode/encode (core + SBR + PS) | 3,022 | |
| 12 | swresample resample/format-convert core | 2,982 | |
| 13 | generic hpel/qpel dsp (non-codec-specific half/quarter-pel) | 2,941 | |
| 14 | x86inc/x86util shared asm macro layer | 2,907 | Not DSP itself — the nasm macro framework every x86 file `%include`s |
| 15 | RV30/RV40 decode | 1,805 | |
| 16 | videodsp/fmtconvert/bswapdsp (generic pixel/format utility dsp) | 1,602 | |
| 17 | Vorbis dsp / MDCT-adjacent (imdct36, synth_filter) | 1,461 | |
| 18 | MLP/TrueHD decode | 1,391 | |
| 19 | FLAC decode/encode dsp | 1,253 | |
| 20 | Bluetooth SBC codec dsp | 1,123 | |
| 21 | CineForm (cfhd) dsp | 1,097 | |
| 22 | AC-3/E-AC-3 dsp | 1,036 | |
| 23 | Older VPx/MSS/WMV2 family | 942 | |
| 24 | HuffYUV dsp | 495 | |
| 25 | DCA/DTS dsp | 439 | |
| 26 | Vorbis (non-MDCT parts) | 266 | |
| 27 | Opus dsp (core postfilter; Opus's own celt math is separate, see below) | 257 | |
| 28 | JPEG2000 dsp | 238 | |
| 29 | ALAC decode | 226 | |
| 30 | G.722 dsp | 192 | |
| 31 | OpenEXR dsp | 157 | |
| 32 | ProRes dsp | 68 | |
| 33 | DNxHD dsp | 49 | |
| 34 | loongson_asm.S (shared LoongArch macro layer, analogous to x86inc) | 945 | infra, not DSP |
| 35 | libavutil float_dsp (x86 + ARM VFP + AArch64 + RISC-V variants) | ~1,600 combined | scalar/vector float ops used by many audio codecs |
| 36 | libavutil crc (x86 + AArch64) | 910 combined | |
| 37 | XviD-compatible IDCT (x86) | 577 | |
| 38 | MP3 dct32 (x86) | 481 | |
| 39 | Opus CELT PVQ search (x86, encoder-side) | 384 | pyramid vector quantization search is one of Opus encode's hottest loops |
| 40 | jrevdct (ARM, legacy reference-style IDCT) | 383 | |
| 41 | fdctdsp (AArch64) | 368 | |
| 42 | v210 10-bit packed YUV encode (x86) | 358 | |
| 43 | Dirac / dirac_dwt / diracdsp (x86) | ~650 combined | wavelet transform |
| 44 | libavutil crc32 (x86 crc.asm) | 298 | |
| 45 | libavutil lls (linear least squares, x86) | 290 | used by some estimators/filters |
| 46 | PNG dsp (x86 + AArch64) | ~430 combined | paeth/defilter predictor |
| 47 | libavutil pixelutils (x86 SAD-family, + AArch64) | ~420 combined | |
| 48 | LPC dsp (x86, used by FLAC/ALAC/TTA encode) | 258 | |
| 49 | APV dsp (x86) | 258 | newer intra-only codec |
| 50 | swresample rematrix (x86, channel mixing) | 234 | |
| 51 | lossless video enc dsp (x86) | 224 | |
| 52 | RISC-V float_dsp/fixed_dsp (rvv) | ~540 combined | |
| 53 | Snow codec dsp (x86) | 191 | |
| 54 | v210 decode (x86) | 187 | |
| 55 | CAVS idct (x86) | 164 | |
| 56 | RV30/34 shared dsp (x86 + ARM, distinct from RV40 above) | ~320 combined | |
| 57 | H.263 loop filter (x86) | 161 | |
| 58 | VP7 dsp (RISC-V) | 150 | |
| 59 | TTA lossless audio dsp (x86) | 144 | |
| 60 | ARM/AArch64/RISCV/PPC shared asm prologue macro files (`asm.S`, `neon.S`) | ~1,100 combined | infra, not DSP |

### 2c. The individual largest single asm files (top 15, any library/arch) — useful as a "what does a maximal DSP kernel file look like" reference

| Lines | File |
|---:|---|
| 6,476 | `libavcodec/aarch64/h26x/qpel_neon.S` (HEVC/VVC-shared quarter-pel motion comp) |
| 5,761 | `libavcodec/aarch64/h26x/epel_neon.S` (HEVC/VVC-shared eighth-pel motion comp) |
| 4,445 | `libavcodec/loongarch/hevc_mc.S` |
| 3,129 | `libavcodec/aarch64/vvc/inter.S` |
| 2,810 | `libavcodec/x86/vp9itxfm.asm` |
| 2,497 | `libavcodec/x86/vp9intrapred_16bpp.asm` |
| 2,236 | `libswscale/loongarch/swscale.S` |
| 2,083 | `libavcodec/x86/vp9intrapred.asm` |
| 2,030 | `libavcodec/x86/vp9itxfm_16bpp.asm` |
| 2,017 | `libavcodec/aarch64/vp9itxfm_16bpp_neon.S` |
| 1,978 | `libavutil/x86/x86inc.asm` (shared macro infra, not a DSP kernel) |
| 1,977 | `libavcodec/loongarch/h264dsp.S` |
| 1,958 | `libavcodec/x86/h264_intrapred.asm` |
| 1,945 | `libavcodec/arm/vp9itxfm_16bpp_neon.S` |
| 1,936 | `libavutil/x86/tx_float.asm` (shared FFT/MDCT kernel) |

### 2d. libavfilter breakdown (largest per-filter files, x86/aarch64/riscv)

`vf_removegrain` (1,218 ln), `colorspacedsp` (1,097 ln), `vf_gblur` (995 ln),
`vf_bwdif` NEON (788 ln), `vf_fspp` (705 ln), `vf_lut3d` (662 ln),
`vf_blend` (498 ln), `vf_colordetect` NEON (480 ln), `vf_convolution`/
`vf_bwdif` x86 (~300 ln each), `yadif-16`/`yadif-10`/`vf_yadif` (deinterlace,
~780 ln combined), `vf_ssim`, `vf_v360`, `vf_atadenoise`, `vf_w3fdif`,
`vf_interlace`, `vf_stereo3d`. Deinterlacing (bwdif/yadif) and blur/denoise
filters dominate; these are good early Rust-SIMD targets since they're
simple separable-kernel patterns (widening multiply-add + saturating pack)
without the branchy entropy-coding neighbors that h264/hevc decode carry.

### 2e. Notably absent: AV1

There is **no** `av1dsp`/AV1 SIMD in `libavcodec/{x86,arm,aarch64,...}`. FFmpeg's
native `libavcodec/av1dec.c` exists (frame/parsing logic) but AV1 pixel
decode SIMD is not maintained in-tree; production AV1 decode goes through
the external `libdav1d` (a separate, dav1d-project-maintained C+asm
codebase, not part of this repo), and AV1 encode goes through
`libaom`/`libsvtav1`/`librav1e` wrappers (`libavcodec/libaomenc.c`,
`libsvtav1.c`, `librav1e.c`). A Rust project targeting AV1 should treat it
as its own SIMD effort (dav1d's kernel set is the closest reference, but is
itself off-limits under the same clean-room constraint unless separately
cleared) rather than expecting anything reusable from FFmpeg's own tree.

## 3. Threading

**Three independent threading axes, chosen per-codec via capability flags:**

- **Frame threading** (`libavcodec/pthread_frame.c`, 1,104 lines): whole
  frames are decoded on separate worker threads, pipelined so thread N+1
  can begin parsing/decoding frame N+1 while thread N is still finishing
  frame N (bounded by codec-declared max lookahead / reference-frame
  dependencies). Selected when the codec advertises
  `AV_CODEC_CAP_FRAME_THREADS` and the caller hasn't set `AV_CODEC_FLAG_LOW_DELAY`
  or the `CHUNKS` flag (both of which force serial decode because frame
  threading adds decode latency — output frame N isn't necessarily ready
  before frame N+1 has started). This is FFmpeg's primary decode-side
  parallelism for modern codecs (h264, hevc, vp8, vp9, av1-via-dav1d, etc.).
- **Slice threading** (`libavcodec/pthread_slice.c`, 154 lines): a single
  frame's independent slices/rows are farmed out to a worker pool via an
  `execute`/`execute2`-style callback the codec calls with a work-item
  count; used when a codec doesn't support frame threading, or in addition
  to it for codecs with genuinely independent slices. Selection logic
  lives in `libavcodec/pthread.c`'s `validate_thread_parameters()`: if
  `thread_count == 1` threading is fully disabled; else frame threading is
  preferred when both the codec and caller-requested `thread_type` support
  it; else slice threading is used if the codec advertises
  `AV_CODEC_CAP_SLICE_THREADS`; else (if the codec doesn't opt into
  "auto threads" internally) `thread_count` is forced back to 1.
- **Filter slice threading** (`libavfilter/pthread.c`): filtergraph nodes
  that support it split a frame into horizontal slices processed in
  parallel across the graph's shared thread pool, orthogonal to the
  decode/encode threading above.

**Thread-count heuristics:** when `thread_count` is left at 0 (auto), many
paths (frame-thread encoder wrapper, several external-encoder wrappers
like libaom/libvpx) resolve it via `av_cpu_count()` (`libavutil/cpu.c`) —
effectively the detected logical core count — but internal frame/slice
threading has a hard **`MAX_AUTO_THREADS = 16`** ceiling
(`libavcodec/pthread_internal.h`) with only a warning (not a hard cap) if
the caller explicitly requests more; this reflects a known scaling limit —
diminishing/negative returns beyond ~16 decode worker threads for typical
frame sizes because reference-frame and bitstream-serialization
dependencies (each frame usually needs the previous frame's motion vectors/
reconstructed pixels) bound achievable parallelism, and slice threading is
bounded by the number of independent slices the bitstream was actually
encoded with (often just 1–4 for consumer content). Low-resolution or
low-latency streams frequently can't productively use more than a handful
of threads regardless of the ceiling.

**Locks/atomics:** frame threading synchronizes via per-frame
progress counters (`libavcodec/threadprogress.c`/`.h` — an
atomic/condvar-backed "this many rows/frames of progress have been made"
primitive) that downstream threads poll/wait on rather than a single global
lock, so contention is per-frame-dependency rather than global. The pool
implementations themselves (`pthread.c` family) use standard mutex+condvar
worker-pool patterns.

**CLI-level (fftools) parallelism — the newer scheduler:** separate from
libavcodec's internal frame/slice threading, `fftools/ffmpeg_sched.c`
(2,834 lines) plus `fftools/thread_queue.c`/`.h` implement a
graph-of-components scheduler for the `ffmpeg` CLI itself: demuxers,
decoders, filtergraphs, encoders, and muxers are modeled as nodes in a
DAG (acyclicity checked at startup) connected by bounded thread-safe
queues (`thread_queue.c`), each component (potentially) running on its own
OS thread, so an entire multi-stream/multi-filter transcode pipeline runs
concurrently rather than being driven by one single-threaded event loop
calling into codecs that happen to be internally multi-threaded. This is
the mechanism that lets `ffmpeg` fan out multiple output streams / complex
filtergraphs across cores even where individual codec instances are
single-threaded. It is a relatively recent (post-monolithic-`ffmpeg.c`)
rewrite and is worth studying closely as the model for a Rust CLI's own
pipeline scheduler — the DAG + bounded-queue structure is a clean,
reusable pattern independent of any FFmpeg-specific code.

## 4. Memory

- **Buffer pooling:** `libavutil/buffer.h`'s `AVBufferPool` is described in
  its own header docs as a **lock-free, thread-safe** pool: `av_buffer_pool_init()`/
  `_init2()` create a pool with a fixed allocation size (and optional custom
  alloc/free callbacks, e.g. for pooling GPU-backed buffers), `av_buffer_pool_get()`
  hands out a refcounted `AVBufferRef` that returns to the pool automatically
  when its refcount drops to zero (no explicit "release to pool" call needed
  by consumers), and `av_buffer_pool_uninit()` marks the pool for teardown
  once all outstanding buffers drain. This is what backs `AVFrame`'s
  `get_buffer2` allocation path so decode doesn't malloc/free full picture
  buffers every frame.
- **Alignment:** picture/line strides are aligned to a `STRIDE_ALIGN`
  constant (`libavcodec/internal.h`) that is **64 bytes when AVX-512 support
  is compiled in, else 32 for AVX/AVX2-only builds, else 16, else a floor of
  8** — directly a function of the widest SIMD load the build might issue,
  so unaligned-load fallbacks aren't needed on the fast path. This is a
  build-time constant baked from `configure`'s SIMD-width detection
  (`HAVE_SIMD_ALIGN_64/32/16`), not a runtime CPU-feature check — i.e. the
  binary commits to one alignment target at build time regardless of which
  runtime ISA tier ends up executing. A Rust port should size its alignment
  the same way: pick alignment from the widest SIMD width the *build*
  targets (or just always use 64B to be safely correct across
  SSE/AVX/AVX-512/NEON/SVE), not the narrowest.
- **Bitstream buffer padding:** `AV_INPUT_BUFFER_PADDING_SIZE = 64`
  (`libavcodec/defs.h`) is required at the end of any buffer handed to a
  bitstream reader/parser, and the padding bytes are guaranteed zeroed by
  convention — this exists purely so SIMD/word-at-a-time bitstream readers
  and start-code scanners can over-read past the logical end without
  bounds-checking every access or triggering ASan/valgrind, and so a
  read that runs off real data reads deterministic zeros instead of
  garbage. Any Rust bitstream reader doing wide reads needs the equivalent
  guaranteed-slop convention (either padded buffers by construction, or a
  checked-tail/unchecked-body split).
- **Picture edge padding / edge emulation:** decoded picture buffers carry
  an `EDGE_WIDTH` (16, `libavcodec/mpegpicture.h`) border replicated beyond
  the visible frame so motion-compensation SIMD kernels reading
  out-of-frame reference pixels (for motion vectors pointing off the edge
  of the picture) don't need per-pixel bounds checks; when a motion vector
  would still reach past even that padding, `videodsp`'s edge-emulation
  path (`EDGE_EMU_LINESIZE = 32` scratch buffer, seen concretely in VP8's
  `libavcodec/vp8.h`) copies/replicates a small local patch into a scratch
  buffer with proper clamping just for that block, so the hot SIMD MC
  kernels themselves never contain edge-clamp branches. This
  "pad the common case, emulate the rare case out-of-line" split is a
  reusable pattern: Rust MC kernels should assume padded, edge-free input
  and push edge handling to a separate cold path.
- **Zero-copy / hardware frames:** `libavutil/hwcontext*.h` (a device-
  specific-context family: cuda, vaapi/vdpau-class, videotoolbox, d3d11,
  mediacodec, amf, etc.) models GPU/decoder-owned frame memory as
  `AVHWFramesContext`-backed `AVBufferPool`s using the *same* `AVBufferRef`
  refcounting API as system-memory frames, so a hardware-decoded frame can
  be passed downstream (to an encoder, a hardware filter, or a renderer)
  without a CPU-side copy as long as every stage understands the same
  hw-frame type; a copy is only forced at an explicit `hwdownload`/format
  conversion boundary. This is the model to replicate for any Rust
  hardware-decode integration: one refcounted "opaque frame handle" type
  usable transparently by both CPU and GPU-backed pools.

## 5. Benchmarking

**What exists today:**
- **checkasm bench mode** (`./checkasm --bench` / `-b`, optionally scoped
  with `--test=<pattern>`/`-t` and `--function=<pattern>`/`-f`): the
  primary micro-benchmark tool, per-DSP-function cycle counts with
  stddev, benchmarked against a target-cycle-count budget (looping until
  enough samples/cycles accumulate) and a nop-overhead baseline subtracted
  out; results can compare every enabled implementation of a function
  (C vs SSE2 vs AVX2 vs AVX-512, etc.) side by side. Wired into
  `make fate-checkasm` for correctness (not benchmarking) in CI; the
  `--bench` timing runs are typically ad hoc, done locally by contributors
  to justify a patch.
- **FATE** (`tests/fate-run.sh` + `tests/fate/*.mak`): FFmpeg's regression
  test suite, primarily correctness (bitexact output / checksum comparison
  against reference samples pulled from a samples server), not a
  performance benchmark per se, though wall-clock timing of FATE runs is
  sometimes used informally to catch gross regressions.
- **`-benchmark`** (`fftools/ffmpeg_opt.c`/`ffmpeg.c`): a CLI flag that
  prints wall-clock/user/sys time and (via `get_benchmark_time_stamps()`)
  timestamps at various pipeline points for an actual transcode run —
  coarse, whole-pipeline throughput measurement, not per-function.
- **Community practice**: comparisons in the wider ecosystem (and dav1d/
  x264/x265/SVT project benchmarking write-ups that FFmpeg contributors
  reference) typically report **fps** and/or **cycles/frame** for decode,
  **encode speed vs. quality curves** (bitrate at fixed quality, or
  quality at fixed bitrate, using PSNR/SSIM/VMAF) for encode, using a
  handful of standard-ish sample sets (raw YUV test sequences at common
  resolutions/framerates, plus real-world clips for practical throughput
  numbers) and controlled hardware (pinned frequency, disabled turbo, one
  process per core) to keep cycle counts stable.

**Concrete, reproducible scenarios a competitor project should adopt**
(these mirror the axes FFmpeg itself is evaluated on, so results are
directly comparable to published FFmpeg numbers):
1. **Decode throughput per codec**: fps and cycles/frame for h264, hevc,
   vp9, av1 (via dav1d or your own), at a fixed matrix of resolutions
   (e.g. 480p/1080p/4K) and thread counts (1, 4, 16), on a fixed sample
   set, using `-benchmark`-equivalent wall-clock plus `perf stat`
   cycle/instruction counts.
2. **Encode speed/quality curves**: at 2–3 fixed CRF/bitrate targets per
   codec, plot fps vs. VMAF/SSIM, both single-threaded and
   multi-threaded, against x264/x265/svt-av1 as the reference points.
3. **Scaling/pixel-format-conversion throughput**: MB/s or fps for
   swscale-equivalent operations across representative conversions
   (yuv420p→rgb24, yuv420p→nv12, bicubic up/downscale) at fixed
   resolutions, single-threaded (this code path is rarely multi-threaded
   in FFmpeg itself, so it's a fair single-core comparison).
4. **Audio resampling throughput**: samples/s for common rate/format/
   channel-layout conversions (e.g. 44.1kHz→48kHz stereo f32).
5. **Filter throughput**: fps for a small set of representative filters
   (scale, deinterlace/bwdif, blur, format convert) at 1080p.
6. **Seek latency**: median/tail wall-clock time to seek to N random
   timestamps in representative container/codec combos (mp4/h264,
   mkv/hevc, etc.) and produce the first decoded frame.
7. **Startup/cold-start latency**: wall-clock from process start to first
   decoded/encoded output, since this matters for short-lived CLI
   invocations and serverless-style usage.
8. **Memory high-water mark**: peak RSS during decode/encode/transcode of
   a fixed sample set, at 1 and N threads, since frame-threaded decode's
   look-ahead buffering has a real memory cost that trades against the
   throughput gains in §3.
9. Run everything with frequency scaling and turbo boost disabled, pinned
   thread affinity, and enough repetitions to report medians with
   variance, mirroring how checkasm and the FATE/dav1d benchmarking
   community control for noise.

## 6. The ~15 Hottest Real-World Code Paths and Their Rust-SIMD Shape

Ranked by where CPU time actually goes in common transcoding/playback
workloads (decode-heavy first, since decode dominates most real deployments):

1. **H.264 motion compensation (qpel/chroma interpolation)** — the single
   largest asm area (§2b #1). Ops: 8-bit widening multiply-add for FIR-style
   half/quarter-pel filter taps, horizontal+vertical two-pass separable
   filtering, saturating narrow/pack back to 8-bit, small fixed-size
   register-resident 4x4/8x8/16x16 block loops. `std::simd` is plausibly
   sufficient for the straightforward separable-filter taps; the tightest
   AVX2/NEON kernels (register blocking across many block sizes, shuffle-
   heavy transpose-free vertical passes) will likely still beat portable
   SIMD and probably need intrinsics.
2. **H.264/HEVC in-loop deblocking filter** — heavily branchy (per-edge
   strength decisions) combined with saturating arithmetic; branch-heavy
   code doesn't vectorize cleanly with `std::simd` alone — needs
   select/blend (masked lanes) idioms; intrinsics likely needed for
   efficient per-lane conditional strength selection.
3. **H.264/HEVC IDCT (4x4/8x8/16x16/32x32)** — butterfly-structured integer
   transforms: multiply-add + shift + transpose (often via shuffle/permute)
   passes. Well-suited to `std::simd` for the arithmetic; the
   transpose/permute steps are where hand-tuned shuffle sequences
   (intrinsics) typically win over portable code.
4. **HEVC/VVC SAO and larger interpolation filters (epel/qpel, up to 8-tap)**
   — same shape as #1 but wider taps and more block-size variants (the
   6,000+ line single-file AArch64 qpel/epel kernels in §2c reflect this
   combinatorial size); gather-like clamped-edge reads at boundaries.
5. **VP9/AV1-style transform (itxfm), including high-bit-depth (10/12-bit)
   variants** — widening multiply-add with 16-bit intermediate precision,
   heavy shuffle/permute for the transform's butterfly network, saturating
   pack down to output bit depth. High-bitdepth doubles register pressure
   and is where AVX-512/SVE lane width genuinely pays off over 128-bit SIMD.
6. **VP9/VP8 loop filter** — like #2, branchy edge-strength logic;
   masked-lane arithmetic.
7. **Motion estimation / SAD-SATD (me_cmp)** — encoder-side, dominant in
   x264-class encoding; ops are wide absolute-difference + horizontal sum
   reduction (Hadamard transform for SATD). `std::simd` horizontal-reduce
   is adequate for SAD; SATD's Hadamard butterfly benefits from
   shuffle/permute intrinsics.
8. **CABAC/entropy coding (H.264/HEVC)** — inherently serial bit-at-a-time
   state machine; this is *not* SIMD-vectorizable in any of FFmpeg's own
   code (no CABAC asm exists in the arch dirs) and is usually the actual
   decode bottleneck once MC/IDCT/deblock are vectorized — a Rust decoder's
   entropy coder needs to win on scalar-code quality (branch prediction,
   table layout, avoiding memory stalls), not SIMD.
9. **swscale color-space/pixel-format conversion (yuv2rgb family)** —
   integer matrix multiply-add per pixel (3x3-ish color matrix), pack/
   widen between 8/10/16-bit and interleaved/planar layouts. Very
   regular, gather-free, saturating-pack-heavy: near-ideal for
   `std::simd`, likely one of the best "start here" areas for a Rust SIMD
   effort with high effort/reward.
10. **swscale bicubic/bilinear rescaling (hscale/vscale)** — per-output-
    pixel weighted-tap gather from source row(s); horizontal pass involves
    variable/precomputed-offset gathers (or precomputed coefficient tables
    walked linearly, as FFmpeg does to avoid true gather instructions) plus
    widening multiply-add and rounding/pack. `std::simd` is workable for
    the multiply-add/pack; avoiding true hardware gather (expensive on many
    x86 generations) via precomputed-coefficient-table tricks, as FFmpeg
    does, matters more than instruction-set choice here.
11. **Audio resampling (swresample)** — per-output-sample FIR convolution
    against a windowed-sinc coefficient table (essentially 1D gather-free
    multiply-add over a sliding window) plus channel rematrixing (small
    fixed matrix multiply-add across channel counts). `std::simd` should be
    sufficient; this is a comparatively low-effort, high-value area.
12. **AAC SBR (spectral band replication) and PS (parametric stereo)** —
    heavier floating-point DSP (envelope adjustment, QMF filterbank);
    FFT/MDCT-adjacent (`av_tx`, §2b #10) multiply-add and butterfly/
    transpose patterns similar to #3/#5 but in float rather than integer.
13. **FFT/MDCT (av_tx, shared by many audio codecs including AAC, Vorbis,
    Opus's internal transforms via CELT-family math, MP3)** — classic
    butterfly network: complex multiply-add + permute/transpose per stage.
    `std::simd` handles straight-line stages; real-world FFT libraries
    universally hand-tune the permute/twiddle stages, so expect
    intrinsics (or vetted external crates) to matter here more than most
    other areas.
14. **H.264/HEVC intra prediction** — per-block directional prediction
    (mostly integer averaging/interpolation along a fixed small set of
    angles); more branchy (mode-dependent code paths) than MC but each
    individual mode's math is simple enough for `std::simd`; the win from
    intrinsics is mainly in avoiding recomputation/table lookups across
    modes rather than exotic instructions.
15. **Deinterlacing (bwdif/yadif) and simple pixel filters (blur,
    denoise)** — separable-kernel convolution, similar shape to #1/#9;
    among the easiest wins for `std::simd` since there's no branchy
    boundary logic beyond simple edge clamping, and FFmpeg's own asm here
    (§2d) is comparatively small/simple relative to codec DSP, meaning a
    Rust implementation could plausibly *exceed* FFmpeg's per-filter
    performance by spending more SIMD-width (AVX-512/SVE) effort than
    FFmpeg's filters currently do (much of libavfilter's x86 SIMD predates
    AVX-512 being common).

**General std::simd-vs-intrinsics guidance distilled from the above:**
`std::simd` (portable SIMD) is plausibly sufficient for straight-line
multiply-add/widen/pack/reduce kernels with regular, fixed-stride memory
access and no data-dependent branching — swscale color conversion,
resampling, simple separable filters, most of the arithmetic *within* a
transform stage. Hand-written intrinsics (or, rarely, actual asm) remain
necessary where: (a) branchy per-lane logic needs explicit masked
select/blend not yet well-served by portable SIMD ergonomics (deblocking,
loop filters); (b) transpose/permute/shuffle patterns are irregular and
architecture-specific enough that portable abstractions can't express the
optimal instruction sequence (transform butterfly stages, FFT twiddle
stages); (c) the target wants to exploit an ISA-specific instruction with
no portable analogue (AVX-512 VNNI-style widening dot-product,
SVE-specific predicated loops, ARM dotprod/i8mm); or (d) truly
serial/branchy scalar code (CABAC) where SIMD doesn't apply at all and the
win is in scalar micro-optimization instead.

## 7. PGO / LTO / BOLT

- **LTO**: `configure` has an explicit `--enable-lto[=arg]` flag
  (`configure` line ~437) that maps to compiler-appropriate flags
  (`-flto*` for GCC/Clang, `-ipo` for ICC) applied to both compile and
  link steps; it also disables `symver_asm_label` when active (a
  known LTO/symbol-versioning interaction). **This is opt-in, not
  upstream's default/release build configuration** — official/distro
  FFmpeg builds are not built with LTO by default.
- **PGO**: no `configure` flags, build-system targets, or documentation
  references to profile-generate/profile-use workflows were found
  anywhere in the tree (`configure`, `doc/*.texi`). FFmpeg does not use
  PGO upstream.
- **BOLT**: no references found anywhere in the repository. FFmpeg does
  not use BOLT upstream.
- **Takeaway**: FFmpeg's real-world performance comes almost entirely from
  the hand-written-asm/intrinsics DSP layer (§1–§2) and algorithmic
  structure, not from whole-program compiler optimization passes; LTO is
  available for downstream builders who opt in but isn't load-bearing for
  FFmpeg's reputation for speed. A Rust project should treat PGO/BOLT as
  a real *additional* opportunity FFmpeg leaves on the table (worth
  evaluating for a competitor project, especially around the
  branch-heavy scalar code — CABAC, deblocking decisions — that SIMD
  can't reach) rather than something it needs to merely match.
```

---

Summary (10 lines):
- Analyzed `~/repos/FFmpeg` @ 564f92cce2 structurally (file counts/line counts/API names only, no code reproduced).
- DSP dispatch = per-arch fn-pointer-struct contexts, resolved once via compile-time arch selection + runtime CPU-flag cascade (EXTERNAL_* macros), verified by checkasm's asm-vs-C fuzz+bench harness.
- Assembly volume: ~190K lines of real asm across x86/arm/aarch64/riscv/loongarch/ppc; x86 (~55.9K) and aarch64 (~39.8K) dominate; h264 (39.6K), vp9 (32.8K), hevc/vvc (22.3K), swscale (16.3K) are the top four DSP areas; AV1 has zero in-tree asm (delegated to libdav1d/libaom externally).
- Produced a ranked ~60-area table plus the 15 single largest asm files (AArch64 HEVC/VVC qpel/epel kernels top the list at 5-6K lines each).
- Threading: three orthogonal axes (frame/slice/filter-slice) selected via codec capability flags, MAX_AUTO_THREADS=16 ceiling, plus a newer DAG+bounded-queue CLI scheduler (`ffmpeg_sched.c`) parallelizing whole pipelines.
- Memory: lock-free refcounted AVBufferPool, STRIDE_ALIGN tied to build-time SIMD width (16/32/64B), 64B bitstream padding and 16px picture-edge padding to avoid bounds checks in hot kernels, hwcontext-based zero-copy for GPU frames.
- Benchmarking: checkasm `--bench` (per-function cycles vs nop baseline), FATE (correctness, informal timing), CLI `-benchmark`; proposed 9 concrete reproducible benchmark scenarios for the Rust competitor.
- Identified 15 hottest real-world paths with concrete Rust-SIMD operation shapes (widening MAC, saturating pack, shuffle/transpose, masked select) and where std::simd suffices vs. needs intrinsics; CABAC flagged as inherently non-SIMD.
- Confirmed FFmpeg uses no PGO/BOLT upstream and only opt-in LTO — a real headroom opportunity for a competing Rust implementation.
- Full detailed content is above; please write it to `/Users/matthew/projects/vaco/planning/research/08-performance-simd.md` since I have no file-write access in this read-only session.