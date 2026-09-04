# Performance baseline, measured 2026-09-01

A single measurement pass across the whole CLI surface, done to give an optimisation-planning
agent everything it needs **without re-measuring anything**. This document collects data; it does
not propose or implement fixes beyond the ranked, unimplemented candidate list in §7.

**Scope note (D20, recorded during this pass — see `planning/00-decisions.md`):** the owner has
ruled that architectural changes are in scope for whatever plan follows this report — rewriting a
crate's public API, replacing a data representation wholesale, collapsing or splitting crates,
abandoning an abstraction. Byte-exactness against ffmpeg and `#![forbid(unsafe_code)]` are the two
things that do not move. §7 labels every candidate **local** or **architectural** accordingly.

## Machine, toolchain, and load

| | |
|---|---|
| CPU | Apple silicon, 10 cores (4P + 6E), `arm64`, `aarch64-apple-darwin` |
| Memory | 16 GiB |
| OS | Darwin 25.6.0 |
| Toolchain | `rustc 1.97.1` (8bab26f4f, 2026-07-14) |
| ffmpeg | 9.0.1, Homebrew build, `libx264`/`libx265`/`libopus`/`libmp3lame`/`dav1d`/`vmaf` enabled |
| samply | 0.13.1 |
| Build | `cargo build --profile dist` (release codegen: `opt-level=3`, `lto="fat"`, `codegen-units=1`; `strip="none"`, `debug="line-tables-only"`), private `--target-dir` under this session's scratchpad, never the shared `target/` |
| Clock | `time.perf_counter()` (wall clock) for all timing; `/usr/bin/time -l` for CPU% and peak RSS. No cycle counter is available — `unsafe` is forbidden workspace-wide and a cycle-counter read needs it. |

**Load was not quiet and moved substantially during this session** (other agents' builds/fuzzing on
the same machine, per the brief): load average (1-min) ranged from **~4** at the start of the
workload-matrix run, to **~15–23** by the time the later scaling runs (bpyramid, 1080p/SD
single-fixture spot checks) executed. Every measurement block below states the load average at the
time it was taken. Where a block was measured under heavy load, its **absolute** wall-clock numbers
are flagged as noisy and the **same-session ratio against ffmpeg** (run interleaved, same load) is
the number to trust — consistent with this project's own recorded rule
(`planning/AGENT-CONSTRAINTS.md` "Under background load, measure cycles and interleave; wall clock
is noise" and `planning/E2E-GAPS.md` §15/§18's repeated cross-session caution).

## Fixtures

No fixture corpus existed at `.../scratchpad/e2e/` at the start of this pass (checked across every
concurrent session's scratchpad directory; none had one either) — all fixtures below were generated
fresh with `ffmpeg` from `lavfi` sources (`testsrc2`, `sine`) and are **not** the project's
historical `uhd.mp4`/`big.mkv` fixtures, so absolute numbers in this report are not directly
comparable to `E2E-GAPS.md`'s historical tables; only structural findings (profile shares, scaling
shape, byte-exactness) are compared. Generated under
`/private/tmp/claude-501/.../fd623546.../scratchpad/e2e/`:

| file | codec | size | notes |
|---|---|---|---|
| `h264_sd.mp4` | H.264, libx264 default (High profile, CABAC, B-frames) | 640x480, 125 frames (5s@25fps) | |
| `h264_720p.mp4` | same | 1280x720, 125 frames | |
| `h264_1080p.mp4` | same | 1920x1080, 125 frames | |
| `h264_4k.mp4` | same | 3840x2160, 75 frames (3s@25fps) | 1 I / 20 P / 54 B (checked with `ffprobe`) |
| `hevc_sd.mp4`…`hevc_4k.mp4` | H.265, libx265 default | same four sizes | 4K: 1 I / 17 P / 57 B |
| `bpyramid_1080p.mp4` | H.264, libx264 default | 1920x1080, 375 frames (15s@25fps) | 2 I / 176 P / 197 B — heaviest B-content fixture |
| `big.mkv` | H.264, libx264 `veryfast` | 1920x1080, 1500 frames (60s@25fps) | remux fixture |
| `audio_aac.m4a` | AAC-LC, 128kbps | 30s sine 440Hz | |
| `audio_mp3.mp3` | MP3, libmp3lame 128kbps | 30s sine 440Hz | |
| `audio_flac.flac` | FLAC | 30s sine 440Hz | **unreachable from the CLI, see §1** |
| `audio_opus.opus` | Opus, libopus 128kbps | 30s sine 440Hz | **no decoder registered at all, see §1** |

Note the fixtures used here have B-frames at every resolution (libx264/libx265's own defaults),
unlike the project's historical `uhd.mp4` (1 I + 74 P, no B). This is deliberate — it makes the
scaling section (§4) exercise picture-level parallelism the all-P fixture cannot — but means my
absolute decode times are not apples-to-apples with `E2E-GAPS.md`'s all-P numbers.

All measurement scripts are committed alongside this report under `scripts/perf-baseline-*`:
`perf-baseline-bench.py` (interleaved matrix harness, checks every subprocess's exit status),
`perf-baseline-gen-spec.py` (workload matrix definition — reads `VACO_BIN`/`VACO_PROBE_BIN`/
`E2E_DIR` env vars, see its own docstring), `perf-baseline-scaling.py` (`/usr/bin/time -l` at N
threads, reads `VACO_BIN`), `perf-baseline-symbolicate.py` (samply → llvm-symbolizer →
per-function self time by outermost physically-emitted frame), `perf-baseline-profile-run.sh`
(glue for the last one, reads `SCRATCH`/`VACO_BIN`/`VACO_DSYM`). The raw JSON results and `.dSYM`
this pass produced live only in this session's scratchpad (ephemeral, not committed, per the "no
multi-GB raw dumps" constraint and because every number that matters is already transcribed into
this document) — re-running the scripts against a fresh `dist` build and fixture set reproduces
them.

---

## 1. Workload matrix

Interleaved A/B (see harness notes below), medians of 6 rounds unless noted, **load average ~4–8**
for every row in this table except where flagged. Every `ffmpeg` invocation used `-y` and a real
output target (`-f null -` for decode-only, a real container for transcodes) — never a bare pipe
with `2>/dev/null`, which would silently swallow both the overwrite prompt and (for the scaling
harness) `/usr/bin/time`'s own output, per this project's own recorded harness traps. Every
subprocess's exit code was checked; no row below is a "measurement" of a process that exited
nonzero.

| workload | vaco `-threads 1` | `ffmpeg -threads 1` | ratio | vaco default (min(cores,4)=4) | ffmpeg default | ratio |
|---|---:|---:|---:|---:|---:|---:|
| H.264 decode, SD 640x480 | 0.508s | 0.071s | **7.1x** | 0.156s | 0.034s | **4.6x** |
| H.264 decode, 720p | 1.541s | 0.169s | **9.1x** | 0.466s | 0.053s | **8.8x** |
| H.264 decode, 1080p | 3.409s | 0.336s | **10.2x** | 1.062s | 0.087s | **12.2x** |
| H.264 decode, 4K | 8.406s | 0.754s | **11.2x** | 3.032s | 0.195s | **15.5x** |
| HEVC decode, SD 640x480 | 1.401s\* | 0.159s\* | **8.8x** | 1.776s\* | 0.080s\* | **22.1x**\* |
| HEVC decode, 720p | 1.291s\* | 0.181s | **7.1x** | 1.283s\* | 0.084s | **15.2x**\* |
| HEVC decode, 1080p | 2.917s | 0.414s | **7.1x** | 2.913s | 0.190s | **15.3x** |
| HEVC decode, 4K | 6.586s | 0.856s | **7.7x** | 6.577s | 0.248s | **26.5x** |
| decode+scale 2160p→1080p (H.264) | 9.736s | 0.797s | **12.2x** | 3.459s | 0.245s | **14.1x** |
| transcode H.264→FFV1, 1080p | 6.824s | 0.402s | **17.0x** | 4.193s | 0.306s | **13.7x** |
| remux mkv→mp4 copy, 60s 1080p | 0.014s | 0.027s | **0.54x (vaco faster)** | — | — | — |
| audio decode AAC, 30s | 7.664s | 0.035s | **217x** | (no threading in this path) | 0.035s | **217x** |
| audio decode MP3, 30s | 0.242s | 0.037s | **6.5x** | — | 0.040s | **6.1x** |
| audio decode FLAC | **unreachable — see below** | — | — | — | — | — |
| audio decode Opus | **unreachable — no decoder registered, see below** | — | — | — | — | — |
| probe, H.264 1080p | 0.0045s | 0.023s | **0.20x (vaco faster)** | | | |
| probe, H.264 4K | 0.0055s | 0.040s | **0.14x (vaco faster)** | | | |
| probe, remux fixture (mkv, 1500 frames) | 0.0058s | 0.026s | **0.22x (vaco faster)** | | | |

\* HEVC SD/720p rows were caught mid-run by a load spike from concurrent agents (load average
climbed from ~4 to ~15–23 during this block; per-round range for HEVC SD was 0.43s–4.79s at
`-threads 1`, i.e. an 11x spread within one interleaved job). The **medians** above are reported as
required, but treat the SD/720p HEVC absolute numbers as lower-confidence than the rest of the
table; the 1080p/4K HEVC rows, measured earlier in the same run before the spike, are solid (range
within ±3% of median).

**Headlines**

- **Video decode is 7–12x behind serial ffmpeg and 8.8–26.5x behind default-threaded ffmpeg**, at
  every resolution tested, for both codecs. The gap **widens** with resolution for H.264
  (7.1x→11.2x from SD to 4K, serial-vs-serial) because ffmpeg's own advantage compounds with size;
  HEVC's serial ratio is flatter (~7-9x across all four sizes) but its default-thread ratio balloons
  to **26.5x at 4K** because — see §4 — **HEVC has no threading in this build at all**, so ffmpeg's
  default-thread win over its own serial mode (3.4x on this fixture) is pure, uncontested gap.
- **Audio decode is the single worst ratio measured, and it is architectural, not incidental**: AAC
  is **217x** behind ffmpeg (single ffmpeg number, since ffmpeg's own AAC decode does not thread and
  its default/`-threads 1` times were identical to two decimal places). §2 and §7 trace this to a
  specific, named, already-documented cause: the production AAC decode path calls an **O(n²)
  reference-quality IMDCT** that a fast O(n log n) implementation already exists in-tree to replace.
- **FLAC and Opus audio decode are unreachable through the CLI today, for two different reasons**,
  neither of them "the decoder is slow":
  - **FLAC**: a bare `.flac` file is misdetected by the demuxer prober as `cdg` (CD+Graphics) —
    `vaco -i audio_flac.flac` opens it as a 300x216 CDG video stream, and `-map 0:a:0` then fails
    with "Stream map matches no streams". Forcing `-f flac` fails with "Function not implemented" —
    **there is no registered FLAC demuxer** (`vaco-registry`'s generated `DEMUXERS` table has no
    `flac` entry at all, only a FLAC *muxer* and a FLAC *codec* decoder/encoder). Muxing FLAC into a
    container the demuxer registry does understand (Matroska) gets past the open, but then
    `Error while filtering: progress limit exceeded: requested 65, cap 65` — reproduced on both the
    30s fixture and on a fresh 3s clip, so this is not proportional to duration; it looks like a
    fixed, too-low internal progress/frame-count estimate hit almost immediately, not a real decode
    failure. Root cause not investigated further — out of scope for a measurement pass — but this is
    a correctness/integration gap, not a missing implementation: `vaco-codec-flac::DECODER_FLAC` and
    `ENCODER_FLAC` both exist and are registered.
  - **Opus**: `vaco-codec-opus` is a **complete, from-scratch, documented Opus decoder** (RFC 6716 +
    8251: range coder, CELT, SILK, hybrid mode — see `docs/codec/vaco-codec-opus.md`) that sits in
    `crates/codec/vaco-codec-opus/` with its own `src/`, `tests/`, and `Cargo.toml` — but
    **`vaco-registry`'s `Cargo.toml` never depends on it, under any feature name**. There is no
    `codec-opus` feature, optional dependency, or `DECODERS`/`ENCODERS` entry anywhere in
    `crates/registry/vaco-registry/`. Every registered Opus-adjacent thing is `vaco-parse-opus` (the
    bitstream/TOC parser, feature `parse-opus`, in the default feature set) — packet framing exists,
    decode does not reach the CLI. This is exactly the shape `E2E-GAPS.md`'s own thesis names
    ("None of these are missing codecs... every one is integration glue") — except here the crate
    genuinely is a full decoder, not glue, and it is still unreachable. **Not a performance finding,
    but directly relevant to any plan that treats "Opus decode ratio" as a gap to close by
    optimising code that a user's command line cannot reach today.**
- **Stream copy (remux) and probe are already faster than ffmpeg** (0.54x and 0.14–0.22x
  respectively) — consistent with `E2E-GAPS.md` §9's finding that I/O, demux and mux layers are
  sound and the gap is specifically in codec inner loops. This report's remux number differs from
  `E2E-GAPS.md` §9's "parity, not a win" conclusion; that conclusion was reached on a different
  fixture in a different session and this report's own rule (ratios over cross-session absolutes)
  says not to read that as a regression or an improvement — it is a new, single-session data point
  in vaco's favour, worth re-checking rather than trusting outright given the earlier finding that a
  too-short clip made stream copy look artificially fast once already.

### Harness notes specific to this run

- `ffmpeg`'s AAC path showed **no measurable threading effect** (`-threads 1` and default came back
  identical to 2 decimal places across 6 rounds) — expected, since AAC-LC decode is inherently
  serial per-frame in ffmpeg too; the 217x ratio is not "vaco failed to thread audio", it is a flat
  per-sample cost difference.
- vaco's own audio decode path showed no `-threads N` effect either (there is no audio decoder
  threading in this codebase at all, checked against `-h`'s option description, which documents
  `-threads` as "decoding threads per codec" with no per-stream-type carve-out, but the audio
  decoders never consult it).

---

## 2. Profiles for the top workloads

**Method** (the mandatory recipe, followed exactly): built with `cargo build --profile dist`
(release codegen, symbols kept), ran `dsymutil` on the resulting binary to produce a `.dSYM`,
recorded with `samply record --save-only` (no `--unstable-presymbolicate` — it was confirmed
useless here too: a spot check left every leaf frame as a bare hex string in the raw profile JSON),
then for every **leaf** sample whose frame resolved to the `vaco` binary, fed
`<module-relative offset> + 0x100000000` (the binary's own Mach-O `__TEXT` `vmaddr`, confirmed via
`otool -l` before trusting it, matching the `0x100000000` this project's prior rounds also found on
arm64) to `llvm-symbolizer --obj=<dSYM> --inlines -f -C -p`, and aggregated self time by each
chain's **last-printed line** — llvm-symbolizer's `--inlines` output lists the innermost (deepest)
inlined frame first and the outermost, physically-emitted (i.e. actually-compiled, symbol-table)
frame last, so the last line is always the function whose own compiled body contains the sampled
instruction, regardless of how many logical calls were inlined into it. This is the same convention
`E2E-GAPS.md` §18/19 and `docs/core/simd-adoption-measurements.md` Group 10 used ("aggregating self
time by each chain's outermost physically-emitted frame"). The script (`symbolicate.py`) is included
in the scratchpad for reproduction.

**Resolved-sample fraction per profile** (reported as required):

| profile | total samples | leaf-in-`vaco` | of those, resolved to a real name (not `??`) |
|---|---:|---:|---:|
| H.264 4K decode, `-threads 1` | 37,048 | 35,152 (94.9%) | 2,865 / 2,871 unique addresses (99.8%) |
| HEVC 4K decode, `-threads 1` | 28,555 | 24,992 (87.5%) | 2,247 / 2,257 unique addresses (99.6%) |
| AAC decode, 30s clip | 30,995 | 5,868 (18.9%) | 201 / 210 unique addresses (95.7%) |

AAC's low leaf-in-`vaco` fraction is not a symbolication failure — it is the finding (§2.3): most
samples are leaves inside a system library, not inside vaco's own code, and that is exactly what the
profile is supposed to show.

### 2.1 H.264, 4K, `-threads 1` (fixture: `h264_4k.mp4`, decode-only, `-c:v rawvideo -f null -`)

Top functions by self time (outermost physically-emitted frame), of 35,152 in-library samples:

| self time | function |
|---:|---|
| 21.15% | `reconstruct::sample_luma_block` |
| 12.74% | `reconstruct::reconstruct_mb` |
| 10.73% | `<PictureReconstructor>::deblock_row` |
| 10.41% | `deblock::boundary_strength` |
| 9.04% | `reconstruct::sample_chroma_2x2` |
| 7.90% | `reconstruct::reconstruct_inter_mb::{closure#0}` |
| 4.36% | `cabac_residual::residual_block_cabac` |
| 3.84% | `mb::decode_slice_cabac` |
| 3.16% | `<DeblockCtx>::chroma_mb_row` |
| 3.03% | `frame_task::build_frame` |
| 2.77% | `vaco_codec_dsp_deblock::batch::filter_luma_edge` |
| 2.53% | `vaco_codec_dsp_idct::h264::idct4x4` |
| 2.19% | `<H264Decoder as Decoder>::send_packet` |
| 0.86% | `<H264FrameTask as FrameTask>::run` |
| 0.86% | `mb::decide` |
| 0.63% | `vaco_codec_dsp_deblock::batch::filter_chroma_edge` |
| 0.58% | `mb::decode_macroblock_cabac` |
| 0.42% | `<Budget>::alloc::<MvInfo>` |
| 0.41% | `mb::apply_direct_quadrant` |
| 0.40% | `core::ptr::drop_glue::<MbResidual>` |

Reads consistently with `E2E-GAPS.md`'s own last full-decoder profile (§19, pre-row-threading):
`sample_luma_block` and `boundary_strength` are still the two largest single named costs, and
`reconstruct_picture` has since been split into `reconstruct_mb` + `<PictureReconstructor>::
deblock_row` by the row-level threading work landed since (§21/22 in `E2E-GAPS.md`) — the
12.74%+10.73% combined (23.5%) lines up with §19's single `reconstruct_picture` figure of 23.01%
almost exactly, which is the expected shape when a function is split into two call sites doing the
same total work. `boundary_strength` at 10.41% is *higher* than §19's post-fix 11.26% figure to
within noise — consistent with no further work having landed on it since. `sample_chroma_2x2`
appearing as its own 9.04% line (it did not exist as a named function in §19's profile) is new since
the row-threading work restructured chroma sampling into 2x2 groups (§21's "Stage 2" in
`E2E-GAPS.md`) — this profile is the first to show its cost named on its own.

### 2.2 HEVC, 4K, `-threads 1` (fixture: `hevc_4k.mp4`, decode-only) — first profile ever taken of this decoder

| self time | function |
|---:|---|
| 26.76% | `mc::predict_block_intermediate` |
| 10.43% | `ctu::build_cu_prediction` |
| 9.32% | `ctu::write_inter_cu_no_residual` |
| 8.08% | `<sao::Snapshot>::capture` |
| 6.83% | `residual::residual_coding` |
| 6.22% | `mc::predict_block` |
| 5.35% | `sao::offset_block` |
| 5.11% | `<HevcDecoder>::emit_pocs` |
| 2.56% | `<CuGrid>::inter_at` |
| 2.38% | `deblock::filter_luma_group` |
| 2.15% | `ctu::predict_component::<...>::{closure#2}` |
| 1.86% | `deblock::boundary_strength` |
| 1.41% | `<CuGrid>::fill_motion` |
| 1.35% | `ctu::coding_unit` |
| 0.71% | `deblock::filter_picture` |
| 0.68% | `residual::decide_at` |
| 0.63% | `intra_pred::predict` |

**`predict_block_intermediate` at 26.76% is the single largest cost in either codec's profile in
this report.** This is HEVC's motion-compensation interpolation path (the intermediate,
higher-precision buffer clause 8.5.4.2.2's luma/chroma interpolation filters write into before
rounding) — structurally the same kind of cost as H.264's `sample_luma_block`, but almost 30% larger
a share, and (§4) **completely unparallelised**: HEVC has no frame- or row-threading in this
codebase at all, so this entire cost sits on one core regardless of `-threads`.

`<sao::Snapshot>::capture` at 8.08% is notable: SAO (sample adaptive offset, HEVC's equivalent of a
second, orthogonal in-loop filter beyond deblocking) apparently takes a snapshot of picture state
before applying its offsets — worth checking whether that snapshot is a full-picture copy that could
be narrowed to the region SAO actually touches, the same shape as several of the wins already
recorded for H.264 (§7 candidate list).

### 2.3 AAC decode, 30s clip — where 81% of the time actually goes

The `-lib` breakdown (leaf frame's resource/library, not filtered to `vaco`):

| library | leaf samples | share |
|---|---:|---:|
| `libsystem_m.dylib` (system libm) | 24,892 | **80.3%** |
| `vaco` | 5,868 | 18.9% |
| `libsystem_malloc.dylib` | 133 | 0.4% |
| everything else | 102 | 0.3% |

`llvm-symbolizer` cannot resolve `libsystem_m.dylib` addresses (it is a dyld-shared-cache-only
library with no on-disk file for `nm`/`dsymutil` to read), so this profile was cross-checked with
macOS's own `sample` tool, which *can* symbolicate the shared cache live. Its top-of-stack ("what
was actually executing") breakdown over a 3-second sampling window during the same decode:

| leaf | samples | as % of this window |
|---|---:|---:|
| `cos` (in `libsystem_m.dylib`) | 586 | 34.6% |
| 12 more distinct unresolved `libsystem_m.dylib` addresses | 476 combined | 28.2% (almost certainly more `cos`/`sin`/trig-family calls at other call sites, given the caller) |
| `vaco_tx::reference::imp::imdct` (in `vaco`) | 148 | 8.8% |
| `DYLD-STUB$$cos` (in `vaco`) | 127 | 7.5% |

`cos`'s direct caller, every time, is `vaco_tx::reference::imp::imdct`. Reading the source
(`crates/signal/vaco-tx/src/reference.rs`) confirms exactly what the name promises:

```rust
/// Inverse MDCT: `n/2` coefficients to all `n` samples.
pub fn imdct(coeffs: &[f64]) -> Vec<f64> {
    let half = coeffs.len();
    let n = half * 2;
    (0..n).map(|j| (0..half).map(|k| {
        let a = TAU / n as f64 * (j as f64 + 0.5 + n as f64 / 4.0) * (k as f64 + 0.5);
        coeffs[k] * a.cos()
    }).sum()).collect()
}
```

This is an **O(n²)** direct evaluation of the IMDCT's defining sum, calling `f64::cos` once per
`(j, k)` pair — for AAC's 1024-sample long blocks that is up to 524,288 `cos()` calls per channel
per frame. The module's own doc comment says plainly: *"Verification only. Nothing in the crate's
fast paths calls this module"* — but `crates/codec/vaco-codec-aac/src/reconstruct.rs:23` does
`use vaco_tx::reference::imdct;` and calls it from the real decode path, not a test. This is not an
oversight: `crates/codec/vaco-codec-aac/src/qmf.rs`'s module doc names it explicitly as a **deliberate,
documented tradeoff** — *"matching this workspace's established correctness-first
reference-implementation convention... `vaco_tx::reference::imdct` takes the identical approach and
is used in this crate's own production decode path, not just as a test oracle"* — made to get
correctness landed first, with the fast path deferred. This profile is the first measurement of what
that tradeoff costs: **on the order of 80% of AAC decode's wall time**, and the 217x ratio in §1 is
substantially this one call site. See §7 for the candidate this converts into.

---

## 3. Where the time goes, structurally

Derived from §2's profiles plus the demux/mux timing in §1 (probe and remux rows), not from a
separate instrumentation pass — this project's own `E2E-GAPS.md` history (§9, §14, §18) already
established the technique of isolating a phase by A/B'ing a pipeline with and without it
(decode-only vs decode+scale, etc.), and this section reuses those isolations rather than
re-deriving them.

**H.264, 4K, single-threaded** (from §2.1's profile, grouped by phase):

| phase | share | functions rolled up |
|---:|---|---|
| entropy decode (CABAC) | ~8.2% | `residual_block_cabac` (4.36%) + `decode_slice_cabac` (3.84%) |
| reconstruction (motion comp, intra, IDCT) | ~44.9% | `sample_luma_block` (21.15%) + `reconstruct_mb` (12.74%) + `sample_chroma_2x2` (9.04%) + `idct4x4` (2.53%)|
| in-loop filtering (deblocking) | ~27.5% | `deblock_row` (10.73%) + `boundary_strength` (10.41%) + `chroma_mb_row` (3.16%) + `filter_luma_edge` (2.77%) + `filter_chroma_edge` (0.63%) |
| inter-prediction closure/bookkeeping | ~7.9% | `reconstruct_inter_mb::{closure#0}` |
| frame/packet glue | ~3.9% | `build_frame` (3.03%) + `send_packet` (2.19%) − overlap |
| everything else | ~2.2% | `mb::decide`, `mb::decode_macroblock_cabac`, allocator, drop glue |

Reconstruction and deblocking together are **~72%** of single-threaded H.264 decode time — this
matches every prior round in `E2E-GAPS.md` (§10/§18/§19) qualitatively, though the internal split
between "reconstruction" and "deblocking" has shifted somewhat as the row-threading refactor changed
which function does what.

**HEVC, 4K, single-threaded** (from §2.2):

| phase | share |
|---:|---|
| motion compensation / inter prediction | ~45.8% (`predict_block_intermediate` 26.76% + `predict_block` 6.22% + `build_cu_prediction` 10.43% + `predict_component` closures ~2.15%) |
| in-loop filtering (deblock + SAO) | ~19.9% (`<Snapshot>::capture` 8.08% + `offset_block` 5.35% + `filter_luma_group` 2.38% + `boundary_strength` 1.86% + `filter_picture` 0.71% + `CuGrid` motion lookups ~1.4%) |
| entropy/residual decode | ~7.5% (`residual_coding` 6.83% + `decide_at` 0.68%) |
| CU-level bookkeeping / non-residual write-back | ~9.3% (`write_inter_cu_no_residual`) |
| frame emission | ~5.1% (`emit_pocs`) |

**Decode+scale, 2160p→1080p** (isolated the same way `E2E-GAPS.md` §14 did — decode-only vs
decode+scale, same clip, interleaved): decode-only median (H.264 4K row, §1) 8.406s at `-threads 1`;
decode+scale median 9.736s — **the scaler's own share is ≈1.33s of 9.736s (≈13.7%)**, the rest is
still decode. This is directionally consistent with `E2E-GAPS.md` §14's own isolation (there: 1.30–
1.97s of 9.88s, ≈15-20%) even though the fixtures differ (mine has B-frames; theirs was all-P), and
with `docs/signal/vaco-scale.md`'s existing, already-profiled breakdown of the scaler's own internals
(bounds-check/iterator scaffolding around a runtime-determined tap-count loop, not the arithmetic) —
**not re-profiled here since a full symbolicated profile of `vaco-scale`'s own hot loop already
exists in that document and nothing in this pass suggests it is stale.**

**Demux/mux/I/O**: the remux row in §1 (vaco faster than ffmpeg) and the probe rows (vaco 4.5–7x
faster than `ffprobe`) are the direct evidence that these layers are not where the gap is, for either
codec. No workload in this matrix showed vaco's demux or mux path as a bottleneck relative to
ffmpeg's equivalent.

---

## 4. Scaling data

`/usr/bin/time -l` at 1/2/4/8/16 threads, interleaved (rotating start thread-count each round),
decode-only. **Load average is called out per block** since two of the four blocks below were
caught by a load spike partway through this session.

### H.264, 4K (`h264_4k.mp4`), load average ~4–5 throughout this block

| threads | wall (median of 3) | speedup | CPU% (median) | peak RSS (median) |
|---:|---:|---:|---:|---:|
| 1 | 9.950s | 1.00x | 95% | 3168 MiB |
| 2 | 5.580s | 1.78x | 213% | 3486 MiB |
| 4 | 3.810s | 2.61x | 396% | 3498 MiB |
| 8 | 2.920s | 3.41x | 613% | 3379 MiB |
| 16 | 2.830s | 3.52x | 627% | 3424 MiB |

Scaling is real and substantial through 8 threads (3.41x), then flattens hard — 16 threads buys
essentially nothing over 8 (3.52x vs 3.41x) while CPU-seconds keep rising, the same "diminishing
past four/eight" shape `E2E-GAPS.md` §21/§22 already documented and the reason the shipped default
is `min(cores, 4)` rather than `cores`. This fixture has B-frames (unlike `E2E-GAPS.md`'s all-P
`uhd.mp4`), so there is genuine picture-level parallelism available beyond the row-level mechanism
alone — CPU% climbing cleanly through 613% at 8 threads (not flatlining at ~129% the way the all-P
fixture did before row-threading landed, per `E2E-GAPS.md` §20) is consistent with the row-threading
+ picture-level combination both contributing on B-content.

### HEVC, 4K (`hevc_4k.mp4`), load average ~4–5 throughout this block

| threads | wall (median of 3) | speedup | CPU% (median) | peak RSS (median) |
|---:|---:|---:|---:|---:|
| 1 | 8.380s | 1.00x | 93% | 312 MiB |
| 2 | 7.470s | 1.12x | 97% | 309 MiB |
| 4 | 6.780s | 1.24x | 99% | 309 MiB |
| 8 | 6.880s | 1.22x | 99% | 309 MiB |
| 16 | 7.100s | 1.18x | 97% | 309 MiB |

**CPU% is flat at ~93–99% regardless of thread count.** This is not noise — it is the direct,
`/usr/bin/time`-measured confirmation that **HEVC decode has no threading implementation at all** in
this codebase: `-threads N` is accepted (it does not error) but does nothing measurable for any
N > 1. The tiny (~1.1-1.24x) wall-clock movement across thread counts is consistent with
run-to-run noise, not real parallelism — contrast with H.264's clean, monotonic, CPU%-confirmed
scaling above using the identical harness on the identical machine in the identical session. RSS is
also flat (~309-312 MiB) — expected for a decoder with nothing extra to buffer across threads.

### H.264, 1080p B-pyramid (`bpyramid_1080p.mp4`, 15s, 197 B / 176 P / 2 I — the heaviest-B fixture in this corpus)

**Load average climbed from ~9 to ~23 during this block** (visible in the data: `-threads 1` CPU%
came back at 44–55%, far below the ~95-99% every other single-threaded measurement in this report
shows on a less-contended machine — a single core genuinely pegged at 100% cannot show 45% overall
CPU utilisation unless something else is displacing it from the core repeatedly). Numbers below are
reported as required, but should be read as directional only, not as a clean scaling curve:

| threads | wall (median of 3) | speedup | CPU% (median) | peak RSS (median) |
|---:|---:|---:|---:|---:|
| 1 | 32.390s | 1.00x | 54%\* | 1009 MiB |
| 2 | 9.530s | 3.40x | 198% | 1060 MiB |
| 4 | 5.410s | 5.99x | 374% | 1100 MiB |
| 8 | 4.240s | 7.64x | 494% | 1189 MiB |
| 16 | 3.720s | 8.71x | 556% | 1262 MiB |

\* Almost certainly an artifact of contention, not the decoder's own serial CPU usage — every other
single-threaded H.264/HEVC measurement in this report (taken at lower load) shows 93-99% at
`-threads 1`. The **speedup ratios**, being wall-clock/wall-clock on the same contended machine, are
less affected than the CPU% column's absolute reading, but should still be treated as an upper bound
on real speedup, not a clean number: with the whole machine under 15-23 load, `-threads 1`'s wall
time is inflated by scheduling delay that higher thread counts partially route around simply by
having more runnable threads competing for the same oversubscribed cores, which would make speedup
look *better* than it is on a quiet machine. **This fixture should be re-measured before being cited
as evidence of anything beyond "more B-content scales further than all-P content," which the earlier
two (quieter) blocks already establish more reliably.**

### Smaller spot checks (1 and 4 threads only, for §5's memory table), load average ~15-20 (same contended window as the bpyramid block)

| fixture | threads | wall | CPU% | peak RSS |
|---|---:|---:|---:|---:|
| `h264_1080p.mp4` | 1 | 11.315s | 51%\* | 937 MiB |
| `h264_1080p.mp4` | 4 | 4.560s | 160%\* | 1093 MiB |
| `h264_sd.mp4` | 1 | 1.910s | 46%\* | 26 MiB |
| `h264_sd.mp4` | 4 | 0.575s | 197%\* | 39 MiB |
| `hevc_1080p.mp4` | 1 | 12.515s | 40%\* | 104 MiB |
| `hevc_1080p.mp4` | 4 | 10.030s | 49%\* | 103 MiB |

\* Same contention caveat as the bpyramid block — these CPU% figures are depressed by machine load,
not by the decoder. HEVC's flat wall-clock (12.5s → 10.0s, nowhere near 4x) at 1 vs 4 threads is
still meaningful despite the noisy CPU% column, and confirms the 4K finding above: **no HEVC
threading, at any resolution.**

---

## 5. Memory

Peak RSS (`/usr/bin/time -l`'s "maximum resident set size", converted to MiB), from §4's runs.

| workload | 1 thread | 4 threads | ratio |
|---|---:|---:|---:|
| H.264 4K | 3168 MiB | 3498 MiB | 1.10x |
| HEVC 4K | 312 MiB | 309 MiB | 0.99x (no threading → no extra buffering) |
| H.264 1080p | 937 MiB | 1093 MiB | 1.17x |
| H.264 SD | 26 MiB | 39 MiB | 1.50x |
| HEVC 1080p | 104 MiB | 103 MiB | 1.00x |
| H.264 B-pyramid 1080p | 1009 MiB | 1100 MiB | 1.09x |

**The disproportionate one is H.264 4K's absolute footprint, not its threading multiplier**: 3.1-3.5
GiB peak RSS to decode a 3-second, 75-frame, 3840x2160 clip is roughly **40-47 MiB per decoded
frame** — for reference, one uncompressed 4K YUV 4:2:0 frame is only ~12 MiB. `E2E-GAPS.md` §20
already found and fixed one specific instance of this shape (`SliceStats::macroblocks`/`MbSummary`
at 59 MiB per 4K picture, previously uncharged), but that fix was about *budget accounting*
(charging memory that was already being allocated), not about *reducing* the allocation — the
underlying ~40+ MiB/frame footprint this report measures is consistent with that same structure
still being what's allocated, now correctly charged rather than free. This report did not re-profile
allocations (no heap profiler was run this pass — `heaptrack`/`instruments` were not used); a
follow-up measurement pass with an allocation profiler is the natural next step and is listed as a
candidate in §7. **HEVC's flat ~309-312 MiB regardless of resolution-adjacent workload is the
contrast case**: an intra-heavier, unthreaded decoder with no per-thread duplication carries a much
smaller, much flatter footprint, which is circumstantial support for "H.264's memory scaling is
threading-buffer-shaped" rather than "decoding 4K inherently needs gigabytes."

---

## 6. What has already been tried

Summarised from `planning/E2E-GAPS.md` and `docs/core/simd-adoption-measurements.md`; every ratio
below is quoted, not paraphrased-into-a-verdict, per this project's own "report ratios, not verdicts"
rule.

### Landed and kept

| change | measured result | source |
|---|---|---|
| Row-wise `copy_from_slice` replacing per-pixel `set_pixel` in reconstruction | ~3.5% | E2E-GAPS §10 |
| Move decoded residual into `MbSummary` instead of deep-cloning per macroblock | ~3% | E2E-GAPS §10 |
| Skip six-tap edge clamping when a 4x4 block is provably in-bounds (luma) | ~3% (noisy) | E2E-GAPS §10 |
| `vaco-scale` `filter_h` specialised to `&[i32; N]` for `N∈{2,4,6,8}` (fixed trip count reaches the optimiser) | mean ≈0.80x (≈1.25x) on the exact 2160p→1080p scenario, 10/10 rounds; up to 0.58x on other conversions | E2E-GAPS §14, SIMD doc Group 9 |
| `boundary_strength` memoised once per 4x4 block-row/col group instead of recomputed per pixel row (4x/2x redundant calls removed) | mean ≈0.786x (≈1.27x) end to end, 10/10 rounds — the single largest recorded win in this project's profiling history | E2E-GAPS §18, SIMD doc Group 10 |
| Masked-select deblocking kernel (`filter_luma_edge`/`filter_chroma_edge`), `vaco-simd` `select_i16`/`select_i32` | isolated: 0.31x/0.41x, 5/5 and 10/10 rounds; **end to end: only ≈0.974x (≈2.6%)** — the filter arithmetic itself was 2.67% of total runtime, not the ~17-26% its caller's aggregate self time suggested | E2E-GAPS §10/§11, SIMD doc Group 8 |
| Frame (picture-level) threading, H.264 | 1.23-1.30x on all-P 4K (ceiling of a 2-stage pipeline whose serial half is ~13%); 1.78-2.00x on B-pyramid content | E2E-GAPS §20 |
| Row-level frame threading, H.264 (3 stages: incremental deblock, banded plane reads, per-row publish/wait) | Stage 1 (restructure only) free to within measurement (median 1.0013x); Stage 2 (banded-plane reads) **2.9% single-threaded speedup as a side effect** (median 0.9713x, 9/10 rounds); combined: 1.28x→**4.05x** at 8 threads on all-P 4K, CPU% 129%→615% | E2E-GAPS §21 |
| Default `-threads` set to `min(available_parallelism, 4)` | 3.43x vs serial, 3.46x vs `ffmpeg -threads 1` on 4K all-P, byte-exact at every N | E2E-GAPS §22 |

### Reverted / negative (at least six distinct instances recorded before this pass; do not re-propose without new evidence)

| attempt | measured result | why it failed | source |
|---|---|---|---|
| `add_pixels_clamped_vector` (hand-written SIMD) | 0.9x/0.84x — a **regression** | lost to plain autovectorisation of the scalar loop; gated to scalar | AGENT-CONSTRAINTS "Performance"/"A benchmark where both paths tie exactly", SIMD doc |
| Batching deblocking's per-pixel reads/writes into contiguous slice ops | wash-to-slight-loss, slower in 6/8 rounds | no drop in `deblock_picture_luma` self time — the batching didn't touch where the time actually was | E2E-GAPS §10 |
| Lazy two-axis "j" derivation in `interp::luma_qpel_sample` | ratio 0.997, won 4/8 — wash | eager computation wasn't the cost | E2E-GAPS §11 |
| Windowed gather in `fetch_pred_4x4` (fetch 9x9 once vs re-reaching per pixel) | ratio 1.0025, won 6/10 — wash | re-fetch cost was already cheap | E2E-GAPS §11 |
| Chroma bilinear in-bounds fast path (mirroring luma's win) | **ratio 1.034 — a 3.4% regression**, won 2/10 | chroma's `.clamp()` is two cheap ops; the guard branch cost more than it saved. The *same transformation* won on luma and lost on chroma — symmetry between two code paths is not a reason to skip measuring the second one | E2E-GAPS §11 |
| Merge Cb/Cr chroma inter-prediction into one pass (share position/weight derivation) | median ratio **1.024 — a 2.4% regression**, lost 9/10 rounds | plausibly increased register pressure / changed inlining vs. two independent single-plane call sites LLVM already handled well; byte-identical output, purely a performance loss | E2E-GAPS §19, SIMD doc Group 11 |

**The pattern across all six**: every one of them *reasoned correctly* about redundant or wasteful
computation — and every one still had to be measured, because "fewer operations" and "faster" are
different claims. Two (deblocking batch, chroma merge) targeted code that *was* provably expensive
in aggregate but not in the specific place the change touched; two (the interpolation washes) touched
code that was already cheap; one (chroma fast path) applied a proven win from a neighbouring,
superficially identical code path where the constants didn't transfer. **The one technique that
reliably worked across this whole history was memoising an already-pure function whose result had
already been computed earlier in the same loop** (`boundary_strength`) — not vectorising arithmetic,
not restructuring loops for cache behaviour, not merging call sites.

### SIMD substrate findings (not attempts at a specific kernel, but load-bearing for any future one)

- `fearless_simd` 0.7.0 (NEON) reconstructs the native instruction from a hand-composed operation in
  every case tested except one: `saturating_add_i16`/`saturating_sub_i16` measure **1.46x**
  (LLVM does not recover `sqadd`/`sqsub` from the widen/narrow composition) — "the one real gap."
- Masked-lane select (`select_i16`/`select_i32`) is genuinely fast in isolation: ≈0.19x and ≈0.43x
  respectively vs scalar branchy code, 5/5 rounds each, essentially zero variance — but (see the
  deblocking kernel row above) isolation speed does not predict end-to-end share.
- **Two authoring rules with measured teeth**: batch 2-4 vectors per loop iteration or LLVM will not
  unroll a `chunks_exact` loop (worth up to 4x on some shapes, invisible to every correctness test);
  never carry a single loop-accumulator (worth ~4x on a reduction, same invisibility).
- No `i16→u8` saturating narrow exists in the substrate (`packuswb`/`sqxtun` equivalent) — composed
  as `max(0)` + bitcast + narrow, two extra ops on the last step of nearly every pixel kernel.
- **Unverified on x86-64 entirely** — every number above is NEON/aarch64 only. This is the standing,
  explicitly-named outstanding item from the original SIMD adoption pass and nothing in this session
  closes it.

---

## 7. Candidate opportunities, ranked

Every candidate is labelled **evidence** (what the profile/measurement in this report actually
shows) separately from **hypothesis** (what I think would happen, unverified), and **local** (fits
the current design) vs **architectural** (changes a representation, API, or crate boundary — sized
roughly, per the owner's D20 ruling that such changes are in scope). None of these were implemented
or benchmarked as changes — they are ranked by (estimated ceiling from the profile) × (confidence),
not attempted.

### 1. Wire AAC's IMDCT to `vaco-tx`'s fast `Plan`-based transform instead of the O(n²) reference

- **Evidence**: §2.3 — 80.3% of AAC decode's sampled leaf time is in `libsystem_m.dylib`, and the
  direct caller is `vaco_tx::reference::imp::imdct`, an explicitly-documented "verification only"
  O(n²) transform that the AAC decoder uses in production
  (`crates/codec/vaco-codec-aac/src/reconstruct.rs:23`). `vaco-tx` already has a `Plan`/`TxKind::Mdct`
  fast path (`crates/signal/vaco-tx/src/plan.rs`) with a documented `FULL_IMDCT` flag that emits all
  `n` samples from `n/2` coefficients — exactly AAC's shape — and the reference module's own doc
  comment frames itself as the oracle *for* that fast path ("if one of these disagrees with a
  `crate::Plan`, the plan is wrong").
- **Ceiling**: if the fast path removes the O(n²)/libm cost entirely, this is bounded by the 80.3%
  figure directly — **up to ~5x on AAC decode alone** (removing 80% of the time leaves 20%, a 5x
  speedup on this specific workload), which would take the 217x ratio in §1 down to roughly **43x**
  — still far from parity, but the largest single-workload win available anywhere in this report by
  a wide margin. (`libsystem_m.dylib` samples not attributable to `cos` specifically were not
  individually resolved — see §2.3 — so this ceiling assumes most of that 80.3% is transform-related,
  which the call-site evidence supports but does not prove to the byte.)
- **Risk / cost**: **local** — this is a call-site swap plus correctness verification, not a new
  design. The real cost is verifying `Plan`'s `Mdct`/`FULL_IMDCT` output matches AAC's exact
  windowing/scaling convention bit-for-bit (or within the tolerance AAC decode already accepts,
  since — per this report's own byte-exactness check in §1 — vaco's AAC output is *not* currently
  byte-identical to ffmpeg's anyway, unlike the video codecs). The QMF filterbank in `qmf.rs` uses
  the same reference-first convention for SBR but at far smaller N (32x64/64x128 vs up to 1024x2)
  — not worth chasing until the main IMDCT is fixed, since its O(N²) cost is negligible by
  comparison at these sizes.
- **Was this deliberate?** Yes — `qmf.rs`'s own doc names the tradeoff explicitly as
  "correctness-first." This candidate is not "undo a mistake," it is "the documented next step in a
  tradeoff whose cost this report is the first to actually measure."

### 2. HEVC frame/row threading (currently: none at all)

- **Evidence**: §4 — CPU% flat at 93-99% across 1/2/4/8/16 threads on 4K HEVC decode, measured with
  `/usr/bin/time -l` on the identical harness that shows H.264 climbing cleanly to 613% CPU at 8
  threads in the same session. This is not inferred from a profile share; it is a direct
  utilisation measurement showing zero parallelism exists.
- **Ceiling**: H.264's own row-threading work took 129%→615% CPU and 1.28x→4.05x wall-clock on
  comparable content (`E2E-GAPS.md` §21). If HEVC's serial fraction is similar, a comparable
  **~3-4x** wall-clock win at 8 threads is a reasonable target, closing HEVC's 26.5x
  default-thread ratio (§1) to roughly **7-9x** — a bigger absolute ratio improvement than any other
  single candidate in this list, because HEVC currently has literally nothing.
- **Risk / cost**: **architectural, large** — this is not "port the H.264 mechanism," it is a
  second full implementation of the same class of problem against a different decoder's data
  structures (`CuGrid`, HEVC's own DPB, `<sao::Snapshot>` state, HEVC's larger and more varied
  CU/PU partition shapes vs. H.264's fixed 16x16 macroblock grid, and SAO as a *second* in-loop
  filter after deblocking that row-publication would also need to lag behind correctly). `E2E-GAPS.md`
  §20/§21's own history is the sizing reference: **frame-level threading for H.264 was itself a
  DPB-refactor-sized change, deliberately sequenced after correctness work, and row-level threading
  after that was three separately-landed, separately-measured commits.** Expect HEVC's equivalent to
  be at least that size, likely larger given SAO. This is a multi-week project, not a "wire up the
  existing scheduler" task — `vaco-sched`'s `Driver::with_threads` machinery is pipeline-stage
  parallelism and (per `E2E-GAPS.md` §10's own finding for H.264) buys "almost nothing on a
  decode-bound job," so this would need the same from-scratch design H.264 got, not a reuse of it.
- **Sequencing note, inherited from H.264's own history**: `E2E-GAPS.md` §20 deliberately deferred
  H.264 frame threading until after the H.264 inter-prediction/weighted-prediction/`Intra_8x8` byte-
  exactness work landed, on the reasoning that parallelising a decoder still producing wrong pixels
  makes correctness bugs harder to bisect. This report did not check HEVC's own byte-exactness
  history in depth (a quick spot check in §1's fixture generation showed 4K HEVC byte-exact against
  ffmpeg on this session's own stock-libx265 fixture), but whoever picks this up should confirm HEVC
  is at the same correctness maturity H.264 was before its own threading work started.

### 3. HEVC `predict_block_intermediate` / motion compensation — investigate the same "bookkeeping vs arithmetic" question H.264's kernels already answered

- **Evidence**: §2.2 — 26.76% of single-threaded HEVC decode time, the largest single named cost
  in either codec's profile in this report.
- **Ceiling**: unknown without a deeper profile (this report did not do an *innermost*-frame
  breakdown of this function the way `E2E-GAPS.md` §19's round 4 did for H.264's
  `predict_chroma_inter` — that would be the next step, not this report's job). **Hypothesis, not
  evidence**: given H.264's own history (§10's `sample_luma_block`/`reconstruct_inter_mb` and
  `vaco-scale`'s `filter_h`, both bookkeeping-bound rather than arithmetic-bound once profiled
  properly), it would not be surprising if a meaningful fraction of this 26.76% is iterator/bounds-
  check scaffolding around HEVC's own variable-size interpolation loops (HEVC's PU sizes range 4x4
  to 64x64, a much wider variance than H.264's fixed 4x4/8x8 blocks, which is exactly the kind of
  runtime-determined trip count `vaco-scale`'s `filter_h` fix addressed). This is explicitly a
  hypothesis pending its own profile pass, not a result.
- **Risk / cost**: **local**, assuming the hypothesis holds (a bookkeeping fix, not a new
  representation) — but sizing is genuinely unknown until someone does the innermost-frame
  symbolication pass this report did not have time for.

### 4. HEVC SAO's `<Snapshot>::capture` — check whether it copies more than SAO reads

- **Evidence**: §2.2 — 8.08% of single-threaded HEVC decode time, a name ("Snapshot"/"capture")
  that strongly suggests a defensive full-state copy taken before applying offsets.
- **Ceiling**: **hypothesis, not evidence** — if this is a full-picture copy where SAO only reads a
  local neighbourhood (the way several of H.264's already-fixed issues were "the whole picture was
  copied/scanned when only a border/row was needed"), narrowing it could remove most of this 8.08%
  directly. Not measured or read in source this pass; flagged purely from the profile + the name.
- **Risk / cost**: **local** if the hypothesis holds — a scoping fix to one function, not a new
  design.

### 5. FLAC demux registration + the `progress limit exceeded: requested 65, cap 65` bug

- **Evidence**: §1 — a raw `.flac` file is misdetected as `cdg` by the format prober (no FLAC
  demuxer is registered at all), and even once FLAC audio reaches the decoder via a container the
  demuxer *does* understand, decode fails deterministically with the same "cap 65" error regardless
  of clip length (reproduced at both 3s and 30s).
- **Ceiling**: not a performance ceiling — this is a **correctness/reachability** gap, not a speed
  one. Fixing it does not move any ratio in §1's table; it turns a "no data" row into a measurable
  one. Listed here because it blocks ever measuring FLAC decode performance at all, and because a
  future agent should not spend time optimising FLAC decode speed before this is fixed — there is
  currently no way to reach the FLAC decoder's real-world performance through the CLI to verify any
  change helped.
- **Risk / cost**: **local** for the demuxer registration (add a `flac` demuxer entry the way every
  other raw-audio format already has one — `vaco-format-audio-simple` looks like the natural home
  given it already owns FLAC's *muxer*). The "cap 65" bug's size is unknown without investigating
  what sets that cap and why it is off by whatever factor is failing — not investigated this pass,
  out of scope for a measurement-only pass.

### 6. Opus decode registration (`vaco-codec-opus` exists, unwired)

- **Evidence**: §1 — the crate is a complete, tested, documented Opus decoder with no path from the
  CLI to it; `vaco-registry`'s `Cargo.toml` has no dependency on it under any name.
- **Ceiling**: **not a performance candidate at all** — there is no code to make faster until it is
  reachable, and this report cannot measure its performance for the same reason. Listed for
  completeness and because "Opus decode ratio" might otherwise get treated as a decode-speed problem
  by a planner working from a codec checklist rather than from this report.
- **Risk / cost**: **local** — add the dependency, a feature flag following the existing
  `codec-*`/`patent-encumbered-*` naming convention, and the `DECODERS` entry `vaco-registry`'s own
  generator (`cargo xtask gen-registry`) would need re-running for, per this project's own generated-
  file rules. Genuinely a wiring task, not a decoder-writing task — the decoder is already written.

### 7. A heap-allocation profile of H.264 4K decode

- **Evidence**: §5 — ~40-47 MiB of peak RSS per decoded 4K frame, against a 12 MiB raw frame size,
  a >3x multiplier this report did not decompose (no heap profiler was run). `E2E-GAPS.md` §20
  already found and *charged* one specific 59 MiB/picture allocation (`SliceStats::macroblocks`)
  without reducing it.
- **Ceiling**: **hypothesis, not evidence** — unknown without an actual allocation profile
  (`heaptrack`, Instruments, or an allocator-hooking crate). Could be large (if several
  per-picture structures are similarly oversized relative to what they need) or could be
  legitimately structural (motion-vector fields, per-macroblock summaries, and multiple in-flight
  pictures under threading all cost real memory by design). Framed as a measurement gap this report
  leaves open, not a performance claim.
- **Risk / cost**: unknown until measured — could be **local** (a few oversized structs, matching
  the `SliceStats::macroblocks` precedent) or could motivate an **architectural** discussion about
  the picture/DPB representation if the answer turns out to be structural. Not sized further here.

### Explicitly not re-proposed

Per §6's own list: any variant of "vectorize `boundary_strength`'s filter arithmetic further" (the
kernel itself is 2.67-3.41% of runtime and was already vectorised for a measured ~2.6% end-to-end
win; the surrounding scalar cost, not the arithmetic, is what mattered and is already fixed),
"batch deblocking's memory access pattern," "share position/weight derivation across chroma planes,"
or any interpolation-loop restructuring resembling round 2's three washes. All six have measured,
recorded, negative or negligible results and none of them changed in this pass's profile in a way
that would predict a different outcome today.

---

## 8. Load-immune measurement, added 2026-09-03

Nothing above this line was re-measured or changed. This section records a
second instrument that answers a narrower question than §1 does, and corrects
one sentence in the *Machine, toolchain, and load* table.

**The correction.** That table says *"No cycle counter is available — `unsafe` is
forbidden workspace-wide and a cycle-counter read needs it."* True for an
**in-process** counter read, and it is not the end of the analysis: an
*external* tool needs no `unsafe` in this tree. Valgrind's cachegrind
**simulates** execution and reports an exact instruction count, which is
deterministic rather than sampled and therefore does not move with machine load
at all. It has no macOS/Apple-silicon port, so it runs in an arm64 Linux
container, natively, on this same Mac.

`docs/instruction-count-benchmarking.md` has the whole design, the measurements
and the caveats. The three things worth knowing from here:

1. **CPU-seconds is not the load-immune metric this document assumed it was.**
   Measured on this machine, identical work costs **1.56x more CPU-seconds** on
   an efficiency core than on a performance core (0.28 s vs 0.17–0.20 s,
   8 interleaved rounds), and a single-threaded run migrates across both core
   types within 1.4 s. Instruction count under the same conditions moved by
   **25 instructions in 11.9 billion** while wall clock moved **4.55x**.
2. **Instruction count is not time, and must never be optimised alone.**
   Measured here: H.264 SD decode is **11.78x** ffmpeg's instruction count but
   only **4.34x** its wall clock, and MP3 decode is **20.07x** the instructions
   for **3.50x** the time. The reference's hand-written vector code retires far
   fewer instructions per unit of work. A change that lowers the instruction
   count by de-vectorising would read as a win and be a loss. §1's interleaved
   wall-clock protocol stays the ground truth, and it is the only one of the two
   that can see a threading result at all — valgrind serialises threads.
3. **§7 candidate 1 has landed, and it has a sibling that has not.** Commit
   `273d60fb` (2026-09-01) moved AAC's IMDCT off `vaco_tx::reference::imdct`.
   The instruction profile's first run found the same defect still present in
   MP3: `crates/codec/vaco-codec-mpegaudio/src/layer3.rs:442` calls that same
   O(n²) reference transform from `windowed_imdct` in the production path, and
   **74.38% of MP3 decode's instructions are inside libm `cos`**
   (5,720,068,487 of 7,690,615,486). A sweep found no other production caller.

`scripts/perf-baseline-bench.py` — the §1 harness — now samples the 1-minute load
average around every run, records it in the results JSON with an
`unusable_wall_clock` flag, and takes `--max-load` / `--refuse-under-load`. That
guard would have flagged the HEVC SD/720p rows in §1, whose 11x within-job spread
is called out there by hand.

### Real cycle totals, added 2026-09-04

`scripts/perf-hwcycles.py` adds the external hardware-counter path the first
instruction-count pass intentionally left open. On a Linux host whose kernel
exposes the PMU, it collects process-and-child cycles and instructions through
`perf stat`, interleaved against the same-session ffmpeg command for at least 10
rounds. It also records task clock, context switches, migrations, user/sys time,
and perf's percentage-running value; unsupported or meaningfully multiplexed
hardware events make the sample fail instead of turning into a synthetic number.

This Mac still has no trustworthy process-total cycle interface available to the
harness. Instruments' CPU Counters template samples per-core counters, and the
Docker Desktop Linux VM exposes no PMU. Cycle measurements therefore run on real
Linux hardware; macOS retains cachegrind instruction counts plus interleaved
wall/CPU context. `docs/instruction-count-benchmarking.md` gives the exact cycle
and Samply workflows and their limits.
